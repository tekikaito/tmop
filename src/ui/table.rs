//! Sortable, virtualised data table in the style of the Processes page, shared by the
//! inventory and advanced pages.

use super::theme::{self, mix, with_alpha, Palette};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Sense, Stroke};

pub const ROW_H: f32 = 26.0;

#[derive(Clone, Copy, Debug)]
pub struct Col {
    pub title: &'static str,
    pub width: f32,
    pub numeric: bool,
}

impl Col {
    pub const fn text(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: false }
    }
    pub const fn num(title: &'static str, width: f32) -> Self {
        Self { title, width, numeric: true }
    }
}

/// One cell's content.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    pub color: Option<Color32>,
    /// 0..1 heat-map intensity behind the cell (green like the Processes page).
    pub heat: f32,
    /// Small coloured status dot before the text.
    pub dot: Option<Color32>,
    /// File whose shell icon is drawn before the text.
    pub icon: Option<String>,
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
    let mut sort_click = None;
    let mut clicked: Option<String> = None;
    let mut activated = None;
    let selected = state.selected.clone();
    let (sort_col, desc) = (state.sort, state.desc);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).id_salt(id), |ui| {
        ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
        let mut table = egui_extras::TableBuilder::new(ui)
            .id_salt(id)
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink([false, false]);
        for (i, c) in cols.iter().enumerate() {
            let col = if i + 1 == cols.len() { egui_extras::Column::remainder().at_least(c.width.min(120.0)) } else { egui_extras::Column::initial(c.width).at_least(36.0) };
            table = table.column(col.clip(true));
        }
        table
            .header(ROW_H, |mut header| {
                for (i, c) in cols.iter().enumerate() {
                    header.col(|ui| {
                        let r = ui.max_rect();
                        let p = ui.painter();
                        p.rect_filled(r, 0.0, pal.body);
                        p.line_segment([pos2(r.right() - 0.5, r.top() + 5.0), pos2(r.right() - 0.5, r.bottom() - 5.0)], Stroke::new(1.0_f32, pal.panel_stroke));
                        p.line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.panel_stroke));
                        let arrow = if sort_col == i { if desc { "  \u{25bc}" } else { "  \u{25b2}" } } else { "" };
                        let label = format!("{}{arrow}", c.title);
                        let color = if sort_col == i { pal.text } else { pal.text_dim };
                        if c.numeric {
                            p.text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, &label, theme::ui(12.5), color);
                        } else {
                            p.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, &label, theme::ui(12.5), color);
                        }
                        let resp = ui.interact(r, ui.id().with(("hdr", i)), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 0.0, with_alpha(pal.text, 0.04));
                        }
                        if resp.clicked() {
                            sort_click = Some(i);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, order.len(), |mut row| {
                    let item = order[row.index()];
                    let k = key(item);
                    let is_sel = selected.as_deref() == Some(k.as_str());
                    row.set_selected(is_sel);
                    for (ci, c) in cols.iter().enumerate() {
                        row.col(|ui| {
                            let r = ui.max_rect();
                            let content = cell(item, ci);
                            let p = ui.painter();
                            if !is_sel && content.heat > 0.01 {
                                p.rect_filled(r.expand2(vec2(0.5, 0.0)), 0.0, mix(pal.body, pal.heat(), 0.25 + 0.75 * content.heat));
                            }
                            p.line_segment([pos2(r.right() - 0.5, r.top()), pos2(r.right() - 0.5, r.bottom())], Stroke::new(1.0_f32, with_alpha(pal.panel_stroke, 0.5)));
                            let color = content.color.unwrap_or(pal.text);
                            let font = theme::ui(12.5);
                            let mut x = r.left() + 8.0;
                            if let Some(d) = content.dot {
                                p.circle_filled(pos2(x + 4.0, r.center().y), 4.0, d);
                                x += 14.0;
                            }
                            if let Some(path) = &content.icon {
                                let ic = Rect::from_min_size(pos2(x, r.center().y - 8.0), vec2(16.0, 16.0));
                                match icons.get(Some(path.as_str())) {
                                    Some(t) => {
                                        p.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                    }
                                    None => super::summary::proc_icon(p, ic.shrink(2.0), false),
                                }
                                x += 22.0;
                            }
                            if c.numeric {
                                p.text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, &content.text, font, color);
                            } else {
                                super::widgets::text_fit(p, pos2(x, r.center().y), Align2::LEFT_CENTER, &content.text, font, color, r.right() - x - 6.0);
                            }
                        });
                    }
                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some(k.clone());
                    }
                    if resp.double_clicked() {
                        activated = Some(item);
                    }
                });
            });
    });
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

    #[test]
    fn sorting_respects_direction() {
        let items = [3, 1, 2];
        let mut order = vec![0, 1, 2];
        let s = TableState::sorted_by(0, true);
        s.sort(&items, &mut order, |a, b, _| a.cmp(b));
        assert_eq!(order, vec![0, 2, 1]);
    }
}
