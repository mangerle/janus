//! Linux 托盘事件循环（ksni / StatusNotifierItem，纯 D-Bus）。

use crossbeam_channel::{bounded, select};
use janus_core::{BootEntry, Result};
use tray_icon::menu::MenuEvent;

use crate::menu::{build_tray, handle_menu_event, refresh, refresh_entries, spawn_settings};
use crate::{MenuOutcome, TrayActions};

pub(crate) fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
    // tray 必须在整个事件循环期间存活（函数返回时才释放 D-Bus 服务）
    let tray = build_tray(&entries, &actions)?;

    log::info!("托盘已启动（ksni/StatusNotifierItem），点击托盘图标打开菜单，Ctrl+C 退出");

    // ksni 后端自带 D-Bus worker 线程，主线程只需等待两类事件：
    // 菜单动作，以及设置窗口关闭后的重载通知。
    // 注意：切换动作会在此线程内阻塞（等待 pkexec 密码框），不影响托盘响应。
    let (notify_tx, notify_rx) = bounded::<()>(1);
    let menu_rx = MenuEvent::receiver().clone();
    let mut entries = entries;

    loop {
        select! {
            recv(menu_rx) -> event => {
                let Ok(event) = event else { break };
                let id = event.id().0.clone();
                match handle_menu_event(&id, &entries, &actions) {
                    MenuOutcome::Quit => break,
                    MenuOutcome::OpenSettings => spawn_settings(&actions, &notify_tx),
                    // 仅在状态可能已变更时重建菜单，避免退出/打开设置等动作触发无效 D-Bus 刷新
                    MenuOutcome::Refresh => refresh(&tray, &entries, &actions),
                    MenuOutcome::Continue => {}
                }
            }
            recv(notify_rx) -> _ => {
                refresh_entries(&mut entries, &actions);
                refresh(&tray, &entries, &actions);
            }
        }
    }
    Ok(())
}
