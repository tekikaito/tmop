//! Sortable, virtualised data table in the style of the Processes page, shared by the
//! inventory and advanced pages.

use super::theme::{self, with_alpha, Palette};
use super::widgets;
use crate::metrics::Series;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Sense, Stroke};

pub const ROW_H: f32 = 32.0;
/// Height of the header row.
pub const HEADER_H: f32 = 36.0;
/// Row height when a cell has a second line (`Cell::sub`).
pub const TALL_ROW_H: f32 = 40.0;

#[derive(Clone, Copy, Debug)]
pub struct Col {
    pub title: &'static str,
    pub width: f32,
    pub numeric: bool,
    /// Holds a sparkline ([`Cell::spark`]).
    pub spark: bool,
}

impl Col {
    pub const fn text(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: false, spark: false }
    }
    /// Right-aligned numbers in mono; the first click sorts descending.
    pub const fn num(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: true, spark: false }
    }
    /// A trend column: the cells' [`Spark`] drawn 84 x 24.
    pub const fn spark(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: false, spark: true }
    }
}

/// A trend line drawn in a cell (84 x 24), scaled from 0 to the series' maximum.
#[derive(Clone, Debug)]
pub struct Spark {
    pub values: Vec<f32>,
    pub color: Color32,
    /// Area fill under the line.
    pub fill: bool,
}

/// One cell's content.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    pub color: Option<Color32>,
    /// 0..1 heat-map intensity behind the cell (an accent wash).
    pub heat: f32,
    /// Small coloured status dot before the text.
    pub dot: Option<Color32>,
    /// File whose shell icon is drawn before the text.
    pub icon: Option<String>,
    /// Text in the mono font (always on in numeric columns).
    pub mono: bool,
    /// Inline 60 x 3 px bar (fraction, colour) before the text.
    pub bar: Option<(f32, Color32)>,
    /// Outline badge ("WINDOWS") after the text.
    pub badge: Option<&'static str>,
    /// Show the text as a status chip in this colour instead of plain text.
    pub chip: Option<Color32>,
    /// Second, dim line under the text (rows become [`TALL_ROW_H`] when a first-column cell has one).
    pub sub: Option<String>,
    pub spark: Option<Spark>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
    pub fn dim(self, pal: &Palette) -> Self {
        self.color(pal.text_dim)
    }
    pub fn heat(mut self, h: f32) -> Self {
        self.heat = h.clamp(0.0, 1.0);
        self
    }
    pub fn dot(mut self, c: Color32) -> Self {
        self.dot = Some(c);
        self
    }
    pub fn icon(mut self, path: Option<&str>) -> Self {
        self.icon = Some(path.unwrap_or_default().to_string());
        self
    }
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
    pub fn bar(mut self, frac: f32, color: Color32) -> Self {
        self.bar = Some((frac.clamp(0.0, 1.0), color));
        self
    }
    pub fn badge(mut self, text: &'static str) -> Self {
        self.badge = Some(text);
        self
    }
    pub fn chip(mut self, c: Color32) -> Self {
        self.chip = Some(c);
        self
    }
    #[allow(clippy::should_implement_trait)]
    pub fn sub(mut self, text: impl Into<String>) -> Self {
        self.sub = Some(text.into());
        self
    }
    pub fn spark(mut self, values: Vec<f32>, color: Color32, fill: bool) -> Self {
        self.spark = Some(Spark { values, color, fill });
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct TableState {
    pub sort: usize,
    pub desc: bool,
    /// Selected row by the caller's stable key.
    pub selected: Option<String>,
    pub filter: String,
}

impl TableState {
    pub fn sorted_by(col: usize, desc: bool) -> Self {
        Self { sort: col, desc, ..Default::default() }
    }

    /// Applies the current sort to `order` using a per-column comparison.
    pub fn sort<T>(&self, items: &[T], order: &mut [usize], cmp: impl Fn(&T, &T, usize) -> std::cmp::Ordering) {
        order.sort_by(|a, b| {
            let o = cmp(&items[*a], &items[*b], self.sort);
            if self.desc {
                o.reverse()
            } else {
                o
            }
        });
    }
}

/// Context menu of a row: `(item index, ui)`.
pub type RowMenu<'a> = &'a dyn Fn(usize, &mut egui::Ui);

/// Draws the table in `rect`. `order` lists the item indices to show (already filtered
/// and sorted); `key` gives each item a stable key for selection; `cell(item, col)`
/// returns the content. Returns the item index that was double-clicked, if any.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    pal: &Palette,
    cols: &[Col],
    order: &[usize],
    state: &mut TableState,
    key: &dyn Fn(usize) -> String,
    cell: &dyn Fn(usize, usize) -> Cell,
    icons: &mut super::icons::IconCache,
) -> Option<usize> {
    show_with_menu(ui, rect, id, pal, cols, order, state, key, cell, icons, None)
}

/// Initial column widths: the widths asked for, shrunk in proportion (not below 36) when they
/// and the minimum of the last column do not fit into `avail`.
fn fit_widths(cols: &[Col], avail: f32) -> Vec<f32> {
    let Some((last, rest)) = cols.split_last() else { return Vec::new() };
    let wanted: f32 = rest.iter().map(|c| c.width).sum();
    let room = (avail - last.width.min(120.0)).max(0.0);
    let scale = if wanted > room && wanted > 0.0 { room / wanted } else { 1.0 };
    let mut w: Vec<f32> = rest.iter().map(|c| (c.width * scale).max(36.0)).collect();
    w.push(last.width);
    w
}

/// Like [`show`], and `menu(item, ui)` fills the context menu of each row. A right click
/// selects the row before the menu opens.
#[allow(clippy::too_many_arguments)]
pub fn show_with_menu(
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    pal: &Palette,
    cols: &[Col],
    order: &[usize],
    state: &mut TableState,
    key: &dyn Fn(usize) -> String,
    cell: &dyn Fn(usize, usize) -> Cell,
    icons: &mut super::icons::IconCache,
    menu: Option<RowMenu<'_>>,
) -> Option<usize> {
    let mut sort_click = None;
    let mut clicked: Option<String> = None;
    let mut activated = None;
    let selected = state.selected.clone();
    let (sort_col, desc) = (state.sort, state.desc);
    // Rows are taller when any row has a second line (only the first column carries one).
    let row_h = if !cols.is_empty() && order.iter().any(|i| cell(*i, 0).sub.is_some()) { TALL_ROW_H } else { ROW_H };
    // The table sits in a rounded box with a faint wash and a hairline.
    ui.painter().rect_filled(rect, 8.0, with_alpha(pal.text, 0.03));
    let inner = rect.shrink(1.0);
    let widths = fit_widths(cols, inner.width());
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner).id_salt(id), |ui| {
        // Columns wider than the box must not paint over what sits beside it (the Services panel).
        ui.set_clip_rect(inner.intersect(ui.clip_rect()));
        ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
        let mut table = egui_extras::TableBuilder::new(ui)
            .id_salt(id)
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink([false, false]);
        for (i, c) in cols.iter().enumerate() {
            let col = if i + 1 == cols.len() { egui_extras::Column::remainder().at_least(c.width.min(120.0)) } else { egui_extras::Column::initial(widths[i]).at_least(36.0) };
            table = table.column(col.clip(true));
        }
        let last = cols.len().saturating_sub(1);
        table
            .header(HEADER_H, |mut header| {
                for (i, c) in cols.iter().enumerate() {
                    header.col(|ui| {
                        let r = ui.max_rect();
                        let p = ui.painter();
                        let rounding = egui::Rounding { nw: if i == 0 { 7.0 } else { 0.0 }, ne: if i == last { 7.0 } else { 0.0 }, ..Default::default() };
                        p.rect_filled(r, rounding, pal.panel);
                        p.line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.panel_stroke));
                        let sorted = sort_col == i;
                        let arrow = if !sorted { "" } else if desc { " \u{2193}" } else { " \u{2191}" };
                        let label = format!("{}{arrow}", c.title.to_uppercase());
                        let (font, color) = if sorted { (theme::semibold(11.0), pal.accent) } else { (theme::ui(11.0), pal.text_faint) };
                        if c.numeric {
                            widgets::tracked_text(p, pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, &label, font, color, 0.08);
                        } else {
                            widgets::tracked_text(p, pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, &label, font, color, 0.08);
                        }
                        let resp = ui.interact(r, ui.id().with(("hdr", i)), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, rounding, pal.raise(0.04));
                        }
                        if resp.clicked() {
                            sort_click = Some(i);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, order.len(), |mut row| {
                    let item = order[row.index()];
                    let k = key(item);
                    let is_sel = selected.as_deref() == Some(k.as_str());
                    for (ci, c) in cols.iter().enumerate() {
                        row.col(|ui| {
                            let r = ui.max_rect();
                            let content = cell(item, ci);
                            let p = ui.painter();
                            if is_sel {
                                p.rect_filled(r, 0.0, pal.selected_row());
                                p.line_segment([pos2(r.left(), r.top() + 0.5), pos2(r.right(), r.top() + 0.5)], Stroke::new(1.0_f32, pal.accent));
                                p.line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.accent));
                                if ci == 0 {
                                    p.line_segment([pos2(r.left() + 0.5, r.top()), pos2(r.left() + 0.5, r.bottom())], Stroke::new(1.0_f32, pal.accent));
                                }
                                if ci == last {
                                    p.line_segment([pos2(r.right() - 0.5, r.top()), pos2(r.right() - 0.5, r.bottom())], Stroke::new(1.0_f32, pal.accent));
                                }
                            } else {
                                if content.heat > 0.01 {
                                    p.rect_filled(r, 0.0, with_alpha(pal.accent, content.heat * 0.18));
                                }
                                p.line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
                            }
                            let mono = content.mono || c.numeric;
                            let color = content.color.unwrap_or(if mono { pal.text_table() } else { pal.text });
                            let font = if mono { theme::mono(12.0) } else { theme::ui(13.0) };
                            let mid = r.center().y;
                            // Second line: the text moves up.
                            let ty = if content.sub.is_some() { mid - 7.0 } else { mid };
                            let mut x = r.left() + 10.0;
                            if let Some(s) = &content.spark {
                                let area = Rect::from_min_size(pos2(x, mid - 12.0), vec2(84.0, 24.0));
                                let mut series = Series::new(s.values.len().max(2));
                                for v in &s.values {
                                    series.push(*v);
                                }
                                let hi = s.values.iter().cloned().fold(1.0_f32, f32::max);
                                widgets::sparkline_static(&p.with_clip_rect(r), area, &[(&series, s.color, (0.0, hi))], if s.fill { 0.15 } else { 0.0 });
                                return;
                            }
                            if let Some(d) = content.dot {
                                p.circle_filled(pos2(x + 4.0, mid), 4.0, d);
                                x += 14.0;
                            }
                            if let Some(path) = &content.icon {
                                let ic = Rect::from_min_size(pos2(x, mid - 8.0), vec2(16.0, 16.0));
                                match icons.get(Some(path.as_str())) {
                                    Some(t) => {
                                        p.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                    }
                                    None => super::summary::proc_icon(p, ic.shrink(2.0), false),
                                }
                                x += 22.0;
                            }
                            if let Some(c) = content.chip {
                                widgets::chip(&p.with_clip_rect(r), pos2(x - 2.0, mid), &content.text, c, None);
                                return;
                            }
                            let clip = p.with_clip_rect(r);
                            if c.numeric {
                                let text_r = clip.text(pos2(r.right() - 10.0, ty), Align2::RIGHT_CENTER, &content.text, font, color);
                                if let Some((frac, bc)) = content.bar {
                                    widgets::bar(&clip, Rect::from_min_size(pos2(text_r.left() - 8.0 - 60.0, mid - 1.5), vec2(60.0, 3.0)), frac, bc);
                                }
                            } else {
                                if let Some((frac, bc)) = content.bar {
                                    widgets::bar(&clip, Rect::from_min_size(pos2(x, mid - 1.5), vec2(60.0, 3.0)), frac, bc);
                                    x += 68.0;
                                }
                                let max_w = r.right() - x - 8.0 - content.badge.map(|b| widgets::badge_width(p, b) + 6.0).unwrap_or(0.0);
                                let text_r = widgets::text_fit(&clip, pos2(x, ty), Align2::LEFT_CENTER, &content.text, font, color, max_w);
                                if let Some(b) = content.badge {
                                    widgets::badge(&clip, pos2(text_r.right() + 6.0, ty), b, pal);
                                }
                            }
                            if let Some(sub) = &content.sub {
                                widgets::text_fit(&clip, pos2(x, mid + 9.0), Align2::LEFT_CENTER, sub, theme::mono(11.0), pal.text_faint, r.right() - x - 8.0);
                            }
                        });
                    }
                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        clicked = Some(k.clone());
                    }
                    if let Some(menu) = menu {
                        resp.context_menu(|ui| menu(item, ui));
                    }
                    if resp.double_clicked() {
                        activated = Some(item);
                    }
                });
            });
    });
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.grid));
    if let Some(c) = sort_click {
        if state.sort == c {
            state.desc = !state.desc;
        } else {
            state.sort = c;
            state.desc = cols[c].numeric;
        }
    }
    if let Some(k) = clicked {
        state.selected = Some(k);
    }
    activated
}

/// Case-insensitive "contains any word" filter.
pub fn matches(filter: &str, fields: &[&str]) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    f.split_whitespace().all(|w| fields.iter().any(|s| s.to_lowercase().contains(w)))
}

pub fn cmp_str(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

pub fn cmp_f(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_words() {
        assert!(matches("", &["x"]));
        assert!(matches("mic word", &["Microsoft Word", "16.0"]));
        assert!(!matches("excel", &["Microsoft Word"]));
    }

    /// Columns that do not fit shrink so the last one still starts inside the box.
    #[test]
    fn widths_fit_the_box() {
        let cols = [Col::text("a", 400.0), Col::text("b", 400.0), Col::text("c", 300.0)];
        assert_eq!(fit_widths(&cols, 2000.0), vec![400.0, 400.0, 300.0]);
        let w = fit_widths(&cols, 520.0);
        assert!((w[0] + w[1] + 120.0 - 520.0).abs() < 0.01);
        assert!(fit_widths(&[], 100.0).is_empty());
    }

    /// Cells with every decoration draw in a table, and a second line makes the rows taller.
    #[test]
    fn decorated_cells_draw() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut icons = super::super::icons::IconCache::new(&ctx);
        let cols = [Col::text("Name", 200.0), Col::num("CPU", 100.0), Col::spark("CPU 60s", 120.0), Col::text("Status", 100.0)];
        let order = [0usize, 1, 2];
        let mut state = TableState::sorted_by(1, true);
        state.selected = Some("1".into());
        for tall in [false, true] {
            let cell = |i: usize, c: usize| -> Cell {
                match c {
                    0 => {
                        let cell = Cell::new(format!("app{i}.exe")).badge("WINDOWS").dot(pal.ok).icon(None);
                        if tall && i == 0 {
                            cell.sub("folder")
                        } else {
                            cell
                        }
                    }
                    1 => Cell::new("12.5%").bar(0.4, pal.accent).heat(0.5),
                    2 => Cell::new("").spark((0..30).map(|v| (v % 7) as f32).collect(), pal.accent, true),
                    _ => Cell::new("Running").chip(pal.ok),
                }
            };
            for _ in 0..3 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(700.0, 300.0));
                        show(ui, rect, "t", &pal, &cols, &order, &mut state, &|i| i.to_string(), &cell, &mut icons);
                    });
                });
            }
        }
        assert_eq!((state.sort, state.desc), (1, true));
    }

    #[test]
    fn sorting_respects_direction() {

        let items = [3, 1, 2];
        let mut order = vec![0, 1, 2];
        let s = TableState::sorted_by(0, true);
        s.sort(&items, &mut order, |a, b, _| a.cmp(b));
        assert_eq!(order, vec![0, 2, 1]);
    }
}
