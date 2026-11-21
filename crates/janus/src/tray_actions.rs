//! 托盘动作编排：托盘回调到"提权执行 / dry-run 模拟"的策略层。
//!
//! # 提权策略（Why）
//! Windows 上托盘进程自身需要管理员权限（UAC 只弹一次，启动时经
//! [`require_elevated`] 重执行）；Linux 上托盘以普通用户运行在桌面会话，
//! 写固件的动作按需经 pkexec 调起 `_apply` 特权 helper——polkit 密码框
//! 本身就是确认步骤，取消即反悔。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use janus_core::{BootEntry, BootId, BootManager, Result};
// JanusError 仅 Linux 的 pkexec helper 使用
#[cfg(target_os = "linux")]
use janus_core::JanusError;

use crate::cli::Cli;
use crate::commands::do_reboot;
#[cfg(target_os = "windows")]
use crate::commands::require_elevated;
use crate::config::Settings;

/// 托盘动作层共享的后端句柄（Arc 动态分发）。
type SharedManager = Arc<dyn BootManager>;

/// 设置窗口打开标志的复位守卫：即使唤起线程 panic 展开，也能复位入口，
/// 避免"设置…"菜单项永久失效。
struct ResetFlag(Arc<AtomicBool>);

impl Drop for ResetFlag {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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
    let (settings, config_path) = Settings::load_from_default_location();
    let mut entries = mgr.list_entries().context("读取启动项列表失败")?;
    settings.apply(&mut entries, cli.all);

    let actions = build_tray_actions(cli, Arc::clone(&mgr), config_path);
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
fn build_tray_actions(
    cli: &Cli,
    mgr: SharedManager,
    config_path: Option<PathBuf>,
) -> janus_tray::TrayActions {
    let confirm = make_confirm();
    let set_next = make_set_next(Arc::clone(&mgr), cli.dry_run);
    let switch_and_reboot = make_switch_and_reboot(Arc::clone(&mgr), cli.dry_run);
    let clear_next = make_clear_next(Arc::clone(&mgr), cli.dry_run);
    let reboot = make_reboot(cli.dry_run);
    let current_next = make_current_next(Arc::clone(&mgr));
    let reload_entries = make_reload_entries(Arc::clone(&mgr), config_path, cli.all);
    let open_settings = make_open_settings(cli.dry_run);

    janus_tray::TrayActions {
        confirm,
        switch_and_reboot,
        set_next,
        clear_next,
        reboot,
        current_next,
        reload_entries,
        open_settings,
    }
}

/// 重载回调：重新读取固件启动项并按最新配置过滤/改名；失败时由托盘保留旧列表。
fn make_reload_entries(
    mgr: SharedManager,
    config_path: Option<PathBuf>,
    show_all: bool,
) -> Arc<dyn Fn() -> Result<Vec<BootEntry>> + Send + Sync> {
    Arc::new(move || {
        let mut entries = mgr.list_entries()?;
        let settings = config_path
            .as_deref()
            .map(Settings::load_or_default)
            .unwrap_or_default();
        settings.apply(&mut entries, show_all);
        Ok(entries)
    })
}

/// 打开设置窗口：以子进程运行 `janus settings`，让 winit 事件循环始终位于
/// 子进程主线程——winit 0.30 在 Linux 上禁止非主线程初始化事件循环，直接在
/// 托盘线程内起 eframe 会 panic。子进程退出即窗口关闭，托盘随后重载配置；
/// 同一时刻只允许一个设置窗口。
fn make_open_settings(dry_run: bool) -> Arc<dyn Fn() + Send + Sync> {
    let opening = Arc::new(AtomicBool::new(false));
    Arc::new(move || {
        if opening.swap(true, Ordering::AcqRel) {
            return;
        }
        let _reset = ResetFlag(Arc::clone(&opening));
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                let msg = format!("打开设置窗口失败：无法定位可执行文件：{e}");
                log::error!("{msg}");
                #[cfg(target_os = "windows")]
                crate::windows_console::show_error_dialog(&msg);
                return;
            }
        };
        let mut cmd = Command::new(exe);
        cmd.arg("settings");
        if dry_run {
            cmd.arg("--dry-run");
        }
        match cmd.status() {
            Ok(status) if status.success() => {}
            Ok(status) => log::error!("设置窗口异常退出：{status}"),
            Err(e) => {
                let msg = format!("启动设置窗口失败：{e}");
                log::error!("{msg}");
                // 托盘无控制台，Windows 上弹窗保证用户可感知
                #[cfg(target_os = "windows")]
                crate::windows_console::show_error_dialog(&msg);
            }
        }
    })
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

/// "仅下次启动"动作。
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
