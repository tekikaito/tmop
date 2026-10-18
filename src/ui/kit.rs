//! Layout pieces shared by the inventory and advanced pages: dot-matrix titled cards, stat
//! cards, header toolbars and simple row/column splitting.

use super::theme::{self, with_alpha, Palette};
use super::widgets::{card, led_panel, led_row, LedStyle, LED_PANEL_H};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect};

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

/// Lays out a row of stat cards.
pub fn stat_row(p: &egui::Painter, rect: Rect, pal: &Palette, bloom: f32, cards: &[(String, String, String, Color32, Option<f32>)]) {
    let w: Vec<f32> = vec![1.0; cards.len()];
    for (r, (t, v, s, c, f)) in hsplit(rect, &w, GAP).into_iter().zip(cards) {
        stat_card(p, r, pal, bloom, t, v, s, *c, *f);
    }
}

/// Right-aligned toolbar area on the page header row.
pub fn header_toolbar<R>(ui: &mut egui::Ui, body: Rect, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = Rect::from_min_max(pos2(body.right() - width, body.top() - super::HEADER_H + 10.0), pos2(body.right() - 16.0, body.top() - 10.0));
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

/// Opens a folder (or selects a file) in Explorer.
pub fn reveal(path: &std::path::Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("explorer.exe");
    if path.is_file() {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

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

/// Hue-cycled neon colours for categorical marks (treemap cells, legends).
pub fn category_color(i: usize) -> Color32 {
    const C: [Color32; 8] = [
        Color32::from_rgb(0x2F, 0x8B, 0xFF),
        Color32::from_rgb(0x1F, 0xC4, 0x48),
        Color32::from_rgb(0xC0, 0x40, 0xFF),
        Color32::from_rgb(0xFF, 0x9A, 0x2E),
        Color32::from_rgb(0x22, 0xD3, 0xEE),
        Color32::from_rgb(0xFF, 0x3B, 0x6B),
        Color32::from_rgb(0xE8, 0xD0, 0x2A),
        Color32::from_rgb(0x8A, 0x9B, 0xFF),
    ];
    C[i % C.len()]
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
}
