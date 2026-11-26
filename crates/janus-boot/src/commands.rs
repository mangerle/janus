//! 各 CLI 子命令的实现与提权策略。
//!
//! # 提权策略（Why）
//! 真实模式下写固件变量需要 root/管理员。CLI 侧在"确实要写"的命令里调用
//! [`require_elevated`]：不足时经 pkexec/sudo（Linux）或 UAC（Windows）
//! 重执行同一条命令并等待——子进程提权后重新走完整流程，父进程随后退出，
//! 输出无缝衔接。dry-run 与 `--no-elevate` 分别豁免与禁止该行为。

use std::sync::Arc;

use crate::autostart;
use crate::cli::{AutostartAction, Cli, Command};
use crate::config::Settings;
use crate::output::{print_entries, progress_printer};
use crate::settings::run_settings_window;
use crate::tray_actions::run_tray;
use anyhow::Context;
use janus_core::{
    contains_ignore_case, parse_boot_target, resolve_target, BootEntry, BootManager, JanusError,
    MockManager,
};

/// 主分发：按子命令路由到具体实现。
pub(crate) fn run(cli: &Cli) -> anyhow::Result<()> {
    // 原样保留命令行参数，供提权重执行使用
    let args: Vec<String> = std::env::args().skip(1).collect();

    match &cli.command {
        // 无子命令即托盘常驻：IDE 运行、双击可执行文件或快捷方式启动都直接进托盘
        None | Some(Command::Tray) => {
            run_tray(cli)?;
        }

        Some(Command::List) => {
            let mgr = backend(cli)?;
            let mut entries = list_entries_maybe_elevate(cli, &args, mgr.as_ref())?;
            let (settings, _) = Settings::load_from_default_location();
            settings.apply(&mut entries, cli.all);
            // BootNext 读取失败只影响标记展示，不中断列表输出，但需记录告警便于排查损坏数据
            let boot_next = match mgr.boot_next() {
                Ok(next) => next,
                Err(e) => {
                    log::warn!("读取 BootNext 失败（忽略）：{e}");
                    None
                }
            };
            print_entries(&entries, boot_next, cli.json);
        }

        Some(Command::Next { target }) => {
            let (id, name) = set_boot_next_for_target(cli, &args, target)?;
            println!("已设置下次启动：{id}（{name}）。重启后生效。");
        }

        Some(Command::Clear) => {
            let mgr = backend(cli)?;
            require_elevated(cli, &args)?;
            mgr.clear_boot_next().context("清除 BootNext 失败")?;
            println!("已清除 BootNext。");
        }

        Some(Command::Reboot { into }) => {
            if let Some(target) = into {
                let (id, name) = set_boot_next_for_target(cli, &args, target)?;
                println!("已设置下次启动：{id}（{name}）。");
            }
            do_reboot()?;
        }

        Some(Command::Settings) => {
            run_settings(cli)?;
        }

        Some(Command::Update {
            manifest,
            check,
            yes,
        }) => {
            run_update(manifest.as_deref(), *check, *yes)?;
        }

        Some(Command::Autostart { action }) => {
            run_autostart(cli, *action)?;
        }

        Some(Command::Apply { target, reboot }) => {
            run_apply(cli, target, *reboot)?;
        }
    }
    Ok(())
}

/// 解析目标启动项、按需提权并写入 BootNext（供 `next` 与 `reboot --into` 共用）。
fn set_boot_next_for_target(
    cli: &Cli,
    args: &[String],
    target: &str,
) -> anyhow::Result<(janus_core::BootId, String)> {
    let mgr = backend(cli)?;
    let entries = list_entries_maybe_elevate(cli, args, mgr.as_ref())?;
    let (settings, _) = Settings::load_from_default_location();
    let entry = resolve_with_settings(&entries, &settings, target)?;
    // 真实模式下写固件需要 root/管理员；不足时自动提权重执行本命令
    require_elevated(cli, args)?;
    mgr.set_boot_next(entry.id).context("写入 BootNext 失败")?;
    Ok((entry.id, entry.name))
}

/// 解析启动目标：优先按显示名（自定义名称或固件名）匹配，再回退固件原名，
/// 保证设置别名后仍可用编号或原始名称定位同一启动项。
fn resolve_with_settings(
    entries: &[BootEntry],
    settings: &Settings,
    target: &str,
) -> janus_core::Result<BootEntry> {
    let parsed = parse_boot_target(target);
    let found = match &parsed {
        janus_core::BootTarget::Id(_) => resolve_target(entries, &parsed)?,
        janus_core::BootTarget::Name(needle) => {
            match find_by_display_name(entries, settings, needle) {
                Some(entry) => entry,
                None => resolve_target(entries, &parsed)?,
            }
        }
        janus_core::BootTarget::IdOrName { id, name } => {
            match entries
                .iter()
                .find(|e| e.id == *id)
                .or_else(|| find_by_display_name(entries, settings, name))
            {
                Some(entry) => entry,
                None => resolve_target(entries, &parsed)?,
            }
        }
    };
    let mut matched = found.clone();
    settings.apply_names(std::slice::from_mut(&mut matched));
    Ok(matched)
}

/// 按显示名（自定义名优先，回退固件名）做不区分大小写的子串匹配。
fn find_by_display_name<'a>(
    entries: &'a [BootEntry],
    settings: &Settings,
    needle: &str,
) -> Option<&'a BootEntry> {
    entries.iter().find(|e| {
        let display_name = settings
            .entry(e.id)
            .and_then(|s| s.name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&e.name);
        contains_ignore_case(display_name, needle)
    })
}

/// `janus-boot settings`：打开图形设置窗口；dry-run 下窗口只读，不写配置文件。
fn run_settings(cli: &Cli) -> anyhow::Result<()> {
    let mgr: Arc<dyn BootManager> = Arc::from(backend(cli)?);
    let path = Settings::config_file_path().context("无法确定配置文件位置")?;
    run_settings_window(mgr, &path, cli.dry_run)
}

/// `janus-boot update`：检查 → 确认 → 下载验签 → 安装。
fn run_update(manifest: Option<&str>, check_only: bool, assume_yes: bool) -> anyhow::Result<()> {
    println!("当前版本 v{}，正在检查更新…", env!("CARGO_PKG_VERSION"));
    let Some(info) = janus_updater::check_update(manifest).map_err(update_error)? else {
        println!("已是最新版本。");
        return Ok(());
    };
    println!(
        "发现新版本：v{}{}",
        info.version(),
        info.pub_date()
            .map(|d| format!("（发布于 {d}）"))
            .unwrap_or_default()
    );
    if let Some(notes) = info.notes() {
        let notes = notes.trim();
        if !notes.is_empty() {
            println!("更新说明：\n{notes}");
        }
    }
    if check_only {
        return Ok(());
    }
    if !assume_yes && !confirm_prompt("是否下载并安装该更新？")? {
        println!("已取消更新。");
        return Ok(());
    }
    let downloaded = info
        .download_blocking(progress_printer())
        .map_err(update_error)?;
    println!("已下载并验签 v{}，开始安装…", downloaded.version());
    downloaded
        .install(progress_printer())
        .map_err(update_error)?;
    println!(
        "更新完成：v{} 已安装，下次运行 janus-boot 即生效。",
        downloaded.version()
    );
    Ok(())
}

/// `janus-boot autostart`：查询、开启或关闭开机自启。
fn run_autostart(cli: &Cli, action: Option<AutostartAction>) -> anyhow::Result<()> {
    match action.unwrap_or(AutostartAction::Status) {
        AutostartAction::Status => {
            let enabled = autostart::is_enabled()?;
            if cli.json {
                println!("{}", serde_json::json!({ "enabled": enabled }));
            } else if enabled {
                println!("开机自启：已开启");
            } else {
                println!("开机自启：未开启");
            }
        }
        AutostartAction::On => {
            if cli.dry_run {
                println!("[dry-run] 已模拟开启开机自启（未写入系统配置）。");
            } else {
                autostart::set_enabled(true)?;
                println!("已开启开机自启（将在登录后自动启动托盘）。");
            }
        }
        AutostartAction::Off => {
            if cli.dry_run {
                println!("[dry-run] 已模拟关闭开机自启（未修改系统配置）。");
            } else {
                autostart::set_enabled(false)?;
                println!("已关闭开机自启。");
            }
        }
    }
    Ok(())
}

/// 终端 y/N 确认。
fn confirm_prompt(prompt: &str) -> anyhow::Result<bool> {
    use std::io::Write as _;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// 把自更新错误并入 anyhow 调用链。
fn update_error(e: janus_updater::UpdaterError) -> JanusError {
    JanusError::Other(e.to_string())
}

/// 特权 helper（`janus-boot _apply`）：仅允许 root/管理员（或 dry-run）执行，
/// 防止普通用户直接调用。
fn run_apply(cli: &Cli, target: &str, reboot: bool) -> anyhow::Result<()> {
    if !cli.dry_run && !janus_privilege::is_elevated() {
        return Err(JanusError::PermissionDenied.into());
    }
    let mgr = backend(cli)?;
    if target == "clear" {
        mgr.clear_boot_next().context("清除 BootNext 失败")?;
        println!("已清除 BootNext。");
    } else {
        let entries = mgr.list_entries()?;
        let (settings, _) = Settings::load_from_default_location();
        let entry = resolve_with_settings(&entries, &settings, target)?;
        mgr.set_boot_next(entry.id).context("写入 BootNext 失败")?;
        println!("已设置 BootNext → {}", entry.id);
    }
    if reboot {
        do_reboot()?;
    }
    Ok(())
}

/// 系统重启。Linux 上由 root 执行时依赖 system_shutdown 的 `shutdown -r now`
/// 回退链；Windows 上走 ExitWindowsEx。
pub(crate) fn do_reboot() -> janus_core::Result<()> {
    system_shutdown::reboot().map_err(|e| JanusError::Other(format!("重启失败：{e}")))?;
    Ok(())
}

// ---------- 后端选择与权限 ----------

/// 按平台与 dry-run 标志选择启动管理器。
pub(crate) fn backend(cli: &Cli) -> anyhow::Result<Box<dyn BootManager>> {
    if cli.dry_run {
        return Ok(Box::new(MockManager::typical_dual_boot()));
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

/// 当前进程是否真正需要提权（dry-run 无需任何特权）。
fn needs_elevation(cli: &Cli) -> bool {
    !cli.dry_run && !janus_privilege::is_elevated()
}

/// 若未提权，则通过 pkexec/runas 以 root/管理员身份重新执行相同命令并等待。
/// 子进程成功接管后会直接退出当前进程；dry-run 与 --no-elevate 时直接返回。
pub(crate) fn require_elevated(cli: &Cli, args: &[String]) -> anyhow::Result<()> {
    if cli.dry_run || janus_privilege::is_elevated() {
        return Ok(());
    }
    if cli.no_elevate {
        return Err(JanusError::PermissionDenied.into());
    }
    let code = janus_privilege::reexec_elevated_wait(args)?;
    std::process::exit(code);
}

/// 列出启动项；Linux 下普通用户通常也能读 efivarfs，但个别系统只允许 root
/// 读取，此时读到的 PermissionDenied 会触发自动提权重执行。
fn list_entries_maybe_elevate(
    cli: &Cli,
    args: &[String],
    mgr: &dyn BootManager,
) -> anyhow::Result<Vec<BootEntry>> {
    match mgr.list_entries() {
        Ok(entries) => Ok(entries),
        Err(e) if is_denied(&e) && needs_elevation(cli) && !cli.no_elevate => {
            // 提权重执行成功会直接退出进程；这里到达即异常，返回原错误兜底
            require_elevated(cli, args)?;
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// 是否为权限不足类错误（含 io::ErrorKind::PermissionDenied）。
fn is_denied(e: &JanusError) -> bool {
    match e {
        JanusError::PermissionDenied => true,
        JanusError::Io(io) => io.kind() == std::io::ErrorKind::PermissionDenied,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_with_settings;
    use crate::config::{EntrySettings, Settings};
    use janus_core::{BootEntry, BootId};

    fn sample_entries() -> Vec<BootEntry> {
        vec![
            BootEntry {
                id: BootId::new(0x0001),
                name: "Windows Boot Manager".into(),
                active: true,
            },
            BootEntry {
                id: BootId::new(0x0002),
                name: "ubuntu".into(),
                active: true,
            },
        ]
    }

    #[test]
    fn resolve_with_settings_matches_alias_firmware_and_id() {
        let mut settings = Settings::default();
        settings.entries.insert(
            "Boot0002".into(),
            EntrySettings {
                hidden: false,
                name: Some("开发系统".into()),
            },
        );
        let entries = sample_entries();

        let by_alias = resolve_with_settings(&entries, &settings, "开发").unwrap();
        assert_eq!(by_alias.id, BootId::new(0x0002));
        assert_eq!(by_alias.name, "开发系统");

        // 改名后仍可用固件原名与编号匹配，且返回条目携带自定义名
        let by_firmware = resolve_with_settings(&entries, &settings, "ubuntu").unwrap();
        assert_eq!(by_firmware.id, BootId::new(0x0002));
        assert_eq!(by_firmware.name, "开发系统");

        let by_id = resolve_with_settings(&entries, &settings, "2").unwrap();
        assert_eq!(by_id.id, BootId::new(0x0002));
        assert_eq!(by_id.name, "开发系统");
    }
}
