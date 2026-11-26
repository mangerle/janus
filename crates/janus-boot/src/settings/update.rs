//! 设置窗口内的版本展示与自更新面板渲染。

use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;

use super::update_worker::{spawn_check, spawn_install, AboutAction, UpdateMessage, UpdateState};
use super::widgets::{ghost_button, primary_button};

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
        let action = self.state.action();
        let status = self.state.status_text();
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
                        let button = primary_button(ui, "下载并安装");
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
                        "v{version} 已安装。若当前仍在运行旧版本，请重启 janus-boot（含托盘）后生效。"
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
                if self.state.apply_message(message) {
                    self.channel = None;
                }
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

    fn start_check(&mut self, ctx: &egui::Context) {
        match spawn_check() {
            Ok(rx) => {
                self.channel = Some(rx);
                self.state = UpdateState::Checking;
            }
            Err(message) => {
                self.state = UpdateState::Failed { message };
                self.channel = None;
            }
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
        match spawn_install(info) {
            Ok(rx) => {
                self.channel = Some(rx);
                self.state = UpdateState::Working {
                    version,
                    percent: None,
                    label: "准备下载…".into(),
                };
            }
            Err(message) => {
                self.state = UpdateState::Failed { message };
                self.channel = None;
            }
        }
        ctx.request_repaint();
    }
}
