//! 设置窗口内的版本展示与自更新流程（检查、下载、验签、安装）。
//!
//! # 设计原理（Why）
//! - 网络与磁盘操作全部放在后台线程，UI 线程只轮询有界通道接收进度，
//!   避免检查更新或下载时窗口假死。
//! - 进度事件通过容量 64 的有界通道 `try_send` 转发：缓冲区满时丢弃中间
//!   进度事件（可合并），下载线程绝不会被 UI 阻塞；最终结果用阻塞 `send`
//!   保证送达。
//! - 更新失败若属于权限问题，附带"如何提权"的中文提示，因为 .deb 安装的
//!   二进制位于系统目录，普通用户无法原地替换。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::Duration;

use eframe::egui;
use janus_updater::{UpdateEvent, UpdateInfo};

/// 当前应用版本（编译期注入）。
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新流程状态机。
enum UpdateState {
    /// 尚未检查
    Idle,
    /// 正在读取远端清单
    Checking,
    /// 已是最新版本
    UpToDate,
    /// 发现新版本，等待用户确认下载
    Available {
        // 该结构体较大（约 400 字节），装箱避免撑大整个状态枚举
        info: Box<UpdateInfo>,
        notes: Option<String>,
        pub_date: Option<String>,
    },
    /// 正在下载/验签/安装
    Working {
        version: String,
        percent: Option<f32>,
        label: String,
    },
    /// 安装完成，等待重启生效
    Installed { version: String },
    /// 检查或安装失败
    Failed { message: String },
}

/// 后台线程发往 UI 的消息。
enum UpdateMessage {
    Checked(Result<Option<Box<UpdateInfo>>, String>),
    Progress { percent: Option<f32>, label: String },
    Installed(Result<String, String>),
}

/// “版本与更新”右侧可执行的操作。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AboutAction {
    Check,
    Retry,
    Install,
    None,
}

/// 设置窗口的更新面板（版本号 + 检查/下载/安装进度）。
pub(crate) struct UpdaterUi {
    read_only: bool,
    state: UpdateState,
    channel: Option<Receiver<UpdateMessage>>,
}

impl UpdaterUi {
    /// `read_only` 为 dry-run：展示版本号但禁止检查与安装。
    pub(crate) fn new(read_only: bool) -> Self {
        Self {
            read_only,
            state: UpdateState::Idle,
            channel: None,
        }
    }

    /// “版本与更新”分区：左侧当前版本与状态，右侧操作按钮；进度与说明紧随其后。
    pub(crate) fn about_ui(&mut self, ui: &mut egui::Ui) {
        self.poll(ui.ctx());
        let action = self.action();
        let status = self.status_text();
        let mut check = false;
        let mut install = false;

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("版本与更新").size(12.0).strong(),
                ));
                ui.add(egui::Label::new(
                    egui::RichText::new(status).size(11.0).weak(),
                ));
            });
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match action {
                    AboutAction::Check | AboutAction::Retry => {
                        let text = if action == AboutAction::Retry {
                            "重试"
                        } else {
                            "检查更新"
                        };
                        let button = ui.add_enabled(!self.read_only, ghost_button(ui, text));
                        if button.clicked() {
                            check = true;
                        }
                        if self.read_only {
                            button.on_hover_text("dry-run 模式：不检查更新");
                        }
                    }
                    AboutAction::Install => {
                        let accent = ui.visuals().selection.bg_fill;
                        let ink = ui.visuals().selection.stroke.color;
                        let button = egui::Button::new(
                            egui::RichText::new("下载并安装").size(12.0).color(ink),
                        )
                        .fill(accent);
                        if ui.add(button).clicked() {
                            install = true;
                        }
                    }
                    AboutAction::None => {}
                },
            );
        });

        if check {
            self.start_check(ui.ctx());
        } else if install {
            self.start_install(ui.ctx());
        }
        self.details(ui);
    }

    /// 当前状态下“版本与更新”右侧应显示的操作。
    fn action(&self) -> AboutAction {
        match &self.state {
            UpdateState::Idle | UpdateState::UpToDate => AboutAction::Check,
            UpdateState::Failed { .. } => AboutAction::Retry,
            UpdateState::Available { .. } => AboutAction::Install,
            UpdateState::Checking | UpdateState::Working { .. } | UpdateState::Installed { .. } => {
                AboutAction::None
            }
        }
    }

    fn status_text(&self) -> String {
        match &self.state {
            UpdateState::Idle => format!("Janus v{CURRENT_VERSION}"),
            UpdateState::Checking => format!("Janus v{CURRENT_VERSION} · 正在检查更新…"),
            UpdateState::UpToDate => format!("Janus v{CURRENT_VERSION} · 已是最新版本"),
            UpdateState::Available { info, .. } => {
                format!("Janus v{CURRENT_VERSION} · 发现新版本 v{}", info.version())
            }
            UpdateState::Working { version, .. } => {
                format!("Janus v{CURRENT_VERSION} · 正在更新到 v{version}…")
            }
            UpdateState::Installed { version } => {
                format!("Janus v{CURRENT_VERSION} · 已安装 v{version}，重启后生效")
            }
            UpdateState::Failed { .. } => format!("Janus v{CURRENT_VERSION} · 更新检查失败"),
        }
    }

    /// 更新详情：检查中、更新说明、进度、完成或错误提示。
    fn details(&mut self, ui: &mut egui::Ui) {
        match &self.state {
            UpdateState::Idle | UpdateState::UpToDate => {}
            UpdateState::Checking => {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("正在读取更新清单…").size(11.0).weak());
                });
            }
            UpdateState::Available {
                info,
                notes,
                pub_date,
            } => {
                let version = info.version();
                let pub_date = pub_date.clone();
                let notes = notes.clone();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("新版本 v{version}")).size(11.0));
                    if let Some(date) = pub_date {
                        ui.label(
                            egui::RichText::new(format!("发布于 {date}"))
                                .size(11.0)
                                .weak(),
                        );
                    }
                });
                if let Some(notes) = notes {
                    let notes = notes.trim();
                    if !notes.is_empty() {
                        egui::CollapsingHeader::new("更新说明")
                            .default_open(false)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(110.0)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new(notes).size(11.0));
                                    });
                            });
                    }
                }
            }
            UpdateState::Working {
                version,
                percent,
                label,
            } => {
                ui.add_space(4.0);
                let mut bar =
                    egui::ProgressBar::new(percent.unwrap_or(0.0)).animate(percent.is_none());
                if percent.is_some() {
                    bar = bar.show_percentage();
                }
                ui.add(bar);
                ui.label(
                    egui::RichText::new(format!("v{version} · {label}"))
                        .size(11.0)
                        .weak(),
                );
            }
            UpdateState::Installed { version } => {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "v{version} 已安装。若当前仍在运行旧版本，请重启 janus（含托盘）后生效。"
                    ))
                    .size(11.0),
                );
            }
            UpdateState::Failed { message } => {
                ui.add_space(4.0);
                let color = ui.visuals().error_fg_color;
                ui.label(egui::RichText::new(message).size(11.0).color(color));
            }
        }
    }

    /// 轮询后台消息；检查/安装期间请求持续重绘。
    fn poll(&mut self, ctx: &egui::Context) {
        let messages: Vec<UpdateMessage> = self
            .channel
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        if !messages.is_empty() {
            for message in messages {
                self.handle(message);
            }
            ctx.request_repaint();
        }
        if matches!(
            self.state,
            UpdateState::Checking | UpdateState::Working { .. }
        ) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn handle(&mut self, message: UpdateMessage) {
        match message {
            UpdateMessage::Checked(Ok(Some(info))) => {
                let notes = info.notes();
                let pub_date = info.pub_date();
                self.state = UpdateState::Available {
                    info,
                    notes,
                    pub_date,
                };
                self.channel = None;
            }
            UpdateMessage::Checked(Ok(None)) => {
                self.state = UpdateState::UpToDate;
                self.channel = None;
            }
            UpdateMessage::Checked(Err(message)) => {
                self.state = UpdateState::Failed {
                    message: with_error_hint(message),
                };
                self.channel = None;
            }
            UpdateMessage::Progress { percent, label } => {
                if let UpdateState::Working {
                    percent: current,
                    label: current_label,
                    ..
                } = &mut self.state
                {
                    // 阶段事件不带百分比：保留最近一次下载进度，避免进度条跳回 0
                    if percent.is_some() {
                        *current = percent;
                    }
                    *current_label = label;
                }
            }
            UpdateMessage::Installed(Ok(version)) => {
                self.state = UpdateState::Installed { version };
                self.channel = None;
            }
            UpdateMessage::Installed(Err(message)) => {
                self.state = UpdateState::Failed {
                    message: with_error_hint(message),
                };
                self.channel = None;
            }
        }
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        let (tx, rx) = sync_channel(4);
        self.channel = Some(rx);
        self.state = UpdateState::Checking;
        let spawn = std::thread::Builder::new()
            .name("janus-update-check".into())
            .spawn(move || {
                let result = janus_updater::check_update(None)
                    .map(|update| update.map(Box::new))
                    .map_err(|e| e.to_string());
                let _ = tx.send(UpdateMessage::Checked(result));
            });
        if let Err(e) = spawn {
            self.state = UpdateState::Failed {
                message: format!("启动更新检查线程失败：{e}"),
            };
            self.channel = None;
        }
        ctx.request_repaint();
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        let previous = std::mem::replace(&mut self.state, UpdateState::Idle);
        let UpdateState::Available { info, .. } = previous else {
            self.state = previous;
            return;
        };
        let version = info.version();
        let (tx, rx) = sync_channel(64);
        self.channel = Some(rx);
        self.state = UpdateState::Working {
            version: version.clone(),
            percent: None,
            label: "准备下载…".into(),
        };

        let spawn = std::thread::Builder::new()
            .name("janus-update-install".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let downloaded =
                    info.download_blocking(move |event| forward_event(&progress_tx, event));
                let result = match downloaded {
                    Ok(downloaded) => {
                        let install_tx = tx.clone();
                        downloaded
                            .install(move |event| forward_event(&install_tx, event))
                            .map_err(|e| e.to_string())
                    }
                    Err(e) => Err(e.to_string()),
                };
                let message = match result {
                    Ok(()) => UpdateMessage::Installed(Ok(version)),
                    Err(e) => UpdateMessage::Installed(Err(e)),
                };
                let _ = tx.send(message);
            });
        if let Err(e) = spawn {
            self.state = UpdateState::Failed {
                message: format!("启动更新线程失败：{e}"),
            };
            self.channel = None;
        }
        ctx.request_repaint();
    }
}

/// 把更新器事件转换为 UI 消息；进度事件用 `try_send` 丢弃可合并的中间态。
fn forward_event(tx: &SyncSender<UpdateMessage>, event: UpdateEvent) {
    let message = match event {
        UpdateEvent::DownloadProgress { percent, .. } => UpdateMessage::Progress {
            // ProgressBar 使用 0..=1；更新器给出的是 0..=100 百分比
            percent: percent.map(|p| p / 100.0),
            label: "正在下载安装包…".into(),
        },
        UpdateEvent::Stage(stage) => UpdateMessage::Progress {
            percent: None,
            label: stage.label().into(),
        },
        UpdateEvent::Retrying {
            attempt,
            max_retries,
            error,
        } => UpdateMessage::Progress {
            percent: None,
            label: format!("下载重试 {attempt}/{max_retries}：{error}"),
        },
        UpdateEvent::Failed { reason } => UpdateMessage::Progress {
            percent: None,
            label: format!("更新失败：{reason}"),
        },
        _ => return,
    };
    let _ = tx.try_send(message);
}

/// 次要操作用幽灵按钮：透明底、强调色文字、细边框。
fn ghost_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let accent = ui.visuals().selection.bg_fill;
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .size(12.0)
            .color(accent),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
}

/// 失败信息补充可操作提示：权限问题给出提权方式，清单 404 说明发布侧缺失。
fn with_error_hint(message: String) -> String {
    let denied = message.contains("Permission denied")
        || message.contains("权限")
        || message.contains("Access is denied")
        || message.contains("os error 13");
    if denied {
        return format!(
            "{message}\n安装目录不可写：请以管理员/root 重新运行 janus update，\
             或把 janus 安装到用户可写目录。"
        );
    }
    if message.contains("404") && message.contains("latest.json") {
        return format!(
            "{message}\n发布页尚未提供 latest.json 更新清单；\
             完成一次带清单的正式发布后即可检查更新。"
        );
    }
    message
}
