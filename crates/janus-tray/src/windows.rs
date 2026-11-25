//! Windows 托盘事件循环（tray-icon + tao）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossbeam_channel::bounded;
use janus_core::{BootEntry, JanusError, Result};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::MenuEvent;

use crate::menu::{build_tray, handle_menu_event, refresh, refresh_entries, spawn_settings};
use crate::{MenuOutcome, TrayActions, ID_QUIT, ID_SETTINGS};

/// tao 事件循环的用户事件：菜单动作、动作完成刷新或设置保存后的重载通知。
#[derive(Clone, Debug)]
enum UserEvent {
    Menu(MenuEvent),
    Refresh,
    Reload,
}

pub(crate) fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
    let tray = build_tray(&entries, &actions)?;
    let (notify_tx, notify_rx) = bounded::<()>(1);
    let actions = Arc::new(actions);
    let shutting_down = Arc::new(AtomicBool::new(false));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    // 后台线程把 muda 菜单事件转发给 tao 事件循环
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    let menu_shutdown = Arc::clone(&shutting_down);
    std::thread::Builder::new()
        .name("janus-menu-events".into())
        .spawn(move || {
            while let Ok(ev) = MenuEvent::receiver().recv() {
                if menu_shutdown.load(Ordering::Acquire)
                    || menu_proxy.send_event(UserEvent::Menu(ev)).is_err()
                {
                    break;
                }
            }
        })
        .map_err(|e| JanusError::Other(format!("创建菜单事件线程失败：{e}")))?;

    // 设置窗口关闭后，把重载通知转交给 tao 事件循环
    let reload_proxy = proxy.clone();
    let reload_shutdown = Arc::clone(&shutting_down);
    std::thread::Builder::new()
        .name("janus-settings-reload".into())
        .spawn(move || {
            while notify_rx.recv().is_ok() {
                if reload_shutdown.load(Ordering::Acquire)
                    || reload_proxy.send_event(UserEvent::Reload).is_err()
                {
                    break;
                }
            }
        })
        .map_err(|e| JanusError::Other(format!("创建设置重载线程失败：{e}")))?;

    let mut entries = entries;
    // tao 的 run 返回 `!`（永不返回），直接作为函数尾表达式
    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                let id = menu_event.id().0.clone();
                if id == ID_QUIT {
                    shutting_down.store(true, Ordering::Release);
                    *control_flow = ControlFlow::Exit;
                } else if id == ID_SETTINGS {
                    spawn_settings(&actions, &notify_tx);
                } else {
                    // 在独立工作线程执行确认弹窗与固件写入，避免阻塞 tao 主消息泵
                    let entries_snapshot = entries.clone();
                    let actions_ref = Arc::clone(&actions);
                    let done_proxy = proxy.clone();
                    if let Err(e) = std::thread::Builder::new()
                        .name("janus-tray-action".into())
                        .spawn(move || {
                            if matches!(
                                handle_menu_event(&id, &entries_snapshot, &actions_ref),
                                MenuOutcome::Refresh
                            ) {
                                let _ = done_proxy.send_event(UserEvent::Refresh);
                            }
                        })
                    {
                        log::error!("创建托盘动作线程失败：{e}");
                    }
                }
            }
            Event::UserEvent(UserEvent::Refresh) => {
                refresh(&tray, &entries, &actions);
            }
            Event::UserEvent(UserEvent::Reload) => {
                refresh_entries(&mut entries, &actions);
                refresh(&tray, &entries, &actions);
            }
            _ => {}
        }
    })
}
