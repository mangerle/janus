//! 设置窗口的控件与卡片样式：分区卡片、计数胶囊、启动项行与自绘开关。

use eframe::egui;
use janus_core::BootId;

/// 一行的编辑状态：显示开关与自定义名称。
pub(crate) struct SettingsRow {
    pub(crate) id: BootId,
    pub(crate) firmware_name: String,
    pub(crate) visible: bool,
    pub(crate) display_name: String,
}

/// 主题感知的分区卡片：白底/深蓝面板、细边框、浅色模式带轻投影。
pub(crate) fn panel_frame(ui: &egui::Ui) -> egui::Frame {
    let frame = egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(16, 12));
    if ui.visuals().dark_mode {
        frame
    } else {
        frame.shadow(egui::epaint::Shadow {
            offset: [0, 8],
            blur: 20,
            spread: 0,
            color: egui::Color32::from_black_alpha(12),
        })
    }
}

/// 胶囊标签；`accent` 为 true 时使用主题强调色底纹。
pub(crate) fn count_pill(ui: &mut egui::Ui, text: impl Into<String>, accent: bool) {
    let accent_color = ui.visuals().selection.bg_fill;
    let (fill, stroke, text_color) = if accent {
        (
            accent_color.gamma_multiply(0.14),
            accent_color.gamma_multiply(0.45),
            accent_color,
        )
    } else {
        (
            ui.visuals().widgets.inactive.weak_bg_fill,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
            ui.visuals().widgets.inactive.fg_stroke.color,
        )
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(11)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text.into())
                    .size(11.0)
                    .color(text_color),
            );
        });
}

/// 错误提示横幅：带圆角、淡红底纹与描边。
pub(crate) fn alert_banner(ui: &mut egui::Ui, message: &str) {
    let err = ui.visuals().error_fg_color;
    egui::Frame::new()
        .fill(err.gamma_multiply(0.10))
        .stroke(egui::Stroke::new(1.0, err.gamma_multiply(0.45)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(message).size(12.0).color(err));
        });
}

/// 单行：显示开关、编号胶囊、固件名/状态、显示名称输入框（带悬停高亮与一键清除）。
pub(crate) fn row_ui(
    ui: &mut egui::Ui,
    row: &mut SettingsRow,
    name_block_width: f32,
    field_width: f32,
) {
    let row_frame = egui::Frame::new()
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 8));
    let mut prepared = row_frame.begin(ui);
    {
        let ui = &mut prepared.content_ui;
        ui.horizontal(|ui| {
            // 开关始终保持全不透明，确保关闭状态下轨道与滑块依然清晰可辨
            let switch = toggle_switch(ui, &mut row.visible);
            switch.on_hover_text("关闭后将从托盘与 CLI 列表中隐藏");

            ui.scope(|ui| {
                if !row.visible {
                    ui.multiply_opacity(0.65);
                }
                row_id_badge(ui, row.id);
                row_name_block(ui, row, name_block_width);
                row_alias_input(ui, &mut row.display_name, field_width);
            });
        });
    }
    let response = prepared.allocate_space(ui);
    if response.hovered() {
        let hover_fill = ui
            .visuals()
            .widgets
            .hovered
            .weak_bg_fill
            .gamma_multiply(0.45);
        prepared.frame.fill = hover_fill;
    }
    prepared.paint(ui);
}

fn row_id_badge(ui: &mut egui::Ui, id: BootId) {
    ui.allocate_ui_with_layout(
        egui::vec2(74.0, 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(74.0);
            egui::Frame::new()
                .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .stroke(egui::Stroke::new(
                    1.0,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(id.to_string())
                            .monospace()
                            .size(11.0)
                            .weak(),
                    );
                });
        },
    );
}

fn row_name_block(ui: &mut egui::Ui, row: &SettingsRow, width: f32) {
    let alias = row.display_name.trim();
    let primary = if alias.is_empty() {
        row.firmware_name.as_str()
    } else {
        alias
    };
    let secondary = if !row.visible {
        "已隐藏 · 仍可按编号指定".to_string()
    } else if alias.is_empty() {
        "固件名称".to_string()
    } else {
        format!("固件原名 · {}", row.firmware_name)
    };
    ui.allocate_ui_with_layout(
        egui::vec2(width, 32.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(primary).size(13.0).strong()).truncate(),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(secondary).size(11.0).weak()).truncate(),
                );
            });
        },
    );
}

fn row_alias_input(ui: &mut egui::Ui, display_name: &mut String, field_width: f32) {
    let has_text = !display_name.trim().is_empty();
    let clear_width = if has_text { 26.0 } else { 0.0 };
    let input_width = (field_width - clear_width).max(110.0);
    ui.add_sized(
        [input_width, 30.0],
        egui::TextEdit::singleline(display_name)
            .margin(egui::Margin::symmetric(10, 6))
            .hint_text("使用固件原名"),
    );
    if has_text
        && clear_icon_button(ui)
            .on_hover_text("恢复固件原名")
            .clicked()
    {
        display_name.clear();
    }
}

/// 矢量绘制的“×”清除按钮。
fn clear_icon_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 6, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let color = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    let stroke = egui::Stroke::new(1.3, color);
    let c = rect.center();
    ui.painter().line_segment(
        [
            egui::pos2(c.x - 3.5, c.y - 3.5),
            egui::pos2(c.x + 3.5, c.y + 3.5),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            egui::pos2(c.x - 3.5, c.y + 3.5),
            egui::pos2(c.x + 3.5, c.y - 3.5),
        ],
        stroke,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 自绘开关：44x32 交互区，轨道 40x20，开启时用品牌强调色。
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(44.0, 32.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(2.0, 6.0),
        egui::vec2(40.0, 20.0),
    );
    let off = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(0x28, 0x35, 0x4A)
    } else {
        egui::Color32::from_rgb(0xCB, 0xD5, 0xE1)
    };
    let accent = ui.visuals().selection.bg_fill;
    let track_color = lerp_color(off, accent, t);
    ui.painter().rect_filled(track, 10, track_color);
    let outline_color = if response.has_focus() {
        accent
    } else if response.hovered() {
        ui.visuals().widgets.hovered.bg_stroke.color
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    ui.painter().rect_stroke(
        track,
        10,
        egui::Stroke::new(1.0, outline_color),
        egui::StrokeKind::Inside,
    );
    let knob_x = egui::lerp((track.left() + 2.0)..=(track.right() - 18.0), t);
    let knob = egui::pos2(knob_x + 8.0, track.center().y);
    ui.painter().circle_filled(knob, 8.0, egui::Color32::WHITE);
    if !ui.visuals().dark_mode {
        ui.painter().circle_stroke(
            knob,
            8.0,
            egui::Stroke::new(0.8, egui::Color32::from_black_alpha(32)),
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 两色线性插值（仅用于开关轨道过渡）。
fn lerp_color(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| -> u8 {
        (a as f32 + (b as f32 - a as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

/// 主操作按钮：强调色填充，文字颜色由主题给出对比色。
pub(crate) fn primary_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let accent = ui.visuals().selection.bg_fill;
    let ink = ui.visuals().selection.stroke.color;
    egui::Button::new(egui::RichText::new(text.to_owned()).size(12.0).color(ink))
        .fill(accent)
        .corner_radius(7)
}

/// 次要操作用幽灵按钮：透明底、强调色文字、细边框。
pub(crate) fn ghost_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    let accent = ui.visuals().selection.bg_fill;
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .size(12.0)
            .color(accent),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
    .corner_radius(7)
}
