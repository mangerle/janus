//! Janus —— 跨平台 UEFI 启动切换工具。
//!
//! 典型用法：
//! - `janus list`                     列出启动项
//! - `janus next ubuntu`              下次启动进入 ubuntu（不重启）
//! - `janus reboot --into 1`          设置 BootNext 并立即重启
//! - `janus tray`                     常驻系统托盘
//! - `janus update`                   自更新（Ed25519 签名清单校验）
//! - `--dry-run`                      模拟模式，不触碰真实固件

mod cli;
mod commands;
mod output;
mod tray_actions;

use std::process::ExitCode;

use clap::Parser;
use cli::Cli;

fn main() -> ExitCode {
    // 诊断日志统一走 log 门面（env_logger 默认 info 级，可用 RUST_LOG 覆盖）；
    // 命令的数据输出（列表、JSON、进度条）是产品 stdout/stderr，豁免于门面
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    // 更新状态管理：特权 helper 以 root 运行（避免污染用户态状态文件）、
    // dry-run 不产生真实行为，两者都不参与自更新生命周期。
    let manage_update_state = !matches!(cli.command, cli::Command::Apply { .. }) && !cli.dry_run;
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
            ExitCode::FAILURE
        }
    }
}
