//! Janus 系统托盘模块。
//!
//! 平台差异：
//! - **Linux**：使用 tray-icon 的 `ksni` 后端（纯 D-Bus 的 StatusNotifierItem，
//!   无需 libappindicator/GTK）。ksni 后端自带 D-Bus worker 线程，因此主线程
//!   只需阻塞在菜单事件 channel 上。
//! - **Windows**：使用 tray-icon 的 Win32 通知区实现，需要 tao 事件循环泵
//!   消息，并用后台线程把菜单事件转发给事件循环。
//!
//! # 设计原理（Why）
//! 托盘本身不做提权：所有写固件的动作都通过 [`TrayActions`] 回调交由上层
//! （janus 主程序）决定是直接执行、pkexec 提权还是 dry-run 模拟——这使
//! 托盘层保持平台中立，权限策略集中在单一位置可审计。

use std::sync::Arc;

use janus_core::{BootEntry, BootId, Result};

/// 托盘动作回调集合（由上层提供）。
pub struct TrayActions {
    /// 确认对话框；返回 `false` 取消。Linux 上通常直接返回 `true`——
    /// pkexec 的密码输入框本身就是确认步骤。
    pub confirm: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// 设置 BootNext 并立即重启。
    pub switch_and_reboot: Arc<dyn Fn(BootId) -> Result<()> + Send + Sync>,
    /// 仅设置 BootNext（不重启）。
    pub set_next: Arc<dyn Fn(BootId) -> Result<()> + Send + Sync>,
    /// 清除 BootNext。
    pub clear_next: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    /// 立即重启（不改启动项）。
    pub reboot: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    /// 查询当前 BootNext（用于菜单信息展示）。
    pub current_next: Arc<dyn Fn() -> Option<BootId> + Send + Sync>,
    /// 重新读取启动项并按最新显示设置过滤/改名；设置保存后重建菜单用。
    pub reload_entries: Arc<dyn Fn() -> Result<Vec<BootEntry>> + Send + Sync>,
    /// 打开设置窗口（阻塞直到窗口关闭）；托盘在后台线程调用。
    pub open_settings: Arc<dyn Fn() + Send + Sync>,
}

/// 启动托盘并进入事件循环（阻塞直到用户点击"退出"）。
///
/// # Errors
/// 托盘创建失败（无 D-Bus 会话、无可用通知区等）时返回
/// [`JanusError::Other`]；事件循环内的动作失败只记录日志，不中断托盘。
pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::run(entries, actions)
    }
    #[cfg(target_os = "windows")]
    {
        windows_impl::run(entries, actions)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (entries, actions);
        Err(janus_core::JanusError::Other("此平台暂不支持托盘".into()))
    }
}

/// 菜单项 ID 前缀约定。
const ID_BOOT: &str = "boot:"; // 重启并进入该系统
const ID_NEXT: &str = "next:"; // 仅下次启动
const ID_INFO: &str = "janus:info";
const ID_EMPTY: &str = "janus:empty";
const ID_CLEAR: &str = "janus:clear";
const ID_REBOOT: &str = "janus:reboot";
const ID_SETTINGS: &str = "janus:settings";
const ID_QUIT: &str = "janus:quit";

/// 菜单事件处理结果（平台循环据此决定退出/开设置窗口）。
pub enum MenuOutcome {
    /// 普通动作，菜单保持运行
    Continue,
    /// 用户点击"退出"
    Quit,
    /// 用户点击"设置…"，需在后台线程打开设置窗口
    OpenSettings,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod shared {
    use std::sync::Arc;

    use crossbeam_channel::Sender;

    use super::{
        MenuOutcome, TrayActions, ID_BOOT, ID_CLEAR, ID_EMPTY, ID_INFO, ID_NEXT, ID_QUIT,
        ID_REBOOT, ID_SETTINGS,
    };
    use janus_core::{BootEntry, BootId, JanusError, Result};
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    /// 构建托盘菜单（每次状态变化后重建整个菜单）。
    ///
    /// # 设计原理（Why）
    /// muda 菜单不支持原位更新单项文本，整棵重建 + `set_menu` 替换是官方
    /// 代价最小的刷新方式；菜单项 ID 保持稳定，重建不影响事件路由。
    pub fn build_menu(entries: &[BootEntry], actions: &TrayActions) -> Menu {
        let menu = Menu::new();
        let current_next = (actions.current_next)();

        // 信息行尽量展示自定义名称；当前下次启动项被隐藏时退回编号
        let next_desc = match current_next {
            Some(id) => entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| format!("{}（{}）", e.name, e.id))
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
            let _ = menu.append(&entry_submenu("重启并进入", ID_BOOT, entries));
            let _ = menu.append(&entry_submenu("仅下次启动", ID_NEXT, entries));
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
    fn entry_submenu(title: &str, prefix: &str, entries: &[BootEntry]) -> Submenu {
        let submenu = Submenu::new(title, true);
        for e in entries {
            let item = MenuItem::with_id(
                format!("{prefix}{:04X}", e.id.raw()),
                format!("{}  {}", e.id, e.name),
                true,
                None,
            );
            let _ = submenu.append(&item);
        }
        submenu
    }

    /// 创建托盘图标。
    ///
    /// # Errors
    /// tray-icon 初始化失败（如无可用通知区宿主）时返回 [`JanusError::Other`]。
    pub fn build_tray(entries: &[BootEntry], actions: &TrayActions) -> Result<TrayIcon> {
        TrayIconBuilder::new()
            .with_menu(Box::new(build_menu(entries, actions)))
            .with_tooltip("Janus —— UEFI 启动切换")
            .with_icon(generate_icon())
            .build()
            .map_err(|e| JanusError::Other(format!("创建托盘失败：{e}")))
    }

    /// 动作执行后刷新菜单（更新 BootNext 信息项与"清除"可见性）。
    pub fn refresh(tray: &TrayIcon, entries: &[BootEntry], actions: &TrayActions) {
        tray.set_menu(Some(Box::new(build_menu(entries, actions))));
    }

    /// 处理一条菜单事件；调用方据 [`MenuOutcome`] 决定退出或打开设置窗口。
    pub fn handle_menu_event(
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
                    }
                }
            }
        } else if let Some(rest) = id.strip_prefix(ID_NEXT) {
            if let Ok(raw) = u16::from_str_radix(rest, 16) {
                let id = BootId::new(raw);
                if let Err(e) = (actions.set_next)(id) {
                    log::error!("设置下次启动失败：{e}");
                } else if let Some(e) = entries.iter().find(|e| e.id == id) {
                    log::info!("已设置下次启动：{}（{}）", e.id, e.name);
                }
            }
        } else if id == ID_CLEAR {
            if let Err(e) = (actions.clear_next)() {
                log::error!("清除 BootNext 失败：{e}");
            } else {
                log::info!("已清除 BootNext");
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
    pub fn spawn_settings(actions: &TrayActions, notify: &Sender<()>) {
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
    pub fn refresh_entries(entries: &mut Vec<BootEntry>, actions: &TrayActions) {
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
    pub fn generate_icon() -> Icon {
        const W: u32 = 32;
        const H: u32 = 32;
        const ICON_RGBA_32: &[u8] = include_bytes!("../assets/icon-32.rgba");
        debug_assert_eq!(ICON_RGBA_32.len(), (W * H * 4) as usize);
        Icon::from_rgba(ICON_RGBA_32.to_vec(), W, H).expect("内置图标数据无效")
    }
}

// ---------- Linux：ksni 后端，无需事件循环 ----------

#[cfg(target_os = "linux")]
mod linux {
    use super::shared::{build_tray, handle_menu_event, refresh, refresh_entries, spawn_settings};
    use super::{BootEntry, MenuOutcome, Result, TrayActions};
    use crossbeam_channel::{bounded, select};
    use tray_icon::menu::MenuEvent;

    pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
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
                        MenuOutcome::Continue => {}
                    }
                    // 每次动作后重建菜单，刷新"当前下次启动"信息项
                    refresh(&tray, &entries, &actions);
                }
                recv(notify_rx) -> _ => {
                    refresh_entries(&mut entries, &actions);
                    refresh(&tray, &entries, &actions);
                }
            }
        }
        Ok(())
    }
}

// ---------- Windows：tray-icon + tao 事件循环 ----------

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::shared::{build_tray, handle_menu_event, refresh, refresh_entries, spawn_settings};
    use super::{BootEntry, MenuOutcome, Result, TrayActions};
    use crossbeam_channel::bounded;
    use janus_core::JanusError;
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::MenuEvent;

    /// tao 事件循环的用户事件：菜单动作或设置保存后的重载通知。
    #[derive(Clone, Debug)]
    enum UserEvent {
        Menu(MenuEvent),
        Reload,
    }

    pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
        let tray = build_tray(&entries, &actions)?;
        let (notify_tx, notify_rx) = bounded::<()>(1);

        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

        // 后台线程把 muda 菜单事件转发给 tao 事件循环
        let proxy = event_loop.create_proxy();
        let menu_proxy = proxy.clone();
        std::thread::Builder::new()
            .name("janus-menu-events".into())
            .spawn(move || {
                while let Ok(ev) = MenuEvent::receiver().recv() {
                    if menu_proxy.send_event(UserEvent::Menu(ev)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| JanusError::Other(format!("创建菜单事件线程失败：{e}")))?;

        // 设置窗口关闭后，把重载通知转交给 tao 事件循环
        let reload_proxy = proxy;
        std::thread::Builder::new()
            .name("janus-settings-reload".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    if reload_proxy.send_event(UserEvent::Reload).is_err() {
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
                    match handle_menu_event(&id, &entries, &actions) {
                        MenuOutcome::Quit => *control_flow = ControlFlow::Exit,
                        MenuOutcome::OpenSettings => spawn_settings(&actions, &notify_tx),
                        MenuOutcome::Continue => {}
                    }
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
}
