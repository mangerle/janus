//! 用户显示设置：按启动项编号定制托盘与 CLI 的可见性与显示名称。
//!
//! # 设计原理（Why）
//! - 设置以 `BootXXXX` 编号为键，而不是名称：名称由固件提供且可能重复，
//!   编号是固件内唯一标识。代价是固件重新编号（极少见）后旧设置不再命中，
//!   此时回退为"全部显示 + 固件原名"，不会导致启动项消失。
//! - 配置文件采用 TOML，位于用户配置目录；未配置项默认显示、使用固件原名，
//!   保证新出现的启动项自动可见，避免"设置后漏掉新系统"。
//! - 损坏或无法读取的配置只告警并回退默认值，绝不阻塞 CLI/托盘启动。

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use janus_core::{BootEntry, BootId};
use serde::{Deserialize, Serialize};

/// 单个启动项的显示设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntrySettings {
    /// 是否在托盘菜单与 CLI 列表中隐藏（隐藏项仍可按编号直接指定）
    #[serde(default)]
    pub(crate) hidden: bool,
    /// 自定义显示名称；`None` 或空白时使用固件原名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

/// 用户设置（`config.toml` 的序列化契约）。
///
/// 键为启动项编号的展示形式（如 `Boot0001`）；使用 `BTreeMap` 保证
/// 文件输出顺序稳定，便于用户手动查看与版本管理。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Settings {
    #[serde(default)]
    pub(crate) entries: BTreeMap<String, EntrySettings>,
}

impl Settings {
    /// 配置文件路径：Linux 为 `$XDG_CONFIG_HOME/janus/config.toml`
    /// （未设置时 `~/.config/janus/config.toml`），Windows 为
    /// `%APPDATA%\janus\config.toml`。
    ///
    /// # Errors
    /// 无法确定用户配置目录（`HOME`/`APPDATA` 均缺失）时返回错误。
    pub(crate) fn config_file_path() -> anyhow::Result<PathBuf> {
        Ok(config_dir()?.join("config.toml"))
    }

    /// 从指定路径读取设置；文件不存在视为默认设置。
    ///
    /// # Errors
    /// 文件存在但读取失败或 TOML 解析失败时返回错误。
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("解析配置文件失败：{}", path.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("读取配置文件失败：{}", path.display())),
        }
    }

    /// 读取设置；任何失败都告警并回退默认值，保证调用方流程不中断。
    pub(crate) fn load_or_default(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|e| {
            log::warn!("读取显示设置失败，已使用默认设置：{e:#}");
            Self::default()
        })
    }

    /// 从默认位置读取设置，同时返回配置文件路径（无法确定路径时为 `None`）。
    pub(crate) fn load_from_default_location() -> (Self, Option<PathBuf>) {
        match Self::config_file_path() {
            Ok(path) => (Self::load_or_default(&path), Some(path)),
            Err(e) => {
                log::warn!("无法确定配置文件位置，已使用默认设置：{e:#}");
                (Self::default(), None)
            }
        }
    }

    /// 原子写回配置文件：在同目录创建唯一临时文件写入后原子替换，避免半截文件与并发覆盖。
    ///
    /// # Errors
    /// 目录创建、序列化或写入/替换失败时返回错误。
    pub(crate) fn save(&self, path: &Path) -> anyhow::Result<()> {
        use std::io::Write as _;

        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("配置文件路径缺少父目录：{}", path.display()))?;
        fs::create_dir_all(dir).with_context(|| format!("创建配置目录失败：{}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("序列化显示设置失败")?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("创建临时配置文件失败：{}", dir.display()))?;
        tmp.write_all(text.as_bytes())
            .with_context(|| format!("写入临时配置文件失败：{}", tmp.path().display()))?;
        tmp.as_file().sync_all().ok();
        tmp.persist(path)
            .map_err(|e| e.error)
            .with_context(|| format!("替换配置文件失败：{}", path.display()))?;
        Ok(())
    }

    /// 指定启动项的设置（未配置时为 `None`；栈上格式化键名，零堆分配）。
    pub(crate) fn entry(&self, id: BootId) -> Option<&EntrySettings> {
        use std::io::Write as _;

        let mut buf = [0u8; 8];
        let _ = write!(&mut buf[..], "{id}");
        let key = std::str::from_utf8(&buf).unwrap_or("");
        self.entries.get(key)
    }

    /// 应用显示设置：过滤隐藏项并替换为自定义名称。
    ///
    /// `show_all = true`（`--all`）时完全忽略设置，返回固件原名与全部项。
    pub(crate) fn apply(&self, entries: &mut Vec<BootEntry>, show_all: bool) {
        if show_all {
            return;
        }
        entries.retain(|e| self.entry(e.id).map(|s| !s.hidden).unwrap_or(true));
        self.apply_names(entries);
    }

    /// 仅应用自定义名称（不过滤隐藏项），用于目标解析等需要全量条目的场景。
    pub(crate) fn apply_names(&self, entries: &mut [BootEntry]) {
        for entry in entries {
            if let Some(name) = self.entry(entry.id).and_then(|s| s.name.as_deref()) {
                let name = name.trim();
                if !name.is_empty() {
                    entry.name = name.to_string();
                }
            }
        }
    }
}

/// 按平台解析用户配置目录。
fn config_dir() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        // XDG 规范要求绝对路径；相对值视为未设置，避免配置落到工作目录
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                return Ok(dir.join("janus"));
            }
        }
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("无法确定用户主目录：HOME 未设置"))?;
        Ok(PathBuf::from(home).join(".config").join("janus"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("无法确定用户配置目录：APPDATA 未设置"))?;
        Ok(PathBuf::from(appdata).join("janus"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(anyhow!("此平台暂不支持配置文件"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(raw: u16, name: &str) -> BootEntry {
        BootEntry {
            id: BootId::new(raw),
            name: name.into(),
            active: true,
        }
    }

    fn sample_settings() -> Settings {
        let mut settings = Settings::default();
        settings.entries.insert(
            BootId::new(0x0001).to_string(),
            EntrySettings {
                hidden: true,
                name: None,
            },
        );
        settings.entries.insert(
            BootId::new(0x0002).to_string(),
            EntrySettings {
                hidden: false,
                name: Some("Ubuntu 24.04".into()),
            },
        );
        settings
    }

    #[test]
    fn default_settings_show_all_entries_with_firmware_names() {
        let settings = Settings::default();
        let mut entries = vec![entry(1, "Windows Boot Manager"), entry(2, "ubuntu")];
        settings.apply(&mut entries, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Windows Boot Manager");
        assert_eq!(entries[1].name, "ubuntu");
    }

    #[test]
    fn apply_hides_and_renames() {
        let mut entries = vec![
            entry(1, "Windows Boot Manager"),
            entry(2, "ubuntu"),
            entry(3, "EFI USB Device"),
        ];
        sample_settings().apply(&mut entries, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, BootId::new(2));
        assert_eq!(entries[0].name, "Ubuntu 24.04");
        assert_eq!(entries[1].name, "EFI USB Device");
    }

    #[test]
    fn apply_with_show_all_ignores_settings() {
        let mut entries = vec![entry(1, "Windows Boot Manager"), entry(2, "ubuntu")];
        sample_settings().apply(&mut entries, true);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Windows Boot Manager");
        assert_eq!(entries[1].name, "ubuntu");
    }

    #[test]
    fn blank_custom_name_falls_back_to_firmware_name() {
        let mut settings = Settings::default();
        settings.entries.insert(
            BootId::new(2).to_string(),
            EntrySettings {
                hidden: false,
                name: Some("   ".into()),
            },
        );
        let mut entries = vec![entry(2, "ubuntu")];
        settings.apply_names(&mut entries);
        assert_eq!(entries[0].name, "ubuntu");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("janus").join("config.toml");
        let settings = sample_settings();
        settings.save(&path).expect("保存设置失败");

        let loaded = Settings::load(&path).expect("读取设置失败");
        assert_eq!(loaded, settings);
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let loaded =
            Settings::load(&dir.path().join("missing.toml")).expect("缺失文件应返回默认值");
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn corrupt_file_is_rejected_but_load_or_default_falls_back() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = dir.path().join("config.toml");
        fs::write(&path, "这不是合法的 TOML [").expect("写入损坏配置失败");

        assert!(Settings::load(&path).is_err());
        assert_eq!(Settings::load_or_default(&path), Settings::default());
    }
}
