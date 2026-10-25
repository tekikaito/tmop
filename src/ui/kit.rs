//! Layout pieces shared by the pages: section cards, stat tiles, key/value grids, code boxes,
//! dialogs and menus, header toolbars and simple row/column splitting.

use super::hardware::{self, Hardware};
use super::theme::{self, with_alpha, Palette};
use super::widgets::{self, card, led_panel, led_row, tracked_text, Button, ButtonKind, ButtonSize, Icon, LedStyle, LED_PANEL_H};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Sense, Stroke};

pub const MARGIN: f32 = 12.0;
pub const GAP: f32 = 10.0;

/// Splits `rect` horizontally into parts proportional to `weights`.
pub fn hsplit(rect: Rect, weights: &[f32], gap: f32) -> Vec<Rect> {
    let total: f32 = weights.iter().sum::<f32>().max(1e-6);
    let avail = rect.width() - gap * (weights.len().saturating_sub(1)) as f32;
    let mut x = rect.left();
    weights
        .iter()
        .map(|w| {
            let wd = avail * w / total;
            let r = Rect::from_min_size(pos2(x, rect.top()), vec2(wd, rect.height()));
            x += wd + gap;
            r
        })
        .collect()
}

/// Splits `rect` vertically; `heights` are fixed sizes, a negative entry takes the rest.
pub fn vsplit(rect: Rect, heights: &[f32], gap: f32) -> Vec<Rect> {
    let fixed: f32 = heights.iter().filter(|h| **h >= 0.0).sum();
    let flex = heights.iter().filter(|h| **h < 0.0).count().max(1) as f32;
    let rest = (rect.height() - fixed - gap * (heights.len().saturating_sub(1)) as f32).max(0.0) / flex;
    let mut y = rect.top();
    heights
        .iter()
        .map(|h| {
            let hh = if *h < 0.0 { rest } else { *h };
            let r = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), hh));
            y += hh + gap;
            r
        })
        .collect()
}

/// Card with a dot-matrix title strip. Returns the content rect below the strip.
pub fn titled_card(p: &egui::Painter, rect: Rect, pal: &Palette, bloom: f32, title: &str, right: &str) -> Rect {
    card(p, rect, pal);
    let inner = rect.shrink(8.0);
    led_panel(p, Rect::from_min_size(inner.min, vec2(inner.width(), LED_PANEL_H)), &title.to_uppercase(), &right.to_uppercase(), pal, bloom);
    Rect::from_min_max(pos2(inner.left(), inner.top() + LED_PANEL_H + 8.0), inner.max)
}

/// Stat card: dot-matrix title, big value, a dim sub line and an optional LED bar.
#[allow(clippy::too_many_arguments)]
pub fn stat_card(p: &egui::Painter, rect: Rect, pal: &Palette, bloom: f32, title: &str, value: &str, sub: &str, color: Color32, frac: Option<f32>) {
    let body = titled_card(p, rect, pal, bloom, title, "");
    let clip = p.with_clip_rect(body);
    clip.text(pos2(body.left() + 2.0, body.top() + 2.0), Align2::LEFT_TOP, value, theme::display(20.0), color);
    if bloom > 0.0 {
        clip.text(pos2(body.left() + 2.0, body.top() + 2.0), Align2::LEFT_TOP, value, theme::display(20.0), with_alpha(color, 0.10 * bloom));
    }
    clip.text(pos2(body.left() + 2.0, body.top() + 34.0), Align2::LEFT_TOP, sub, theme::ui(12.0), pal.text_dim);
    if let Some(f) = frac {
        let bar = Rect::from_min_max(pos2(body.left(), body.bottom() - 12.0), pos2(body.right(), body.bottom()));
        let segs = ((bar.width() / 9.0).floor() as usize).clamp(8, 80);
        led_row(p, bar, segs, f, &LedStyle { unlit_factor: 0.18, rim: false, rounding: 1.0, ..LedStyle::new(color, bloom) });
    }
}

/// Lays out a row of stat tiles from `(title, value, sub, colour, bar fraction)`: the colour tints the bar.
pub fn stat_row(p: &egui::Painter, rect: Rect, pal: &Palette, _bloom: f32, cards: &[(String, String, String, Color32, Option<f32>)]) {
    let w: Vec<f32> = vec![1.0; cards.len()];
    for (r, (t, v, s, c, f)) in hsplit(rect, &w, STAT_GAP).into_iter().zip(cards) {
        stat_tile(p, r, pal, t, v, s, f.map(|f| (f, *c)), None);
    }
}

/// Right-aligned toolbar area on the page header row.
pub fn header_toolbar<R>(ui: &mut egui::Ui, body: Rect, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = Rect::from_min_max(pos2(body.right() - width, body.top() - super::HEADER_H + 14.0), pos2(body.right() - 16.0, body.top() - 14.0));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(r).layout(Layout::right_to_left(Align::Center)), add).inner
}

/// Interactive child UI in `rect` (left-to-right).
pub fn row_ui<R>(ui: &mut egui::Ui, rect: Rect, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), add).inner
}

/// Centered hint text (empty states, loading).
pub fn empty_state(p: &egui::Painter, rect: Rect, pal: &Palette, text: &str) {
    p.text(rect.center(), Align2::CENTER_CENTER, text, theme::ui(13.0), pal.text_faint);
}

/// Label/value grid inside `rect`; returns the bottom y.
pub fn kv_list(p: &egui::Painter, rect: Rect, pal: &Palette, items: &[(String, String)], label_w: f32, row_h: f32) -> f32 {
    let mut y = rect.top();
    for (k, v) in items {
        if y + row_h > rect.bottom() + 1.0 {
            break;
        }
        super::widgets::text_fit(p, pos2(rect.left(), y + row_h / 2.0), Align2::LEFT_CENTER, &format!("{k}:"), theme::ui(12.0), pal.text_dim, label_w - 8.0);
        let clip = p.with_clip_rect(Rect::from_min_max(pos2(rect.left() + label_w, y), pos2(rect.right(), y + row_h)));
        clip.text(pos2(rect.left() + label_w, y + row_h / 2.0), Align2::LEFT_CENTER, v, theme::ui(12.0), pal.text);
        y += row_h;
    }
    y
}

/// Opens a folder (or selects a file) in the file manager (Explorer, Finder, FileManager1).
pub use crate::platform::reveal;

/// What a [`file_dialog`] asks for.
#[derive(Clone, Debug)]
pub struct FileDialog {
    /// Save (pick a new or existing file name) rather than open an existing file.
    pub save: bool,
    pub title: String,
    /// File-type filter, e.g. `("TMOP palette", "tmoptheme")`.
    pub filter: (String, String),
    /// Suggested file name (save only).
    pub file_name: String,
    /// HWND of the window that owns the dialog (0 for none). The dialog opens above it and is
    /// modal to it.
    pub owner: isize,
}

/// Shows a native open/save dialog on its own thread (so the UI keeps running). The receiver
/// gets the chosen path, or `None` if the dialog was cancelled or couldn't be shown.
pub fn file_dialog(d: FileDialog) -> std::sync::mpsc::Receiver<Option<std::path::PathBuf>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_file_dialog(&d));
    });
    rx
}

#[cfg(windows)]
fn run_file_dialog(d: &FileDialog) -> Option<std::path::PathBuf> {
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, FileSaveDialog, IFileDialog, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH};
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let run = || -> windows::core::Result<std::path::PathBuf> {
            let clsid = if d.save { &FileSaveDialog } else { &FileOpenDialog };
            let dialog: IFileDialog = CoCreateInstance::<_, windows::core::IUnknown>(clsid, None, CLSCTX_INPROC_SERVER)?.cast()?;
            dialog.SetTitle(&HSTRING::from(d.title.as_str()))?;
            let (name, spec, ext) = (HSTRING::from(format!("{} (*.{})", d.filter.0, d.filter.1)), HSTRING::from(format!("*.{}", d.filter.1)), HSTRING::from(d.filter.1.as_str()));
            dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }])?;
            dialog.SetDefaultExtension(&ext)?;
            let mut opts = dialog.GetOptions()? | FOS_FORCEFILESYSTEM;
            if d.save {
                opts |= FOS_OVERWRITEPROMPT;
                dialog.SetFileName(&HSTRING::from(d.file_name.as_str()))?;
            }
            dialog.SetOptions(opts)?;
            dialog.Show(HWND(d.owner as *mut _))?;
            let path = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let s = path.to_string();
            CoTaskMemFree(Some(path.0 as *const _));
            Ok(std::path::PathBuf::from(s.map_err(|_| windows::core::Error::from_win32())?))
        };
        let out = run().ok();
        if init.is_ok() {
            CoUninitialize();
        }
        out
    }
}

#[cfg(not(windows))]
fn run_file_dialog(_: &FileDialog) -> Option<std::path::PathBuf> {
    None
}

/// The categorical colours of treemap cells, share bars and legends, in the design's order.
pub fn category_color(i: usize) -> Color32 {
    const C: [Color32; 8] = [
        Color32::from_rgb(0x5E, 0xEA, 0xD4),
        Color32::from_rgb(0x8B, 0x9C, 0xFF),
        Color32::from_rgb(0xC9, 0xA6, 0xFF),
        Color32::from_rgb(0xFF, 0xB4, 0xC8),
        Color32::from_rgb(0xF5, 0xB8, 0x6B),
        Color32::from_rgb(0x6E, 0xE7, 0xA8),
        Color32::from_rgb(0xE8, 0xEC, 0xF5),
        Color32::from_rgb(0xFF, 0x7A, 0x8A),
    ];
    C[i % C.len()]
}

// ---------------------------------------------------------------------------
// Cards, tiles and grids
// ---------------------------------------------------------------------------

/// Gap between stat tiles.
pub const STAT_GAP: f32 = 12.0;
/// Height of a stat tile.
pub const STAT_TILE_H: f32 = 96.0;

/// Card with a section heading (11 px Medium, uppercase, 0.16 em, faint) and an optional mono
/// `right` text. Returns the content rect below the 22 px heading row (padding 18 / 22).
pub fn section_card(p: &egui::Painter, rect: Rect, pal: &Palette, title: &str, right: &str) -> Rect {
    card(p, rect, pal);
    tracked_text(p, pos2(rect.left() + 22.0, rect.top() + 18.0), Align2::LEFT_TOP, &title.to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.16);
    if !right.is_empty() {
        let clip = p.with_clip_rect(rect.shrink(2.0));
        clip.text(pos2(rect.right() - 22.0, rect.top() + 18.0), Align2::RIGHT_TOP, right, theme::mono(12.0), pal.text_faint);
    }
    Rect::from_min_max(pos2(rect.left() + 22.0, rect.top() + 18.0 + 22.0), pos2(rect.right() - 22.0, rect.bottom() - 18.0))
}

/// Stat tile (radius 8): uppercase label, a Light 24 px value, a sub line, an optional 2 px bar
/// along the bottom and an optional hardware drawing at the right. [`STAT_TILE_H`] high.
#[allow(clippy::too_many_arguments)]
pub fn stat_tile(p: &egui::Painter, rect: Rect, pal: &Palette, label: &str, value: &str, sub: &str, bar: Option<(f32, Color32)>, icon: Option<Hardware>) {
    p.rect_filled(rect, 8.0, pal.card);
    p.rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.card_stroke));
    let clip = p.with_clip_rect(rect);
    let inner_w = rect.width() - 32.0 - if icon.is_some() { 64.0 } else { 0.0 };
    tracked_text(&clip, pos2(rect.left() + 16.0, rect.top() + 14.0), Align2::LEFT_TOP, &label.to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.14);
    widgets::text_fit(&clip, pos2(rect.left() + 16.0, rect.top() + 31.0), Align2::LEFT_TOP, value, theme::light(24.0), pal.text, inner_w.max(40.0));
    widgets::text_fit(&clip, pos2(rect.left() + 16.0, rect.top() + 62.0), Align2::LEFT_TOP, sub, theme::ui(12.0), pal.text_faint, rect.width() - 32.0);
    if let Some(kind) = icon {
        hardware::draw(&clip, Rect::from_min_size(pos2(rect.right() - 16.0 - 54.0, rect.top() + 14.0), vec2(54.0, 36.0)), kind, pal.text_rail());
    }
    if let Some((frac, color)) = bar {
        widgets::bar(&clip, Rect::from_min_size(pos2(rect.left() + 16.0, rect.bottom() - 14.0), vec2(rect.width() - 32.0, 2.0)), frac, color);
    }
}

/// What [`stat_grid`] draws for one tile.
#[derive(Clone, Debug)]
pub struct StatTile {
    pub label: String,
    pub value: String,
    pub sub: String,
    pub bar: Option<(f32, Color32)>,
    pub icon: Option<Hardware>,
}

impl StatTile {
    pub fn new(label: impl Into<String>, value: impl Into<String>, sub: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into(), sub: sub.into(), bar: None, icon: None }
    }
    pub fn bar(mut self, frac: f32, color: Color32) -> Self {
        self.bar = Some((frac, color));
        self
    }
    pub fn icon(mut self, icon: Hardware) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// Stat tiles in auto-fitting columns (each at least `min_w` wide, like `repeat(auto-fit,
/// minmax(min_w, 1fr))`) from the top of `rect`. Returns the height used.
pub fn stat_grid(p: &egui::Painter, rect: Rect, pal: &Palette, tiles: &[StatTile], min_w: f32) -> f32 {
    if tiles.is_empty() {
        return 0.0;
    }
    let cols = (((rect.width() + STAT_GAP) / (min_w + STAT_GAP)).floor() as usize).clamp(1, tiles.len());
    let w = (rect.width() - STAT_GAP * (cols - 1) as f32) / cols as f32;
    let rows = tiles.len().div_ceil(cols);
    for (i, t) in tiles.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        let tile = Rect::from_min_size(pos2(rect.left() + c as f32 * (w + STAT_GAP), rect.top() + r as f32 * (STAT_TILE_H + STAT_GAP)), vec2(w, STAT_TILE_H));
        stat_tile(p, tile, pal, &t.label, &t.value, &t.sub, t.bar, t.icon);
    }
    rows as f32 * STAT_TILE_H + (rows - 1) as f32 * STAT_GAP
}

/// Inset tile strip: `n` panel tiles side by side, a 1 px `grid` line between them and around,
/// rounded as one box (radius 8). Returns the tiles' rects to draw into.
pub fn tile_strip(p: &egui::Painter, rect: Rect, pal: &Palette, n: usize) -> Vec<Rect> {
    let n = n.max(1);
    p.rect_filled(rect, 8.0, pal.grid);
    let inner = rect.shrink(1.0);
    let w = (inner.width() - (n - 1) as f32) / n as f32;
    (0..n)
        .map(|i| {
            let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (w + 1.0), inner.top()), vec2(w, inner.height()));
            let rounding = egui::Rounding { nw: if i == 0 { 7.0 } else { 0.0 }, sw: if i == 0 { 7.0 } else { 0.0 }, ne: if i + 1 == n { 7.0 } else { 0.0 }, se: if i + 1 == n { 7.0 } else { 0.0 } };
            p.rect_filled(r, rounding, pal.panel);
            r
        })
        .collect()
}

/// Filter field: 36 px high with a magnifier and a hint (see [`widgets::search_box`]).
pub fn filter_box(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32, pal: &Palette) -> egui::Response {
    widgets::search_box(ui, text, hint, width, pal)
}

/// Label / value grid inside `rect` for the design's `dl` lists: dim labels `label_w` wide, values
/// in 13 px or mono, optionally coloured. Returns the bottom y.
pub fn kv_grid(p: &egui::Painter, rect: Rect, pal: &Palette, items: &[(String, String, bool, Option<Color32>)], label_w: f32, row_h: f32) -> f32 {
    let mut y = rect.top();
    for (k, v, mono, color) in items {
        if y + row_h > rect.bottom() + 1.0 {
            break;
        }
        widgets::text_fit(p, pos2(rect.left(), y + row_h / 2.0), Align2::LEFT_CENTER, k, theme::ui(13.0), pal.text_dim, label_w - 8.0);
        let font = if *mono { theme::mono(12.0) } else { theme::ui(13.0) };
        let color = color.unwrap_or(if *mono { pal.text_table() } else { pal.text });
        widgets::text_fit(p, pos2(rect.left() + label_w, y + row_h / 2.0), Align2::LEFT_CENTER, v, font, color, (rect.width() - label_w).max(20.0));
        y += row_h;
    }
    y
}

/// A path or command line in an inset box with a copy button: `label` above (uppercase, faint),
/// the text wrapped in mono 12 below. Returns true on the frame the copy button was clicked (the
/// text is on the clipboard then).
pub fn code_box(ui: &mut egui::Ui, rect: Rect, pal: &Palette, label: &str, text: &str) -> bool {
    let p = ui.painter().clone();
    tracked_text(&p, pos2(rect.left(), rect.top()), Align2::LEFT_TOP, &label.to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.12);
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + 20.0), rect.max);
    p.rect_filled(body, 6.0, pal.well());
    p.rect_stroke(body, 6.0, Stroke::new(1.0_f32, pal.row_rule()));
    let galley = p.layout(text.to_string(), theme::mono(12.0), pal.text_code(), (body.width() - 52.0).max(20.0));
    p.with_clip_rect(body.shrink(1.0)).galley(body.min + vec2(12.0, 10.0), galley, pal.text_code());
    let btn = Rect::from_min_size(pos2(body.right() - 34.0, body.top() + 6.0), vec2(28.0, 28.0));
    let resp = ui.interact(btn, ui.id().with(("code-copy", label)), Sense::click());
    if resp.hovered() {
        p.rect_filled(btn, 4.0, pal.raise(0.08));
    }
    widgets::draw_icon(&p, Rect::from_center_size(btn.center(), vec2(15.0, 15.0)), Icon::Copy, if resp.hovered() { pal.text } else { pal.text_faint });
    let clicked = resp.clicked();
    if clicked {
        ui.ctx().copy_text(text.to_string());
    }
    resp.on_hover_text("Copy").on_hover_cursor(egui::CursorIcon::PointingHand);
    clicked
}

// ---------------------------------------------------------------------------
// Floating bars, dialogs and menus
// ---------------------------------------------------------------------------

/// A glass bar floating above the bottom edge of `anchor` (centred), holding `add` (selection
/// actions). It uses the dialog look.
pub fn floating_bar<R>(ui: &egui::Ui, anchor: Rect, id: impl std::hash::Hash, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let ctx = ui.ctx();
    let frame = egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::symmetric(14.0, 8.0)).rounding(10.0);
    egui::Area::new(egui::Id::new(("floating-bar", id)))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(anchor.center().x, anchor.bottom() - 16.0))
        .show(ctx, |ui| frame.show(ui, |ui| ui.horizontal(add).inner).inner)
        .inner
}

/// A modal-looking dialog window (560 px wide, centred, no title bar, glass fill). `add` draws
/// the content: [`dialog_header`], [`fact_box`], then [`dialog_buttons`].
pub fn dialog<R>(ctx: &egui::Context, id: impl std::hash::Hash, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let frame = egui::Frame::window(&ctx.style()).inner_margin(egui::Margin { left: 28.0, right: 28.0, top: 28.0, bottom: 24.0 }).rounding(10.0);
    egui::Window::new("")
        .id(egui::Id::new(("dialog", id)))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(560.0);
            add(ui)
        })
        .and_then(|r| r.inner)
}

/// Dialog heading: an optional 48 px tinted icon box, the title in Light 22 (text between `**`
/// pairs is Medium) and a 14 px paragraph below it.
pub fn dialog_header(ui: &mut egui::Ui, pal: &Palette, icon: Option<(Icon, Color32)>, title: &str, body: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        if let Some((icon, color)) = icon {
            let (r, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
            ui.painter().rect_filled(r, 8.0, with_alpha(color, 0.14));
            widgets::draw_icon(ui.painter(), Rect::from_center_size(r.center(), vec2(22.0, 22.0)), icon, color);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let mut job = egui::text::LayoutJob::default();
            for (i, part) in title.split("**").enumerate() {
                let font = if i % 2 == 1 { theme::semibold(22.0) } else { theme::light(22.0) };
                job.append(part, 0.0, egui::TextFormat { font_id: font, color: pal.text, ..Default::default() });
            }
            job.wrap.max_width = ui.available_width();
            ui.label(job);
            if !body.is_empty() {
                ui.label(egui::RichText::new(body).font(theme::ui(14.0)).color(pal.text_dim));
            }
        });
    });
}

/// Facts in an inset box, two columns: `(label, value, value colour)`. Labels are 11 px uppercase,
/// values Light 17.
pub fn fact_box(ui: &mut egui::Ui, pal: &Palette, facts: &[(&str, &str, Option<Color32>)]) {
    let rows = facts.len().div_ceil(2);
    let h = 28.0 + rows as f32 * 40.0 + rows.saturating_sub(1) as f32 * 12.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 6.0, pal.well());
    p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, pal.row_rule()));
    let col_w = (rect.width() - 32.0 - 20.0) / 2.0;
    for (i, (label, value, color)) in facts.iter().enumerate() {
        let (c, r) = (i % 2, i / 2);
        let x = rect.left() + 16.0 + c as f32 * (col_w + 20.0);
        let y = rect.top() + 14.0 + r as f32 * 52.0;
        tracked_text(p, pos2(x, y), Align2::LEFT_TOP, &label.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.12);
        widgets::text_fit(p, pos2(x, y + 16.0), Align2::LEFT_TOP, value, theme::light(17.0), color.unwrap_or(pal.text), col_w);
    }
}

/// A button of [`dialog_buttons`].
#[derive(Clone, Copy, Debug)]
pub enum DialogButton<'a> {
    Ghost(&'a str),
    Secondary(&'a str),
    Primary(&'a str),
    Danger(&'a str),
}

/// Right-aligned 44 px dialog buttons, in the order given (the last is at the right edge).
/// Returns the index clicked.
pub fn dialog_buttons(ui: &mut egui::Ui, pal: &Palette, buttons: &[DialogButton<'_>]) -> Option<usize> {
    let mut clicked = None;
    ui.add_space(4.0);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), ButtonSize::Hero.height()), Layout::right_to_left(Align::Center), |ui| {
        for (i, b) in buttons.iter().enumerate().rev() {
            let (label, kind) = match b {
                DialogButton::Ghost(l) => (*l, ButtonKind::Ghost),
                DialogButton::Secondary(l) => (*l, ButtonKind::Secondary),
                DialogButton::Primary(l) => (*l, ButtonKind::Primary),
                DialogButton::Danger(l) => (*l, ButtonKind::DangerFill),
            };
            if Button::new(label).kind(kind).size(ButtonSize::Hero).show(ui, pal).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// A context-menu row (34 px, radius 4) with the label at the left and a mono shortcut `hint`
/// at the right; `danger` rows use the danger text colour. Closes the menu when clicked.
pub fn menu_item(ui: &mut egui::Ui, label: &str, hint: Option<&str>, danger: bool) -> bool {
    let w = ui.available_width().max(220.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
    let p = ui.painter();
    let v = ui.visuals();
    let (text, faint) = (v.text_color(), v.weak_text_color());
    if resp.hovered() {
        p.rect_filled(rect, 4.0, with_alpha(text, 0.07));
    }
    let color = if !danger {
        text
    } else if v.dark_mode {
        theme::RED_TEXT
    } else {
        theme::dim(theme::RED_TEXT, 0.62)
    };
    p.text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, theme::ui(13.0), color);
    if let Some(h) = hint {
        p.text(pos2(rect.right() - 12.0, rect.center().y), Align2::RIGHT_CENTER, h, theme::mono(11.0), faint);
    }
    let clicked = resp.clicked();
    if clicked {
        ui.close_menu();
    }
    clicked
}

/// Small mono heading of a context menu ("chrome.exe · PID 4821").
pub fn menu_header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    let faint = ui.visuals().weak_text_color();
    ui.label(egui::RichText::new(text).font(theme::mono(11.0)).color(faint));
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(310.0, 100.0));
        let h = hsplit(r, &[1.0, 2.0], 10.0);
        assert_eq!(h[0].width(), 100.0);
        assert_eq!(h[1].left(), 110.0);
        let v = vsplit(r, &[20.0, -1.0, 30.0], 5.0);
        assert_eq!(v[1].height(), 40.0);
        assert_eq!(v[2].top(), 70.0);
    }

    fn frame(add: impl Fn(&egui::Context, &egui::Painter)) {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| add(ctx, &ui.painter().clone()));
            });
        }
    }

    /// Tiles flow into as many columns as fit; the strip hands out one inner rect per tile.
    #[test]
    fn tile_layouts() {
        frame(|_, p| {
            let pal = theme::DARK;
            let tiles: Vec<StatTile> = (0..5).map(|i| StatTile::new(format!("T{i}"), "1", "sub").bar(0.5, pal.accent)).collect();
            let wide = Rect::from_min_size(pos2(10.0, 10.0), vec2(1000.0, 400.0));
            assert_eq!(stat_grid(p, wide, &pal, &tiles, 190.0), STAT_TILE_H, "five 190 px tiles fit in one row of 1000 px");
            let narrow = Rect::from_min_size(pos2(10.0, 10.0), vec2(400.0, 400.0));
            assert_eq!(stat_grid(p, narrow, &pal, &tiles, 190.0), 3.0 * STAT_TILE_H + 2.0 * STAT_GAP, "two columns, three rows");
            assert_eq!(stat_grid(p, narrow, &pal, &[], 190.0), 0.0);
            let strip = tile_strip(p, Rect::from_min_size(pos2(0.0, 0.0), vec2(303.0, 80.0)), &pal, 3);
            assert_eq!(strip.len(), 3);
            assert!(strip[0].right() < strip[1].left() && strip[1].right() < strip[2].left(), "a hairline between tiles");
            let inner = section_card(p, Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0)), &pal, "Events", "6");
            assert_eq!((inner.left(), inner.top(), inner.right(), inner.bottom()), (22.0, 40.0, 378.0, 182.0));
            let items = [("A".to_string(), "1".to_string(), false, None), ("B".to_string(), "2".to_string(), true, Some(pal.ok))];
            assert_eq!(kv_grid(p, Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 100.0)), &pal, &items, 100.0, 24.0), 48.0);
        });
    }

    /// Code boxes, filter boxes, menu rows and the floating bar lay out inside a page.
    #[test]
    fn controls_lay_out() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut filter = String::new();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(!code_box(ui, Rect::from_min_size(pos2(10.0, 10.0), vec2(300.0, 70.0)), &pal, "Path", "svchost.exe -k netsvcs"));
                    let r = filter_box(ui, &mut filter, "Filter", 220.0, &pal);
                    assert!((r.rect.height() - 20.0).abs() < 40.0);
                    assert!(!menu_item(ui, "End task", Some("Del"), true));
                    menu_header(ui, "chrome.exe \u{b7} PID 4821");
                    floating_bar(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)), "sel", |ui| ui.label("selected"));
                });
            });
        }
    }

    #[test]
    fn dialog_pieces_draw() {
        frame(|ctx, _| {
            let pal = theme::DARK;
            let shown = dialog(ctx, "t", |ui| {
                dialog_header(ui, &pal, Some((Icon::Octagon, theme::RED)), "End **chrome** and all **24 processes**?", "Unsaved data is lost.");
                fact_box(ui, &pal, &[("Frees about", "3.4 GB", Some(pal.ok)), ("Open windows", "3", None), ("System", "None affected", None)]);
                dialog_buttons(ui, &pal, &[DialogButton::Ghost("Cancel"), DialogButton::Secondary("End one"), DialogButton::Primary("Keep"), DialogButton::Danger("End all")])
            });
            assert_eq!(shown, Some(None));
        });
    }
}

