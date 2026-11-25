//! janus-core：平台无关的核心类型与解析逻辑。
//!
//! - [`BootId`] / [`BootEntry`] / [`BootManager`]：统一的启动项抽象，由各平台后端实现
//! - [`parse_load_option`]：解析 `EFI_LOAD_OPTION` 二进制结构（UTF-16LE 描述符）
//! - [`MockManager`]：内置模拟后端，供 `--dry-run` 与单元测试使用
//!
//! # 设计原理（Why）
//! - 将核心模型、二进制协议编解码（`efi`）与目标解析（`target`）拆分为独立子模块，
//!   各模块单一职责并对外保持统一重导出。

use std::collections::HashSet;

mod efi;
pub mod error;
mod mock;
mod target;

pub use efi::*;
pub use error::{JanusError, Result};
pub use mock::MockManager;
pub use target::*;

/// 启动项编号（BootXXXX 的 XXXX 部分，4 位十六进制展示）。
///
/// # 设计原理（Why）
/// 采用新类型模式（Newtype）而非裸 `u16`：启动项编号、变量属性位、变量
/// 数据长度都是 u16，裸类型极易在传参时张冠李戴；新类型让编译器在编译期
/// 排除"把属性位当编号写入 BootNext"这类非法状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BootId(u16);

impl BootId {
    /// 由原始 u16 构造。
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// 取回原始编号值。
    pub const fn raw(self) -> u16 {
        self.0
    }
}

impl std::fmt::Display for BootId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 在栈上格式化为固定 8 字节 ASCII 后交由 f.pad，零堆分配且支持宽度对齐
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let v = self.0;
        let buf = [
            b'B',
            b'o',
            b'o',
            b't',
            HEX[((v >> 12) & 0xF) as usize],
            HEX[((v >> 8) & 0xF) as usize],
            HEX[((v >> 4) & 0xF) as usize],
            HEX[(v & 0xF) as usize],
        ];
        let s = std::str::from_utf8(&buf).expect("ASCII 十六进制字节必定为合法 UTF-8");
        f.pad(s)
    }
}

impl serde::Serialize for BootId {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        // JSON 契约中编号保持数字形态（num 字段），展示字符串由 id 字段承担
        serializer.serialize_u16(self.0)
    }
}

impl From<u16> for BootId {
    fn from(raw: u16) -> Self {
        Self::new(raw)
    }
}

/// 一个 UEFI 启动项（对应固件中的 BootXXXX 变量）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BootEntry {
    /// 启动项编号，展示为 Boot0001 形式。
    pub id: BootId,
    /// 描述名称，如 "Windows Boot Manager" 或 "ubuntu"。
    pub name: String,
    /// EFI_LOAD_OPTION 是否带 ACTIVE 属性位。
    pub active: bool,
}

/// 统一的启动管理接口；Linux 用 efivarfs、Windows 用固件环境变量 API 实现。
///
/// # 设计原理（Why）
/// 以 trait 隔离平台差异，应用层（CLI/托盘）只面向接口编程；方法以 `&self`
/// 接收而非关联函数，便于上层以 `Arc<dyn BootManager>` 在托盘动作闭包中
/// 共享同一后端实例。
pub trait BootManager: Send + Sync {
    /// 按 BootOrder 顺序列出所有可解析的启动项。
    ///
    /// # Errors
    /// 非 UEFI 环境（缺 efivarfs 或 BootOrder）返回
    /// [`JanusError::NotUefiSystem`]；无读取权限返回
    /// [`JanusError::PermissionDenied`]；底层 IO 故障返回 [`JanusError::Io`]。
    fn list_entries(&self) -> Result<Vec<BootEntry>>;

    /// 当前 BootNext（下次启动项），未设置时返回 `Ok(None)`。
    ///
    /// # Errors
    /// 变量读取失败或数据格式异常时返回对应 [`JanusError`] 变体。
    fn boot_next(&self) -> Result<Option<BootId>>;

    /// 写入 BootNext，下次重启时优先从该项启动。
    ///
    /// # Errors
    /// 无写权限返回 [`JanusError::PermissionDenied`]；efivarfs 写入失败或
    /// Windows 固件 API 调用失败返回对应 [`JanusError`] 变体。
    fn set_boot_next(&self, id: BootId) -> Result<()>;

    /// 删除 BootNext；变量本就不存在时静默成功。
    ///
    /// # Errors
    /// 无写权限返回 [`JanusError::PermissionDenied`]；底层删除失败返回
    /// 对应 [`JanusError`] 变体。
    fn clear_boot_next(&self) -> Result<()>;
}

/// 按 `BootOrder` 顺序列出并解析所有启动项；去重、缺失或无法解析的条目静默跳过。
///
/// # 设计原理（Why）
/// 将“读 BootOrder → 去重 → 逐项读 BootXXXX → 解析 EFI_LOAD_OPTION”这一
/// 平台无关逻辑收敛到核心库，Linux（efivarfs）与 Windows（固件 API）只需
/// 提供各自的变量读取回调，杜绝两处实现漂移。
///
/// # Errors
/// 找不到 `BootOrder` 变量时返回 [`JanusError::NotUefiSystem`]；底层变量
/// 读取失败时透传对应 [`JanusError`]。
pub fn collect_boot_entries<F>(mut read_var: F) -> Result<Vec<BootEntry>>
where
    F: FnMut(&str) -> Result<Option<Vec<u8>>>,
{
    let Some(order_data) = read_var("BootOrder")? else {
        return Err(JanusError::NotUefiSystem(
            "未找到 BootOrder 变量（系统可能以 BIOS/Legacy 模式启动，或权限不足）".into(),
        ));
    };
    let order = parse_boot_order(&order_data);
    let mut entries = Vec::with_capacity(order.len());
    let mut seen = HashSet::with_capacity(order.len());
    for id in order {
        if !seen.insert(id) {
            continue;
        }
        let Some(data) = read_var(&boot_var_name(id))? else {
            continue; // BootOrder 引用了不存在的变量
        };
        let Ok(opt) = parse_load_option(&data) else {
            continue; // 无法解析（如固件内部条目）
        };
        entries.push(BootEntry {
            id,
            name: opt.description,
            active: opt.attributes & LOAD_OPTION_ACTIVE != 0,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_id_display_and_raw() {
        let id = BootId::new(0x0001);
        assert_eq!(id.to_string(), "Boot0001");
        assert_eq!(id.raw(), 0x0001);
        assert_eq!(format!("{:<10}", id), "Boot0001  ");
        assert_eq!(BootId::from(0xFFFFu16).to_string(), "BootFFFF");
    }

    #[test]
    fn boot_id_serialize_keeps_numeric_form() {
        let entry = BootEntry {
            id: BootId::new(2),
            name: "ubuntu".into(),
            active: true,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["id"], 2);
    }
}
