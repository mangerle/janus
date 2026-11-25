//! 跨平台图形设置窗口（egui/eframe）。
//!
//! # 设计原理（Why）
//! - 托盘与 CLI 共用同一份 `config.toml`，窗口只负责编辑文件，不持有运行时
//!   状态；保存后由调用方（托盘）重新读盘并重建菜单，避免两处状态不同步。
//! - 托盘侧通过子进程运行 `janus settings` 打开窗口：winit 0.30 在 Linux 上
//!   禁止在非主线程初始化事件循环，子进程方案让窗口始终位于其主线程，同时
//!   避免与托盘的 tao 事件循环相互嵌套。
//! - 主题跟随系统浅色/深色，但使用官网同一套品牌色（靛蓝强调、深蓝面板）；
//!   操作栏固定在底部面板，内容区独立滚动，长列表不会挤掉保存/取消按钮。
//! - 使用 `system_fonts` 按需加载系统中文字形，避免把数 MB 的 CJK 字体
//!   打进二进制；代价是极端精简的系统若无中文字体，界面会显示替换符。

mod chrome;
mod theme;
mod update;
mod update_worker;
mod widgets;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use eframe::egui;
use janus_core::{BootEntry, BootManager};

use crate::config::{EntrySettings, Settings};
use update::UpdaterUi;
use widgets::{
    alert_banner, count_pill, ghost_button, panel_frame, primary_button, row_ui, SettingsRow,
};

/// 打开设置窗口并阻塞到窗口关闭；保存成功后写回配置文件。
///
/// `read_only` 为 true（dry-run）时禁用保存与自更新，不产生任何磁盘副作用。
///
/// # Errors
/// 读取启动项失败或窗口初始化失败时返回错误。
pub(crate) fn run_settings_window(
    mgr: Arc<dyn BootManager>,
    config_path: &Path,
    read_only: bool,
) -> anyhow::Result<()> {
    let entries = mgr.list_entries().context("读取启动项列表失败")?;
    let (settings, warning) = match Settings::load(config_path) {
        Ok(settings) => (settings, None),
        Err(e) => (
            Settings::default(),
            Some(format!("读取现有设置失败，已使用默认值：{e:#}")),
        ),
    };
    let config_path = config_path.to_path_buf();
    let icon = egui::IconData {
        rgba: janus_tray::APP_ICON_128_RGBA.to_vec(),
        width: janus_tray::APP_ICON_128_DIM,
        height: janus_tray::APP_ICON_128_DIM,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("com.github.mangerle.janus")
            .with_inner_size([740.0, 540.0])
            .with_min_inner_size([600.0, 420.0])
            // 自绘标题栏：去掉原生灰色装饰条，与深色内容一体化
            .with_decorations(false)
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "Janus 设置",
        options,
        Box::new(move |cc| {
            // Linux 上 winit 的系统主题恒为 None，需自行探测后显式设置
            cc.egui_ctx.set_theme(theme::system_theme_preference());
            theme::apply_theme(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(
                SettingsInit {
                    entries,
                    settings: &settings,
                    config_path,
                    read_only,
                    warning,
                },
                &cc.egui_ctx,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("设置窗口启动失败：{e}"))
}

/// 构造 [`SettingsApp`] 的初始化参数，避免多参数平铺。
struct SettingsInit<'a> {
    entries: Vec<BootEntry>,
    settings: &'a Settings,
    config_path: PathBuf,
    read_only: bool,
    warning: Option<String>,
}

/// 设置窗口应用状态。
struct SettingsApp {
    rows: Vec<SettingsRow>,
    /// 打开窗口时读到的完整设置：保存时以它为基线，保留当前固件中暂不存在的
    /// 启动项配置（例如未插入的 U 盘），避免误删
    base: Settings,
    config_path: PathBuf,
    read_only: bool,
    /// 读取或保存错误提示；保存成功窗口关闭，无需成功提示
    error: Option<String>,
    /// 版本展示与自更新面板
    updater: UpdaterUi,
    /// 标题 Logo 纹理（由托盘图标生成）
    logo: egui::TextureHandle,
    /// 最近一次复制配置文件路径的时间（用于展示“已复制”反馈）
    copied_at: Option<Instant>,
}

impl SettingsApp {
    fn new(init: SettingsInit<'_>, ctx: &egui::Context) -> Self {
        let rows = init
            .entries
            .into_iter()
            .map(|entry| {
                let configured = init.settings.entry(entry.id);
                SettingsRow {
                    id: entry.id,
                    firmware_name: entry.name,
                    visible: configured.map(|s| !s.hidden).unwrap_or(true),
                    display_name: configured.and_then(|s| s.name.clone()).unwrap_or_default(),
                }
            })
            .collect();
        let logo = ctx.load_texture(
            "janus-logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [janus_tray::APP_ICON_128_DIM as usize; 2],
                janus_tray::APP_ICON_128_RGBA,
            ),
            egui::TextureOptions::LINEAR,
        );
        Self {
            rows,
            base: init.settings.clone(),
            config_path: init.config_path,
            read_only: init.read_only,
            error: init.warning,
            updater: UpdaterUi::new(init.read_only),
            logo,
            copied_at: None,
        }
    }

    /// 构造拟保存的配置快照。
    fn build_settings(&self) -> Settings {
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
    fn is_dirty(&self) -> bool {
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

    /// 按行状态更新基线设置并原子写盘；成功则关闭窗口，失败则保留窗口展示错误。
    fn save(&mut self, ctx: &egui::Context) {
        let settings = self.build_settings();
        match settings.save(&self.config_path) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.error = Some(format!("保存设置失败：{e:#}")),
        }
    }

    /// 标题、启动项分区与“关于与更新”分区。
    fn content_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        let total = self.rows.len();
        let visible_count = self.rows.iter().filter(|r| r.visible).count();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("启动项显示").size(22.0).strong(),
                ));
                ui.add(egui::Label::new(
                    egui::RichText::new(
                        "选择会出现在托盘与 CLI 切换列表里的系统，并可自定义别名。",
                    )
                    .size(12.0)
                    .weak(),
                ));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                count_pill(ui, format!("显示 {visible_count} / {total}"), true);
            });
        });
        ui.add_space(14.0);

        if let Some(error) = &self.error {
            alert_banner(ui, error);
            ui.add_space(10.0);
        }

        self.list_panel_ui(ui);
        ui.add_space(14.0);
        self.about_panel_ui(ui, ctx);
    }

    /// 启动项分区：标题、快捷“全部显示”按钮与条目列表。
    fn list_panel_ui(&mut self, ui: &mut egui::Ui) {
        let has_hidden = self.rows.iter().any(|r| !r.visible);
        panel_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("启动项").size(13.0).strong(),
                ));
                ui.label(
                    egui::RichText::new("· 隐藏项仍可按编号直接切换")
                        .size(11.0)
                        .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if has_hidden && ui.add(ghost_button(ui, "全部显示")).clicked() {
                        for row in &mut self.rows {
                            row.visible = true;
                        }
                    }
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);

            if self.rows.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("当前没有可配置的启动项").weak());
                });
                ui.add_space(24.0);
                return;
            }

            let available = ui.available_width();
            let field_width = (available * 0.38).clamp(160.0, 250.0);
            let name_block_width = (available - 44.0 - 74.0 - field_width - 48.0).max(110.0);
            for (index, row) in self.rows.iter_mut().enumerate() {
                if index > 0 {
                    ui.add_space(2.0);
                }
                row_ui(ui, row, name_block_width, field_width);
            }
        });
    }

    /// 关于分区：版本与更新、配置文件（支持一键复制路径）。
    fn about_panel_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let copied_recently = self
            .copied_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
        if copied_recently {
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        panel_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.updater.about_ui(ui);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(
                        egui::RichText::new("配置文件").size(12.0).strong(),
                    ));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(self.config_path.display().to_string())
                                .monospace()
                                .size(11.0)
                                .weak(),
                        )
                        .truncate(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let copy_label = if copied_recently {
                        "已复制"
                    } else {
                        "复制路径"
                    };
                    if ui.add(ghost_button(ui, copy_label)).clicked() {
                        ctx.copy_text(self.config_path.display().to_string());
                        self.copied_at = Some(Instant::now());
                    }
                    if self.read_only {
                        ui.label(egui::RichText::new("dry-run · 只读").size(11.0).weak());
                    }
                });
            });
        });
    }

    /// 底部固定操作栏：状态提示、取消与保存（支持 Ctrl+S 保存与 Esc 取消）。
    fn footer_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, dirty: bool) {
        let border = ui.visuals().widgets.noninteractive.bg_stroke;
        let response = egui::Panel::bottom("settings_footer")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(18, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let hint = if self.read_only {
                        "dry-run 预览：修改不会写入设置，也不执行更新"
                    } else if dirty {
                        "有未保存的更改（Ctrl+S 保存 · Esc 取消）"
                    } else {
                        "修改仅影响本机显示，不写固件"
                    };
                    let hint_width = (ui.available_width() - 200.0).max(120.0);
                    ui.add_sized(
                        [hint_width, 20.0],
                        egui::Label::new(egui::RichText::new(hint).size(11.0).weak()).truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save_button = primary_button(ui, "保存更改");
                        let save = ui.add_enabled(!self.read_only && dirty, save_button);
                        if save.clicked() {
                            self.save(ctx);
                        }
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("取消").size(12.0))
                                    .corner_radius(7),
                            )
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            })
            .response;
        let rect = response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 1.0, rect.top()),
                egui::pos2(rect.right() - 1.0, rect.top()),
            ],
            border,
        );
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dirty = self.is_dirty();

        // 键盘快捷键：Ctrl+S / Cmd+S 保存，Esc 取消关闭
        if !self.read_only
            && dirty
            && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S))
        {
            self.save(&ctx);
        } else if !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let window_rect = ui.max_rect();
        // 面板顺序：顶部标题栏、底部操作栏固定，中央内容独立滚动
        chrome::titlebar_ui(ui, &ctx, &self.logo, dirty);
        self.footer_ui(ui, &ctx, dirty);
        let panel_fill = ui.visuals().panel_fill;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(panel_fill)
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.content_ui(ui, &ctx);
                    });
            });
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        chrome::resize_edges(ui, &ctx, window_rect, maximized);
        // 无边框窗口补一圈细边框，避免与深色桌面融为一体
        let border = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().rect_stroke(
            window_rect.shrink(0.5),
            egui::CornerRadius::ZERO,
            border,
            egui::StrokeKind::Inside,
        );
    }
}
