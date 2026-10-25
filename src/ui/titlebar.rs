//! TMOP's own window frame, for Wayland compositors that draw no title bar (GNOME; see
//! [`crate::platform::desktop::own_title_bar`]): the header doubles as the title bar (drag to
//! move, double-click to maximize), window buttons sit at its right end, and the window edges
//! resize it. Elsewhere the window manager's decorations are used and none of this is drawn.

use super::theme::Palette;
use eframe::egui::{self, pos2, vec2, Color32, Id, LayerId, Order, Pos2, Rect, ResizeDirection, Sense, Stroke, ViewportCommand};

/// Width of the band along each window edge that resizes the window.
pub const EDGE: f32 = 5.0;
/// How far a corner's diagonal resize reaches along the edges.
pub const CORNER: f32 = 14.0;
/// A window button (minimize, maximize, close), like the header's icon buttons.
const BUTTON: f32 = 40.0;
const BUTTON_GAP: f32 = 4.0;
/// Fill of the close button under the pointer.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xD9, 0x3A, 0x4A);

/// The window button under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

/// Which resize `pos` starts in a window of `rect`: within `band` of an edge, diagonal within
/// `corner` of a corner. `None` inside or outside the window.
pub fn resize_direction(rect: Rect, pos: Pos2, band: f32, corner: f32) -> Option<ResizeDirection> {
    if !rect.contains(pos) {
        return None;
    }
    let near = |d: f32| (d < band, d < corner);
    let (l, lc) = near(pos.x - rect.left());
    let (r, rc) = near(rect.right() - pos.x);
    let (t, tc) = near(pos.y - rect.top());
    let (b, bc) = near(rect.bottom() - pos.y);
    use ResizeDirection::*;
    Some(match () {
        _ if (t && lc) || (l && tc) => NorthWest,
        _ if (t && rc) || (r && tc) => NorthEast,
        _ if (b && lc) || (l && bc) => SouthWest,
        _ if (b && rc) || (r && bc) => SouthEast,
        _ if t => North,
        _ if b => South,
        _ if l => West,
        _ if r => East,
        _ => return None,
    })
}

fn cursor(dir: ResizeDirection) -> egui::CursorIcon {
    use egui::CursorIcon as C;
    match dir {
        ResizeDirection::North => C::ResizeNorth,
        ResizeDirection::South => C::ResizeSouth,
        ResizeDirection::East => C::ResizeEast,
        ResizeDirection::West => C::ResizeWest,
        ResizeDirection::NorthEast => C::ResizeNorthEast,
        ResizeDirection::SouthEast => C::ResizeSouthEast,
        ResizeDirection::NorthWest => C::ResizeNorthWest,
        ResizeDirection::SouthWest => C::ResizeSouthWest,
    }
}

/// Maximized or fullscreen: no edges to resize or outline.
fn filled(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false))
}

/// Makes the empty part of the header `rect` the title bar. Register it before the header's
/// controls so they stay on top of it.
pub fn drag_area(ui: &mut egui::Ui, rect: Rect) {
    let resp = ui.interact(rect, Id::new("tmop-titlebar-drag"), Sense::click_and_drag());
    if resp.double_clicked() {
        let maximized = filled(ui.ctx());
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if resp.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Minimize, maximize (restore) and close at the right of the header, ending at `right` and
/// centred on `cy`, with a rule left of them. Returns the x left of the rule.
pub fn window_buttons(ui: &mut egui::Ui, right: f32, cy: f32, pal: &Palette) -> f32 {
    let maximized = filled(ui.ctx());
    let order = [WindowButton::Minimize, WindowButton::Maximize, WindowButton::Close];
    let left = right - 3.0 * BUTTON - 2.0 * BUTTON_GAP;
    for (i, kind) in order.into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(left + i as f32 * (BUTTON + BUTTON_GAP), cy - BUTTON / 2.0), vec2(BUTTON, BUTTON));
        let resp = ui.interact(rect, Id::new(("tmop-window-button", i)), Sense::click());
        let p = ui.painter();
        let hot = resp.hovered();
        let color = match (kind, hot) {
            (WindowButton::Close, true) => {
                p.rect_filled(rect, 6.0, CLOSE_HOVER);
                Color32::WHITE
            }
            (_, true) => {
                p.rect_filled(rect, 6.0, pal.raise(0.06));
                pal.text
            }
            _ => pal.text_faint,
        };
        draw_glyph(p, rect.center(), kind, maximized, color);
        let tip = match kind {
            WindowButton::Minimize => "Minimize",
            WindowButton::Maximize if maximized => "Restore",
            WindowButton::Maximize => "Maximize",
            WindowButton::Close => "Close",
        };
        if resp.on_hover_text(tip).clicked() {
            ui.ctx().send_viewport_cmd(match kind {
                WindowButton::Minimize => ViewportCommand::Minimized(true),
                WindowButton::Maximize => ViewportCommand::Maximized(!maximized),
                WindowButton::Close => ViewportCommand::Close,
            });
        }
    }
    let rule = left - 10.0;
    ui.painter().line_segment([pos2(rule + 0.5, cy - 12.0), pos2(rule + 0.5, cy + 12.0)], Stroke::new(1.0_f32, pal.sidebar_rule));
    rule
}

/// The 10 px line glyphs of the window buttons.
fn draw_glyph(p: &egui::Painter, c: Pos2, kind: WindowButton, maximized: bool, color: Color32) {
    let s = Stroke::new(1.3_f32, color);
    let h = 5.0;
    match kind {
        WindowButton::Minimize => {
            p.line_segment([pos2(c.x - h, c.y + 0.5), pos2(c.x + h, c.y + 0.5)], s);
        }
        WindowButton::Maximize if maximized => {
            // Restore: a front square and the edge of one behind it.
            let front = Rect::from_min_size(pos2(c.x - h, c.y - h + 2.0), vec2(2.0 * h - 2.0, 2.0 * h - 2.0));
            p.rect_stroke(front, 1.0, s);
            p.line_segment([pos2(front.left() + 2.0, front.top() - 2.0), pos2(front.right() + 2.0, front.top() - 2.0)], s);
            p.line_segment([pos2(front.right() + 2.0, front.top() - 2.0), pos2(front.right() + 2.0, front.bottom() - 2.0)], s);
        }
        WindowButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, vec2(2.0 * h, 2.0 * h)), 1.0, s);
        }
        WindowButton::Close => {
            p.line_segment([pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)], s);
            p.line_segment([pos2(c.x + h, c.y - h), pos2(c.x - h, c.y + h)], s);
        }
    }
}

/// The window's edges: a 1 px outline (the window has no frame of its own), and the resize
/// cursor and resize along them. Call it after the panels so its cursor wins.
pub fn frame(ctx: &egui::Context, pal: &Palette) {
    if filled(ctx) {
        return;
    }
    let screen = ctx.screen_rect();
    ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("tmop-window-edge"))).rect_stroke(screen.shrink(0.5), 0.0, Stroke::new(1.0_f32, pal.sidebar_rule));
    let Some(dir) = ctx.input(|i| i.pointer.hover_pos()).and_then(|pos| resize_direction(screen, pos, EDGE, CORNER)) else { return };
    ctx.set_cursor_icon(cursor(dir));
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;

    #[test]
    fn edges_and_corners_pick_the_resize() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let at = |x: f32, y: f32| resize_direction(r, pos2(x, y), EDGE, CORNER);
        use ResizeDirection::*;
        assert_eq!(at(400.0, 300.0), None, "inside");
        assert_eq!(at(400.0, 1.0), Some(North));
        assert_eq!(at(400.0, 599.0), Some(South));
        assert_eq!(at(0.0, 300.0), Some(West));
        assert_eq!(at(800.0, 300.0), Some(East));
        assert_eq!(at(1.0, 1.0), Some(NorthWest));
        assert_eq!(at(10.0, 2.0), Some(NorthWest), "along the top, still within the corner");
        assert_eq!(at(20.0, 2.0), Some(North));
        assert_eq!(at(798.0, 590.0), Some(SouthEast));
        assert_eq!(at(790.0, 2.0), Some(NorthEast));
        assert_eq!(at(2.0, 590.0), Some(SouthWest));
        assert_eq!(at(400.0, EDGE), None, "the band ends at EDGE");
        assert_eq!(at(-1.0, 300.0), None, "outside");
    }

    /// The header pieces and the frame lay out in a frame, maximized or not.
    #[test]
    fn draws_in_a_frame() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let full = ui.max_rect();
                    drag_area(ui, full);
                    let left = window_buttons(ui, full.right() - 12.0, full.top() + 36.0, &pal);
                    assert!((full.right() - 12.0 - left - (3.0 * BUTTON + 2.0 * BUTTON_GAP + 10.0)).abs() < 0.01);
                });
                frame(ctx, &pal);
            });
        }
    }
}
