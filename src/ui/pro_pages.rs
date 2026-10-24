//! State of the advanced and inventory pages (Power & Freq, Flight Recorder, Connections,
//! Installed Apps, Drivers, Disk Space) and the small pieces those pages share; each page lives in
//! its own module.

use super::kit::{STAT_GAP, STAT_TILE_H};
use super::table::TableState;
use super::theme::{self, with_alpha, Palette};
use super::widgets::{self, Button, ButtonKind, ButtonSize};
use crate::metrics::Series;
use crate::pro::apps::App as InstalledApp;
use crate::pro::connections::Conn;
use crate::pro::diskspace::{DirFile, Scanner, Tree, Volume};
use crate::pro::drivers::Service;
use crate::pro::power::PowerInfo;
use crate::pro::recorder::{Record, RecordingInfo};
use crate::pro::Loader;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Shape, Stroke};
use std::path::PathBuf;
use std::sync::Arc;

/// UI state of the advanced and inventory pages.
#[derive(Default)]
pub struct ProState {
    pub power: Loader<PowerInfo>,
    /// Fastest core clock per sampler generation, for the Power & Freq history.
    pub power_fastest: ClockTrace,
    pub conns: Loader<Vec<Conn>>,
    pub conns_table: TableState,
    pub conns_view: usize,
    pub apps: Loader<Vec<InstalledApp>>,
    pub apps_table: TableState,
    pub drivers: Loader<Vec<Service>>,
    pub drivers_table: TableState,
    pub volumes: Loader<Vec<Volume>>,
    pub scanner: Scanner,
    pub disk_root: String,
    pub disk_tree: Option<(Arc<Tree>, u64)>,
    pub disk_cursor: u32,
    pub disk_tab: usize,
    /// Files tab sort column; direction in `disk_files_asc` (false = descending).
    pub disk_files_sort: FileCol,
    pub disk_files_asc: bool,
    /// Folder the Files tab listing was requested for (None = nothing requested yet).
    pub disk_files_for: Option<PathBuf>,
    pub disk_files: Loader<Vec<DirFile>>,
    /// Sorted index order of `disk_files.ok()`, valid for the (col, asc) it was built with.
    pub disk_files_order: Option<(FileCol, bool, Vec<usize>)>,
    /// Files tab "Include subfolders": on, and the sorted index order (file indices, biggest size) of the
    /// last finished sort with the key it was requested for (`disk_sub_key`) and the key shown (`disk_sub_ready`).
    pub disk_files_sub: bool,
    pub disk_sub_order: Loader<(Vec<u32>, u64)>,
    pub disk_sub_key: Option<SubKey>,
    pub disk_sub_ready: Option<SubKey>,
    /// Largest-files indices into `Tree::files` for (tree generation, cursor).
    pub disk_largest: Option<(u64, u32, Vec<usize>)>,
    pub disk_confirm_stop: bool,
    /// Deletions applied to the scan tree (part of the tree version, see `disk_epoch`).
    pub disk_edits: u64,
    pub del: super::disk_delete::DiskDeleteUi,
    pub disk_mode: super::disk_cleanup::DiskMode,
    pub clean: super::disk_cleanup::CleanupUi,
    pub recordings: Vec<RecordingInfo>,
    pub recordings_listed: bool,
    pub playback: Option<(PathBuf, Vec<Record>)>,
    pub rec_dir_text: String,
    pub rec_limit: usize,
    pub inv: super::inventory_pages::InventoryState,
    pub cpu_details: Option<crate::pro::inventory::CpuDetails>,
}

/// (scan generation, folder, sort column, ascending) a sorted listing was built for.
pub type SubKey = (u64, u32, FileCol, bool);

impl ProState {
    /// Version of the scan tree shown: scan generation and the deletions applied to it.
    pub fn disk_epoch(&self) -> u64 {
        self.disk_tree.as_ref().map(|(_, g)| *g).unwrap_or(0) << 20 | (self.disk_edits & 0xF_FFFF)
    }
}

/// Column of the Disk Space Files tab (in-memory only, never saved).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileCol {
    Name,
    #[default]
    Size,
    Modified,
}


/// Recording length limits of the Flight Recorder page: label and seconds.
pub(super) const LIMITS: [(&str, Option<u64>); 6] = [("Until stopped", None), ("1 minute", Some(60)), ("5 minutes", Some(300)), ("15 minutes", Some(900)), ("1 hour", Some(3600)), ("8 hours", Some(28800))];

/// The fastest core's clock (MHz) once per sampler generation, lined up with `History::clock`.
#[derive(Default)]
pub struct ClockTrace {
    series: Option<Series>,
    seq: u64,
}

impl ClockTrace {
    /// Records `mhz` for sampler generation `seq` (once per generation). A skipped generation (the
    /// page was not shown) starts a new trace, so the line never drifts from the history behind it.
    pub fn push(&mut self, seq: u64, mhz: f32, capacity: usize) {
        if seq == 0 || (seq == self.seq && self.series.is_some()) {
            return;
        }
        let series = match self.series.take() {
            Some(mut s) if seq == self.seq + 1 => {
                s.resize(capacity);
                s
            }
            _ => Series::new(capacity),
        };
        self.series.insert(series).push(mhz);
        self.seq = seq;
    }

    pub fn series(&self) -> Option<&Series> {
        self.series.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Page layout
// ---------------------------------------------------------------------------

/// Width kept for the title block when deciding whether the toolbar fits beside it.
const TITLE_W: f32 = 340.0;
pub(super) const TOOLBAR_H: f32 = 36.0;

/// The toolbar rect and the content area of a page whose title block was drawn by
/// [`super::App::page_title`] (`body` is what it returned). The toolbar sits at the right end of the
/// title row when `need` px fit beside the title, else on a row of its own below it.
pub(super) fn page_layout(body: Rect, need: f32) -> (Rect, Rect) {
    let left = body.left() + 12.0;
    if body.right() - left - TITLE_W >= need {
        let bottom = body.top() - 6.0;
        let toolbar = Rect::from_min_max(pos2(body.right() - need, bottom - TOOLBAR_H), pos2(body.right(), bottom));
        (toolbar, Rect::from_min_max(pos2(left, body.top() + 8.0), body.max))
    } else {
        let toolbar = Rect::from_min_max(pos2(left, body.top() + 2.0), pos2(body.right(), body.top() + 2.0 + TOOLBAR_H));
        (toolbar, Rect::from_min_max(pos2(left, toolbar.bottom() + 12.0), body.max))
    }
}

/// Runs `add` right to left inside `rect` (the toolbar of [`page_layout`]): the first control added
/// ends up at the right edge.
pub(super) fn toolbar<R>(ui: &mut egui::Ui, rect: Rect, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        add(ui)
    })
    .inner
}

/// Rects of `n` stat tiles in auto-fitting columns (see [`super::kit::stat_grid`]) from the top of
/// `rect`, and the height they use. For the pages that draw extra things into a tile.
pub(super) fn tile_rects(rect: Rect, n: usize, min_w: f32) -> (Vec<Rect>, f32) {
    if n == 0 {
        return (Vec::new(), 0.0);
    }
    let cols = (((rect.width() + STAT_GAP) / (min_w + STAT_GAP)).floor() as usize).clamp(1, n);
    let w = (rect.width() - STAT_GAP * (cols - 1) as f32) / cols as f32;
    let rects = (0..n).map(|i| Rect::from_min_size(pos2(rect.left() + (i % cols) as f32 * (w + STAT_GAP), rect.top() + (i / cols) as f32 * (STAT_TILE_H + STAT_GAP)), vec2(w, STAT_TILE_H))).collect();
    let rows = n.div_ceil(cols);
    (rects, rows as f32 * STAT_TILE_H + (rows - 1) as f32 * STAT_GAP)
}

// ---------------------------------------------------------------------------
// Controls and small drawings
// ---------------------------------------------------------------------------

/// Text field in the mono font (36 px high, no icon): folders, drives and paths.
pub(super) fn mono_input(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32, pal: &Palette) -> egui::Response {
    let frame = egui::Frame::none().fill(pal.control).stroke(Stroke::new(1.0_f32, pal.control_stroke)).rounding(6.0).inner_margin(egui::Margin::symmetric(12.0, 0.0));
    frame
        .show(ui, |ui| {
            ui.set_min_height(34.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::TextEdit::singleline(text).hint_text(egui::RichText::new(hint).color(pal.text_faint)).desired_width(width - 24.0).frame(false).font(theme::mono(12.0)))
            })
            .inner
        })
        .inner
}

/// How an entry of [`swatch_legend`] is marked.
#[derive(Clone, Copy)]
pub(super) enum Swatch {
    /// A 14 x 2 px bar: a series.
    Line(Color32),
    /// A dashed bar: a reference line.
    Dashed(Color32),
    /// A 2 x 12 px tick: an event marker.
    Tick(Color32),
}

/// Width [`swatch_legend`] needs.
pub(super) fn swatch_legend_width(p: &egui::Painter, items: &[(&str, Swatch)]) -> f32 {
    items.iter().map(|(t, _)| 22.0 + widgets::text_width(p, t, theme::ui(12.0))).sum::<f32>() + 18.0 * items.len().saturating_sub(1) as f32
}

/// Legend of a line chart: swatch and label per entry in a row from `pos` (left, vertically
/// centred). Returns the width used.
pub(super) fn swatch_legend(p: &egui::Painter, pal: &Palette, pos: egui::Pos2, items: &[(&str, Swatch)]) -> f32 {
    let mut x = pos.x;
    for (i, (text, swatch)) in items.iter().enumerate() {
        if i > 0 {
            x += 18.0;
        }
        match *swatch {
            Swatch::Line(c) => {
                p.rect_filled(Rect::from_min_size(pos2(x, pos.y - 1.0), vec2(14.0, 2.0)), 1.0, c);
            }
            Swatch::Dashed(c) => p.extend(Shape::dashed_line(&[pos2(x, pos.y), pos2(x + 14.0, pos.y)], Stroke::new(1.5_f32, c), 3.0, 2.0)),
            Swatch::Tick(c) => {
                p.rect_filled(Rect::from_min_size(pos2(x + 6.0, pos.y - 6.0), vec2(2.0, 12.0)), 1.0, c);
            }
        }
        let r = p.text(pos2(x + 22.0, pos.y), Align2::LEFT_CENTER, text, theme::ui(12.0), pal.text_dim);
        x = r.right();
    }
    x - pos.x
}

/// How [`list_row`] looks.
#[derive(Clone, Copy)]
pub(super) enum RowLook {
    /// Hairline border.
    Plain,
    /// Washed and bordered in this colour: the active power plan, the recording being played.
    Marked(Color32),
}

/// A bordered list entry (radius 6) as used by the power plans and the recordings.
pub(super) fn list_row(p: &egui::Painter, rect: Rect, pal: &Palette, look: RowLook, hovered: bool) {
    match look {
        RowLook::Plain => {
            if hovered {
                p.rect_filled(rect, 6.0, pal.raise(0.04));
            }
            p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, pal.raise(0.08)));
        }
        RowLook::Marked(c) => {
            p.rect_filled(rect, 6.0, with_alpha(c, 0.08));
            p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, with_alpha(c, 0.35)));
        }
    }
}

/// The two buttons at the foot of a dialog, right-aligned: a ghost `cancel` and the `action` of
/// `kind` (greyed unless `enabled`). `Some(true)` when the action was clicked, `Some(false)` for cancel.
pub(super) fn confirm_row(ui: &mut egui::Ui, pal: &Palette, cancel: &str, action: &str, kind: ButtonKind, enabled: bool) -> Option<bool> {
    let mut answer = None;
    ui.add_space(4.0);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), ButtonSize::Hero.height()), Layout::right_to_left(Align::Center), |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            if Button::new(action).kind(kind).size(ButtonSize::Hero).show(ui, pal).clicked() {
                answer = Some(true);
            }
        });
        if Button::ghost(cancel).size(ButtonSize::Hero).show(ui, pal).clicked() {
            answer = Some(false);
        }
    });
    answer
}

// ---------------------------------------------------------------------------
// Time text
// ---------------------------------------------------------------------------

/// A duration for a heading: "45 s", "12 min", "2.5 h".
pub(super) fn fmt_span(secs: u64) -> String {
    match secs {
        0..=89 => format!("{secs} s"),
        90..=5399 => format!("{} min", (secs + 30) / 60),
        _ => format!("{:.1} h", secs as f32 / 3600.0),
    }
}

/// "HH:MM:SS" of a local stamp ("YYYY-MM-DD HH:MM:SS").
pub(super) fn clock_of(stamp: &str) -> &str {
    stamp.get(11..).unwrap_or(stamp)
}

/// "HH:MM" of a local stamp.
pub(super) fn hm_of(stamp: &str) -> &str {
    stamp.get(11..16).unwrap_or(stamp)
}

/// Seconds since 1970 of a local stamp ("YYYY-MM-DD HH:MM:SS", read as if it were UTC), for
/// differences between two stamps.
pub(super) fn stamp_secs(s: &str) -> Option<i64> {
    let field = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (h, mi, sec) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    // Days from the civil date (proleptic Gregorian), counted from 1970-01-01.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared controls draw, and a legend measures what it draws.
    #[test]
    fn controls_draw() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut path = String::from("C:\\");
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = ui.painter().clone();
                    let items = [("CPU", Swatch::Line(pal.accent)), ("Base 3.6 GHz", Swatch::Dashed(pal.text_faint)), ("Process event", Swatch::Tick(pal.warn))];
                    let drawn = swatch_legend(&p, &pal, pos2(10.0, 10.0), &items);
                    assert!((drawn - swatch_legend_width(&p, &items)).abs() < 0.5);
                    list_row(&p, Rect::from_min_size(pos2(10.0, 30.0), vec2(200.0, 40.0)), &pal, RowLook::Plain, true);
                    list_row(&p, Rect::from_min_size(pos2(10.0, 80.0), vec2(200.0, 40.0)), &pal, RowLook::Marked(pal.accent), false);
                    let top = ui.cursor().top();
                    mono_input(ui, &mut path, "Folder", 220.0, &pal);
                    assert!(ui.min_rect().bottom() - top >= 34.0, "the field is 36 px high");
                    assert_eq!(confirm_row(ui, &pal, "Cancel", "Delete", ButtonKind::DangerFill, false), None);
                    toolbar(ui, Rect::from_min_size(pos2(10.0, 200.0), vec2(400.0, 36.0)), |ui| ui.label("x"));
                });
            });
        }
    }

    /// The symbols the pages put in labels exist in the fonts they are drawn with.
    #[test]
    fn page_glyphs_exist() {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                for font in [theme::ui(13.0), theme::semibold(13.0), theme::mono(12.0)] {
                    let missing: Vec<char> = "\u{25a0}\u{25cf}\u{2191}\u{2193}\u{2192}\u{203a}\u{b7}\u{2022}\u{2014}\u{2026}".chars().filter(|c| !ctx.fonts(|f| f.has_glyph(&font, *c))).collect();
                    assert!(missing.is_empty(), "{:?} lacks {missing:?}", font.family);
                }
            });
        }
    }

    #[test]
    fn clock_trace_follows_sampler_generations() {
        let mut t = ClockTrace::default();
        t.push(0, 1.0, 10);
        assert!(t.series().is_none(), "generation 0 is the empty snapshot");
        t.push(5, 4000.0, 10);
        t.push(5, 9999.0, 10);
        t.push(6, 4200.0, 10);
        assert_eq!(t.series().unwrap().values.iter().copied().collect::<Vec<_>>(), vec![4000.0, 4200.0]);
        t.push(9, 3000.0, 10);
        assert_eq!(t.series().unwrap().values.len(), 1, "a skipped generation starts a new trace");
    }

    #[test]
    fn tiles_flow_like_stat_grid() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 400.0));
        let (r, h) = tile_rects(rect, 5, 190.0);
        assert_eq!((r.len(), h), (5, STAT_TILE_H));
        let (_, h) = tile_rects(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0)), 5, 190.0);
        assert_eq!(h, 3.0 * STAT_TILE_H + 2.0 * STAT_GAP);
        assert_eq!(tile_rects(rect, 0, 190.0).1, 0.0);
    }

    #[test]
    fn toolbar_moves_below_a_narrow_title_row() {
        let wide = Rect::from_min_size(pos2(100.0, 64.0), vec2(1200.0, 600.0));
        let (tb, area) = page_layout(wide, 800.0);
        assert!(tb.bottom() < wide.top() && area.top() > wide.top(), "on the title row");
        let narrow = Rect::from_min_size(pos2(100.0, 64.0), vec2(900.0, 600.0));
        let (tb, area) = page_layout(narrow, 800.0);
        assert!(tb.top() >= narrow.top() && area.top() > tb.bottom(), "on a row of its own");
    }

    #[test]
    fn stamps_and_spans() {
        assert_eq!(stamp_secs("2026-10-24 14:02:07").unwrap() - stamp_secs("2026-10-24 13:12:07").unwrap(), 3000);
        assert_eq!(stamp_secs("2026-03-01 00:00:00").unwrap() - stamp_secs("2026-02-28 23:59:59").unwrap(), 1);
        assert_eq!(stamp_secs("2026-10-24"), None);
        assert_eq!((clock_of("2026-10-24 14:02:07"), hm_of("2026-10-24 14:02:07")), ("14:02:07", "14:02"));
        assert_eq!((fmt_span(45), fmt_span(300), fmt_span(3600), fmt_span(9000)), ("45 s".into(), "5 min".into(), "60 min".into(), "2.5 h".into()));
    }
}
