//! Processes page: column set, Apps / Background groups with an expandable
//! process tree, heat-map cells, real shell icons, a details panel and the actions.

use super::icons::IconCache;
use super::summary::proc_icon;
use super::theme::{self, metric_color, mix, with_alpha, Palette};
use super::widgets::{self, draw_graph, GraphLine, Icon};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::process::{self, format_bytes, format_cpu_time, format_rate, Action, Column, Priority, ProcessRow, Status};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke};
use std::collections::{HashMap, HashSet};

const ROW_H: f32 = 26.0;
const INDENT: f32 = 18.0;

/// Per-page state.
#[derive(Default)]
pub struct ProcState {
    pub expanded: HashSet<(u32, i64)>,
    /// CPU and memory history of the selected process.
    pub sel_key: Option<(u32, i64)>,
    pub sel_cpu: Option<Series>,
    pub sel_mem: Option<Series>,
    pub sel_seq: u64,
}

enum Entry {
    Group(&'static str, usize),
    Row { idx: usize, depth: usize, kids: usize, open: bool },
}

/// Activity totals of a collapsed subtree.
#[derive(Clone, Copy, Default)]
struct Agg {
    cpu: f32,
    mem: u64,
    read: f64,
    write: f64,
    gpu: f32,
}

fn agg_of(i: usize, rows: &[ProcessRow], children: &HashMap<usize, Vec<usize>>, depth: usize) -> Agg {
    let r = &rows[i];
    let mut a = Agg { cpu: r.cpu_pct, mem: r.private_working_set, read: r.read_bps, write: r.write_bps, gpu: r.gpu_pct };
    if depth < 32 {
        for c in children.get(&i).into_iter().flatten() {
            let b = agg_of(*c, rows, children, depth + 1);
            a.cpu += b.cpu;
            a.mem += b.mem;
            a.read += b.read;
            a.write += b.write;
            a.gpu = a.gpu.max(b.gpu);
        }
    }
    a
}

fn mark_app(i: usize, children: &HashMap<usize, Vec<usize>>, in_app: &mut [bool], depth: usize) {
    in_app[i] = true;
    if depth < 32 {
        for c in children.get(&i).into_iter().flatten() {
            mark_app(*c, children, in_app, depth + 1);
        }
    }
}

struct TreeCtx<'a> {
    rows: &'a [ProcessRow],
    children: &'a HashMap<usize, Vec<usize>>,
    expanded: &'a HashSet<(u32, i64)>,
}

fn push_entry(i: usize, depth: usize, t: &TreeCtx<'_>, entries: &mut Vec<Entry>, agg: &mut HashMap<usize, Agg>) {
    let kids = t.children.get(&i).map(|v| v.len()).unwrap_or(0);
    let key = (t.rows[i].pid, t.rows[i].create_time);
    let open = t.expanded.contains(&key);
    entries.push(Entry::Row { idx: i, depth, kids, open });
    if kids > 0 && !open {
        agg.insert(i, agg_of(i, t.rows, t.children, 0));
    }
    if open && depth < 16 {
        for c in t.children.get(&i).into_iter().flatten() {
            push_entry(*c, depth + 1, t, entries, agg);
        }
    }
}

impl App {
    fn tree_enabled(&self) -> bool {
        self.settings.window.process_tree
    }

    /// CPU % as displayed: share of the whole machine, or of one core with "Per Core CPU
    /// Statistics" (TMOP's reading of that setting).
    pub fn shown_cpu(&self, pct: f32, logical: usize) -> f32 {
        if self.settings.general.per_core_cpu_statistics {
            pct * logical.max(1) as f32
        } else {
            pct
        }
    }

    pub fn processes_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Processes", Icon::List);
        let mut refresh = false;
        let mut set_tree: Option<bool> = None;
        let tree_on = self.tree_enabled();
        super::kit::header_toolbar(ui, body, 640.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.filter, "Filter by name, user, or PID", 300.0, &pal);
            let label = if tree_on { "Process View  \u{02C5}" } else { "Flat List  \u{02C5}" };
            ui.menu_button(RichText::new(label).font(theme::ui(13.0)), |ui| {
                if ui.radio(tree_on, "Process view (grouped tree)").clicked() {
                    set_tree = Some(true);
                    ui.close_menu();
                }
                if ui.radio(!tree_on, "Flat list").clicked() {
                    set_tree = Some(false);
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button("Columns", |ui| {
                    for c in Column::ALL.into_iter().filter(|c| *c != Column::Name) {
                        let mut visible = self.column_visible(c);
                        if ui.checkbox(&mut visible, c.title()).changed() {
                            self.set_column_visible(c, visible);
                        }
                    }
                });
            });
        });
        if let Some(t) = set_tree {
            self.settings.window.process_tree = t;
            self.settings_dirty = true;
        }
        if refresh {
            self.sampler.refresh();
        }
        let detail_h = 236.0;
        let table_rect = Rect::from_min_max(body.min + vec2(4.0, 0.0), pos2(body.right() - 4.0, body.bottom() - detail_h));
        let detail_rect = Rect::from_min_max(pos2(body.left(), table_rect.bottom()), body.max);

        // ---- model
        let f = self.filter.to_lowercase();
        let rows = fd.rows;
        let visible: Vec<usize> = (0..rows.len())
            .filter(|i| {
                let r = &rows[*i];
                (self.settings.general.show_self_in_reports || r.pid != std::process::id())
                    && (f.is_empty()
                        || r.name.to_lowercase().contains(&f)
                        || r.pid.to_string() == f
                        || r.user.as_deref().map(|u| u.to_lowercase().contains(&f)).unwrap_or(false)
                        || r.description.as_deref().map(|u| u.to_lowercase().contains(&f)).unwrap_or(false))
            })
            .collect();
        let (col, desc) = self.sort;
        let sort = |v: &mut Vec<usize>| {
            v.sort_by(|a, b| {
                let o = process::compare(&rows[*a], &rows[*b], col);
                if desc {
                    o.reverse()
                } else {
                    o
                }
            })
        };
        let tree = tree_on && f.is_empty();
        let by_pid: HashMap<u32, usize> = visible.iter().map(|i| (rows[*i].pid, *i)).collect();
        let parent_of = |i: usize| -> Option<usize> {
            let r = &rows[i];
            if r.pid == 0 || r.pid == 4 || r.parent_pid == r.pid {
                return None;
            }
            by_pid.get(&r.parent_pid).copied().filter(|p| rows[*p].create_time <= r.create_time && rows[*p].pid != 0)
        };
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut is_child = vec![false; rows.len()];
        if tree {
            for &i in &visible {
                if let Some(p) = parent_of(i) {
                    // Windowed processes always start their own app entry.
                    if !rows[i].has_window {
                        children.entry(p).or_default().push(i);
                        is_child[i] = true;
                    }
                }
            }
            for v in children.values_mut() {
                sort(v);
            }
        }
        // App membership: windowed processes and everything below them.
        let mut in_app = vec![false; rows.len()];
        for &i in &visible {
            if rows[i].has_window && !is_child[i] {
                mark_app(i, &children, &mut in_app, 0);
            }
        }
        let mut apps: Vec<usize> = visible.iter().copied().filter(|i| rows[*i].has_window && !is_child[*i]).collect();
        let mut bg: Vec<usize> = visible.iter().copied().filter(|i| !in_app[*i] && !is_child[*i]).collect();
        sort(&mut apps);
        sort(&mut bg);
        let mut agg_cache: HashMap<usize, Agg> = HashMap::new();
        let mut entries = Vec::with_capacity(visible.len() + 2);
        let tctx = TreeCtx { rows, children: &children, expanded: &self.proc.expanded };
        entries.push(Entry::Group("Apps", apps.len()));
        for i in &apps {
            push_entry(*i, 0, &tctx, &mut entries, &mut agg_cache);
        }
        entries.push(Entry::Group("Background processes", bg.len()));
        for i in &bg {
            push_entry(*i, 0, &tctx, &mut entries, &mut agg_cache);
        }

        let max_mem = visible.iter().map(|i| rows[*i].private_working_set).max().unwrap_or(1).max(1) as f32;
        let max_io = visible.iter().map(|i| rows[*i].read_bps.max(rows[*i].write_bps)).fold(0.0f64, f64::max).max(1.0) as f32;
        let cols: Vec<Column> = Column::ALL.into_iter().filter(|c| self.column_visible(*c)).collect();
        let mut clicked: Option<(u32, i64)> = None;
        let mut toggle: Option<(u32, i64)> = None;
        let mut requested: Option<(Action, ProcessRow)> = None;
        let mut sort_click: Option<Column> = None;
        let mut resized: Vec<(Column, f32)> = Vec::new();
        let selected = self.selected;
        let logical = fd.snap.cpu.logical_count;
        let per_core = self.settings.general.per_core_cpu_statistics;
        let widths: Vec<f32> = cols.iter().map(|c| self.settings.window.process_table_column_widths.get(c.id()).map(|w| *w as f32).unwrap_or(c.default_width())).collect();
        let icons = &mut self.icons;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(table_rect), |ui| {
            ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
            let mut table = egui_extras::TableBuilder::new(ui)
                .id_salt("proc-table")
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false]);
            for w in &widths {
                table = table.column(egui_extras::Column::initial(*w).at_least(36.0).clip(true));
            }
            table = table.column(egui_extras::Column::remainder().at_least(10.0));
            table
                .header(ROW_H, |mut header| {
                    for (k, c) in cols.iter().enumerate() {
                        header.col(|ui| {
                            let r = ui.max_rect();
                            if (r.width() - widths[k]).abs() > 1.0 {
                                resized.push((*c, r.width()));
                            }
                            let p = ui.painter();
                            p.line_segment([pos2(r.right() - 0.5, r.top() + 5.0), pos2(r.right() - 0.5, r.bottom() - 5.0)], Stroke::new(1.0_f32, pal.panel_stroke));
                            let arrow = if col == *c { if desc { "\u{25bc}" } else { "\u{25b2}" } } else { "" };
                            let color = if col == *c { pal.text } else { pal.text_dim };
                            let font = theme::ui(12.5);
                            if c.numeric() {
                                let rr = p.text(pos2(r.right() - 7.0, r.center().y), Align2::RIGHT_CENTER, arrow, theme::ui(9.0), color);
                                p.text(pos2(rr.left() - if arrow.is_empty() { 0.0 } else { 2.0 }, r.center().y), Align2::RIGHT_CENTER, c.title(), font, color);
                            } else {
                                let rr = p.text(pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, c.title(), font, color);
                                p.text(pos2(rr.right() + 3.0, r.center().y), Align2::LEFT_CENTER, arrow, theme::ui(9.0), color);
                            }
                            let resp = ui.interact(r, ui.id().with(("hdr", c.id())), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, 0.0, with_alpha(pal.text, 0.04));
                            }
                            if resp.clicked() {
                                sort_click = Some(*c);
                            }
                        });
                    }
                    header.col(|_| {});
                })
                .body(|body| {
                    body.rows(ROW_H, entries.len(), |mut row| match &entries[row.index()] {
                        Entry::Group(name, n) => {
                            for (ci, _) in cols.iter().enumerate() {
                                row.col(|ui| {
                                    let r = ui.max_rect();
                                    ui.painter().rect_filled(r.expand2(vec2(1.0, 0.0)), 0.0, mix(pal.body, pal.panel, 0.6));
                                    if ci == 0 {
                                        let p = ui.painter().with_clip_rect(r.expand2(vec2(400.0, 0.0)));
                                        p.text(pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, format!("{name} ({n})"), theme::ui(12.5), pal.text);
                                    }
                                });
                            }
                            row.col(|ui| {
                                ui.painter().rect_filled(ui.max_rect().expand2(vec2(1.0, 0.0)), 0.0, mix(pal.body, pal.panel, 0.6));
                            });
                        }
                        Entry::Row { idx, depth, kids, open } => {
                            let r = &rows[*idx];
                            let key = (r.pid, r.create_time);
                            let is_sel = selected == Some(key);
                            row.set_selected(is_sel);
                            let agg = agg_cache.get(idx).copied();
                            for c in &cols {
                                row.col(|ui| {
                                    let cx = CellCtx { max_mem, max_io, selected: is_sel, depth: *depth, kids: *kids, open: *open, agg, logical, per_core };
                                    if cell_ui(ui, r, *c, &pal, cx, icons) {
                                        toggle = Some(key);
                                    }
                                });
                            }
                            row.col(|_| {});
                            let resp = row.response();
                            if resp.clicked() || resp.secondary_clicked() {
                                clicked = Some(key);
                            }
                            if resp.double_clicked() && *kids > 0 {
                                toggle = Some(key);
                            }
                            resp.context_menu(|ui| {
                                if let Some(a) = action_menu(ui, r) {
                                    requested = Some((a, r.clone()));
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
        });
        for (c, w) in resized {
            self.settings.window.process_table_column_widths.insert(c.id().to_string(), w.round() as u32);
            self.settings_dirty = true;
        }
        if let Some(c) = sort_click {
            self.sort = if col == c { (c, !desc) } else { (c, c.numeric()) };
        }
        if let Some(k) = toggle {
            if !self.proc.expanded.remove(&k) {
                self.proc.expanded.insert(k);
            }
        } else if let Some(k) = clicked {
            self.selected = Some(k);
        }
        self.details_panel(ui, detail_rect, fd, &mut requested);
        if let Some((a, r)) = requested {
            self.request(a, r);
        }
    }

    fn details_panel(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>, requested: &mut Option<(Action, ProcessRow)>) {
        let pal = fd.pal;
        let p = ui.painter().clone();
        p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0_f32, pal.panel_stroke));
        let sel = self.selected.and_then(|k| fd.rows.iter().find(|r| (r.pid, r.create_time) == k).cloned());
        let Some(r) = sel else {
            self.proc.sel_key = None;
            p.text(rect.center(), Align2::CENTER_CENTER, "Select a single entry for detail information.", theme::ui(12.5), pal.text_dim);
            return;
        };
        // Track the selection's history.
        let key = (r.pid, r.create_time);
        if self.proc.sel_key != Some(key) {
            self.proc.sel_key = Some(key);
            self.proc.sel_cpu = Some(Series::new(240));
            self.proc.sel_mem = Some(Series::new(240));
            self.proc.sel_seq = 0;
        }
        if self.proc.sel_seq != fd.snap.seq {
            self.proc.sel_seq = fd.snap.seq;
            if let Some(s) = self.proc.sel_cpu.as_mut() {
                s.push(r.cpu_pct);
            }
            if let Some(s) = self.proc.sel_mem.as_mut() {
                s.push(r.private_working_set as f32);
            }
        }
        let inner = rect.shrink2(vec2(20.0, 14.0));
        let cols = super::kit::hsplit(inner, &[1.15, 1.0, 1.1], 24.0);
        // Identity column.
        let ic = Rect::from_min_size(cols[0].left_top(), vec2(32.0, 32.0));
        match self.icons.for_process(r.pid, r.path.as_deref()) {
            Some(t) => {
                p.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => proc_icon(&p, ic.shrink(6.0), r.has_window),
        }
        let title_clip = p.with_clip_rect(Rect::from_min_max(cols[0].min, pos2(cols[0].right(), cols[0].top() + 40.0)));
        title_clip.text(pos2(ic.right() + 10.0, ic.top() - 1.0), Align2::LEFT_TOP, &r.name, theme::display(15.0), pal.text);
        title_clip.text(pos2(ic.right() + 10.0, ic.top() + 20.0), Align2::LEFT_TOP, r.description.as_deref().unwrap_or("No description"), theme::ui(12.0), pal.text_dim);
        let kv = [
            ("PID".to_string(), format!("{}  (parent {})", r.pid, r.parent_pid)),
            ("Status".to_string(), r.status.label().to_string()),
            ("User".to_string(), r.user.clone().unwrap_or("Access denied".into())),
            ("Architecture".to_string(), if r.arch.is_empty() { "\u{2014}".into() } else { r.arch.to_string() }),
            ("Image path".to_string(), r.path.clone().unwrap_or("Access denied".into())),
            ("Command line".to_string(), r.command_line.clone().unwrap_or("Access denied".into())),
        ];
        super::kit::kv_list(&p, Rect::from_min_max(pos2(cols[0].left(), cols[0].top() + 44.0), cols[0].max), &pal, &kv, 96.0, 21.0);
        // Numbers column.
        let kv = [
            ("CPU".to_string(), format!("{:.1}%", self.shown_cpu(r.cpu_pct, fd.snap.cpu.logical_count))),
            ("CPU time".to_string(), format_cpu_time(r.cpu_time)),
            ("Memory (private)".to_string(), format_bytes(r.private_working_set as f64)),
            ("Working set".to_string(), format!("{} (peak {})", format_bytes(r.working_set as f64), format_bytes(r.peak_working_set as f64))),
            ("Commit size".to_string(), format_bytes(r.commit as f64)),
            ("Disk read / write".to_string(), format!("{} / {}", format_rate(r.read_bps), format_rate(r.write_bps))),
            ("Threads / handles".to_string(), format!("{} / {}", r.threads, r.handles)),
            ("Page faults".to_string(), r.page_faults.to_string()),
            ("Priority / session".to_string(), format!("{} / {}", r.base_priority, r.session_id)),
        ];
        super::kit::kv_list(&p, cols[1], &pal, &kv, 128.0, 21.0);
        // Graph + actions column.
        let g_rect = Rect::from_min_max(cols[2].min, pos2(cols[2].right(), cols[2].bottom() - 78.0));
        if let (Some(cpu), Some(mem)) = (&self.proc.sel_cpu, &self.proc.sel_mem) {
            let top_cpu = widgets::eased(ui.ctx(), "process-cpu", cpu.max().max(1.0) * 1.2);
            let (lo, hi) = widgets::eased_range(ui.ctx(), "process-memory", super::summary::visible_range(mem, 240));
            let mut g = self.graph(vec![GraphLine { series: cpu, color: metric_color("cpu"), range: (0.0, top_cpu) }, GraphLine { series: mem, color: metric_color("memory"), range: (lo, hi) }], fd);
            g.regions = 1;
            g.px_per_update = 4.0;
            g.border = Some(pal.card_stroke);
            g.line_width = 1.6;
            draw_graph(&p, g_rect, &g);
            p.text(g_rect.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, "CPU", theme::ui(11.0), metric_color("cpu"));
            p.text(g_rect.left_top() + vec2(36.0, 4.0), Align2::LEFT_TOP, "Memory", theme::ui(11.0), metric_color("memory"));
        }
        let buttons = Rect::from_min_max(pos2(cols[2].left(), cols[2].bottom() - 68.0), cols[2].max);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(buttons).layout(Layout::top_down(Align::Min)), |ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, "End task", &pal, None).clicked() {
                    *requested = Some((Action::EndTask, r.clone()));
                }
                if widgets::button(ui, "End process tree", &pal, None).clicked() {
                    *requested = Some((Action::EndTree, r.clone()));
                }
                let (label, action) = if r.status == Status::Suspended { ("Resume", Action::Resume) } else { ("Suspend", Action::Suspend) };
                if widgets::button(ui, label, &pal, None).clicked() {
                    *requested = Some((action, r.clone()));
                }
            });
            ui.horizontal(|ui| {
                let eff = if r.efficiency { "Efficiency mode: off" } else { "Efficiency mode" };
                if widgets::button(ui, eff, &pal, None).clicked() {
                    *requested = Some((Action::Efficiency(!r.efficiency), r.clone()));
                }
                if r.path.is_some() && widgets::button(ui, "Open file location", &pal, Some(Icon::Folder)).clicked() {
                    *requested = Some((Action::OpenFileLocation, r.clone()));
                }
            });
        });
    }

    pub fn column_visible(&self, c: Column) -> bool {
        if c == Column::Name {
            return true;
        }
        let hidden = &self.settings.window.process_table_hidden_columns;
        let shown_marker = format!("+{}", c.id());
        if c.hidden_by_default() {
            hidden.contains(&shown_marker)
        } else {
            !hidden.iter().any(|h| h == c.id())
        }
    }

    /// Default-hidden columns are enabled with a `+id` entry (design choice).
    pub fn set_column_visible(&mut self, c: Column, visible: bool) {
        let hidden = &mut self.settings.window.process_table_hidden_columns;
        let id = c.id().to_string();
        let plus = format!("+{id}");
        hidden.retain(|h| *h != id && *h != plus);
        if c.hidden_by_default() && visible {
            hidden.push(plus);
        } else if !c.hidden_by_default() && !visible {
            hidden.push(id);
        }
        self.settings_dirty = true;
    }
}

#[derive(Clone, Copy)]
struct CellCtx {
    max_mem: f32,
    max_io: f32,
    selected: bool,
    depth: usize,
    kids: usize,
    open: bool,
    agg: Option<Agg>,
    logical: usize,
    per_core: bool,
}

/// Draws one cell. Returns true when the tree chevron was clicked.
fn cell_ui(ui: &mut egui::Ui, r: &ProcessRow, c: Column, pal: &Palette, cx: CellCtx, icons: &mut IconCache) -> bool {
    let rect = ui.max_rect();
    let a = cx.agg.unwrap_or(Agg { cpu: r.cpu_pct, mem: r.private_working_set, read: r.read_bps, write: r.write_bps, gpu: r.gpu_pct });
    let cpu = if cx.per_core { a.cpu * cx.logical.max(1) as f32 } else { a.cpu };
    // Heat-map intensity for the activity columns.
    let heat = match c {
        Column::Cpu => (a.cpu / 8.0).clamp(0.0, 1.0),
        Column::Memory => (a.mem as f32 / cx.max_mem).clamp(0.0, 1.0).sqrt() * 0.8,
        Column::DiskRead => (a.read as f32 / cx.max_io).clamp(0.0, 1.0).sqrt(),
        Column::DiskWrite => (a.write as f32 / cx.max_io).clamp(0.0, 1.0).sqrt(),
        Column::Gpu => (a.gpu / 15.0).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let active = a.cpu > 0.05 || a.read + a.write > 1024.0 || a.gpu > 0.05;
    {
        let p = ui.painter();
        if !cx.selected && (heat > 0.03 || (active && c.numeric() && c != Column::Pid)) {
            let t = if heat > 0.03 { 0.30 + 0.70 * heat } else { 0.22 };
            p.rect_filled(rect.expand2(vec2(0.5, 0.0)), 0.0, mix(pal.body, pal.heat(), t));
        }
        p.line_segment([pos2(rect.right() - 0.5, rect.top()), pos2(rect.right() - 0.5, rect.bottom())], Stroke::new(1.0_f32, with_alpha(pal.panel_stroke, 0.45)));
    }
    let font = theme::ui(12.5);
    let dimmed = matches!(c, Column::Pid | Column::User | Column::Priority | Column::Architecture | Column::CommandLine | Column::Path | Column::Description);
    let color = if cx.selected || !dimmed { pal.text } else { pal.text_dim };
    let mut toggled = false;
    match c {
        Column::Name => {
            let x0 = rect.left() + 4.0 + cx.depth as f32 * INDENT;
            if cx.kids > 0 {
                let chev = Rect::from_center_size(pos2(x0 + 6.0, rect.center().y), vec2(14.0, 14.0));
                let resp = ui.interact(chev, ui.id().with(("chev", r.pid, r.create_time)), Sense::click());
                let s = Stroke::new(1.5_f32, if resp.hovered() { pal.text } else { pal.text_dim });
                let pts = if cx.open {
                    vec![pos2(chev.left() + 3.0, chev.center().y - 2.0), pos2(chev.center().x, chev.center().y + 2.5), pos2(chev.right() - 3.0, chev.center().y - 2.0)]
                } else {
                    vec![pos2(chev.center().x - 2.0, chev.top() + 3.0), pos2(chev.center().x + 2.5, chev.center().y), pos2(chev.center().x - 2.0, chev.bottom() - 3.0)]
                };
                ui.painter().add(egui::Shape::line(pts, s));
                toggled = resp.clicked();
            }
            let ic = Rect::from_center_size(pos2(x0 + 24.0, rect.center().y), vec2(16.0, 16.0));
            let p = ui.painter();
            match icons.for_process(r.pid, r.path.as_deref()) {
                Some(t) => {
                    p.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => proc_icon(p, ic.shrink(1.5), r.has_window),
            }
            let mut text = r.name.clone();
            if cx.kids > 0 && !cx.open {
                text = format!("{text} ({})", cx.kids + 1);
            }
            if r.efficiency {
                text.push_str("  (eco)");
            }
            widgets::text_fit(p, pos2(ic.right() + 6.0, rect.center().y), Align2::LEFT_CENTER, &text, font, color, rect.right() - ic.right() - 10.0);
        }
        Column::Status => {
            let c = match r.status {
                Status::Running => color,
                Status::Suspended => metric_color("gpu"),
                Status::NotResponding => metric_color("clock"),
            };
            widgets::text_fit(ui.painter(), pos2(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, r.status.label(), font, c, rect.width() - 10.0);
        }
        _ => {
            let text = match c {
                Column::Cpu => format!("{cpu:.1}%"),
                Column::Memory => format_bytes(a.mem as f64),
                Column::DiskRead => format_rate(a.read),
                Column::DiskWrite => format_rate(a.write),
                Column::Disk => format_rate(a.read + a.write),
                Column::Gpu => format!("{:.1}%", a.gpu),
                _ => cell(r, c),
            };
            let p = ui.painter();
            if c.numeric() {
                p.text(pos2(rect.right() - 7.0, rect.center().y), Align2::RIGHT_CENTER, &text, font, color);
            } else {
                widgets::text_fit(p, pos2(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, &text, font, color, rect.width() - 10.0);
            }
        }
    }
    toggled
}

pub fn cell(r: &ProcessRow, c: Column) -> String {
    let denied = || if r.pid <= 4 { String::new() } else { "Access denied".to_string() };
    match c {
        Column::Name => r.name.clone(),
        Column::Pid => r.pid.to_string(),
        Column::Status => r.status.label().into(),
        Column::User => r.user.clone().unwrap_or_else(denied),
        Column::Cpu => format!("{:.1}%", r.cpu_pct),
        Column::Memory => format_bytes(r.private_working_set as f64),
        Column::DiskRead => format_rate(r.read_bps),
        Column::DiskWrite => format_rate(r.write_bps),
        Column::Disk => format_rate(r.io_bps),
        Column::Gpu => format!("{:.1}%", r.gpu_pct),
        Column::PeakMemory => format_bytes(r.peak_working_set as f64),
        Column::PageFaults => r.page_faults.to_string(),
        Column::Architecture => r.arch.to_string(),
        Column::CommandLine => r.command_line.clone().unwrap_or_else(denied),
        Column::Description => r.description.clone().unwrap_or_default(),
        Column::Threads => r.threads.to_string(),
        Column::Handles => r.handles.to_string(),
        Column::Session => r.session_id.to_string(),
        Column::Priority => r.base_priority.to_string(),
        Column::ParentPid => r.parent_pid.to_string(),
        Column::CpuTime => format_cpu_time(r.cpu_time),
        Column::Commit => format_bytes(r.commit as f64),
        Column::Path => r.path.clone().unwrap_or_else(denied),
    }
}

fn action_menu(ui: &mut egui::Ui, r: &ProcessRow) -> Option<Action> {
    let mut out = None;
    ui.label(RichText::new(format!("{} ({})", r.name, r.pid)).strong());
    if ui.button("End task").clicked() {
        out = Some(Action::EndTask);
    }
    if ui.button("End process tree").clicked() {
        out = Some(Action::EndTree);
    }
    ui.separator();
    if ui.button(if r.efficiency { "Efficiency mode: turn off" } else { "Efficiency mode" }).clicked() {
        out = Some(Action::Efficiency(!r.efficiency));
    }
    if r.status == Status::Suspended {
        if ui.button("Resume").clicked() {
            out = Some(Action::Resume);
        }
    } else if ui.button("Suspend").clicked() {
        out = Some(Action::Suspend);
    }
    ui.menu_button("Set priority", |ui| {
        for p in Priority::ALL {
            if ui.button(p.label()).clicked() {
                out = Some(Action::SetPriority(p));
                ui.close_menu();
            }
        }
    });
    ui.separator();
    if ui.add_enabled(r.path.is_some(), egui::Button::new("Open file location")).clicked() {
        out = Some(Action::OpenFileLocation);
    }
    if ui.button("Copy name").clicked() {
        ui.ctx().copy_text(r.name.clone());
        ui.close_menu();
    }
    if let Some(cmd) = &r.command_line {
        if ui.button("Copy command line").clicked() {
            ui.ctx().copy_text(cmd.clone());
            ui.close_menu();
        }
    }
    out
}
