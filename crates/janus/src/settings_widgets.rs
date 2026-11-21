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
        .inner_margin(egui::Margin::symmetric(14, 6));
    if ui.visuals().dark_mode {
        frame
    } else {
        frame.shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 24,
            spread: 0,
            color: egui::Color32::from_black_alpha(14),
        })
    }
}

/// 右上角计数胶囊。
pub(crate) fn count_pill(ui: &mut egui::Ui, text: String) {
    egui::Frame::new()
        .fill(ui.visuals().widgets.inactive.weak_bg_fill)
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(11)
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0));
        });
}

/// 单行：显示开关、编号胶囊、固件名/状态、显示名称输入框。
pub(crate) fn row_ui(
    ui: &mut egui::Ui,
    row: &mut SettingsRow,
    name_block_width: f32,
    field_width: f32,
) {
    ui.add_space(9.0);
    ui.scope(|ui| {
        if !row.visible {
            ui.multiply_opacity(0.58);
        }
        ui.horizontal(|ui| {
            let switch = toggle_switch(ui, &mut row.visible);
            switch.on_hover_text("关闭后将从托盘与 CLI 列表中隐藏");

            ui.allocate_ui_with_layout(
                egui::vec2(74.0, 22.0),
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
                                egui::RichText::new(row.id.to_string())
                                    .monospace()
                                    .size(11.0)
                                    .weak(),
                            );
                        });
                },
            );

            let alias = row.display_name.trim().to_string();
            let primary = if alias.is_empty() {
                row.firmware_name.clone()
            } else {
                alias
            };
            let secondary = if !row.visible {
                "已隐藏 · 仍可按编号指定".to_string()
            } else if row.display_name.trim().is_empty() {
                "固件名称".to_string()
            } else {
                format!("固件名称 · {}", row.firmware_name)
            };
            ui.allocate_ui_with_layout(
                egui::vec2(name_block_width, 30.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(name_block_width);
                    ui.vertical(|ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(primary).size(13.0)).truncate(),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(secondary).size(11.0).weak())
                                .truncate(),
                        );
                    });
                },
            );

            ui.add_sized(
                [field_width, 32.0],
                egui::TextEdit::singleline(&mut row.display_name).hint_text("使用固件原名"),
            );
        });
    });
    ui.add_space(9.0);
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
    let off = ui.visuals().widgets.inactive.bg_fill;
    let accent = ui.visuals().selection.bg_fill;
    let track_color = lerp_color(off, accent, t);
    ui.painter().rect_filled(track, 10, track_color);
    if response.hovered() || response.has_focus() {
        let color = if response.has_focus() {
            accent
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        ui.painter().rect_stroke(
            track.expand(2.0),
            12,
            egui::Stroke::new(1.0, color),
            egui::StrokeKind::Outside,
        );
    }
    let knob_x = egui::lerp((track.left() + 2.0)..=(track.right() - 18.0), t);
    let knob = egui::pos2(knob_x + 8.0, track.center().y);
    ui.painter().circle_filled(knob, 8.0, egui::Color32::WHITE);
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
    egui::Button::new(egui::RichText::new(text.to_owned()).size(12.0).color(ink)).fill(accent)
}
