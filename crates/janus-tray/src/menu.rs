//! 托盘菜单构建、图标生成、事件路由与设置重载辅助。

use std::collections::HashMap;
use std::sync::Arc;

use crossbeam_channel::Sender;
use janus_core::{BootEntry, BootId, JanusError, Result};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::{
    MenuOutcome, TrayActions, ID_BOOT, ID_CLEAR, ID_EMPTY, ID_INFO, ID_NEXT, ID_QUIT, ID_REBOOT,
    ID_SETTINGS,
};

/// 构建托盘菜单（每次状态变化后重建整个菜单）。
///
/// # 设计原理（Why）
/// muda 菜单不支持原位更新单项文本，整棵重建 + `set_menu` 替换是官方
/// 代价最小的刷新方式；菜单项 ID 保持稳定，重建不影响事件路由。
pub(crate) fn build_menu(entries: &[BootEntry], actions: &TrayActions) -> Menu {
    let menu = Menu::new();
    let current_next = (actions.current_next)();

    // 信息行展示名称；当前下次启动项被隐藏时退回编号
    let next_desc = match current_next {
        Some(id) => entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| id.to_string()),
        None => "未设置".into(),
    };
    let info = MenuItem::with_id(ID_INFO, format!("当前下次启动：{next_desc}"), false, None);
    let _ = menu.append(&info);
    let _ = menu.append(&PredefinedMenuItem::separator());

    if entries.is_empty() {
        // 全部被隐藏或固件无启动项时给出可操作提示，设置入口仍可用
        let hint = MenuItem::with_id(
            ID_EMPTY,
            "没有可显示的启动项（请在设置中调整）",
            false,
            None,
        );
        let _ = menu.append(&hint);
    } else {
        // 预先统计名称频次，避免两个子菜单各自 O(N^2) 扫描重名条目
        let mut counts = HashMap::with_capacity(entries.len());
        for e in entries {
            *counts.entry(e.name.as_str()).or_insert(0usize) += 1;
        }
        let _ = menu.append(&entry_submenu("重启并进入", ID_BOOT, entries, &counts));
        let _ = menu.append(&entry_submenu("仅下次启动", ID_NEXT, entries, &counts));
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let clear = MenuItem::with_id(ID_CLEAR, "清除 BootNext", current_next.is_some(), None);
    let _ = menu.append(&clear);
    let reboot = MenuItem::with_id(ID_REBOOT, "立即重启", true, None);
    let _ = menu.append(&reboot);

    let _ = menu.append(&PredefinedMenuItem::separator());
    let settings = MenuItem::with_id(ID_SETTINGS, "设置", true, None);
    let _ = menu.append(&settings);
    let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
    let _ = menu.append(&quit);

    menu
}

/// 构建一个按启动项展开的子菜单（动作前缀区分"重启进入"与"仅设置"）。
///
/// 默认仅展示启动项名称，去掉 `BootXXXX` 前缀保持菜单清爽；
/// 仅当列表中存在同名条目时才附加编号消歧义。
fn entry_submenu(
    title: &str,
    prefix: &str,
    entries: &[BootEntry],
    counts: &HashMap<&str, usize>,
) -> Submenu {
    let submenu = Submenu::new(title, true);
    for e in entries {
        let duplicated = counts.get(e.name.as_str()).copied().unwrap_or(0) > 1;
        let label = if duplicated {
            format!("{}（{}）", e.name, e.id)
        } else {
            e.name.clone()
        };
        let item = MenuItem::with_id(format!("{prefix}{:04X}", e.id.raw()), label, true, None);
        let _ = submenu.append(&item);
    }
    submenu
}

/// 创建托盘图标。
///
/// # Errors
/// tray-icon 初始化失败（如无可用通知区宿主）时返回 [`JanusError::Other`]。
pub(crate) fn build_tray(entries: &[BootEntry], actions: &TrayActions) -> Result<TrayIcon> {
    TrayIconBuilder::new()
        .with_menu(Box::new(build_menu(entries, actions)))
        .with_tooltip("Janus —— UEFI 启动切换")
        .with_icon(generate_icon())
        .build()
        .map_err(|e| JanusError::Other(format!("创建托盘失败：{e}")))
}

/// 动作执行后刷新菜单（更新 BootNext 信息项与"清除"可见性）。
pub(crate) fn refresh(tray: &TrayIcon, entries: &[BootEntry], actions: &TrayActions) {
    tray.set_menu(Some(Box::new(build_menu(entries, actions))));
}

/// 处理一条菜单事件；调用方据 [`MenuOutcome`] 决定退出或打开设置窗口。
pub(crate) fn handle_menu_event(
    id: &str,
    entries: &[BootEntry],
    actions: &TrayActions,
) -> MenuOutcome {
    if let Some(rest) = id.strip_prefix(ID_BOOT) {
        if let Ok(raw) = u16::from_str_radix(rest, 16) {
            let id = BootId::new(raw);
            let name = entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| id.to_string());
            if (actions.confirm)(&format!("确定要重启进入「{name}」吗？")) {
                if let Err(e) = (actions.switch_and_reboot)(id) {
                    log::error!("重启进入失败：{e}");
                } else {
                    return MenuOutcome::Refresh;
                }
            }
        }
    } else if let Some(rest) = id.strip_prefix(ID_NEXT) {
        if let Ok(raw) = u16::from_str_radix(rest, 16) {
            let id = BootId::new(raw);
            if let Err(e) = (actions.set_next)(id) {
                log::error!("设置下次启动失败：{e}");
            } else {
                if let Some(e) = entries.iter().find(|e| e.id == id) {
                    log::info!("已设置下次启动：{}（{}）", e.id, e.name);
                }
                return MenuOutcome::Refresh;
            }
        }
    } else if id == ID_CLEAR {
        if let Err(e) = (actions.clear_next)() {
            log::error!("清除 BootNext 失败：{e}");
        } else {
            log::info!("已清除 BootNext");
            return MenuOutcome::Refresh;
        }
    } else if id == ID_REBOOT {
        if let Err(e) = (actions.reboot)() {
            log::error!("重启失败：{e}");
        }
    } else if id == ID_SETTINGS {
        return MenuOutcome::OpenSettings;
    } else if id == ID_QUIT {
        return MenuOutcome::Quit;
    }
    MenuOutcome::Continue
}

/// 在后台线程唤起设置（上层实现为独立子进程，规避 winit 的非主线程
/// 事件循环限制）；设置关闭后通过有界通道通知托盘重载。
/// 容量 1 的 `try_send` 会合并连续通知，窗口关闭只需刷新一次。
pub(crate) fn spawn_settings(actions: &TrayActions, notify: &Sender<()>) {
    let open = Arc::clone(&actions.open_settings);
    let notify = notify.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("janus-settings".into())
        .spawn(move || {
            open();
            let _ = notify.try_send(());
        })
    {
        log::error!("创建设置窗口线程失败：{e}");
    }
}

/// 设置保存后重新拉取条目；失败时保留旧列表并告警。
pub(crate) fn refresh_entries(entries: &mut Vec<BootEntry>, actions: &TrayActions) {
    match (actions.reload_entries)() {
        Ok(new_entries) => *entries = new_entries,
        Err(e) => log::warn!("重新加载启动项失败（保留旧列表）：{e}"),
    }
}

/// 托盘图标：由 logo/janus-logo.svg 预渲染的 32x32 RGBA 字节（构建期嵌入）。
/// 再生成方式：
/// `magick -background none -density 1152 logo/janus-logo.svg \
///    -resize 32x32 -depth 8 rgba:crates/janus-tray/assets/icon-32.rgba`
///
/// # Panics
/// 仅当嵌入的 RGBA 数据长度与 32x32 尺寸不符时 panic——该数据由构建
/// 脚本生成且经 `debug_assert` 校验，属于编译期不变量。
pub(crate) fn generate_icon() -> Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    const ICON_RGBA_32: &[u8] = include_bytes!("../assets/icon-32.rgba");
    debug_assert_eq!(ICON_RGBA_32.len(), (W * H * 4) as usize);
    Icon::from_rgba(ICON_RGBA_32.to_vec(), W, H).expect("内置图标数据无效")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use janus_core::{BootEntry, BootId, JanusError};

    use super::{handle_menu_event, refresh_entries};
    use crate::{MenuOutcome, TrayActions, ID_CLEAR, ID_QUIT, ID_REBOOT, ID_SETTINGS};

    fn sample_entries() -> Vec<BootEntry> {
        vec![
            BootEntry {
                id: BootId::new(0x0001),
                name: "Windows Boot Manager".into(),
                active: true,
            },
            BootEntry {
                id: BootId::new(0x0002),
                name: "ubuntu".into(),
                active: true,
            },
        ]
    }

    fn dummy_actions(
        confirm_result: bool,
        log: Arc<Mutex<Vec<String>>>,
        reload_ok: bool,
    ) -> TrayActions {
        let l1 = Arc::clone(&log);
        let l2 = Arc::clone(&log);
        let l3 = Arc::clone(&log);
        let l4 = Arc::clone(&log);
        let l5 = Arc::clone(&log);
        TrayActions {
            confirm: Arc::new(move |_| confirm_result),
            switch_and_reboot: Arc::new(move |id| {
                l1.lock().unwrap().push(format!("switch:{id}"));
                Ok(())
            }),
            set_next: Arc::new(move |id| {
                l2.lock().unwrap().push(format!("next:{id}"));
                Ok(())
            }),
            clear_next: Arc::new(move || {
                l3.lock().unwrap().push("clear".into());
                Ok(())
            }),
            reboot: Arc::new(move || {
                l4.lock().unwrap().push("reboot".into());
                Ok(())
            }),
            current_next: Arc::new(|| None),
            reload_entries: Arc::new(move || {
                if reload_ok {
                    Ok(vec![BootEntry {
                        id: BootId::new(0x0002),
                        name: "Ubuntu 24.04".into(),
                        active: true,
                    }])
                } else {
                    Err(JanusError::Other("模拟读取失败".into()))
                }
            }),
            open_settings: Arc::new(move || {
                l5.lock().unwrap().push("settings".into());
            }),
        }
    }

    #[test]
    fn menu_event_routing_dispatches_expected_actions() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let actions = dummy_actions(true, Arc::clone(&log), true);
        let entries = sample_entries();

        assert!(matches!(
            handle_menu_event("boot:0001", &entries, &actions),
            MenuOutcome::Refresh
        ));
        assert!(matches!(
            handle_menu_event("next:0002", &entries, &actions),
            MenuOutcome::Refresh
        ));
        assert!(matches!(
            handle_menu_event(ID_CLEAR, &entries, &actions),
            MenuOutcome::Refresh
        ));
        assert!(matches!(
            handle_menu_event(ID_REBOOT, &entries, &actions),
            MenuOutcome::Continue
        ));
        assert!(matches!(
            handle_menu_event(ID_SETTINGS, &entries, &actions),
            MenuOutcome::OpenSettings
        ));
        assert!(matches!(
            handle_menu_event(ID_QUIT, &entries, &actions),
            MenuOutcome::Quit
        ));

        let recorded = log.lock().unwrap();
        assert_eq!(
            *recorded,
            vec!["switch:Boot0001", "next:Boot0002", "clear", "reboot"]
        );
    }

    #[test]
    fn cancelled_boot_confirmation_skips_switch() {
        let called = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut actions = dummy_actions(false, log, true);
        let flag = Arc::clone(&called);
        actions.switch_and_reboot = Arc::new(move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        handle_menu_event("boot:0001", &sample_entries(), &actions);
        assert!(!called.load(Ordering::SeqCst));
    }

    #[test]
    fn refresh_entries_keeps_old_list_on_error() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut entries = sample_entries();
        let failing = dummy_actions(true, Arc::clone(&log), false);
        refresh_entries(&mut entries, &failing);
        assert_eq!(entries.len(), 2);

        let succeeding = dummy_actions(true, log, true);
        refresh_entries(&mut entries, &succeeding);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Ubuntu 24.04");
    }
}
