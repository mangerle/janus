//! 设置窗口主题：品牌配色应用与 Linux 系统明暗探测。
//!
//! # 设计原理（Why）
//! - winit 0.30 在 Linux 上 `EventLoop::system_theme()` 恒为 `None`，egui 的
//!   `ThemePreference::System` 实际会落到深色；这里在 Linux 上自行探测系统
//!   偏好，再显式设置主题。Windows 由 winit 提供系统主题，继续用 System。
//! - 探测顺序：XDG Desktop Portal 的 `org.freedesktop.appearance` 颜色方案
//!   （GNOME/KDE 通用）→ gsettings（GNOME）→ `GTK_THEME` 环境变量 → GTK/KDE
//!   配置文件。全部失败时回退 System（egui 的既有默认）。
//! - 外部命令一律设置 400ms 超时并回收子进程，避免探测拖住窗口启动或留下
//!   僵尸进程；超时只记 debug 日志，不影响窗口打开。

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use eframe::egui;

/// 返回设置窗口应使用的主题偏好。
pub(crate) fn system_theme_preference() -> egui::ThemePreference {
    #[cfg(target_os = "linux")]
    {
        if let Some(dark) = detect_linux_dark() {
            let preference = if dark {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            };
            log::debug!("检测到 Linux 系统主题：{preference:?}");
            return preference;
        }
    }
    log::debug!("未能探测系统主题，回退 egui 默认主题偏好");
    egui::ThemePreference::System
}

/// 应用品牌配色、圆角与字号；浅色/深色两套 Style 同步处理。
pub(crate) fn apply_theme(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let dark = style.visuals.dark_mode;
        let accent = if dark {
            egui::Color32::from_rgb(0x7C, 0x8C, 0xFF)
        } else {
            egui::Color32::from_rgb(0x4F, 0x5D, 0xFF)
        };
        let accent_strong = egui::Color32::from_rgb(0x8B, 0x97, 0xF2);

        let visuals = &mut style.visuals;
        if dark {
            visuals.panel_fill = egui::Color32::from_rgb(0x0B, 0x0F, 0x16);
            visuals.window_fill = egui::Color32::from_rgb(0x0B, 0x0F, 0x16);
            visuals.faint_bg_color = egui::Color32::from_rgb(0x12, 0x1A, 0x26);
            visuals.extreme_bg_color = egui::Color32::from_rgb(0x0F, 0x16, 0x21);
            let line = egui::Color32::from_rgb(0x23, 0x30, 0x47);
            visuals.window_stroke = egui::Stroke::new(1.0, line);
            for widget in [
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.corner_radius = egui::CornerRadius::same(8);
            }
            visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, line);
            visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, line);
            visuals.widgets.noninteractive.fg_stroke.color =
                egui::Color32::from_rgb(0xEA, 0xF0, 0xF8);
            visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(0xEA, 0xF0, 0xF8);
            visuals.widgets.hovered.fg_stroke.color = egui::Color32::from_rgb(0xEA, 0xF0, 0xF8);
            visuals.widgets.active.fg_stroke.color = egui::Color32::from_rgb(0xEA, 0xF0, 0xF8);
            visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(0x1B, 0x26, 0x38);
            visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x1B, 0x26, 0x38);
            visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0x23, 0x2F, 0x45);
            visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
            visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0x2B, 0x3A, 0x57);
            visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);
            // 深色里强调色偏亮，按钮文字用深墨色保证对比度
            visuals.selection.stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(0x0B, 0x10, 0x30));
        } else {
            visuals.panel_fill = egui::Color32::WHITE;
            visuals.window_fill = egui::Color32::WHITE;
            visuals.faint_bg_color = egui::Color32::WHITE;
            visuals.extreme_bg_color = egui::Color32::from_rgb(0xF7, 0xF9, 0xFC);
            let line = egui::Color32::from_rgb(0xE7, 0xEB, 0xF2);
            visuals.window_stroke = egui::Stroke::new(1.0, line);
            for widget in [
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.corner_radius = egui::CornerRadius::same(8);
            }
            visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, line);
            visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, line);
            visuals.widgets.noninteractive.fg_stroke.color =
                egui::Color32::from_rgb(0x10, 0x18, 0x28);
            visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(0x10, 0x18, 0x28);
            visuals.widgets.hovered.fg_stroke.color = egui::Color32::from_rgb(0x10, 0x18, 0x28);
            visuals.widgets.active.fg_stroke.color = egui::Color32::from_rgb(0x10, 0x18, 0x28);
            visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(0xF1, 0xF4, 0xFA);
            visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0xF1, 0xF4, 0xFA);
            visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0xF7, 0xF9, 0xFC);
            visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
            visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0xE9, 0xED, 0xF9);
            visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);
            visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        }

        visuals.selection.bg_fill = accent;
        visuals.hyperlink_color = accent_strong;
        visuals.window_corner_radius = egui::CornerRadius::same(12);

        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        style.spacing.interact_size = egui::vec2(24.0, 30.0);
        style.spacing.window_margin = egui::Margin::same(16);
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::new(22.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
        );
    });
}

/// Linux 系统明暗探测；仅作为设置窗口启动时的一次性判断。
#[cfg(target_os = "linux")]
fn detect_linux_dark() -> Option<bool> {
    if let Some(dark) = portal_dark() {
        return Some(dark);
    }
    if let Some(dark) = gsettings_dark() {
        return Some(dark);
    }
    if let Some(dark) = env_dark() {
        return Some(dark);
    }
    config_file_dark()
}

/// XDG Desktop Portal 的 `org.freedesktop.appearance color-scheme`
/// （0=无偏好，1=偏好深色，2=偏好浅色）。
#[cfg(target_os = "linux")]
fn portal_dark() -> Option<bool> {
    let output = run_command(
        "dbus-send",
        &[
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ],
    )?;
    let value = output
        .rsplit("uint32 ")
        .next()
        .and_then(|tail| tail.trim().parse::<u32>().ok())?;
    match value {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// GNOME 的 color-scheme；随后用 gtk-theme 兜底判断。
#[cfg(target_os = "linux")]
fn gsettings_dark() -> Option<bool> {
    let scheme = run_command(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "color-scheme"],
    );
    if let Some(scheme) = scheme {
        if scheme.contains("prefer-dark") {
            return Some(true);
        }
        if scheme.contains("prefer-light") {
            return Some(false);
        }
    }

    let theme = run_command(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "gtk-theme"],
    )?;
    let theme = theme.trim().trim_matches('\'').to_ascii_lowercase();
    if theme.is_empty() {
        return None;
    }
    Some(theme.contains("dark"))
}

/// `GTK_THEME` 环境变量（如 `Adwaita:dark`）。
#[cfg(target_os = "linux")]
fn env_dark() -> Option<bool> {
    let theme = std::env::var("GTK_THEME").ok()?;
    if theme.is_empty() {
        None
    } else {
        Some(theme.to_ascii_lowercase().contains("dark"))
    }
}

/// GTK 3/4 与 KDE 配置文件的兜底探测。
#[cfg(target_os = "linux")]
fn config_file_dark() -> Option<bool> {
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let candidates = [
        home.join(".config/gtk-4.0/settings.ini"),
        home.join(".config/gtk-3.0/settings.ini"),
        home.join(".config/kdeglobals"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lower = text.to_ascii_lowercase();
        for line in lower.lines() {
            let line = line.trim();
            if line == "gtk-application-prefer-dark-theme=1" {
                return Some(true);
            }
            if let Some(value) = line.strip_prefix("gtk-theme-name=") {
                if !value.is_empty() {
                    return Some(value.contains("dark"));
                }
            }
            if let Some(value) = line.strip_prefix("colorscheme=") {
                if !value.is_empty() {
                    return Some(value.contains("dark"));
                }
            }
        }
    }
    None
}

/// 带超时执行探测命令：400ms 未退出即强制结束并回收，避免拖住窗口启动。
#[cfg(target_os = "linux")]
fn run_command(program: &str, args: &[&str]) -> Option<String> {
    use std::io::Read as _;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut output = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut output);
                }
                return Some(output);
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                log::debug!("系统主题探测命令超时：{program}");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
