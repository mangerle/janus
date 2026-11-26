//! 设置窗口的自绘窗口装饰：一体化标题栏、窗口控制与无边框拉伸热区。
//!
//! # 设计原理（Why）
//! 原生标题栏颜色由桌面环境决定，与应用深蓝/浅色主题难以一致，因此设置窗口
//! 关闭系统装饰，自绘与内容一体的标题栏。代价是窗口移动/最大化/拉伸需要
//! 自行实现：这里通过 egui 的 `StartDrag`、`Maximized`、`BeginResize` 指令
//! 交给底层 winit 完成，不自行计算窗口几何。

use eframe::egui;

/// 自绘标题栏：品牌标识、未保存指示、窗口拖动与最小化/最大化/关闭。
pub(crate) fn titlebar_ui(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    logo: &egui::TextureHandle,
    dirty: bool,
) {
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let response = egui::Panel::top("janus_titlebar")
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .inner_margin(egui::Margin::symmetric(10, 6)),
        )
        .show(ui, |ui| {
            // 整条空白区域可拖动窗口；后添加的按钮在交互优先级上高于背景
            let drag = ui.interact(
                ui.max_rect(),
                egui::Id::unique("janus-titlebar-drag"),
                egui::Sense::click_and_drag(),
            );
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }

            ui.horizontal(|ui| {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    logo.id(),
                    egui::vec2(26.0, 26.0),
                )));
                ui.add_space(2.0);
                ui.add(egui::Label::new(
                    egui::RichText::new("Janus Boot").size(15.5).strong(),
                ));
                ui.add(egui::Label::new(
                    egui::RichText::new("设置").size(14.0).weak(),
                ));
                if dirty {
                    ui.add_space(4.0);
                    let accent = ui.visuals().selection.bg_fill;
                    let (dot_rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 3.5, accent);
                    ui.add(egui::Label::new(
                        egui::RichText::new("未保存").size(11.5).color(accent),
                    ));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if window_control(ui, WindowControl::Close).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let toggle = if maximized {
                        WindowControl::Restore
                    } else {
                        WindowControl::Maximize
                    };
                    if window_control(ui, toggle).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                    if window_control(ui, WindowControl::Minimize).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                });
            });
        })
        .response;
    // 标题栏底部分隔线，强化层次
    let border = ui.visuals().widgets.noninteractive.bg_stroke;
    let rect = response.rect;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 1.0, rect.bottom()),
            egui::pos2(rect.right() - 1.0, rect.bottom()),
        ],
        border,
    );
}

/// 自绘标题栏的窗口控制按钮。
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowControl {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// 绘制一个单色窗口控制按钮（悬停显示背景，关闭按钮悬停为红色）。
fn window_control(ui: &mut egui::Ui, kind: WindowControl) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(34.0, 26.0), egui::Sense::click());
    let hovered = response.hovered();
    let closing = kind == WindowControl::Close && hovered;
    let background = if closing {
        egui::Color32::from_rgb(0xE5, 0x48, 0x4D)
    } else if hovered {
        ui.visuals().widgets.hovered.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    if background != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(6), background);
    }

    let color = if closing {
        egui::Color32::WHITE
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    draw_window_control_icon(ui.painter(), rect.center(), kind, color);
    response
}

fn draw_window_control_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    kind: WindowControl,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.2, color);
    match kind {
        WindowControl::Minimize => {
            painter.line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y + 3.0),
                    egui::pos2(center.x + 5.0, center.y + 3.0),
                ],
                stroke,
            );
        }
        WindowControl::Maximize => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0)),
                egui::CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        WindowControl::Restore => {
            let front =
                egui::Rect::from_center_size(center + egui::vec2(-1.5, -1.5), egui::vec2(8.0, 8.0));
            let back =
                egui::Rect::from_center_size(center + egui::vec2(1.5, 1.5), egui::vec2(8.0, 8.0));
            painter.rect_stroke(
                back,
                egui::CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                front,
                egui::CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        WindowControl::Close => {
            painter.line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y - 5.0),
                    egui::pos2(center.x + 5.0, center.y + 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x - 5.0, center.y + 5.0),
                    egui::pos2(center.x + 5.0, center.y - 5.0),
                ],
                stroke,
            );
        }
    }
}

/// 无边框窗口的边缘/四角拉伸热区；四角后添加以优先于边缘。
pub(crate) fn resize_edges(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    rect: egui::Rect,
    maximized: bool,
) {
    if maximized {
        return;
    }
    for (index, (region, direction, cursor)) in resize_regions(rect).into_iter().enumerate() {
        let response = ui.interact(
            region,
            egui::Id::unique(("janus-window-resize", index)),
            egui::Sense::drag(),
        );
        if response.hovered() {
            ctx.set_cursor_icon(cursor);
        }
        if response.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}

fn resize_regions(rect: egui::Rect) -> [(egui::Rect, egui::ResizeDirection, egui::CursorIcon); 8] {
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 12.0;
    let (left, right, top, bottom) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    [
        (
            egui::Rect::from_min_max(
                egui::pos2(left + CORNER, top),
                egui::pos2(right - CORNER, top + EDGE),
            ),
            egui::ResizeDirection::North,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(left + CORNER, bottom - EDGE),
                egui::pos2(right - CORNER, bottom),
            ),
            egui::ResizeDirection::South,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(left, top + CORNER),
                egui::pos2(left + EDGE, bottom - CORNER),
            ),
            egui::ResizeDirection::West,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(right - EDGE, top + CORNER),
                egui::pos2(right, bottom - CORNER),
            ),
            egui::ResizeDirection::East,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(left, top),
                egui::pos2(left + CORNER, top + CORNER),
            ),
            egui::ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(right - CORNER, top),
                egui::pos2(right, top + CORNER),
            ),
            egui::ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(left, bottom - CORNER),
                egui::pos2(left + CORNER, bottom),
            ),
            egui::ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_max(
                egui::pos2(right - CORNER, bottom - CORNER),
                egui::pos2(right, bottom),
            ),
            egui::ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeNwSe,
        ),
    ]
}
