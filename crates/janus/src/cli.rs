//! 命令行接口定义（clap derive）。

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "janus",
    version,
    about = "Janus —— 跨平台 UEFI 启动切换工具（写 BootNext + 重启，托盘常驻 + CLI）",
    after_help = "不带子命令时默认启动托盘；启动目标参数支持：BootXXXX 编号（十六进制，如 1 / 0x0002 / Boot0001）或名称子串（不区分大小写）"
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

    /// 忽略显示设置：展示全部启动项并使用固件原名
    #[arg(long, global = true)]
    pub(crate) all: bool,

    /// 子命令；缺省时直接启动托盘（等价于 `tray`），便于 IDE 运行、
    /// 双击可执行文件或快捷方式启动时进入常驻形态
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
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

    /// 启动系统托盘（常驻；不带子命令时的默认行为）
    Tray,

    /// 打开图形设置窗口：选择显示的启动项并自定义显示名称
    Settings,

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

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn no_subcommand_parses_to_none() {
        // 无参数必须合法解析（commands::run 据此默认进入托盘）
        let cli = Cli::try_parse_from(["janus"]).expect("无参数应解析成功");
        assert!(cli.command.is_none());
    }

    #[test]
    fn explicit_tray_subcommand_still_parses() {
        let cli = Cli::try_parse_from(["janus", "tray"]).expect("tray 应解析成功");
        assert!(matches!(cli.command, Some(Command::Tray)));
    }

    #[test]
    fn global_flag_without_subcommand_keeps_default() {
        // --dry-run 托盘等全局开关不应隐式要求子命令
        let cli = Cli::try_parse_from(["janus", "--dry-run"]).expect("--dry-run 应解析成功");
        assert!(cli.dry_run);
        assert!(cli.command.is_none());
    }

    #[test]
    fn settings_subcommand_parses() {
        let cli = Cli::try_parse_from(["janus", "settings"]).expect("settings 应解析成功");
        assert!(matches!(cli.command, Some(Command::Settings)));
    }

    #[test]
    fn all_flag_is_global_after_subcommand() {
        let cli = Cli::try_parse_from(["janus", "list", "--all"]).expect("--all 应解析成功");
        assert!(cli.all);
    }
}
