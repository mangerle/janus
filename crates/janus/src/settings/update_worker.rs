//! 自更新状态机、后台线程调度与事件/错误映射（与 egui 渲染解耦）。
//!
//! # 设计原理（Why）
//! - 网络与磁盘操作全部放在后台线程，UI 线程只轮询有界通道接收进度，
//!   避免检查更新或下载时窗口假死。
//! - 进度事件通过容量 64 的有界通道 `try_send` 转发：缓冲区满时丢弃中间
//!   进度事件（可合并），下载线程绝不会被 UI 阻塞；最终结果用阻塞 `send`
//!   保证送达。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use janus_updater::{UpdateEvent, UpdateInfo};

/// 当前应用版本（编译期注入）。
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新流程状态机。
pub(crate) enum UpdateState {
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
pub(crate) enum UpdateMessage {
    Checked(Result<Option<Box<UpdateInfo>>, String>),
    Progress { percent: Option<f32>, label: String },
    Installed(Result<String, String>),
}

/// “版本与更新”右侧可执行的操作。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AboutAction {
    Check,
    Retry,
    Install,
    None,
}

impl UpdateState {
    pub(crate) fn action(&self) -> AboutAction {
        match self {
            Self::Idle | Self::UpToDate => AboutAction::Check,
            Self::Failed { .. } => AboutAction::Retry,
            Self::Available { .. } => AboutAction::Install,
            Self::Checking | Self::Working { .. } | Self::Installed { .. } => AboutAction::None,
        }
    }

    pub(crate) fn status_text(&self) -> String {
        match self {
            Self::Idle => format!("Janus v{CURRENT_VERSION}"),
            Self::Checking => format!("Janus v{CURRENT_VERSION} · 正在检查更新…"),
            Self::UpToDate => format!("Janus v{CURRENT_VERSION} · 已是最新版本"),
            Self::Available { info, .. } => {
                format!("Janus v{CURRENT_VERSION} · 发现新版本 v{}", info.version())
            }
            Self::Working { version, .. } => {
                format!("Janus v{CURRENT_VERSION} · 正在更新到 v{version}…")
            }
            Self::Installed { version } => {
                format!("Janus v{CURRENT_VERSION} · 已安装 v{version}，重启后生效")
            }
            Self::Failed { .. } => format!("Janus v{CURRENT_VERSION} · 更新检查失败"),
        }
    }

    /// 消费一条后台消息并推进状态；返回 `true` 表示本轮任务已结束、可清理通道。
    pub(crate) fn apply_message(&mut self, message: UpdateMessage) -> bool {
        match message {
            UpdateMessage::Checked(Ok(Some(info))) => {
                let notes = info.notes();
                let pub_date = info.pub_date();
                *self = Self::Available {
                    info,
                    notes,
                    pub_date,
                };
                true
            }
            UpdateMessage::Checked(Ok(None)) => {
                *self = Self::UpToDate;
                true
            }
            UpdateMessage::Checked(Err(message)) | UpdateMessage::Installed(Err(message)) => {
                *self = Self::Failed {
                    message: with_error_hint(message),
                };
                true
            }
            UpdateMessage::Progress { percent, label } => {
                if let Self::Working {
                    percent: current,
                    label: current_label,
                    ..
                } = self
                {
                    // 阶段事件不带百分比：保留最近一次下载进度，避免进度条跳回 0
                    if percent.is_some() {
                        *current = percent;
                    }
                    *current_label = label;
                }
                false
            }
            UpdateMessage::Installed(Ok(version)) => {
                *self = Self::Installed { version };
                true
            }
        }
    }
}

/// 启动后台检查更新线程，返回用于轮询结果的有界接收端。
pub(crate) fn spawn_check() -> Result<Receiver<UpdateMessage>, String> {
    let (tx, rx) = sync_channel(4);
    std::thread::Builder::new()
        .name("janus-update-check".into())
        .spawn(move || {
            let result = janus_updater::check_update(None)
                .map(|update| update.map(Box::new))
                .map_err(|e| e.to_string());
            let _ = tx.send(UpdateMessage::Checked(result));
        })
        .map_err(|e| format!("启动更新检查线程失败：{e}"))?;
    Ok(rx)
}

/// 启动后台下载、验签与安装线程，返回用于轮询进度与结果的有界接收端。
pub(crate) fn spawn_install(info: Box<UpdateInfo>) -> Result<Receiver<UpdateMessage>, String> {
    let version = info.version();
    let (tx, rx) = sync_channel(64);
    std::thread::Builder::new()
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
        })
        .map_err(|e| format!("启动更新线程失败：{e}"))?;
    Ok(rx)
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

/// 失败信息补充可操作提示：权限问题给出提权方式，清单 404 说明发布侧缺失。
pub(crate) fn with_error_hint(message: String) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_hints_append_actionable_guidance() {
        let denied = with_error_hint("Permission denied (os error 13)".into());
        assert!(denied.contains("安装目录不可写"));

        let missing_manifest = with_error_hint("HTTP 404 Not Found: latest.json".into());
        assert!(missing_manifest.contains("尚未提供 latest.json"));

        let plain = with_error_hint("网络超时".into());
        assert_eq!(plain, "网络超时");
    }

    #[test]
    fn updater_state_transitions_and_progress_preserve_percent() {
        let mut state = UpdateState::Idle;
        assert!(matches!(state.action(), AboutAction::Check));

        state = UpdateState::Working {
            version: "0.2.0".into(),
            percent: Some(0.5),
            label: "下载中".into(),
        };
        // 阶段事件不带百分比时应保留已有进度 0.5
        let finished = state.apply_message(UpdateMessage::Progress {
            percent: None,
            label: "正在校验 SHA-256…".into(),
        });
        assert!(!finished);
        assert!(matches!(
            state,
            UpdateState::Working {
                percent: Some(p),
                ref label,
                ..
            } if (p - 0.5).abs() < f32::EPSILON && label == "正在校验 SHA-256…"
        ));

        assert!(state.apply_message(UpdateMessage::Installed(Ok("0.2.0".into()))));
        assert!(matches!(state.action(), AboutAction::None));

        assert!(state.apply_message(UpdateMessage::Checked(Ok(None))));
        assert!(matches!(state, UpdateState::UpToDate));
        assert!(matches!(state.action(), AboutAction::Check));
    }
}
