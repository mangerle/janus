//! 设置窗口的启动项编辑状态与零分配脏检查逻辑（与 egui 渲染解耦）。

use janus_core::BootEntry;

use crate::config::{EntrySettings, Settings};
use crate::settings::widgets::SettingsRow;

/// 启动项配置与开机自启的编辑状态。
pub(crate) struct SettingsState {
    pub(crate) rows: Vec<SettingsRow>,
    /// 打开窗口时读到的完整设置：保存时以它为基线，保留当前固件中暂不存在的
    /// 启动项配置（例如未插入的 U 盘），避免误删
    pub(crate) base: Settings,
    /// 打开窗口时的系统开机自启状态
    pub(crate) autostart_initial: bool,
    /// 当前开关编辑状态
    pub(crate) autostart_enabled: bool,
}

impl SettingsState {
    pub(crate) fn new(
        entries: Vec<BootEntry>,
        settings: &Settings,
        autostart_initial: bool,
    ) -> Self {
        let rows = entries
            .into_iter()
            .map(|entry| {
                let configured = settings.entry(entry.id);
                SettingsRow {
                    id: entry.id,
                    firmware_name: entry.name,
                    visible: configured.map(|s| !s.hidden).unwrap_or(true),
                    display_name: configured.and_then(|s| s.name.clone()).unwrap_or_default(),
                }
            })
            .collect();
        Self {
            rows,
            base: settings.clone(),
            autostart_initial,
            autostart_enabled: autostart_initial,
        }
    }

    /// 构造拟保存的配置快照。
    pub(crate) fn build_settings(&self) -> Settings {
        let mut settings = self.base.clone();
        for row in &self.rows {
            let key = row.id.to_string();
            let name = row.display_name.trim();
            if !row.visible || !name.is_empty() {
                settings.entries.insert(
                    key,
                    EntrySettings {
                        hidden: !row.visible,
                        name: (!name.is_empty()).then(|| name.to_string()),
                    },
                );
            } else {
                // 恢复默认：移除该启动项的显式配置
                settings.entries.remove(&key);
            }
        }
        settings
    }

    /// 当前界面编辑状态相对初始配置是否有未保存的变更（零堆分配逐行比对）。
    pub(crate) fn is_dirty(&self) -> bool {
        if self.autostart_enabled != self.autostart_initial {
            return true;
        }
        self.rows.iter().any(|row| {
            let trimmed = row.display_name.trim();
            let current_hidden = !row.visible;
            let current_name = (!trimmed.is_empty()).then_some(trimmed);
            match self.base.entry(row.id) {
                Some(base) => {
                    let base_name = base
                        .name
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    base.hidden != current_hidden || base_name != current_name
                }
                None => current_hidden || current_name.is_some(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsState;
    use crate::config::{EntrySettings, Settings};
    use janus_core::{BootEntry, BootId};

    #[test]
    fn dirty_check_detects_row_and_autostart_changes() {
        let mut base = Settings::default();
        base.entries.insert(
            "Boot0001".into(),
            EntrySettings {
                hidden: false,
                name: Some("   ".into()),
            },
        );
        let entries = vec![BootEntry {
            id: BootId::new(1),
            name: "Windows".into(),
            active: true,
        }];
        let mut state = SettingsState::new(entries, &base, false);
        assert!(!state.is_dirty());

        state.autostart_enabled = true;
        assert!(state.is_dirty());

        state.autostart_enabled = false;
        state.rows[0].display_name = "Win11".into();
        assert!(state.is_dirty());
        assert_eq!(
            state
                .build_settings()
                .entry(BootId::new(1))
                .and_then(|s| s.name.as_deref()),
            Some("Win11")
        );
    }
}
