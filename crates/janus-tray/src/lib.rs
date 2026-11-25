//! Janus 系统托盘模块。
//!
//! 平台差异：
//! - **Linux**：使用 tray-icon 的 `ksni` 后端（纯 D-Bus 的 StatusNotifierItem，
//!   无需 libappindicator/GTK）。ksni 后端自带 D-Bus worker 线程，因此主线程
//!   只需阻塞在菜单事件 channel 上。
//! - **Windows**：使用 tray-icon 的 Win32 通知区实现，需要 tao 事件循环泵
//!   消息，并用后台线程把菜单事件转发给事件循环。
//!
//! # 设计原理（Why）
//! 托盘本身不做提权：所有写固件的动作都通过 [`TrayActions`] 回调交由上层
//! （janus 主程序）决定是直接执行、pkexec 提权还是 dry-run 模拟——这使
//! 托盘层保持平台中立，权限策略集中在单一位置可审计。

use std::sync::Arc;

use janus_core::{BootEntry, BootId, Result};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod menu;
#[cfg(target_os = "windows")]
mod windows;

/// 应用高清图标（128x128 RGBA 字节，编译期嵌入），供托盘与设置窗口共享，
/// 避免上层 crate 通过相对路径穿透本 crate 内部目录。
pub const APP_ICON_128_RGBA: &[u8] = include_bytes!("../assets/icon-128.rgba");
/// [`APP_ICON_128_RGBA`] 的边长（像素）。
pub const APP_ICON_128_DIM: u32 = 128;

/// 托盘动作回调集合（由上层提供）。
pub struct TrayActions {
    /// 确认对话框；返回 `false` 取消。Linux 上通常直接返回 `true`——
    /// pkexec 的密码输入框本身就是确认步骤。
    pub confirm: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// 设置 BootNext 并立即重启。
    pub switch_and_reboot: Arc<dyn Fn(BootId) -> Result<()> + Send + Sync>,
    /// 仅设置 BootNext（不重启）。
    pub set_next: Arc<dyn Fn(BootId) -> Result<()> + Send + Sync>,
    /// 清除 BootNext。
    pub clear_next: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    /// 立即重启（不改启动项）。
    pub reboot: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    /// 查询当前 BootNext（用于菜单信息展示）。
    pub current_next: Arc<dyn Fn() -> Option<BootId> + Send + Sync>,
    /// 重新读取启动项并按最新显示设置过滤/改名；设置保存后重建菜单用。
    pub reload_entries: Arc<dyn Fn() -> Result<Vec<BootEntry>> + Send + Sync>,
    /// 打开设置窗口（阻塞直到窗口关闭）；托盘在后台线程调用。
    pub open_settings: Arc<dyn Fn() + Send + Sync>,
}

/// 启动托盘并进入事件循环（阻塞直到用户点击"退出"）。
///
/// # Errors
/// 托盘创建失败（无 D-Bus 会话、无可用通知区等）时返回
/// [`JanusError::Other`]；事件循环内的动作失败只记录日志，不中断托盘。
pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::run(entries, actions)
    }
    #[cfg(target_os = "windows")]
    {
        windows::run(entries, actions)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (entries, actions);
        Err(janus_core::JanusError::Other("此平台暂不支持托盘".into()))
    }
}

/// 菜单项 ID 前缀约定。
pub(crate) const ID_BOOT: &str = "boot:"; // 重启并进入该系统
pub(crate) const ID_NEXT: &str = "next:"; // 仅下次启动
pub(crate) const ID_INFO: &str = "janus:info";
pub(crate) const ID_EMPTY: &str = "janus:empty";
pub(crate) const ID_CLEAR: &str = "janus:clear";
pub(crate) const ID_REBOOT: &str = "janus:reboot";
pub(crate) const ID_SETTINGS: &str = "janus:settings";
pub(crate) const ID_QUIT: &str = "janus:quit";

/// 菜单事件处理结果（平台循环据此决定退出/开设置窗口）。
pub(crate) enum MenuOutcome {
    /// 普通动作，菜单保持运行
    Continue,
    /// 用户点击"退出"
    Quit,
    /// 用户点击"设置…"，需在后台线程打开设置窗口
    OpenSettings,
}
