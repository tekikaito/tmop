//! Sortable, virtualised data table in the style of the Processes page, shared by the
//! inventory and advanced pages.

use super::theme::{self, with_alpha, Palette};
use super::widgets;
use crate::metrics::Series;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Sense, Stroke};
use super::widgets::ScrollArea029 as _;

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
    /// Keeps its width when the table is too narrow for all columns (see [`fit_widths`]).
    pub rigid: bool,
    /// The cells may carry a second line ([`Cell::sub`]); on the first column this makes every
    /// row [`TALL_ROW_H`] high.
    pub two_line: bool,
}

impl Col {
    /// Text that may be shortened with an ellipsis when the table is narrow (names, paths).
    pub const fn text(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: false, rigid: false, two_line: false }
    }
    /// Right-aligned numbers in mono; the first click sorts descending. Keeps its width: a
    /// number cut off reads as another.
    pub const fn num(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: true, rigid: true, two_line: false }
    }
    /// A trend column: the cells' [`Spark`] drawn 84 x 24. Keeps its width.
    pub const fn spark(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: false, rigid: true, two_line: false }
    }
    /// A text column that keeps its width: a short word from a fixed set ("Running", "Manual"),
    /// which says nothing once it is cut to "Runn…".
    pub const fn rigid(mut self) -> Self {
        self.rigid = true;
        self
    }
    /// See [`Col::two_line`].
    pub const fn two_line(mut self, on: bool) -> Self {
        self.two_line = on;
        self
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

/// Share of its asked-for width a column keeps at least when the table is narrow; what still
/// does not fit is reached by scrolling sideways.
const MIN_COL_SHARE: f32 = 0.6;

/// Initial column widths: the widths asked for, with text columns shrunk in proportion when they
/// and the minimum of the last column do not fit into `avail`, but not below [`MIN_COL_SHARE`] of
/// each (nor 36). Rigid columns (numbers, trends, short status words) keep their width.
fn fit_widths(cols: &[Col], avail: f32) -> Vec<f32> {
    let Some((last, rest)) = cols.split_last() else { return Vec::new() };
    let rigid: f32 = rest.iter().filter(|c| c.rigid).map(|c| c.width).sum();
    let wanted: f32 = rest.iter().filter(|c| !c.rigid).map(|c| c.width).sum();
    let room = (avail - last.width.min(120.0) - rigid).max(0.0);
    let scale = if wanted > room && wanted > 0.0 { room / wanted } else { 1.0 };
    let mut w: Vec<f32> = rest.iter().map(|c| if c.rigid { c.width } else { (c.width * scale).max(c.width * MIN_COL_SHARE).max(36.0) }).collect();
    w.push(last.width);
    w
}

/// A key that moves the selection of a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// Whether plain keys belong to the page's table: no widget has the keyboard (a filter box), no
/// modifier is held, and no dialog or menu is open.
pub fn keys_free(ctx: &egui::Context) -> bool {
    !ctx.egui_wants_keyboard_input() && !super::kit::dialog_open(ctx) && !egui::Popup::is_any_open(ctx) && ctx.input(|i| !i.modifiers.any())
}

/// The selection key pressed this frame (key repeat counts), if the keys are free ([`keys_free`]).
pub fn nav_key(ctx: &egui::Context) -> Option<Nav> {
    const KEYS: [(egui::Key, Nav); 6] =
        [(egui::Key::ArrowUp, Nav::Up), (egui::Key::ArrowDown, Nav::Down), (egui::Key::PageUp, Nav::PageUp), (egui::Key::PageDown, Nav::PageDown), (egui::Key::Home, Nav::Home), (egui::Key::End, Nav::End)];
    if !keys_free(ctx) {
        return None;
    }
    ctx.input(|i| KEYS.iter().find(|(k, _)| i.key_pressed(*k)).map(|(_, nav)| *nav))
}

/// The row `nav` moves the selection to from row `current`, in a list of `len` rows of which
/// `page` fit the view. Without a selection, the keys that go down start at the first row and
/// those that go up at the last. `None` for an empty list.
pub fn nav_target(current: Option<usize>, len: usize, nav: Nav, page: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let page = page.max(1);
    Some(match (nav, current) {
        (Nav::Home, _) | (Nav::Down | Nav::PageDown, None) => 0,
        (Nav::End, _) | (Nav::Up | Nav::PageUp, None) => last,
        (Nav::Up, Some(c)) => c.min(last).saturating_sub(1),
        (Nav::Down, Some(c)) => (c + 1).min(last),
        (Nav::PageUp, Some(c)) => c.min(last).saturating_sub(page),
        (Nav::PageDown, Some(c)) => (c + page).min(last),
    })
}

/// Where the id of the table the selection keys act on is kept: the table clicked last, so that
/// two tables on one page (GPU activity, Docker files) do not both move.
fn active_table() -> egui::Id {
    egui::Id::unique("tmop-active-table")
}

/// Like [`show`], and `menu(item, ui)` fills the context menu of each row. A right click
/// selects the row before the menu opens. Once a row was clicked, Up, Down, Page Up, Page Down,
/// Home and End move the selection (see [`nav_key`]) and scroll it into view.
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
    let (sort_col, desc) = (state.sort, state.desc);
    // Rows are taller when the first column carries a second line. The column says so: asking
    // every row's cell for it here would build a cell per row and frame, shown or not.
    let row_h = if cols.first().is_some_and(|c| c.two_line) { TALL_ROW_H } else { ROW_H };
    // The selection keys, for the table that was clicked last.
    let table_id = egui::Id::unique(("tmop-table", id));
    let mut scroll_to = None;
    if ui.data(|d| d.get_temp::<egui::Id>(active_table())) == Some(table_id) {
        if let Some(nav) = nav_key(ui.ctx()) {
            let current = state.selected.as_ref().and_then(|k| order.iter().position(|i| key(*i) == *k));
            let page = (((rect.height() - HEADER_H) / row_h) as usize).saturating_sub(1);
            if let Some(row) = nav_target(current, order.len(), nav, page) {
                state.selected = Some(key(order[row]));
                scroll_to = Some(row);
            }
        }
    }
    let selected = state.selected.clone();
    // The table sits in a rounded box with a faint wash and a hairline.
    ui.painter().rect_filled(rect, 8.0, with_alpha(pal.text, 0.03));
    let inner = rect.shrink(1.0);
    let widths = fit_widths(cols, inner.width());
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner).id_salt(id), |ui| {
        // Columns wider than the box must not paint over what sits beside it (the Services panel).
        ui.set_clip_rect(inner.intersect(ui.clip_rect()));
        ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
        // Columns that cannot shrink any further are reached by scrolling sideways.
        egui::ScrollArea::horizontal().id_salt((id, "h")).auto_shrink([false, false]).show_029(ui, [true, false], |ui| {
            let mut table = egui_extras::TableBuilder::new(ui)
                .id_salt(id)
                // Mouse drags scroll the rows, as in egui 0.29 (since 0.35 only on touch screens).
                .drag_to_scroll(egui::scroll_area::DragScroll::Always)
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .min_scrolled_height(0.0)
                .auto_shrink([false, false]);
            for (i, c) in cols.iter().enumerate() {
                let col = if i + 1 == cols.len() { egui_extras::Column::remainder().at_least(c.width.min(120.0)) } else { egui_extras::Column::initial(widths[i]).at_least(36.0) };
                table = table.column(col.clip(true));
            }
            if let Some(row) = scroll_to {
                table = table.scroll_to_row(row, None);
            }
            let last = cols.len().saturating_sub(1);
            table
                .header(HEADER_H, |mut header| {
                    for (i, c) in cols.iter().enumerate() {
                        header.col(|ui| {
                            let r = ui.max_rect();
                            let p = ui.painter();
                            let rounding = egui::CornerRadius { nw: if i == 0 { 7 } else { 0 }, ne: if i == last { 7 } else { 0 }, ..Default::default() };
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
                            let resp = ui.interact(r, ui.scope_id().with(("hdr", i)), Sense::click());
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
                                    let cut = widgets::text_width(p, &content.text, font.clone()) > max_w.max(8.0);
                                    let text_r = widgets::text_fit(&clip, pos2(x, ty), Align2::LEFT_CENTER, &content.text, font, color, max_w);
                                    // A shortened value shows in full on hover.
                                    if cut {
                                        ui.interact(text_r.intersect(r), ui.scope_id().with(("cut", ci)), Sense::hover()).on_hover_text(&content.text);
                                    }
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
                            super::kit::context_menu(&resp, |ui| menu(item, ui));
                        }
                        if resp.double_clicked() {
                            activated = Some(item);
                        }
                    });
                });
        });
    });
    ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0_f32, pal.grid), egui::StrokeKind::Outside);
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
        ui.data_mut(|d| d.insert_temp(active_table(), table_id));
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

/// Case-insensitive order: that of the two strings in lower case (character by character, so
/// without the final-sigma rule of `str::to_lowercase`), compared without building
/// them (a sort calls this thousands of times per frame).
pub fn cmp_str(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
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
        let w = fit_widths(&cols, 720.0);
        assert!((w[0] + w[1] + 120.0 - 720.0).abs() < 0.01);
        // Narrower than that, columns keep 60 % and the rest is scrolled to.
        let w = fit_widths(&cols, 300.0);
        assert!((w[0] - 240.0).abs() < 0.01 && (w[1] - 240.0).abs() < 0.01 && w[2] == 300.0);
        assert!(fit_widths(&[], 100.0).is_empty());
        // Numbers stay whole; the text columns give up the room.
        let w = fit_widths(&[Col::text("a", 400.0), Col::num("b", 90.0), Col::text("c", 100.0)], 390.0);
        assert!((w[0] - 240.0).abs() < 0.01 && w[1] == 90.0);
        // So do trends and the text columns marked rigid ("Running" must not become "Runn…").
        let w = fit_widths(&[Col::text("name", 300.0), Col::text("status", 100.0).rigid(), Col::spark("trend", 120.0), Col::text("path", 300.0)], 520.0);
        assert_eq!((w[1], w[2]), (100.0, 120.0));
        assert!((w[0] - 180.0).abs() < 0.01, "the name gives up the room: {}", w[0]);
    }

    /// `cmp_str` orders like the lower-cased strings, without allocating them.
    #[test]
    fn cmp_str_ignores_case() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_str("alpha", "Beta"), Less);
        assert_eq!(cmp_str("Zeta", "alpha"), Greater);
        assert_eq!(cmp_str("SvcHost", "svchost"), Equal);
        assert_eq!(cmp_str("app", "apple"), Less);
        assert_eq!(cmp_str("", "a"), Less);
        for (a, b) in [("Ärger", "zebra"), ("ǅ", "ǆ"), ("İstanbul", "istanbul"), ("straße", "STRASSE"), ("a-b", "A_b")] {
            assert_eq!(cmp_str(a, b), a.to_lowercase().cmp(&b.to_lowercase()), "{a} / {b}");
        }
    }

    /// Cells with every decoration draw in a table, and a second line makes the rows taller.
    #[test]
    fn decorated_cells_draw() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut icons = super::super::icons::IconCache::new(&ctx);
        let order = [0usize, 1, 2];
        let mut state = TableState::sorted_by(1, true);
        state.selected = Some("1".into());
        for tall in [false, true] {
            let cols = [Col::text("Name", 200.0).two_line(tall), Col::num("CPU", 100.0), Col::spark("CPU 60s", 120.0), Col::text("Status", 100.0).rigid()];
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
                ctx.run_ui(egui::RawInput::default(), |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(700.0, 300.0));
                        show(ui, rect, "t", &pal, &cols, &order, &mut state, &|i| i.to_string(), &cell, &mut icons);
                    });
                }).drop_without_applying_deltas();
            }
        }
        assert_eq!((state.sort, state.desc), (1, true));
    }

    /// The selection keys stay inside the list, and start at the matching end without a selection.
    #[test]
    fn selection_keys_move_within_the_list() {
        let go = |cur, nav| nav_target(cur, 10, nav, 4);
        assert_eq!((go(Some(3), Nav::Up), go(Some(3), Nav::Down)), (Some(2), Some(4)));
        assert_eq!((go(Some(0), Nav::Up), go(Some(9), Nav::Down)), (Some(0), Some(9)), "no wrap at the ends");
        assert_eq!((go(Some(5), Nav::PageUp), go(Some(5), Nav::PageDown)), (Some(1), Some(9)));
        assert_eq!((go(Some(5), Nav::Home), go(Some(5), Nav::End)), (Some(0), Some(9)));
        assert_eq!((go(None, Nav::Down), go(None, Nav::PageDown), go(None, Nav::Up), go(None, Nav::PageUp)), (Some(0), Some(0), Some(9), Some(9)));
        assert_eq!(go(Some(42), Nav::Up), Some(8), "a stale row (the list shrank) counts as the last");
        assert_eq!(nav_target(Some(0), 0, Nav::Down, 4), None, "nothing to select");
        assert_eq!(nav_target(Some(2), 10, Nav::PageDown, 0), Some(3), "a view of less than a row still moves");
    }

    /// Down moves the selection of the table that was clicked, and only of that one.
    #[test]
    fn arrow_keys_move_the_clicked_tables_selection() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut icons = super::super::icons::IconCache::new(&ctx);
        let cols = [Col::text("Name", 200.0), Col::text("Kind", 100.0)];
        let order = [0usize, 1, 2];
        let (mut a, mut b) = (TableState::default(), TableState::default());
        a.selected = Some("0".into());
        b.selected = Some("0".into());
        let down = egui::Event::Key { key: egui::Key::ArrowDown, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let mut frame = |events: Vec<egui::Event>, a: &mut TableState, b: &mut TableState| {
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    let cell = |i: usize, _: usize| Cell::new(format!("row {i}"));
                    show(ui, Rect::from_min_size(pos2(10.0, 10.0), vec2(400.0, 200.0)), "a", &pal, &cols, &order, a, &|i| i.to_string(), &cell, &mut icons);
                    show(ui, Rect::from_min_size(pos2(10.0, 220.0), vec2(400.0, 200.0)), "b", &pal, &cols, &order, b, &|i| i.to_string(), &cell, &mut icons);
                });
            })
            .drop_without_applying_deltas();
        };
        frame(vec![down.clone()], &mut a, &mut b);
        assert_eq!((a.selected.as_deref(), b.selected.as_deref()), (Some("0"), Some("0")), "no table was clicked yet");
        ctx.data_mut(|d| d.insert_temp(active_table(), egui::Id::unique(("tmop-table", "b"))));
        frame(vec![down.clone()], &mut a, &mut b);
        frame(vec![down], &mut a, &mut b);
        assert_eq!((a.selected.as_deref(), b.selected.as_deref()), (Some("0"), Some("2")));
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
