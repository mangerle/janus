//! 托盘动作编排：托盘回调到"提权执行 / dry-run 模拟"的策略层。
//!
//! # 提权策略（Why）
//! Windows 上托盘进程自身需要管理员权限（UAC 只弹一次，启动时经
//! [`require_elevated`] 重执行）；Linux 上托盘以普通用户运行在桌面会话，
//! 写固件的动作按需经 pkexec 调起 `_apply` 特权 helper——polkit 密码框
//! 本身就是确认步骤，取消即反悔。

use std::sync::Arc;

use anyhow::Context;
use janus_core::{BootId, BootManager, Result};
// JanusError 仅 Linux 的 pkexec helper 使用
#[cfg(target_os = "linux")]
use janus_core::JanusError;

/// 托盘动作层共享的后端句柄（Arc 动态分发）。
type SharedManager = Arc<dyn BootManager>;

use crate::cli::Cli;
use crate::commands::do_reboot;
#[cfg(target_os = "windows")]
use crate::commands::require_elevated;

/// 启动托盘：准备后端与启动项数据，装配动作回调后进入事件循环。
pub(crate) fn run_tray(cli: &Cli) -> anyhow::Result<()> {
    // Windows 上托盘进程自身需要管理员权限（UAC 只弹一次）
    #[cfg(target_os = "windows")]
    {
        if !cli.dry_run {
            let args: Vec<String> = std::env::args().skip(1).collect();
            require_elevated(cli, &args)?;
        }
    }

    let mgr: SharedManager = Arc::from(backend_for_tray(cli)?);
    let entries = mgr.list_entries().context("读取启动项列表失败")?;

    let actions = build_tray_actions(cli, Arc::clone(&mgr));
    janus_tray::run(entries, actions)?;
    Ok(())
}

/// 托盘使用的后端选择（Linux 托盘无需特权，故不走 require_elevated）。
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn backend_for_tray(cli: &Cli) -> anyhow::Result<Box<dyn BootManager>> {
    if cli.dry_run {
        return Ok(Box::new(janus_core::MockManager::typical_dual_boot()));
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(
            janus_backend_linux::LinuxBootManager::for_running_system()?,
        ))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(
            janus_backend_windows::WindowsBootManager::for_running_system()?,
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(JanusError::Other("此平台暂不支持".into()).into())
    }
}

/// 装配托盘动作回调集合；每个动作拆分为独立构造函数，闭包保持扁平。
fn build_tray_actions(cli: &Cli, mgr: SharedManager) -> janus_tray::TrayActions {
    let confirm = make_confirm();
    let set_next = make_set_next(Arc::clone(&mgr), cli.dry_run);
    let switch_and_reboot = make_switch_and_reboot(Arc::clone(&mgr), cli.dry_run);
    let clear_next = make_clear_next(Arc::clone(&mgr), cli.dry_run);
    let reboot = make_reboot(cli.dry_run);
    let current_next = make_current_next(Arc::clone(&mgr));

    janus_tray::TrayActions {
        confirm,
        switch_and_reboot,
        set_next,
        clear_next,
        reboot,
        current_next,
    }
}

/// 确认回调：Linux 由 pkexec 密码框充当确认；Windows 用 MessageBoxW。
fn make_confirm() -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows_confirm)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Arc::new(|_| true)
    }
}

/// "仅设置下次启动"动作。
fn make_set_next(
    mgr: SharedManager,
    dry_run: bool,
) -> Arc<dyn Fn(BootId) -> Result<()> + Send + Sync> {
    Arc::new(move |id| set_next_action(&mgr, dry_run, id))
}

fn set_next_action(mgr: &SharedManager, dry_run: bool, id: BootId) -> Result<()> {
    if dry_run {
        mgr.set_boot_next(id)?;
        log::info!("[dry-run] 已设置 BootNext={id}");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        pkexec_apply(&format!("{:04X}", id.raw()), false)
    }
    #[cfg(target_os = "windows")]
    {
        mgr.set_boot_next(id)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(JanusError::Other("此平台暂不支持".into()))
    }
}

/// "设置 BootNext 并重启"动作。
fn make_switch_and_reboot(
    mgr: SharedManager,
    dry_run: bool,
) -> Arc<dyn Fn(BootId) -> Result<()> + Send + Sync> {
    Arc::new(move |id| switch_and_reboot_action(&mgr, dry_run, id))
}

fn switch_and_reboot_action(mgr: &SharedManager, dry_run: bool, id: BootId) -> Result<()> {
    if dry_run {
        mgr.set_boot_next(id)?;
        log::info!("[dry-run] 已设置 BootNext={id}，模拟重启完成");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        pkexec_apply(&format!("{:04X}", id.raw()), true)
    }
    #[cfg(target_os = "windows")]
    {
        mgr.set_boot_next(id)?;
        do_reboot()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(JanusError::Other("此平台暂不支持".into()))
    }
}

/// "清除 BootNext"动作。
fn make_clear_next(mgr: SharedManager, dry_run: bool) -> Arc<dyn Fn() -> Result<()> + Send + Sync> {
    Arc::new(move || clear_next_action(&mgr, dry_run))
}

fn clear_next_action(mgr: &SharedManager, dry_run: bool) -> Result<()> {
    if dry_run {
        mgr.clear_boot_next()?;
        log::info!("[dry-run] 已清除 BootNext");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        pkexec_apply("clear", false)
    }
    #[cfg(target_os = "windows")]
    {
        mgr.clear_boot_next()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(JanusError::Other("此平台暂不支持".into()))
    }
}

/// "立即重启"动作（不改启动项）。
fn make_reboot(dry_run: bool) -> Arc<dyn Fn() -> Result<()> + Send + Sync> {
    Arc::new(move || {
        if dry_run {
            log::info!("[dry-run] 模拟重启完成");
            return Ok(());
        }
        do_reboot()
    })
}

/// "查询当前 BootNext"动作：失败时返回 None（菜单信息行容错展示）。
fn make_current_next(mgr: SharedManager) -> Arc<dyn Fn() -> Option<BootId> + Send + Sync> {
    Arc::new(move || mgr.boot_next().ok().flatten())
}

/// Linux 托盘动作：通过 pkexec 以 root 身份调用自身的 `_apply` helper。
/// polkit 密码框即确认步骤；helper 完成 BootNext 写入与（可选）重启。
#[cfg(target_os = "linux")]
fn pkexec_apply(target: &str, reboot: bool) -> Result<()> {
    use std::process::Command;

    let exe = std::env::current_exe()
        .map_err(|e| JanusError::Elevation(format!("无法定位可执行文件：{e}")))?;
    let mut cmd = Command::new("pkexec");
    cmd.arg(&exe).arg("_apply").arg(target);
    if reboot {
        cmd.arg("--reboot");
    }
    let status = cmd
        .status()
        .map_err(|e| JanusError::Elevation(format!("pkexec 启动失败（需要 polkit agent）：{e}")))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(JanusError::Elevation(format!(
            "特权 helper 退出码 {code}（可能取消了授权）"
        ))),
        None => Err(JanusError::Elevation("特权 helper 被信号终止".into())),
    }
}

/// Windows 托盘确认对话框（MessageBoxW）。
#[cfg(target_os = "windows")]
fn windows_confirm(msg: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_YESNO,
    };

    let to_wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let msg_w = to_wide(msg);
    let title_w = to_wide("Janus 启动切换");
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(msg_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND,
        ) == IDYES
    }
}
