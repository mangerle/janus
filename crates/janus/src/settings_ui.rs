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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use eframe::egui;
use janus_core::{BootEntry, BootManager};

use crate::config::{EntrySettings, Settings};
use crate::settings_update::UpdaterUi;
use crate::settings_widgets::{count_pill, panel_frame, primary_button, row_ui, SettingsRow};

/// 托盘图标同时用作窗口图标与标题 Logo（与托盘同源，32x32 RGBA）。
const ICON_RGBA: &[u8] = include_bytes!("../../janus-tray/assets/icon-32.rgba");
const ICON_DIM: u32 = 32;

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
        rgba: ICON_RGBA.to_vec(),
        width: ICON_DIM,
        height: ICON_DIM,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
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
            cc.egui_ctx
                .set_theme(crate::settings_theme::system_theme_preference());
            crate::settings_theme::apply_theme(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(
                entries,
                &settings,
                config_path,
                read_only,
                warning,
                &cc.egui_ctx,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("设置窗口启动失败：{e}"))
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
}

impl SettingsApp {
    fn new(
        entries: Vec<BootEntry>,
        settings: &Settings,
        config_path: PathBuf,
        read_only: bool,
        warning: Option<String>,
        ctx: &egui::Context,
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
        let logo = ctx.load_texture(
            "janus-logo",
            egui::ColorImage::from_rgba_unmultiplied([ICON_DIM as usize; 2], ICON_RGBA),
            egui::TextureOptions::LINEAR,
        );
        Self {
            rows,
            base: settings.clone(),
            config_path,
            read_only,
            error: warning,
            updater: UpdaterUi::new(read_only),
            logo,
        }
    }

    /// 按行状态更新基线设置并原子写盘；成功则关闭窗口，失败则保留窗口展示错误。
    fn save(&mut self, ctx: &egui::Context) {
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
        match settings.save(&self.config_path) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.error = Some(format!("保存设置失败：{e:#}")),
        }
    }

    /// 标题、启动项分区与“关于与更新”分区。
    fn content_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        let total = self.rows.len();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("启动项显示").size(22.0).strong(),
                ));
                ui.add(egui::Label::new(
                    egui::RichText::new("选择会出现在托盘与 CLI 切换列表里的系统。")
                        .size(12.0)
                        .weak(),
                ));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                count_pill(ui, format!("{total} 个启动项"));
            });
        });
        ui.add_space(12.0);

        if let Some(error) = &self.error {
            let color = ui.visuals().error_fg_color;
            ui.colored_label(color, error);
            ui.add_space(8.0);
        }

        self.list_panel_ui(ui);
        ui.add_space(12.0);
        self.about_panel_ui(ui);
    }

    /// 启动项分区：标题、提示与条目列表。
    fn list_panel_ui(&mut self, ui: &mut egui::Ui) {
        panel_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(
                    egui::RichText::new("启动项").size(13.0).strong(),
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("隐藏项仍可按编号直接切换")
                            .size(11.0)
                            .weak(),
                    );
                });
            });
            ui.add_space(4.0);
            ui.separator();

            if self.rows.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("当前没有可配置的启动项").weak());
                });
                ui.add_space(24.0);
                return;
            }

            let available = ui.available_width();
            let field_width = (available * 0.38).clamp(150.0, 240.0);
            let name_block_width = (available - 44.0 - 74.0 - field_width - 30.0).max(110.0);
            for (index, row) in self.rows.iter_mut().enumerate() {
                if index > 0 {
                    ui.separator();
                }
                row_ui(ui, row, name_block_width, field_width);
            }
        });
    }

    /// 关于分区：版本与更新、配置文件。
    fn about_panel_ui(&mut self, ui: &mut egui::Ui) {
        panel_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(4.0);
            self.updater.about_ui(ui);
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
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
                    let note = if self.read_only {
                        "dry-run · 不写配置"
                    } else {
                        "跟随系统主题"
                    };
                    ui.label(egui::RichText::new(note).size(11.0).weak());
                });
            });
            ui.add_space(4.0);
        });
    }

    /// 底部固定操作栏：提示、取消与保存。
    fn footer_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::bottom("settings_footer")
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let hint = if self.read_only {
                        "dry-run 预览：修改不会写入设置，也不执行更新"
                    } else {
                        "修改仅影响本机显示，不写固件"
                    };
                    let hint_width = (ui.available_width() - 220.0).max(120.0);
                    ui.add_sized(
                        [hint_width, 20.0],
                        egui::Label::new(egui::RichText::new(hint).size(11.0).weak()).truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save_button = primary_button(ui, "保存更改");
                        let save = ui.add_enabled(!self.read_only, save_button);
                        if save.clicked() {
                            self.save(ctx);
                        }
                        if ui.button(egui::RichText::new("取消").size(12.0)).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
                ui.add_space(8.0);
            });
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let window_rect = ui.max_rect();
        // 面板顺序：顶部标题栏、底部操作栏固定，中央内容独立滚动
        crate::settings_chrome::titlebar_ui(ui, &ctx, &self.logo);
        self.footer_ui(ui, &ctx);
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
                        self.content_ui(ui);
                    });
            });
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        crate::settings_chrome::resize_edges(ui, &ctx, window_rect, maximized);
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
