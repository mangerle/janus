//! 命令行接口定义（clap derive）。

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "janus",
    version,
    about = "Janus —— 跨平台 UEFI 启动切换工具（写 BootNext + 重启，托盘常驻可选）",
    after_help = "启动目标参数支持：BootXXXX 编号（十六进制，如 1 / 0x0002 / Boot0001）或名称子串（不区分大小写）"
)]
pub(crate) struct Cli {
    /// 模拟模式：使用内置 Mock 后端，不读写真实固件
    #[arg(long, global = true)]
    pub(crate) dry_run: bool,

    /// list 命令输出 JSON
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// 禁止自动提权（pkexec/sudo/UAC）
    #[arg(long, global = true)]
    pub(crate) no_elevate: bool,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// 列出 UEFI 启动项（按 BootOrder 顺序）
    List,

    /// 设置下次启动项（BootNext），重启后生效
    Next {
        /// 启动项编号（十六进制）或名称子串
        target: String,
    },

    /// 清除 BootNext
    Clear,

    /// 立即重启；配合 --into 可先设置 BootNext
    Reboot {
        /// 重启进入的目标启动项（编号或名称子串）
        #[arg(long)]
        into: Option<String>,
    },

    /// 启动系统托盘（常驻）
    Tray,

    /// 检查并安装新版本（shipup 签名清单自更新）
    Update {
        /// 覆盖清单端点（默认 GitHub Releases；测试可用 file://）
        #[arg(long)]
        manifest: Option<String>,
        /// 仅检查新版本，不下载安装
        #[arg(long)]
        check: bool,
        /// 跳过安装前的交互确认
        #[arg(long)]
        yes: bool,
    },

    /// 内部命令：特权 helper（由 pkexec/runas 提权后调用），勿手动执行
    #[command(hide = true)]
    Apply {
        /// "clear" 或启动项编号/名称子串
        target: String,
        /// 设置完成后立即重启
        #[arg(long)]
        reboot: bool,
    },
}
