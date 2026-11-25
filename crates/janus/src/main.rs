//! Janus —— 跨平台 UEFI 启动切换工具。
//!
//! 典型用法：
//! - `janus`                          不带子命令时默认常驻系统托盘
//! - `janus list`                     列出启动项（应用显示设置）
//! - `janus next ubuntu`              下次启动进入 ubuntu（不重启）
//! - `janus reboot --into 1`          设置 BootNext 并立即重启
//! - `janus settings`                 图形设置：选择显示项与自定义名称
//! - `janus tray`                     显式启动系统托盘
//! - `janus update`                   自更新（Ed25519 签名清单校验）
//! - `--dry-run`                      模拟模式，不触碰真实固件
//! - `--all`                          忽略显示设置，展示全部启动项与固件原名

// Windows 以 GUI 子系统链接：托盘/双击启动不创建控制台黑窗；
// CLI 形态在 main 入口附加父终端控制台（见 windows_console）
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod autostart;
mod cli;
mod commands;
mod config;
mod output;
mod settings;
mod tray_actions;
#[cfg(target_os = "windows")]
mod windows_console;

use std::process::ExitCode;

use clap::Parser;
use cli::Cli;

fn main() -> ExitCode {
    // 必须在任何输出/日志初始化之前完成：附加父终端控制台，
    // 托盘形态（无父控制台）保持全程无窗口
    #[cfg(target_os = "windows")]
    let console_attached = windows_console::attach_parent_console();

    // 诊断日志统一走 log 门面（env_logger 默认 info 级，可用 RUST_LOG 覆盖）；
    // 命令的数据输出（列表、JSON、进度条）是产品 stdout/stderr，豁免于门面
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 不用 Cli::parse 直接退出：无控制台（双击/快捷方式）时需要弹窗兜底
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            #[cfg(target_os = "windows")]
            if !console_attached {
                windows_console::show_error_dialog(&e.to_string());
            } else {
                let _ = e.print();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = e.print();
            }
            return ExitCode::from(e.exit_code() as u8);
        }
    };
    // 更新状态管理：特权 helper 以 root 运行（避免污染用户态状态文件）、
    // 设置窗口是托盘唤起的辅助进程且可能与托盘并发读写更新状态、dry-run
    // 不产生真实行为，三者都不参与自更新生命周期。
    let manage_update_state = !matches!(
        cli.command,
        Some(cli::Command::Apply { .. }) | Some(cli::Command::Settings)
    ) && !cli.dry_run;
    if manage_update_state {
        match janus_updater::check_and_recover_on_startup() {
            Ok(janus_updater::RecoveryStatus::RolledBack { from_version }) => {
                log::warn!("检测到 v{from_version} 连续启动失败，已自动回滚到上一稳定版本");
            }
            Ok(_) => {}
            Err(e) => log::warn!("更新自检失败（忽略）：{e}"),
        }
    }

    let result = commands::run(&cli);
    if manage_update_state && result.is_ok() {
        // 命令成功执行 = 新版本工作正常，确认升级闭环并清理备份
        if let Err(e) = janus_updater::confirm_update_success() {
            log::warn!("更新状态确认失败（忽略）：{e}");
        }
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // 顶层统一记录错误（anyhow 调用链，{:#} 展开上下文），禁止中途散落打印
            log::error!("命令执行失败：{e:#}");
            // 托盘形态无控制台：启动失败必须弹窗，否则用户毫无感知
            #[cfg(target_os = "windows")]
            if !console_attached {
                windows_console::show_error_dialog(&format!("命令执行失败：{e:#}"));
            }
            ExitCode::FAILURE
        }
    }
}
