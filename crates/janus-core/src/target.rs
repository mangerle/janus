//! 启动目标解析与条目匹配。

use crate::{BootEntry, BootId, JanusError, Result};

/// 用户输入的启动目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootTarget {
    /// 明确的 BootXXXX 编号（纯数字或带 `boot`/`0x` 前缀的十六进制）。
    Id(BootId),
    /// 按名称子串匹配（不区分大小写）。
    Name(String),
    /// 无前缀且含十六进制字母 `a-f` 的 1~4 位输入（如 `"cafe"`/`"bed"`）：
    /// 优先按编号精确匹配，未命中时回退名称子串匹配。
    IdOrName {
        /// 按十六进制解析的候选编号。
        id: BootId,
        /// 原始小写名称子串。
        name: String,
    },
}

impl From<&str> for BootTarget {
    fn from(s: &str) -> Self {
        parse_boot_target(s)
    }
}

/// 解析用户输入的目标：
/// - 纯数字（如 `"1"`）或带 `boot`/`0x` 前缀（如 `"0x1"`、`"Boot0002"`）视为明确编号；
/// - 无前缀但含 `a-f` 的 1~4 位十六进制串（如 `"cafe"`）视为 [`BootTarget::IdOrName`]，
///   优先匹配编号、未命中时回退名称子串匹配；
/// - 其余视为名称子串。
pub fn parse_boot_target(s: &str) -> BootTarget {
    let lower = s.trim().to_ascii_lowercase();
    let (explicit_prefix, hex) = if let Some(rest) = lower.strip_prefix("boot") {
        (true, rest)
    } else if let Some(rest) = lower.strip_prefix("0x") {
        (true, rest)
    } else {
        (false, lower.as_str())
    };
    if !hex.is_empty() && hex.len() <= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(raw) = u16::from_str_radix(hex, 16) {
            let id = BootId::new(raw);
            let only_digits = hex.chars().all(|c| c.is_ascii_digit());
            if explicit_prefix || only_digits {
                return BootTarget::Id(id);
            }
            return BootTarget::IdOrName { id, name: lower };
        }
    }
    BootTarget::Name(lower)
}

/// 判断 `haystack` 是否包含 `needle_lower`（不区分大小写）。
///
/// 对纯 ASCII 文本走字节窗口比较，零堆分配；包含非 ASCII 字符时回退到
/// Unicode 小写转换以正确处理多语言别名。
pub fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle_lower.is_ascii() {
        return haystack
            .as_bytes()
            .windows(needle_lower.len())
            .any(|window| window.eq_ignore_ascii_case(needle_lower.as_bytes()));
    }
    haystack
        .to_lowercase()
        .contains(&needle_lower.to_lowercase())
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
            .find(|e| contains_ignore_case(&e.name, name))
            .ok_or_else(|| JanusError::EntryNotFound(name.clone())),
        BootTarget::IdOrName { id, name } => entries
            .iter()
            .find(|e| e.id == *id)
            .or_else(|| entries.iter().find(|e| contains_ignore_case(&e.name, name)))
            .ok_or_else(|| JanusError::EntryNotFound(name.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BootManager, MockManager};

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

        // 无前缀十六进制单词（如 "cafe"）优先按编号匹配，未命中时回退名称子串
        let cafe_entries = vec![BootEntry {
            id: BootId::new(0x0001),
            name: "CafeOS".into(),
            active: true,
        }];
        let cafe = resolve_target(&cafe_entries, &parse_boot_target("cafe")).unwrap();
        assert_eq!(cafe.name, "CafeOS");
    }
}
