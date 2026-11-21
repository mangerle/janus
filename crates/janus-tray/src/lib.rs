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
const ID_NEXT: &str = "next:"; // 仅设置 BootNext
const ID_INFO: &str = "janus:info";
const ID_CLEAR: &str = "janus:clear";
const ID_REBOOT: &str = "janus:reboot";
const ID_QUIT: &str = "janus:quit";

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod shared {
    use super::{TrayActions, ID_BOOT, ID_CLEAR, ID_INFO, ID_NEXT, ID_QUIT, ID_REBOOT};
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

        let next_desc = (actions.current_next)()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "未设置".into());
        let info = MenuItem::with_id(
            ID_INFO,
            format!("当前下次启动（BootNext）：{next_desc}"),
            false,
            None,
        );
        let _ = menu.append(&info);
        let _ = menu.append(&PredefinedMenuItem::separator());

        let m_reboot = Submenu::new("重启并进入…", true);
        for e in entries {
            let item = MenuItem::with_id(
                format!("{ID_BOOT}{:04X}", e.id.raw()),
                format!("{}  {}", e.id, e.name),
                true,
                None,
            );
            let _ = m_reboot.append(&item);
        }
        let _ = menu.append(&m_reboot);

        let m_next = Submenu::new("仅设置下次启动（不重启）", true);
        for e in entries {
            let item = MenuItem::with_id(
                format!("{ID_NEXT}{:04X}", e.id.raw()),
                format!("{}  {}", e.id, e.name),
                true,
                None,
            );
            let _ = m_next.append(&item);
        }
        let _ = menu.append(&m_next);

        let _ = PredefinedMenuItem::separator();
        if (actions.current_next)().is_some() {
            let clear = MenuItem::with_id(ID_CLEAR, "清除 BootNext", true, None);
            let _ = menu.append(&clear);
        }
        let reboot = MenuItem::with_id(ID_REBOOT, "立即重启（不改启动项）", true, None);
        let _ = menu.append(&reboot);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let _ = menu.append(&quit);

        menu
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

    /// 处理一条菜单事件；返回 `true` 表示用户点击了"退出"。
    pub fn handle_menu_event(id: &str, entries: &[BootEntry], actions: &TrayActions) -> bool {
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
                        log::error!("切换并重启失败：{e}");
                    }
                }
            }
        } else if let Some(rest) = id.strip_prefix(ID_NEXT) {
            if let Ok(raw) = u16::from_str_radix(rest, 16) {
                let id = BootId::new(raw);
                if let Err(e) = (actions.set_next)(id) {
                    log::error!("设置 BootNext 失败：{e}");
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
        } else if id == ID_QUIT {
            return true;
        }
        false
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
    use super::shared::{build_tray, handle_menu_event, refresh};
    use super::{BootEntry, Result, TrayActions};
    use tray_icon::menu::MenuEvent;

    pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
        // tray 必须在整个事件循环期间存活（函数返回时才释放 D-Bus 服务）
        let tray = build_tray(&entries, &actions)?;

        log::info!("托盘已启动（ksni/StatusNotifierItem），点击托盘图标打开菜单，Ctrl+C 退出");

        // ksni 后端自带 D-Bus worker 线程，主线程只需阻塞在菜单事件上。
        // 注意：切换动作会在此线程内阻塞（等待 pkexec 密码框），不影响托盘响应。
        while let Ok(event) = MenuEvent::receiver().recv() {
            let id = event.id().0.clone();
            let quit = handle_menu_event(&id, &entries, &actions);
            // 每次动作后重建菜单，刷新"当前 BootNext"信息项
            refresh(&tray, &entries, &actions);
            if quit {
                break;
            }
        }
        Ok(())
    }
}

// ---------- Windows：tray-icon + tao 事件循环 ----------

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::shared::{build_tray, handle_menu_event, refresh};
    use super::{BootEntry, Result, TrayActions};
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::MenuEvent;

    pub fn run(entries: Vec<BootEntry>, actions: TrayActions) -> Result<()> {
        let tray = build_tray(&entries, &actions)?;

        let event_loop = EventLoopBuilder::<MenuEvent>::with_user_event().build();

        // 后台线程把 muda 菜单事件转发给 tao 事件循环
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            while let Ok(ev) = MenuEvent::receiver().recv() {
                if proxy.send_event(ev).is_err() {
                    break;
                }
            }
        });

        // tao 的 run 返回 `!`（永不返回），直接作为函数尾表达式
        event_loop.run(move |event, _target, control_flow| {
            *control_flow = ControlFlow::Wait;
            if let Event::UserEvent(menu_event) = event {
                let id = menu_event.id().0.clone();
                let quit = handle_menu_event(&id, &entries, &actions);
                refresh(&tray, &entries, &actions);
                if quit {
                    *control_flow = ControlFlow::Exit;
                }
            }
        })
    }
}
