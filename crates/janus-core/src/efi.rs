//! UEFI 全局变量常量与 `EFI_LOAD_OPTION` 二进制编解码。
//!
//! # 设计原理（Why）
//! - 解析逻辑全部收敛在本模块，平台后端只负责"读原始字节"与"写字节"，
//!   保证 `EFI_LOAD_OPTION` 的二进制契约只有一份权威实现。
//! - 描述符解析采用逐字符流式解码并容错替换：固件厂商的描述串可能包含
//!   非法代理项，可用性优先于严格性；外部来的长度字段一律做饱和运算与
//!   越界校验，绝不因脏数据 panic。

use crate::{BootId, JanusError, Result};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_load_option(attrs: u32, desc: &str, path: &[u8]) -> Vec<u8> {
        encode_load_option(attrs, desc, path)
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
        let path = [0x02, 0x01, 0x0C, 0x00];
        let data = make_load_option(LOAD_OPTION_ACTIVE, "Windows Boot Manager", &path);
        let opt = parse_load_option(&data).unwrap();
        assert_eq!(opt.description, "Windows Boot Manager");
        assert_eq!(opt.file_path_list_length, 4);
        assert_eq!(data.len(), 6 + 21 * 2 + 4);
    }

    #[test]
    fn parse_load_option_invalid_utf16_is_lossy() {
        let mut data = Vec::new();
        data.extend(0u32.to_le_bytes());
        data.extend(0u16.to_le_bytes());
        data.extend([0x00, 0xD8, 0x41, 0x43, 0x00, 0x00]);
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
        data.extend([0x61, 0x00, 0x62, 0x00]);
        assert!(parse_load_option(&data).is_err());
    }

    #[test]
    fn parse_load_option_path_length_overflow() {
        let mut data = Vec::new();
        data.extend(0u32.to_le_bytes());
        data.extend(0xFFFFu16.to_le_bytes());
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
}
