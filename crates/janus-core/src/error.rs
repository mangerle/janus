//! 统一错误类型。

use thiserror::Error;

/// 便捷 Result 别名。
pub type Result<T> = std::result::Result<T, JanusError>;

/// Janus 全局错误类型。
#[derive(Debug, Error)]
pub enum JanusError {
    /// 系统不是 UEFI 启动，或 efivarfs 不可用。
    #[error("当前系统不是 UEFI 启动，或 efivarfs 不可用：{0}")]
    NotUefiSystem(String),

    /// 访问 UEFI 固件变量失败。
    #[error("访问 UEFI 固件变量失败：{0}")]
    EfiVar(String),

    /// 解析 UEFI 变量内容失败。
    #[error("解析 UEFI 变量 {var} 失败：{msg}")]
    Parse { var: String, msg: String },

    /// 找不到匹配的启动项。
    #[error("找不到启动项：{0}")]
    EntryNotFound(String),

    /// 当前进程缺少 root/管理员权限。
    #[error("需要 root/管理员权限才能执行此操作")]
    PermissionDenied,

    /// 自动提权（pkexec/runas）失败。
    #[error("权限提升失败：{0}")]
    Elevation(String),

    /// 底层 IO 错误。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    /// 其他错误。
    #[error("{0}")]
    Other(String),
}
