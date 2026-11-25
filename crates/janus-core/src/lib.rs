//! janus-core：平台无关的核心类型与解析逻辑。
//!
//! - [`BootId`] / [`BootEntry`] / [`BootManager`]：统一的启动项抽象，由各平台后端实现
//! - [`parse_load_option`]：解析 EFI_LOAD_OPTION 二进制结构（UTF-16LE 描述符）
//! - [`MockManager`]：内置模拟后端，供 `--dry-run` 与单元测试使用
//!
//! # 设计原理（Why）
//! - 解析逻辑全部收敛在本 crate，平台后端只负责"读原始字节"与"写字节"，
//!   保证 EFI_LOAD_OPTION 的二进制契约只有一份权威实现。
//! - 描述符解析采用 `String::from_utf16_lossy`：固件厂商的描述串可能包含
//!   非法代理项，宁可显示替换符也不能让整个启动项列表解析失败——这是
//!   "可用性优先于严格性"的权衡，代价是极端脏数据下名称会带 U+FFFD。
//! - 外部来的长度字段一律做饱和运算与越界校验：变量内容可能被固件写坏，
//!   解析器绝不能因脏数据 panic。

use std::collections::HashSet;

pub mod error;
mod mock;

pub use error::{JanusError, Result};
pub use mock::MockManager;

/// UEFI 全局变量命名空间 GUID（BootOrder/BootXXXX/BootNext 所在）。
pub const EFI_VARIABLE_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// 同上，带花括号的字符串形式（Windows 固件环境变量 API 需要）。
pub const EFI_VARIABLE_GUID_BRACED: &str = "{8be4df61-93ca-11d2-aa0d-00e098032b8c}";

/// EFI_VARIABLE_NON_VOLATILE：变量持久化存储。
pub const EFI_VARIABLE_NON_VOLATILE: u32 = 0x0000_0001;

/// EFI_VARIABLE_BOOTSERVICE_ACCESS：启动阶段可访问。
pub const EFI_VARIABLE_BOOTSERVICE_ACCESS: u32 = 0x0000_0002;

/// EFI_VARIABLE_RUNTIME_ACCESS：运行时可访问。
pub const EFI_VARIABLE_RUNTIME_ACCESS: u32 = 0x0000_0004;

/// BootNext 等启动相关变量所需的属性组合（NV | BS | RT）。
pub const EFI_VARIABLE_BOOT_ACCESS: u32 =
    EFI_VARIABLE_NON_VOLATILE | EFI_VARIABLE_BOOTSERVICE_ACCESS | EFI_VARIABLE_RUNTIME_ACCESS;

/// EFI_LOAD_OPTION 属性位：该启动项处于"启用"状态。
pub const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;

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

/// 解析后的 EFI_LOAD_OPTION 结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOption {
    /// EFI_LOAD_OPTION 属性位（如 [`LOAD_OPTION_ACTIVE`]）。
    pub attributes: u32,
    /// UTF-16LE 解码后的描述字符串。
    pub description: String,
    /// 描述之后的设备路径列表长度（字节）。
    pub file_path_list_length: u16,
}

/// 解析 EFI_LOAD_OPTION 原始数据（**不含** efivarfs 的 4 字节属性头）。
///
/// 二进制布局：
/// ```text
/// u32  attributes
/// u16  file_path_list_length
/// u16  description[]   UTF-16LE，以 0x0000 结尾
/// u8   file_path_list[file_path_list_length]
/// u8   optional_data[]
/// ```
///
/// # 设计原理（Why）
/// 描述符用 `from_utf16_lossy` 容错而非严格校验：固件厂商数据可能含非法
/// 代理项，可用性优先于严格性（代价见模块级文档）。长度字段越界直接判为
/// 脏数据拒绝，绝不做有 panic 风险的索引。
///
/// # Errors
/// 数据不足 8 字节、描述串未以 `0x0000` 结尾、或设备路径长度越界时返回
/// [`JanusError::Parse`]。
pub fn parse_load_option(data: &[u8]) -> Result<LoadOption> {
    const VAR: &str = "BootXXXX";
    // 至少要容纳 attributes(4) + length(2) + 结束符(2)
    if data.len() < 8 {
        return Err(JanusError::Parse {
            var: VAR.into(),
            msg: format!("数据过短（{} 字节）", data.len()),
        });
    }
    let attributes = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let file_path_list_length = u16::from_le_bytes([data[4], data[5]]);

    // 扫描 UTF-16LE 描述串直到 0x0000 结束符，再一次性流式解码进 String，避免中间 Vec<u16> 分配
    let mut end = 6;
    let mut terminated = false;
    while end + 1 < data.len() {
        if data[end] == 0 && data[end + 1] == 0 {
            terminated = true;
            break;
        }
        end += 2;
    }
    if !terminated {
        return Err(JanusError::Parse {
            var: VAR.into(),
            msg: "描述字符串未以 \\0 结尾".into(),
        });
    }
    let units = data[6..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c));
    let mut description = String::with_capacity((end - 6) / 2);
    for ch in char::decode_utf16(units) {
        description.push(ch.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    let i = end + 2;

    // 描述之后紧跟 file_path_list_length 字节的设备路径列表；长度来自外部
    // 固件数据，用饱和加法防御，再与实际长度核对，杜绝越界
    let path_end = i.saturating_add(file_path_list_length as usize);
    if path_end > data.len() {
        return Err(JanusError::Parse {
            var: VAR.into(),
            msg: "设备路径长度越界".into(),
        });
    }

    Ok(LoadOption {
        attributes,
        description,
        file_path_list_length,
    })
}

/// 解析 BootOrder 原始数据（不含属性头）：u16 LE 数组。
///
/// 奇数长度尾字节被丢弃（`as_chunks` 保证不越界）。
pub fn parse_boot_order(data: &[u8]) -> Vec<BootId> {
    data.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .map(BootId::new)
        .collect()
}

/// 解析 BootNext 原始数据（不含属性头），2 字节 u16 LE。
pub fn parse_boot_next(data: &[u8]) -> Option<BootId> {
    if data.len() < 2 {
        return None;
    }
    Some(BootId::new(u16::from_le_bytes([data[0], data[1]])))
}

/// 严格解析 BootNext 变量数据：变量存在但不足 2 字节时返回 [`JanusError::Parse`]，
/// 避免把损坏的固件数据静默当作“未设置”。
///
/// # Errors
/// 数据长度不足 2 字节时返回 [`JanusError::Parse`]。
pub fn parse_boot_next_var(data: &[u8]) -> Result<BootId> {
    parse_boot_next(data).ok_or_else(|| JanusError::Parse {
        var: "BootNext".into(),
        msg: format!("数据过短（{} 字节，需要至少 2 字节）", data.len()),
    })
}

/// 编码 BootNext 原始值（2 字节 u16 LE）。
pub fn encode_boot_next(id: BootId) -> [u8; 2] {
    id.raw().to_le_bytes()
}

/// 编码 EFI_LOAD_OPTION 原始字节（用于构造测试数据或固件写入）。
pub fn encode_load_option(attrs: u32, desc: &str, path: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + desc.len() * 2 + 2 + path.len());
    v.extend(attrs.to_le_bytes());
    v.extend((path.len() as u16).to_le_bytes());
    for u in desc.encode_utf16() {
        v.extend(u.to_le_bytes());
    }
    v.extend(0u16.to_le_bytes());
    v.extend(path);
    v
}

/// 编码 UTF-16LE + NUL 结尾字符串（供 Win32 宽字符 API 使用）。
#[cfg(windows)]
pub fn to_wide_null(s: &str) -> Vec<u16> {
    let mut wide = Vec::with_capacity(s.len() + 1);
    wide.extend(s.encode_utf16());
    wide.push(0);
    wide
}

/// efivarfs 变量文件开头的 4 字节属性头（小端）。
pub fn efivarfs_attr_header(attrs: u32) -> [u8; 4] {
    attrs.to_le_bytes()
}

/// BootXXXX 变量名：`id = 0x1` -> `"Boot0001"`。
pub fn boot_var_name(id: BootId) -> String {
    format!("Boot{:04X}", id.raw())
}

/// 从变量文件名提取启动项编号：`"Boot0001-8be4df61-..."` -> `Some(BootId)`。
pub fn boot_id_from_var_name(name: &str) -> Option<BootId> {
    let stem = name.split('-').next()?;
    if stem.len() == 8 && stem.starts_with("Boot") {
        u16::from_str_radix(&stem[4..], 16).ok().map(BootId::new)
    } else {
        None
    }
}

/// 用户输入的启动目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootTarget {
    /// 按 BootXXXX 编号（输入按十六进制解析）。
    Id(BootId),
    /// 按名称子串匹配（不区分大小写）。
    Name(String),
}

impl From<&str> for BootTarget {
    fn from(s: &str) -> Self {
        parse_boot_target(s)
    }
}

/// 解析用户输入的目标：
/// `"1"`、`"0x1"`、`"Boot0002"`（十六进制）视为编号；其余视为名称子串。
///
/// 纯十六进制字符的短输入优先按编号解析（如 `"beef"` 会命中 BootBEFF 而非
/// 名称匹配），这是文档化的有损取舍：编号是双系统场景最高频的输入。
pub fn parse_boot_target(s: &str) -> BootTarget {
    let lower = s.trim().to_ascii_lowercase();
    let hex = if let Some(rest) = lower.strip_prefix("boot") {
        rest
    } else if let Some(rest) = lower.strip_prefix("0x") {
        rest
    } else {
        lower.as_str()
    };
    if !hex.is_empty() && hex.len() <= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(raw) = u16::from_str_radix(hex, 16) {
            return BootTarget::Id(BootId::new(raw));
        }
    }
    BootTarget::Name(lower)
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

/// 在启动项列表中解析目标：编号精确匹配；名称不区分大小写的子串匹配。
///
/// # Errors
/// 编号或名称均未命中任何启动项时返回 [`JanusError::EntryNotFound`]，
/// 错误信息携带原始输入便于定位。
pub fn resolve_target<'a>(entries: &'a [BootEntry], target: &BootTarget) -> Result<&'a BootEntry> {
    match target {
        BootTarget::Id(id) => entries
            .iter()
            .find(|e| e.id == *id)
            .ok_or_else(|| JanusError::EntryNotFound(id.to_string())),
        BootTarget::Name(name) => entries
            .iter()
            .find(|e| e.name.to_lowercase().contains(name.as_str()))
            .ok_or_else(|| JanusError::EntryNotFound(name.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 EFI_LOAD_OPTION 布局构造原始字节（用于测试）。
    fn make_load_option(attrs: u32, desc: &str, path: &[u8]) -> Vec<u8> {
        encode_load_option(attrs, desc, path)
    }

    #[test]
    fn boot_id_display_and_raw() {
        let id = BootId::new(0x0001);
        assert_eq!(id.to_string(), "Boot0001");
        assert_eq!(id.raw(), 0x0001);
        // f.pad 让宽度标记生效
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

    #[test]
    fn parse_load_option_ubuntu() {
        let data = make_load_option(LOAD_OPTION_ACTIVE, "ubuntu", &[]);
        let opt = parse_load_option(&data).unwrap();
        assert_eq!(opt.description, "ubuntu");
        assert_eq!(opt.attributes, LOAD_OPTION_ACTIVE);
        assert_eq!(opt.file_path_list_length, 0);
    }

    #[test]
    fn parse_load_option_windows_boot_manager_with_path() {
        let path = [0x02, 0x01, 0x0C, 0x00]; // 假设备路径 4 字节
        let data = make_load_option(LOAD_OPTION_ACTIVE, "Windows Boot Manager", &path);
        let opt = parse_load_option(&data).unwrap();
        assert_eq!(opt.description, "Windows Boot Manager");
        assert_eq!(opt.file_path_list_length, 4);
        // 描述之后应当正好是 4 字节路径
        assert_eq!(data.len(), 6 + 21 * 2 + 4);
    }

    #[test]
    fn parse_load_option_invalid_utf16_is_lossy() {
        // 非法 UTF-16 代理项：应 lossy 而不是 panic
        let mut data = Vec::new();
        data.extend(0u32.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend([0x00, 0xD8, 0x41, 0x43, 0x00, 0x00]); // 高代理 + 'A' + 结束符
        let opt = parse_load_option(&data).unwrap();
        assert!(opt.description.contains('\u{FFFD}') || opt.description.contains('A'));
    }

    #[test]
    fn parse_load_option_too_short() {
        assert!(parse_load_option(&[0, 1, 2]).is_err());
    }

    #[test]
    fn parse_load_option_unterminated_description() {
        let mut data = Vec::new();
        data.extend(0u32.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend([0x61, 0x00, 0x62, 0x00]); // "ab" 无结束符
        assert!(parse_load_option(&data).is_err());
    }

    #[test]
    fn parse_load_option_path_length_overflow() {
        let mut data = Vec::new();
        data.extend(0u32.to_le_bytes());
        data.extend(0xFFFFu16.to_le_bytes()); // 路径长度 65535 越界
        data.extend([0x61, 0x00, 0x00, 0x00]);
        assert!(parse_load_option(&data).is_err());
    }

    #[test]
    fn boot_order_roundtrip() {
        let raw = [0x01, 0x00, 0xFF, 0xFF, 0x10, 0x27];
        assert_eq!(
            parse_boot_order(&raw),
            vec![
                BootId::new(0x0001),
                BootId::new(0xFFFF),
                BootId::new(0x2710)
            ]
        );
        // 奇数长度：截断余数
        assert_eq!(
            parse_boot_order(&[0x01, 0x00, 0x99]),
            vec![BootId::new(0x0001)]
        );
    }

    #[test]
    fn boot_next_roundtrip() {
        let raw = encode_boot_next(BootId::new(0x0002));
        assert_eq!(parse_boot_next(&raw), Some(BootId::new(0x0002)));
        assert_eq!(parse_boot_next(&[]), None);
        assert_eq!(parse_boot_next_var(&raw).unwrap(), BootId::new(0x0002));
        assert!(matches!(
            parse_boot_next_var(&[0x01]),
            Err(JanusError::Parse { .. })
        ));
    }

    #[test]
    fn var_name_helpers() {
        assert_eq!(boot_var_name(BootId::new(0x0001)), "Boot0001");
        assert_eq!(boot_var_name(BootId::new(0xFFFF)), "BootFFFF");
        assert_eq!(
            boot_id_from_var_name("Boot0001-8be4df61-93ca-11d2-aa0d-00e098032b8c"),
            Some(BootId::new(0x0001))
        );
        assert_eq!(
            boot_id_from_var_name("BootOrder-8be4df61-93ca-11d2-aa0d-00e098032b8c"),
            None
        );
        assert_eq!(
            boot_id_from_var_name("Boot00XX-8be4df61-93ca-11d2-aa0d-00e098032b8c"),
            None
        );
    }

    #[test]
    fn parse_boot_target_formats() {
        assert_eq!(parse_boot_target("1"), BootTarget::Id(BootId::new(0x0001)));
        assert_eq!(
            parse_boot_target("0x0002"),
            BootTarget::Id(BootId::new(0x0002))
        );
        assert_eq!(
            parse_boot_target("Boot0002"),
            BootTarget::Id(BootId::new(0x0002))
        );
        assert_eq!(
            parse_boot_target("  BOOT0010 "),
            BootTarget::Id(BootId::new(0x0010))
        );
        assert_eq!(
            parse_boot_target("ubuntu"),
            BootTarget::Name("ubuntu".into())
        );
        assert_eq!(
            parse_boot_target("Windows"),
            BootTarget::Name("windows".into())
        );
        // 超过 4 位十六进制字符或混合前缀不能是合法编号 -> 按名称
        assert_eq!(parse_boot_target("12345"), BootTarget::Name("12345".into()));
        assert_eq!(
            parse_boot_target("boot0x1"),
            BootTarget::Name("boot0x1".into())
        );
        // From<&str> 便捷转换与函数等价
        assert_eq!(BootTarget::from("1"), parse_boot_target("1"));
    }

    #[test]
    fn resolve_target_matches() {
        let entries = MockManager::typical_dual_boot().list_entries().unwrap();
        let by_name = resolve_target(&entries, &BootTarget::Name("win".into())).unwrap();
        assert_eq!(by_name.id, BootId::new(0x0001));
        let by_id = resolve_target(&entries, &BootTarget::Id(BootId::new(0x0002))).unwrap();
        assert_eq!(by_name.name, "Windows Boot Manager");
        assert_eq!(by_id.name, "ubuntu");
        assert!(resolve_target(&entries, &BootTarget::Name("linux".into())).is_err());
    }
}
