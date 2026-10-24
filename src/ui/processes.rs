//! Processes page: system history, filter tabs, Apps / Background groups with an expandable
//! process tree, trend columns, heat-map cells, real shell icons, the process details view and
//! the actions.

use super::icons::IconCache;
use super::kit;
use super::table::{self, Cell as TableCell, Col, TableState};
use super::theme::{self, with_alpha, Palette};
use super::widgets::{self, Button, ButtonKind, ButtonSize, Dot, Icon};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::pro::connections::{Conn, Proto};
use crate::process::{self, format_bytes, format_cpu_time, format_rate, Action, Column, Priority, ProcessRow, Status};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ROW_H: f32 = table::ROW_H;
const CHILD_H: f32 = 28.0;
/// Samples kept per row for the trend columns.
const SPARK_LEN: usize = 60;
const SPARK_W: f32 = 84.0;
const SPARK_H: f32 = 24.0;
/// A group shows this many member rows before "+ n more processes".
const SHOWN_MEMBERS: usize = 3;
/// Height of the system history strip.
const STRIP_H: f32 = 108.0;
/// Width of the "Columns" button at the right end of the filter tabs.
const COLUMNS_W: f32 = 140.0;
const VIEWS: [&str; 6] = ["All", "Apps", "Background", "Windows", "Services", "Suspended"];

/// Column display order; the first entries are the ones shown by default.
const ORDER: [Column; 23] = [
    Column::Name,
    Column::Pid,
    Column::Status,
    Column::User,
    Column::Cpu,
    Column::Memory,
    Column::Disk,
    Column::DiskRead,
    Column::DiskWrite,
    Column::Gpu,
    Column::Threads,
    Column::Handles,
    Column::PeakMemory,
    Column::CpuTime,
    Column::PageFaults,
    Column::Priority,
    Column::Architecture,
    Column::Session,
    Column::ParentPid,
    Column::Commit,
    Column::CommandLine,
    Column::Path,
    Column::Description,
];

/// Columns not shown until the user enables them.
fn default_hidden(c: Column) -> bool {
    !matches!(c, Column::Name | Column::Pid | Column::Status | Column::Cpu | Column::Memory | Column::Disk | Column::Gpu | Column::Description)
}

/// Per-page state.
#[derive(Default)]
pub struct ProcState {
    pub expanded: HashSet<(u32, i64)>,
    /// Groups listing all their members instead of the first few.
    pub show_all: HashSet<(u32, i64)>,
    /// Filter tab (index into [`VIEWS`]).
    pub view: usize,
    /// The details view of the selected process replaces the list.
    pub details: bool,
    /// Details tab: 0 Summary, 1 Network.
    pub details_tab: usize,
    /// CPU, memory, disk and GPU history of the selected process.
    pub sel_key: Option<(u32, i64)>,
    pub sel_cpu: Option<Series>,
    pub sel_mem: Option<Series>,
    pub sel_disk: Option<Series>,
    pub sel_gpu: Option<Series>,
    pub sel_seq: u64,
    /// CPU and memory trend of every process, one sample per snapshot.
    pub spark: HashMap<(u32, i64), (Series, Series)>,
    spark_seq: u64,
    conns_table: TableState,
    /// The header search text seen last frame (editing it closes the details view).
    filter_seen: String,
}

enum Entry {
    Group(&'static str, usize),
    Row { idx: usize, depth: usize, kids: usize, open: bool },
    /// "+ n more processes" of the group `key`.
    More { key: (u32, i64), n: usize },
}

/// Activity totals of a subtree.
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

/// `i` and every process below it.
fn members_of(i: usize, children: &HashMap<usize, Vec<usize>>, out: &mut Vec<usize>, depth: usize) {
    out.push(i);
    if depth < 32 {
        for c in children.get(&i).into_iter().flatten() {
            members_of(*c, children, out, depth + 1);
        }
    }
}

fn mark_app(i: usize, children: &HashMap<usize, Vec<usize>>, in_app: &mut [bool], depth: usize) {
    in_app[i] = true;
    if depth < 32 {
        for c in children.get(&i).into_iter().flatten() {
            mark_app(*c, children, in_app, depth + 1);
        }
    }
}

/// Parent of row `i` among the processes in `by_pid` (the creation time must not precede the parent's).
fn parent_in(rows: &[ProcessRow], by_pid: &HashMap<u32, usize>, i: usize) -> Option<usize> {
    let r = &rows[i];
    if r.pid == 0 || r.pid == 4 || r.parent_pid == r.pid {
        return None;
    }
    by_pid.get(&r.parent_pid).copied().filter(|p| rows[*p].create_time <= r.create_time && rows[*p].pid != 0)
}

fn sort_idx(v: &mut [usize], rows: &[ProcessRow], col: Column, desc: bool) {
    v.sort_by(|a, b| {
        let o = process::compare(&rows[*a], &rows[*b], col);
        if desc {
            o.reverse()
        } else {
            o
        }
    })
}

/// Apps (windowed processes with everything started below them) and background processes.
#[derive(Default)]
struct Groups {
    apps: Vec<usize>,
    bg: Vec<usize>,
    children: HashMap<usize, Vec<usize>>,
}

fn group_rows(rows: &[ProcessRow], visible: &[usize], tree: bool, col: Column, desc: bool) -> Groups {
    let by_pid: HashMap<u32, usize> = visible.iter().map(|i| (rows[*i].pid, *i)).collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut is_child = vec![false; rows.len()];
    if tree {
        for &i in visible {
            if let Some(p) = parent_in(rows, &by_pid, i) {
                // Windowed processes always start their own app entry.
                if !rows[i].has_window {
                    children.entry(p).or_default().push(i);
                    is_child[i] = true;
                }
            }
        }
        for v in children.values_mut() {
            sort_idx(v, rows, col, desc);
        }
    }
    // App membership: windowed processes and everything below them.
    let mut in_app = vec![false; rows.len()];
    for &i in visible {
        if rows[i].has_window && !is_child[i] {
            mark_app(i, &children, &mut in_app, 0);
        }
    }
    let mut apps: Vec<usize> = visible.iter().copied().filter(|i| rows[*i].has_window && !is_child[*i]).collect();
    let mut bg: Vec<usize> = visible.iter().copied().filter(|i| !in_app[*i] && !is_child[*i]).collect();
    sort_idx(&mut apps, rows, col, desc);
    sort_idx(&mut bg, rows, col, desc);
    Groups { apps, bg, children }
}

/// The list rows and what they aggregate.
#[derive(Default)]
struct Built {
    entries: Vec<Entry>,
    /// Totals of a group row (top-level groups always, nested ones while collapsed).
    agg: HashMap<usize, Agg>,
    /// Processes in a group row, itself included.
    size: HashMap<usize, usize>,
}

struct TreeCtx<'a> {
    rows: &'a [ProcessRow],
    children: &'a HashMap<usize, Vec<usize>>,
    expanded: &'a HashSet<(u32, i64)>,
    show_all: &'a HashSet<(u32, i64)>,
}

fn push_entry(i: usize, depth: usize, t: &TreeCtx<'_>, out: &mut Built) {
    let kids = t.children.get(&i).map(|v| v.len()).unwrap_or(0);
    let key = (t.rows[i].pid, t.rows[i].create_time);
    let open = t.expanded.contains(&key);
    out.entries.push(Entry::Row { idx: i, depth, kids, open });
    if kids > 0 {
        let mut all = Vec::new();
        members_of(i, t.children, &mut all, 0);
        out.size.insert(i, all.len());
        if !open || depth == 0 {
            out.agg.insert(i, agg_of(i, t.rows, t.children, 0));
        }
    }
    if open && depth < 16 {
        for c in t.children.get(&i).into_iter().flatten() {
            push_entry(*c, depth + 1, t, out);
        }
    }
}

/// A top-level entry; an open group with many members lists the first few and a "more" line.
fn push_root(i: usize, t: &TreeCtx<'_>, out: &mut Built) {
    let start = out.entries.len();
    push_entry(i, 0, t, out);
    let key = (t.rows[i].pid, t.rows[i].create_time);
    let Some(&size) = out.size.get(&i) else { return };
    let shown = out.entries.len() - start - 1;
    if t.expanded.contains(&key) && !t.show_all.contains(&key) && size > SHOWN_MEMBERS + 1 && shown > SHOWN_MEMBERS {
        out.entries.truncate(start + 1 + SHOWN_MEMBERS);
        out.entries.push(Entry::More { key, n: size - 1 - SHOWN_MEMBERS });
    }
}

/// CPU and memory trend of a group: its members' samples added up, newest aligned.
fn summed_spark(rows: &[ProcessRow], members: &[usize], spark: &HashMap<(u32, i64), (Series, Series)>) -> (Series, Series) {
    let mut cpu = [0.0f32; SPARK_LEN];
    let mut mem = [0.0f32; SPARK_LEN];
    let mut len = 0;
    for m in members {
        let Some((c, mm)) = spark.get(&(rows[*m].pid, rows[*m].create_time)) else { continue };
        let n = c.values.len().min(SPARK_LEN);
        len = len.max(n);
        for (k, v) in c.values.iter().skip(c.values.len() - n).enumerate() {
            cpu[SPARK_LEN - n + k] += v;
        }
        let n = mm.values.len().min(SPARK_LEN);
        for (k, v) in mm.values.iter().skip(mm.values.len() - n).enumerate() {
            mem[SPARK_LEN - n + k] += v;
        }
    }
    let (mut c, mut m) = (Series::new(SPARK_LEN), Series::new(SPARK_LEN));
    for k in SPARK_LEN - len..SPARK_LEN {
        c.push(cpu[k]);
        m.push(mem[k]);
    }
    (c, m)
}

/// Lower-cased `%SystemRoot%`.
fn system_root() -> &'static str {
    static ROOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).trim_end_matches('\\').to_lowercase())
}

/// Whether `path` lies under the folder `root` (case-insensitive, `root` lower case).
fn path_under(path: &str, root: &str) -> bool {
    let (p, r) = (path.as_bytes(), root.as_bytes());
    p.len() > r.len() && p[..r.len()].eq_ignore_ascii_case(r) && p[r.len()] == b'\\'
}

fn is_windows_process(r: &ProcessRow) -> bool {
    r.path.as_deref().is_some_and(|p| path_under(p, system_root()))
}

/// Runs as SYSTEM, LOCAL SERVICE or NETWORK SERVICE.
pub(super) fn is_system_user(user: Option<&str>) -> bool {
    let name = user.map(|u| u.rsplit('\\').next().unwrap_or(u).trim().to_ascii_uppercase());
    matches!(name.as_deref(), Some("SYSTEM" | "LOCAL SERVICE" | "NETWORK SERVICE"))
}

/// Filter tab `view` (3 Windows, 4 Services, 5 Suspended) accepts `r`.
fn in_view(view: usize, r: &ProcessRow) -> bool {
    match view {
        3 => is_windows_process(r),
        4 => is_system_user(r.user.as_deref()) && !r.has_window,
        5 => r.status == Status::Suspended,
        _ => true,
    }
}

/// How an app is called in dialogs and breadcrumbs: the image description when it is short.
pub(super) fn app_label(r: &ProcessRow) -> String {
    match r.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() && d.chars().count() <= 40 => d.to_string(),
        _ => r.name.clone(),
    }
}

/// `true` on the frame `key` went down (key repeat does not count).
pub(super) fn key_once(ctx: &egui::Context, key: egui::Key) -> bool {
    ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, repeat: false, .. } if *k == key)))
}

/// "1 s", "0.5 s" or "paused".
fn interval_text(iv: Option<Duration>) -> String {
    match iv {
        Some(d) => format!("{} s", d.as_secs_f32()),
        None => "paused".into(),
    }
}

/// "15s", "60s", "2m".
fn span_text(secs: f32) -> String {
    if secs >= 120.0 {
        format!("{:.0}m", secs / 60.0)
    } else if (secs - secs.round()).abs() < 0.05 {
        format!("{secs:.0}s")
    } else {
        format!("{secs:.1}s")
    }
}

/// Length of `n` samples as text; without a sampling interval (paused) the sample count.
fn samples_text(n: usize, iv: Option<Duration>) -> String {
    match iv {
        Some(d) => span_text(n as f32 * d.as_secs_f32()),
        None => format!("{n} samples"),
    }
}

fn split_unit(s: &str) -> (&str, &str) {
    s.split_once(' ').unwrap_or((s, ""))
}

fn tail(s: &Series, n: usize) -> Series {
    let mut out = Series::new(n);
    for v in s.values.iter().skip(s.values.len().saturating_sub(n)) {
        out.push(*v);
    }
    out
}

fn sum_tail(a: &Series, b: &Series, n: usize) -> Series {
    let (a, b) = (tail(a, n), tail(b, n));
    let mut out = Series::new(n);
    for (x, y) in a.values.iter().zip(&b.values) {
        out.push(x + y);
    }
    out
}

/// Value range of `s` with room around it, at least `min_span` wide, so small moves show.
fn auto_range(s: &Series, min_span: f32) -> (f32, f32) {
    let lo = s.values.iter().copied().fold(f32::MAX, f32::min);
    let hi = s.values.iter().copied().fold(0.0, f32::max);
    if lo > hi {
        return (0.0, min_span.max(1.0));
    }
    let span = (hi - lo).max(min_span.max(1.0));
    let mid = (hi + lo) / 2.0;
    (mid - span * 0.6, mid + span * 0.6)
}

/// A checkbox in a 36 px chip; true when clicked.
fn check_chip(ui: &mut egui::Ui, label: &str, on: bool, pal: &Palette) -> bool {
    let font = theme::ui(13.0);
    let tw = widgets::text_width(ui.painter(), label, font.clone());
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + 12.0 + 16.0 + 10.0 + 14.0, 36.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 6.0, if resp.hovered() { pal.raise(0.09) } else { pal.control });
    p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, pal.control_stroke));
    let boxr = Rect::from_center_size(pos2(rect.left() + 12.0 + 8.0, rect.center().y), vec2(16.0, 16.0));
    if on {
        p.rect_filled(boxr, 4.0, pal.accent);
        widgets::draw_icon(p, boxr.shrink(2.5), Icon::Check, pal.body);
    } else {
        p.rect_stroke(boxr, 4.0, Stroke::new(1.2_f32, pal.raise(0.35)));
    }
    p.text(pos2(boxr.right() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, font, pal.text_table());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// One tile of the history strip: label, current value and unit, and the lines to draw
/// (series, colour, value range).
struct StripTile<'a> {
    label: &'static str,
    value: &'a str,
    unit: String,
    lines: Vec<(&'a Series, Color32, (f32, f32))>,
    /// Network: the second line is dashed and the middle axis label names the two.
    net: bool,
}

/// The four history tiles: the newest minute of CPU, memory, disk and network.
fn history_strip(p: &egui::Painter, rect: Rect, fd: &FrameData<'_>, pal: &Palette, iv: Option<Duration>) {
    let tiles = kit::tile_strip(p, rect, pal, 4);
    let h = fd.hist;
    let used = fd.snap.memory.total.saturating_sub(fd.snap.memory.available) as f64;
    let net = fd.snap.network.rx_bps + fd.snap.network.tx_bps;
    let cpu = tail(&h.cpu, SPARK_LEN);
    let mem = tail(&h.memory_bytes, SPARK_LEN);
    let disk = sum_tail(&h.disk_read, &h.disk_write, SPARK_LEN);
    let rx = tail(&h.net_rx, SPARK_LEN);
    let tx = tail(&h.net_tx, SPARK_LEN);
    let net_hi = rx.max().max(tx.max()).max(100.0 * 1024.0);
    let disk_rate = format_rate(fd.snap.disk.read_bps + fd.snap.disk.write_bps);
    let net_rate = format_rate(net);
    let first = format!("{:.0}", fd.snap.cpu.total_pct);
    let second = format!("{:.1}", used / (1u64 << 30) as f64);
    let (disk_v, disk_u) = split_unit(&disk_rate);
    let (net_v, net_u) = split_unit(&net_rate);
    let n = SPARK_LEN;
    let (left, mid) = match iv {
        Some(d) => (span_text(n as f32 * d.as_secs_f32()), span_text(n as f32 * d.as_secs_f32() / 2.0)),
        None => (format!("{n}"), format!("{}", n / 2)),
    };
    let cells = [
        StripTile { label: "CPU", value: &first, unit: "%".into(), lines: vec![(&cpu, fd.metric("cpu"), (0.0, 100.0))], net: false },
        StripTile { label: "Memory", value: &second, unit: " GB".into(), lines: vec![(&mem, fd.metric("memory"), auto_range(&mem, fd.snap.memory.total as f32 * 0.02))], net: false },
        StripTile { label: "Disk", value: disk_v, unit: format!(" {disk_u}"), lines: vec![(&disk, fd.metric("disk"), (0.0, disk.max().max(1024.0 * 1024.0)))], net: false },
        StripTile { label: "Network", value: net_v, unit: format!(" {net_u}"), lines: vec![(&rx, fd.metric("network"), (0.0, net_hi)), (&tx, fd.metric("network"), (0.0, net_hi))], net: true },
    ];
    for (t, StripTile { label, value, unit, lines, net }) in tiles.iter().zip(cells) {
        widgets::tracked_text(p, pos2(t.left() + 14.0, t.top() + 12.0), Align2::LEFT_TOP, &label.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.14);
        let ur = p.text(pos2(t.right() - 14.0, t.top() + 10.0), Align2::RIGHT_TOP, &unit, theme::mono(13.0), pal.text_faint);
        p.text(pos2(ur.left(), t.top() + 10.0), Align2::RIGHT_TOP, value, theme::mono(13.0), pal.text);
        let graph = Rect::from_min_max(pos2(t.left() + 14.0, t.top() + 34.0), pos2(t.right() - 14.0, t.top() + 82.0));
        let line = Stroke::new(1.0_f32, with_alpha(pal.text, 0.05));
        p.line_segment([pos2(graph.left(), graph.center().y), pos2(graph.right(), graph.center().y)], Stroke::new(1.0_f32, pal.row_rule()));
        for k in 1..4 {
            let x = graph.left() + graph.width() * k as f32 / 4.0;
            p.line_segment([pos2(x, graph.top()), pos2(x, graph.bottom())], line);
        }
        widgets::sparkline(p, graph, &lines, 0.15, net);
        let ay = graph.bottom() + 6.0;
        p.text(pos2(graph.left(), ay), Align2::LEFT_TOP, &left, theme::mono(10.0), pal.text_faint);
        p.text(pos2(graph.center().x, ay), Align2::CENTER_TOP, if net { "\u{2193} solid \u{b7} \u{2191} dashed" } else { mid.as_str() }, theme::mono(10.0), pal.text_faint);
        p.text(pos2(graph.right(), ay), Align2::RIGHT_TOP, "now", theme::mono(10.0), pal.text_faint);
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

    /// One trend sample per process and snapshot; processes that are gone are dropped.
    fn update_spark(&mut self, fd: &FrameData<'_>) {
        if self.proc.spark_seq == fd.snap.seq && !self.proc.spark.is_empty() {
            return;
        }
        self.proc.spark_seq = fd.snap.seq;
        for r in fd.rows {
            let e = self.proc.spark.entry((r.pid, r.create_time)).or_insert_with(|| (Series::new(SPARK_LEN), Series::new(SPARK_LEN)));
            e.0.push(r.cpu_pct);
            e.1.push(r.private_working_set as f32);
        }
        let live: HashSet<(u32, i64)> = fd.rows.iter().map(|r| (r.pid, r.create_time)).collect();
        self.proc.spark.retain(|k, _| live.contains(k));
    }

    /// History of the selected process for the details view.
    fn track_selection(&mut self, fd: &FrameData<'_>) {
        let Some(r) = self.selected.and_then(|k| fd.rows.iter().find(|r| (r.pid, r.create_time) == k)) else {
            self.proc.sel_key = None;
            return;
        };
        let key = (r.pid, r.create_time);
        if self.proc.sel_key != Some(key) {
            self.proc.sel_key = Some(key);
            self.proc.sel_cpu = Some(Series::new(240));
            self.proc.sel_mem = Some(Series::new(240));
            self.proc.sel_disk = Some(Series::new(240));
            self.proc.sel_gpu = Some(Series::new(240));
            self.proc.sel_seq = 0;
        }
        if self.proc.sel_seq != fd.snap.seq {
            self.proc.sel_seq = fd.snap.seq;
            let values = [r.cpu_pct, r.private_working_set as f32, (r.read_bps + r.write_bps) as f32, r.gpu_pct];
            for (s, v) in [&mut self.proc.sel_cpu, &mut self.proc.sel_mem, &mut self.proc.sel_disk, &mut self.proc.sel_gpu].into_iter().zip(values) {
                if let Some(s) = s.as_mut() {
                    s.push(v);
                }
            }
        }
    }

    pub fn processes_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.update_spark(fd);
        self.track_selection(fd);
        if self.proc.filter_seen != self.filter {
            self.proc.filter_seen.clone_from(&self.filter);
            self.proc.details = false;
        }
        let mut requested: Option<(Action, ProcessRow)> = None;
        let sel_row = self.selected.and_then(|k| fd.rows.iter().find(|r| (r.pid, r.create_time) == k)).cloned();
        if self.proc.details {
            match &sel_row {
                Some(r) => {
                    self.details_view(ui, fd, r, &mut requested);
                    if let Some((a, r)) = requested {
                        self.request(a, r);
                    }
                    return;
                }
                None => self.proc.details = false,
            }
        }

        let iv = self.settings.update_speed().interval();
        let tree_on = self.tree_enabled();

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
        let tree = tree_on && f.is_empty();
        let all = group_rows(rows, &visible, tree, col, desc);
        let view = self.proc.view.min(VIEWS.len() - 1);
        let counts: [Option<usize>; 6] = [
            None,
            Some(all.apps.len()),
            Some(all.bg.len()),
            Some(visible.iter().filter(|i| in_view(3, &rows[**i])).count()),
            Some(visible.iter().filter(|i| in_view(4, &rows[**i])).count()),
            Some(visible.iter().filter(|i| in_view(5, &rows[**i])).count()),
        ];
        let subset;
        let (groups, sub_roots): (&Groups, Vec<usize>) = if view >= 3 {
            let ids: Vec<usize> = visible.iter().copied().filter(|i| in_view(view, &rows[*i])).collect();
            subset = group_rows(rows, &ids, tree, col, desc);
            let mut roots = [subset.apps.as_slice(), subset.bg.as_slice()].concat();
            sort_idx(&mut roots, rows, col, desc);
            (&subset, roots)
        } else {
            (&all, Vec::new())
        };
        let mut built = Built::default();
        let tctx = TreeCtx { rows, children: &groups.children, expanded: &self.proc.expanded, show_all: &self.proc.show_all };
        match view {
            0 => {
                built.entries.push(Entry::Group("Apps", groups.apps.len()));
                for i in &groups.apps {
                    push_root(*i, &tctx, &mut built);
                }
                built.entries.push(Entry::Group("Background processes", groups.bg.len()));
                for i in &groups.bg {
                    push_root(*i, &tctx, &mut built);
                }
            }
            1 => {
                for i in &groups.apps {
                    push_root(*i, &tctx, &mut built);
                }
            }
            2 => {
                for i in &groups.bg {
                    push_root(*i, &tctx, &mut built);
                }
            }
            _ => {
                for i in &sub_roots {
                    push_root(*i, &tctx, &mut built);
                }
            }
        }
        let sel_info = sel_row.as_ref().and_then(|r| rows.iter().position(|x| (x.pid, x.create_time) == (r.pid, r.create_time))).map(|i| {
            let mut members = Vec::new();
            members_of(i, &groups.children, &mut members, 0);
            (agg_of(i, rows, &groups.children, 0), members.len())
        });

        // ---- header
        let title_area = ui.max_rect();
        let subtitle = format!("  {} \u{b7} {} apps \u{b7} {} processes \u{b7} sorted by {}", if iv.is_some() { format!("live \u{b7} {}", interval_text(iv)) } else { "paused".to_string() }, all.apps.len(), visible.len(), col.title());
        let body = self.page_title(ui, &pal, "Processes", &subtitle);
        let st = theme::status(&pal);
        {
            let c = pos2(title_area.left() + 12.0 + 1.0 + 4.0, title_area.top() + 44.0 + 8.0);
            let color = if iv.is_some() { st.ok } else { pal.text_faint };
            if iv.is_some() && fd.bloom > 0.0 {
                ui.painter().circle_filled(c, 6.0, with_alpha(color, 0.25));
            }
            ui.painter().circle_filled(c, 3.0, color);
        }
        let mut refresh = false;
        let mut toggle_tree = false;
        let mut end_task = false;
        kit::header_toolbar(ui, body, 460.0, |ui| {
            ui.add_enabled_ui(sel_row.is_some(), |ui| {
                end_task = Button::primary("End task").show(ui, &pal).clicked();
            });
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            toggle_tree = check_chip(ui, "Group by app", tree_on, &pal);
        });
        if toggle_tree {
            self.settings.window.process_tree = !tree_on;
            self.settings_dirty = true;
        }
        if refresh {
            self.sampler.refresh();
        }
        if end_task {
            if let Some(r) = &sel_row {
                requested = Some((Action::EndTask, r.clone()));
            }
        }

        // ---- layout: history strip, filter tabs, table
        let area = Rect::from_min_max(pos2(body.left() + 12.0, body.top() + 2.0), body.max);
        let strip = Rect::from_min_size(area.min, vec2(area.width(), STRIP_H));
        history_strip(ui.painter(), strip, fd, &pal, iv);
        let tabs_rect = Rect::from_min_size(pos2(area.left(), strip.bottom() + 14.0), vec2(area.width(), 36.0));
        let tab_items: Vec<(&str, Option<usize>)> = VIEWS.iter().zip(counts).map(|(l, c)| (*l, c)).collect();
        // The tabs stop short of the Columns button; the hairline carries on under it.
        let tabs_only = Rect::from_min_max(tabs_rect.min, pos2(tabs_rect.right() - COLUMNS_W - 8.0, tabs_rect.bottom()));
        ui.painter().line_segment([pos2(tabs_only.right(), tabs_rect.bottom() - 0.5), pos2(tabs_rect.right(), tabs_rect.bottom() - 0.5)], Stroke::new(1.0_f32, pal.sidebar_rule));
        if let Some(v) = widgets::tab_bar(ui, tabs_only, &pal, &tab_items, view, None) {
            self.proc.view = v;
        }
        kit::row_ui(ui, Rect::from_min_max(pos2(tabs_rect.right() - COLUMNS_W, tabs_rect.top()), pos2(tabs_rect.right(), tabs_rect.bottom() - 2.0)), |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let resp = Button::ghost("Columns").icon(Icon::ChevronDown).size(ButtonSize::Compact).show(ui, &pal);
                let id = ui.make_persistent_id("proc-columns");
                if resp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(id));
                }
                egui::popup::popup_below_widget(ui, id, &resp, egui::popup::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    ui.set_min_width(170.0);
                    for c in ORDER.into_iter().filter(|c| *c != Column::Name) {
                        let mut shown = self.column_visible(c);
                        if ui.checkbox(&mut shown, c.title()).changed() {
                            self.set_column_visible(c, shown);
                        }
                    }
                });
            });
        });
        let bar_room = if sel_row.is_some() { 64.0 } else { 0.0 };
        let table_rect = Rect::from_min_max(pos2(area.left(), tabs_rect.bottom() + 12.0), pos2(area.right(), (area.bottom() - bar_room).max(tabs_rect.bottom() + 80.0)));

        let max_mem = visible.iter().map(|i| rows[*i].private_working_set).max().unwrap_or(1).max(1) as f32;
        let max_io = visible.iter().map(|i| rows[*i].read_bps.max(rows[*i].write_bps)).fold(0.0f64, f64::max).max(1.0) as f32;
        let span = samples_text(SPARK_LEN, iv);
        let mut cols: Vec<PCol> = Vec::new();
        for c in ORDER.into_iter().filter(|c| self.column_visible(*c)) {
            cols.push(PCol::Data(c));
            if c == Column::Cpu {
                cols.push(PCol::CpuTrend);
            } else if c == Column::Memory {
                cols.push(PCol::MemTrend);
            }
        }
        let titles: Vec<String> = cols.iter().map(|c| c.title(&span)).collect();
        let widths: Vec<f32> = cols.iter().map(|c| c.width(&self.settings.window.process_table_column_widths)).collect();
        let heights: Vec<f32> = built
            .entries
            .iter()
            .map(|e| match e {
                Entry::Row { depth: 0, .. } | Entry::Group(..) => ROW_H,
                _ => CHILD_H,
            })
            .collect();
        let mut clicked: Option<(u32, i64)> = None;
        let mut toggle: Option<(u32, i64)> = None;
        let mut open_details: Option<(u32, i64)> = None;
        let mut more: Option<(u32, i64)> = None;
        let mut menu_pick: Option<(MenuPick, ProcessRow)> = None;
        let mut sort_click: Option<Column> = None;
        let mut resized: Vec<(String, f32)> = Vec::new();
        let selected = self.selected;
        let logical = fd.snap.cpu.logical_count;
        let per_core = self.settings.general.per_core_cpu_statistics;
        let mem_color = fd.metric("memory");
        let icons = &mut self.icons;
        let spark = &self.proc.spark;
        let entries = &built.entries;
        let p = ui.painter().clone();
        p.rect_filled(table_rect, 8.0, with_alpha(pal.text, 0.03));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(table_rect.shrink(1.0)).id_salt("proc-table-box"), |ui| {
            ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
            let mut table = egui_extras::TableBuilder::new(ui)
                .id_salt("proc-table")
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false]);
            for (c, w) in cols.iter().zip(&widths) {
                table = table.column(if c.is_trend() { egui_extras::Column::exact(*w) } else { egui_extras::Column::initial(*w).at_least(36.0).clip(true) });
            }
            table = table.column(egui_extras::Column::remainder().at_least(10.0));
            table
                .header(table::HEADER_H, |mut header| {
                    for (k, c) in cols.iter().enumerate() {
                        header.col(|ui| {
                            let r = ui.max_rect();
                            if !c.is_trend() && (r.width() - widths[k]).abs() > 1.0 {
                                resized.push((c.id().to_string(), r.width()));
                            }
                            let p = ui.painter();
                            let rounding = egui::Rounding { nw: if k == 0 { 7.0 } else { 0.0 }, ..Default::default() };
                            p.rect_filled(r, rounding, pal.panel);
                            p.line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.panel_stroke));
                            let sorted = col == c.base() && !c.is_trend();
                            let arrow = if !sorted { "" } else if desc { " \u{2193}" } else { " \u{2191}" };
                            let label = format!("{}{arrow}", titles[k].to_uppercase());
                            let (font, color) = if sorted { (theme::semibold(11.0), pal.accent) } else { (theme::ui(11.0), pal.text_faint) };
                            let clip = p.with_clip_rect(r);
                            if c.right() {
                                widgets::tracked_text(&clip, pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, &label, font, color, 0.08);
                            } else {
                                widgets::tracked_text(&clip, pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, &label, font, color, 0.08);
                            }
                            let resp = ui.interact(r, ui.id().with(("hdr", c.id())), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, rounding, pal.raise(0.04));
                            }
                            if resp.clicked() {
                                sort_click = Some(c.base());
                            }
                        });
                    }
                    header.col(|ui| {
                        let r = ui.max_rect();
                        let rounding = egui::Rounding { ne: 7.0, ..Default::default() };
                        ui.painter().rect_filled(r, rounding, pal.panel);
                        ui.painter().line_segment([pos2(r.left(), r.bottom() - 0.5), pos2(r.right(), r.bottom() - 0.5)], Stroke::new(1.0_f32, pal.panel_stroke));
                    });
                })
                .body(|body| {
                    body.heterogeneous_rows(heights.iter().copied(), |mut row| match &entries[row.index()] {
                        Entry::Group(name, n) => {
                            for ci in 0..=cols.len() {
                                row.col(|ui| {
                                    let r = ui.max_rect();
                                    let p = ui.painter();
                                    p.rect_filled(r, 0.0, with_alpha(pal.text, 0.02));
                                    p.line_segment([pos2(r.left(), r.top() + 0.5), pos2(r.right(), r.top() + 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
                                    if ci == 0 {
                                        let clip = p.with_clip_rect(r.expand2(vec2(600.0, 0.0)));
                                        widgets::tracked_text(&clip, pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, &format!("{} \u{b7} {n}", name.to_uppercase()), theme::semibold(11.0), pal.text_faint, 0.12);
                                    }
                                });
                            }
                        }
                        Entry::More { key, n } => {
                            for ci in 0..=cols.len() {
                                row.col(|ui| {
                                    let r = ui.max_rect();
                                    ui.painter().rect_filled(r, 0.0, with_alpha(pal.text, 0.025));
                                    if ci == 0 {
                                        ui.painter().text(pos2(r.left() + 56.0, r.center().y), Align2::LEFT_CENTER, format!("+ {n} more processes"), theme::ui(13.0), pal.text_faint);
                                    }
                                });
                            }
                            let resp = row.response();
                            if resp.clicked() {
                                more = Some(*key);
                            }
                            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        }
                        Entry::Row { idx, depth, kids, open } => {
                            let r = &rows[*idx];
                            let key = (r.pid, r.create_time);
                            let is_sel = selected == Some(key);
                            let agg = built.agg.get(idx).copied();
                            let group_spark;
                            let trend: Option<(&Series, &Series)> = if agg.is_some() {
                                let mut members = Vec::new();
                                members_of(*idx, &groups.children, &mut members, 0);
                                group_spark = summed_spark(rows, &members, spark);
                                Some((&group_spark.0, &group_spark.1))
                            } else {
                                spark.get(&key).map(|s| (&s.0, &s.1))
                            };
                            let cx = CellCtx { max_mem, max_io, selected: is_sel, depth: *depth, kids: *kids, open: *open, size: built.size.get(idx).copied().unwrap_or(1), agg, logical, per_core, trend, mem_color, st };
                            for (ci, c) in cols.iter().enumerate() {
                                row.col(|ui| {
                                    if cell_ui(ui, r, *c, &pal, cx, icons, ci == 0, false) {
                                        toggle = Some(key);
                                    }
                                });
                            }
                            row.col(|ui| {
                                cell_ui(ui, r, PCol::Data(Column::Name), &pal, CellCtx { kids: 0, ..cx }, icons, false, true);
                            });
                            let resp = row.response();
                            if resp.clicked() || resp.secondary_clicked() {
                                clicked = Some(key);
                            }
                            if resp.double_clicked() {
                                if *kids > 0 {
                                    toggle = Some(key);
                                } else {
                                    open_details = Some(key);
                                }
                            }
                            resp.context_menu(|ui| {
                                if let Some(a) = action_menu(ui, r) {
                                    menu_pick = Some((a, r.clone()));
                                }
                            });
                        }
                    });
                });
        });
        if built.entries.is_empty() {
            kit::empty_state(&p, table_rect.shrink2(vec2(0.0, 40.0)), &pal, "No matching processes");
        }
        p.rect_stroke(table_rect, 8.0, Stroke::new(1.0_f32, pal.grid));
        for (id, w) in resized {
            self.settings.window.process_table_column_widths.insert(id, w.round() as u32);
            self.settings_dirty = true;
        }
        if let Some(c) = sort_click {
            self.sort = if col == c { (c, !desc) } else { (c, c.numeric()) };
        }
        if let Some(k) = more {
            self.proc.show_all.insert(k);
        }
        if let Some(k) = toggle {
            if !self.proc.expanded.remove(&k) {
                self.proc.expanded.insert(k);
            } else {
                self.proc.show_all.remove(&k);
            }
        } else if let Some(k) = clicked {
            self.selected = Some(k);
        }
        if let Some(k) = open_details {
            self.selected = Some(k);
            self.proc.details = true;
            self.proc.details_tab = 0;
        }
        if let Some((pick, r)) = menu_pick {
            self.selected = Some((r.pid, r.create_time));
            match pick {
                MenuPick::Do(a) => requested = Some((a, r)),
                MenuPick::Details => {
                    self.proc.details = true;
                    self.proc.details_tab = 0;
                }
            }
        }

        // ---- selection bar and keys
        if let (Some(r), Some((agg, n))) = (&sel_row, sel_info) {
            kit::floating_bar(ui, area, "proc-selection", |ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                ui.label(RichText::new(format!("{} selected", r.name)).font(theme::semibold(13.0)).color(pal.text));
                let info = format!("{n} process{} \u{b7} {:.1}% CPU \u{b7} {}", if n == 1 { "" } else { "es" }, self.shown_cpu(agg.cpu, logical), format_bytes(agg.mem as f64));
                ui.label(RichText::new(info).font(theme::mono(12.0)).color(pal.text_dim));
                let (label, action) = if r.status == Status::Suspended { ("Resume", Action::Resume) } else { ("Suspend", Action::Suspend) };
                if Button::new(label).size(ButtonSize::Medium).show(ui, &pal).clicked() {
                    requested = Some((action, r.clone()));
                }
                if Button::primary("End task").size(ButtonSize::Medium).show(ui, &pal).clicked() {
                    requested = Some((Action::EndTask, r.clone()));
                }
            });
            let ctx = ui.ctx();
            if self.pending.is_none() && !ctx.wants_keyboard_input() {
                if key_once(ctx, egui::Key::Delete) {
                    let action = if ctx.input(|i| i.modifiers.shift) { Action::EndTree } else { Action::EndTask };
                    requested = Some((action, r.clone()));
                } else if key_once(ctx, egui::Key::Enter) {
                    self.proc.details = true;
                    self.proc.details_tab = 0;
                }
            }
        }
        if let Some((a, r)) = requested {
            self.request(a, r);
        }
    }

    // ------------------------------------------------------------------ details view

    /// The process details view: header with the actions, then the Summary or Network tab.
    fn details_view(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>, r: &ProcessRow, requested: &mut Option<(Action, ProcessRow)>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let ctx = ui.ctx().clone();
        if self.pending.is_none() && !ctx.wants_keyboard_input() && key_once(&ctx, egui::Key::Escape) {
            self.proc.details = false;
            return;
        }
        // Connections of this process, refreshed while the view is open.
        self.pro.conns.poll();
        if self.pro.conns.stale(Duration::from_secs(3)) {
            self.pro.conns.start(crate::pro::connections::read);
        }
        let mut conns: Vec<Conn> = self.pro.conns.ok().map(|v| v.iter().filter(|c| c.pid == r.pid).cloned().collect()).unwrap_or_default();
        conns.sort_by_key(|c| (!c.is_established(), !c.is_listen(), c.proto));
        let loading_conns = self.pro.conns.ok().is_none();

        let rows = fd.rows;
        let by_pid: HashMap<u32, usize> = rows.iter().enumerate().map(|(i, x)| (x.pid, i)).collect();
        let me = by_pid.get(&r.pid).copied().filter(|i| rows[*i].create_time == r.create_time);
        let (group_name, group_n) = match me {
            Some(i) => app_group(rows, &by_pid, i),
            None => (r.name.clone(), 1),
        };
        let parent = me.and_then(|i| parent_in(rows, &by_pid, i)).map(|p| (rows[p].name.clone(), (rows[p].pid, rows[p].create_time)));

        let full = ui.max_rect();
        let p = ui.painter().clone();
        let x0 = full.left() + 12.0;
        let mut y = full.top() + 6.0;

        // Breadcrumb.
        let crumb_font = theme::ui(13.0);
        let back = Rect::from_min_size(pos2(x0, y), vec2(widgets::text_width(&p, "\u{2039} Processes", crumb_font.clone()), 20.0));
        let back_resp = ui.interact(back, ui.id().with("proc-back"), Sense::click());
        p.text(back.left_center(), Align2::LEFT_CENTER, "\u{2039} Processes", crumb_font.clone(), if back_resp.hovered() { pal.text } else { pal.text_faint });
        let rest = p.text(pos2(back.right() + 8.0, y + 10.0), Align2::LEFT_CENTER, format!("/ {group_name} /"), crumb_font.clone(), pal.text_faint);
        p.text(pos2(rest.right() + 8.0, y + 10.0), Align2::LEFT_CENTER, &r.name, crumb_font, pal.text);
        if back_resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.proc.details = false;
        }
        y += 34.0;

        // Title block.
        let tex = self.icons.for_process(r.pid, r.path.as_deref());
        let icon = Rect::from_min_size(pos2(x0, y), vec2(64.0, 64.0));
        widgets::icon_box(&p, icon, &pal, tex, r.name.chars().next().unwrap_or('?'));
        let tx = icon.right() + 22.0;
        let buttons = [
            (r.path.is_some().then_some("Open file location"), ButtonKind::Secondary, Some(Icon::Folder), Action::OpenFileLocation),
            (Some(if r.efficiency { "Efficiency mode: off" } else { "Efficiency mode" }), ButtonKind::Secondary, None, Action::Efficiency(!r.efficiency)),
            (Some(if r.status == Status::Suspended { "Resume" } else { "Suspend" }), ButtonKind::Secondary, None, if r.status == Status::Suspended { Action::Resume } else { Action::Suspend }),
            (Some("End task"), ButtonKind::Primary, None, Action::EndTask),
            (Some("End task tree\u{2026}"), ButtonKind::Danger, None, Action::EndTree),
        ];
        let btn_w: f32 = buttons.iter().filter_map(|b| b.0).map(|l| widgets::text_width(&p, l, theme::semibold(13.0)) + 28.0 + 8.0).sum::<f32>() + if r.path.is_some() { 22.0 } else { 0.0 };
        let inline = full.width() - (tx - full.left()) - btn_w > 380.0;
        let title_w = if inline { full.right() - btn_w - tx - 16.0 } else { full.right() - tx };
        let name_r = widgets::text_fit(&p, pos2(tx, y - 4.0), Align2::LEFT_TOP, &r.name, theme::light(36.0), pal.text, title_w - 120.0);
        p.text(pos2(name_r.right() + 12.0, name_r.bottom() - 8.0), Align2::LEFT_BOTTOM, format!("PID {}", r.pid), theme::mono(16.0), pal.text_faint);
        widgets::text_fit(&p, pos2(tx, y + 42.0), Align2::LEFT_TOP, r.description.as_deref().unwrap_or("No description"), theme::ui(15.0), pal.text_dim, title_w);
        let (btn_rect, header_bottom) = if inline {
            (Rect::from_min_max(pos2(full.right() - btn_w - 4.0, y + 14.0), pos2(full.right(), y + 50.0)), y + 64.0)
        } else {
            (Rect::from_min_max(pos2(x0, y + 76.0), pos2(full.right(), y + 112.0)), y + 112.0)
        };
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(btn_rect).layout(if inline { Layout::right_to_left(Align::Center) } else { Layout::left_to_right(Align::Center) }), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let shown = buttons.iter().filter(|b| b.0.is_some());
            let mut add = |ui: &mut egui::Ui, b: &(Option<&str>, ButtonKind, Option<Icon>, Action)| {
                if Button::new(b.0.unwrap_or("")).kind(b.1).icon(b.2).show(ui, &pal).clicked() {
                    *requested = Some((b.3, r.clone()));
                }
            };
            if inline {
                for b in shown.rev() {
                    add(ui, b);
                }
            } else {
                for b in shown {
                    add(ui, b);
                }
            }
        });
        y = header_bottom + 18.0;

        // Tabs.
        let tabs_rect = Rect::from_min_size(pos2(x0, y), vec2(full.right() - x0, 36.0));
        let items: [(&str, Option<usize>); 2] = [("Summary", None), ("Network", Some(conns.len()))];
        if let Some(t) = widgets::tab_bar(ui, tabs_rect, &pal, &items, self.proc.details_tab, None) {
            self.proc.details_tab = t;
        }
        let area = Rect::from_min_max(pos2(x0, tabs_rect.bottom() + 16.0), full.max);

        if self.proc.details_tab == 1 {
            self.network_tab(ui, &pal, area, r, &conns, loading_conns);
            return;
        }

        // Summary: resources and connections at the left, facts at the right.
        let interval = self.settings.update_speed().interval();
        let logical = fd.snap.cpu.logical_count;
        let mut go_parent: Option<(u32, i64)> = None;
        let mut go_network = false;
        let facts = self.facts(r, &group_name, group_n, parent.as_ref().map(|p| p.0.as_str()), logical, &st);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(area), |ui| {
            egui::ScrollArea::vertical().id_salt("proc-details-scroll").auto_shrink([false, false]).show(ui, |ui| {
                let w = ui.available_width();
                let p = ui.painter().clone();
                let two_col = w >= 860.0;
                let aside_w = if two_col { ((w - 16.0) * 0.4).floor() } else { w };
                let main_w = if two_col { w - 16.0 - aside_w } else { w };
                // Heights.
                let res_h = 156.0;
                let shown_conns = conns.len().min(6);
                let net_h = 18.0 + 22.0 + 30.0 + shown_conns.max(1) as f32 * 34.0 + 16.0;
                let path_text = r.path.clone().unwrap_or_else(|| if r.pid <= 4 { "\u{2014}".into() } else { "Access denied".into() });
                let cmd_text = r.command_line.clone().unwrap_or_else(|| if r.pid <= 4 { "\u{2014}".into() } else { "Access denied".into() });
                let code_h = |t: &str| 20.0 + (p.layout(t.to_string(), theme::mono(12.0), pal.text_code(), (aside_w - 44.0 - 52.0).max(20.0)).size().y + 20.0).clamp(40.0, 150.0);
                let (path_h, cmd_h) = (code_h(&path_text), code_h(&cmd_text));
                let kv_h = facts.len() as f32 * 28.0;
                let aside_h = 18.0 + 22.0 + kv_h + 16.0 + path_h + 14.0 + cmd_h + 18.0;
                let main_h = res_h + 16.0 + net_h;
                let total = if two_col { main_h.max(aside_h) } else { main_h + 16.0 + aside_h };
                let (c, _) = ui.allocate_exact_size(vec2(w, total + 8.0), Sense::hover());
                let main = Rect::from_min_size(c.min, vec2(main_w, main_h));
                let aside = if two_col { Rect::from_min_size(pos2(main.right() + 16.0, c.top()), vec2(aside_w, aside_h)) } else { Rect::from_min_size(pos2(c.left(), main.bottom() + 16.0), vec2(aside_w, aside_h)) };

                // Resources.
                let res = Rect::from_min_size(main.min, vec2(main_w, res_h));
                let n = self.proc.sel_cpu.as_ref().map(|s| s.values.len()).unwrap_or(0);
                let inner = kit::section_card(&p, res, &pal, &format!("Resources \u{b7} last {}", samples_text(n.max(1), interval)), "");
                let cols = kit::hsplit(inner, &[1.0; 4], 20.0);
                let cpu_txt = format!("{:.1}", self.shown_cpu(r.cpu_pct, logical));
                let mem_txt = format_bytes(r.private_working_set as f64);
                let disk_txt = format_rate(r.read_bps + r.write_bps);
                let gpu_txt = format!("{:.0}", r.gpu_pct);
                let mem_range = self.proc.sel_mem.as_ref().map(|s| auto_range(s, s.max() * 0.02)).unwrap_or((0.0, 1.0));
                let cpu_hi = self.proc.sel_cpu.as_ref().map(|s| (s.max() * 1.25).max(2.0)).unwrap_or(2.0);
                let disk_hi = self.proc.sel_disk.as_ref().map(|s| s.max().max(64.0 * 1024.0)).unwrap_or(1.0);
                let gpu_hi = self.proc.sel_gpu.as_ref().map(|s| (s.max() * 1.25).max(5.0)).unwrap_or(5.0);
                let cells = [
                    ResColumn { label: "CPU", value: &cpu_txt, unit: "%", series: self.proc.sel_cpu.as_ref(), color: fd.metric("cpu"), range: (0.0, cpu_hi) },
                    ResColumn { label: "Memory", value: split_unit(&mem_txt).0, unit: split_unit(&mem_txt).1, series: self.proc.sel_mem.as_ref(), color: fd.metric("memory"), range: mem_range },
                    ResColumn { label: "Disk", value: split_unit(&disk_txt).0, unit: split_unit(&disk_txt).1, series: self.proc.sel_disk.as_ref(), color: fd.metric("disk"), range: (0.0, disk_hi) },
                    ResColumn { label: "GPU", value: &gpu_txt, unit: "%", series: self.proc.sel_gpu.as_ref(), color: fd.metric("gpu"), range: (0.0, gpu_hi) },
                ];
                for (rc, ResColumn { label, value, unit, series, color, range }) in cols.iter().zip(cells) {
                    widgets::tracked_text(&p, rc.left_top(), Align2::LEFT_TOP, &label.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.14);
                    let vr = p.text(pos2(rc.left(), rc.top() + 20.0), Align2::LEFT_TOP, value, theme::light(26.0), pal.text);
                    p.text(pos2(vr.right() + 3.0, vr.bottom() - 5.0), Align2::LEFT_BOTTOM, if unit.is_empty() { "" } else { unit }, theme::ui(14.0), pal.text_faint);
                    if let Some(s) = series {
                        widgets::sparkline(&p, Rect::from_min_size(pos2(rc.left(), rc.top() + 58.0), vec2(rc.width(), 36.0)), &[(s, color, range)], 0.12, false);
                    }
                }

                // Connections.
                let net = Rect::from_min_size(pos2(main.left(), res.bottom() + 16.0), vec2(main_w, net_h));
                let inner = kit::section_card(&p, net, &pal, "Network connections", "");
                let link = p.text(pos2(net.right() - 22.0, net.top() + 14.0), Align2::RIGHT_TOP, "Network tab \u{2192}", theme::ui(13.0), st.info);
                let link_resp = ui.interact(link.expand(4.0), ui.id().with("proc-net-link"), Sense::click());
                if link_resp.hovered() {
                    p.text(link.right_top(), Align2::RIGHT_TOP, "Network tab \u{2192}", theme::ui(13.0), theme::lighten(st.info, 0.25));
                }
                if link_resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    go_network = true;
                }
                let xs = [inner.left(), inner.left() + inner.width() * 0.44, inner.left() + inner.width() * 0.58, inner.left() + inner.width() * 0.76];
                for (x, h) in xs.iter().zip(["Remote", "Port", "Protocol", "State"]) {
                    widgets::tracked_text(&p, pos2(*x, inner.top() + 2.0), Align2::LEFT_TOP, &h.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.08);
                }
                if conns.is_empty() {
                    let text = if loading_conns { "Reading connection tables\u{2026}" } else { "No network connections" };
                    p.text(pos2(inner.left(), inner.top() + 30.0 + 17.0), Align2::LEFT_CENTER, text, theme::ui(13.0), pal.text_faint);
                }
                for (k, cn) in conns.iter().take(6).enumerate() {
                    let ry = inner.top() + 30.0 + k as f32 * 34.0;
                    p.line_segment([pos2(inner.left(), ry), pos2(inner.right(), ry)], Stroke::new(1.0_f32, pal.row_rule()));
                    let cy = ry + 17.0;
                    let (ip, port) = match cn.remote {
                        Some(a) if !a.is_unspecified() => (a, cn.remote_port),
                        _ => (cn.local, cn.local_port),
                    };
                    let clip = p.with_clip_rect(Rect::from_min_max(pos2(xs[0], ry), pos2(xs[1] - 8.0, ry + 34.0)));
                    clip.text(pos2(xs[0], cy), Align2::LEFT_CENTER, ip.to_string(), theme::mono(12.0), pal.text_table());
                    p.text(pos2(xs[1], cy), Align2::LEFT_CENTER, port.to_string(), theme::mono(12.0), pal.text_table());
                    p.text(pos2(xs[2], cy), Align2::LEFT_CENTER, proto_text(cn), theme::mono(12.0), pal.text_table());
                    let (color, dot) = if cn.is_established() {
                        (st.ok, Dot::Full)
                    } else if cn.is_listen() || cn.proto == Proto::Udp {
                        (pal.text_faint, Dot::Hollow)
                    } else {
                        (st.warn, Dot::Half)
                    };
                    widgets::status_text(&p, pos2(xs[3], cy), cn.state_name(), color, dot);
                }

                // Facts.
                let inner = kit::section_card(&p, aside, &pal, "About this process", "");
                let items: Vec<(String, String, bool, Option<Color32>)> = facts.iter().map(|f| (f.0.to_string(), f.1.clone(), f.2, f.3)).collect();
                let bottom = kit::kv_grid(&p, Rect::from_min_size(inner.min, vec2(inner.width(), kv_h + 1.0)), &pal, &items, 104.0, 28.0);
                if let (Some((name, key)), Some(row)) = (&parent, facts.iter().position(|f| f.0 == "Parent")) {
                    let label_w = widgets::text_width(&p, &format!("{name} ({})", r.parent_pid), theme::mono(12.0));
                    let hit = Rect::from_min_size(pos2(inner.left() + 104.0, inner.top() + row as f32 * 28.0), vec2(label_w, 28.0));
                    let resp = ui.interact(hit, ui.id().with("proc-parent"), Sense::click());
                    if resp.hovered() {
                        let y = hit.center().y + 8.0;
                        p.line_segment([pos2(hit.left(), y), pos2(hit.right(), y)], Stroke::new(1.0_f32, st.info));
                    }
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        go_parent = Some(*key);
                    }
                }
                let top = bottom + 16.0;
                let _ = kit::code_box(ui, Rect::from_min_size(pos2(inner.left(), top), vec2(inner.width(), path_h)), &pal, "Path", &path_text);
                let _ = kit::code_box(ui, Rect::from_min_size(pos2(inner.left(), top + path_h + 14.0), vec2(inner.width(), cmd_h)), &pal, "Command line", &cmd_text);
            });
        });
        if go_network {
            self.proc.details_tab = 1;
        }
        if let Some(k) = go_parent {
            self.selected = Some(k);
        }
    }

    /// Rows of "About this process": label, text, mono, colour.
    fn facts(&self, r: &ProcessRow, group: &str, group_n: usize, parent: Option<&str>, logical: usize, st: &theme::Status) -> Vec<(&'static str, String, bool, Option<Color32>)> {
        let dash = || "\u{2014}".to_string();
        let started = started_text(r.create_time, SystemTime::now());
        let kind = if r.has_window { "App" } else { "Background" };
        let ty = match r.user.as_deref() {
            Some(u) if !u.is_empty() => format!("{kind} \u{b7} runs as {u}"),
            _ => kind.to_string(),
        };
        let parent_text = match (r.parent_pid, parent) {
            (0, _) => dash(),
            (pid, Some(name)) => format!("{name} ({pid})"),
            (pid, None) => format!("({pid})"),
        };
        vec![
            ("App", format!("{group} \u{b7} {group_n} process{}", if group_n == 1 { "" } else { "es" }), false, None),
            ("Parent", parent_text, true, parent.map(|_| st.info)),
            ("Started", started, true, None),
            ("Type", ty, false, None),
            ("Priority", priority_text(r.base_priority), false, None),
            ("Threads", format!("{} \u{b7} {} handles", r.threads, r.handles), true, None),
            ("Status", r.status.label().to_string(), false, None),
            ("Architecture", if r.arch.is_empty() { dash() } else { r.arch.to_string() }, false, None),
            ("CPU", format!("{:.1}% \u{b7} {}", self.shown_cpu(r.cpu_pct, logical), format_cpu_time(r.cpu_time)), true, None),
            ("Working set", format!("{} (peak {})", format_bytes(r.working_set as f64), format_bytes(r.peak_working_set as f64)), true, None),
            ("Commit", format_bytes(r.commit as f64), true, None),
            ("Page faults", r.page_faults.to_string(), true, None),
            ("Session", r.session_id.to_string(), true, None),
        ]
    }

    /// The Network tab: the process's connections in a table.
    fn network_tab(&mut self, ui: &mut egui::Ui, pal: &Palette, area: Rect, r: &ProcessRow, conns: &[Conn], loading: bool) {
        if conns.is_empty() {
            kit::empty_state(ui.painter(), area.shrink2(vec2(0.0, 40.0)), pal, if loading { "Reading connection tables\u{2026}" } else { "No network connections for this process" });
            return;
        }
        let st = theme::status(pal);
        let cols = [Col::text("Protocol", 100.0), Col::text("Local address", 250.0), Col::text("Remote address", 270.0), Col::text("State", 140.0), Col::text("Scope", 120.0)];
        let mut order: Vec<usize> = (0..conns.len()).collect();
        self.proc.conns_table.sort(conns, &mut order, |a, b, col| match col {
            0 => a.proto.cmp(&b.proto),
            1 => a.local.cmp(&b.local).then(a.local_port.cmp(&b.local_port)),
            2 => a.remote.cmp(&b.remote).then(a.remote_port.cmp(&b.remote_port)),
            3 => a.state_name().cmp(b.state_name()),
            _ => a.scope().cmp(b.scope()),
        });
        let key = |i: usize| format!("{:?}{}{}{}", conns[i].proto, conns[i].local_text(), conns[i].remote_text(), r.pid);
        let cell = |i: usize, col: usize| -> TableCell {
            let c = &conns[i];
            match col {
                0 => TableCell::new(proto_text(c)).dim(pal),
                1 => TableCell::new(c.local_text()).mono(),
                2 => TableCell::new(c.remote_text()).mono(),
                3 => {
                    let color = if c.is_established() {
                        st.ok
                    } else if c.is_listen() {
                        st.info
                    } else if c.proto == Proto::Udp {
                        theme::PURPLE
                    } else {
                        st.warn
                    };
                    TableCell::new(c.state_name()).dot(color)
                }
                _ => {
                    let s = c.scope();
                    TableCell::new(s).color(if s == "Internet" { st.info } else { pal.text_dim })
                }
            }
        };
        let icons = &mut self.icons;
        table::show(ui, area, "proc-conn-table", pal, &cols, &order, &mut self.proc.conns_table, &key, &cell, icons);
    }

    pub fn column_visible(&self, c: Column) -> bool {
        if c == Column::Name {
            return true;
        }
        let hidden = &self.settings.window.process_table_hidden_columns;
        if default_hidden(c) {
            hidden.contains(&format!("+{}", c.id()))
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
        if default_hidden(c) && visible {
            hidden.push(plus);
        } else if !default_hidden(c) && !visible {
            hidden.push(id);
        }
        self.settings_dirty = true;
    }
}

/// One column of the Resources card in the details view.
struct ResColumn<'a> {
    label: &'static str,
    value: &'a str,
    unit: &'a str,
    series: Option<&'a Series>,
    color: Color32,
    range: (f32, f32),
}

/// "TCP", "UDPv6", ...
fn proto_text(c: &Conn) -> &'static str {
    match (c.proto, c.local.is_ipv6()) {
        (Proto::Tcp, false) => "TCP",
        (Proto::Tcp, true) => "TCPv6",
        (Proto::Udp, false) => "UDP",
        (Proto::Udp, true) => "UDPv6",
    }
}

/// The app a process belongs to (the windowed ancestor, or the top of its tree) and how many
/// processes it has.
fn app_group(rows: &[ProcessRow], by_pid: &HashMap<u32, usize>, i: usize) -> (String, usize) {
    let mut root = i;
    for _ in 0..32 {
        if rows[root].has_window {
            break;
        }
        match parent_in(rows, by_pid, root) {
            Some(p) => root = p,
            None => break,
        }
    }
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, r) in rows.iter().enumerate() {
        if !r.has_window {
            if let Some(p) = parent_in(rows, by_pid, k) {
                children.entry(p).or_default().push(k);
            }
        }
    }
    let mut members = Vec::new();
    members_of(root, &children, &mut members, 0);
    (app_label(&rows[root]), members.len())
}

/// Priority class of a base priority value, else the number.
fn priority_text(base: i32) -> String {
    let class = match base {
        4 => Some(Priority::Idle),
        6 => Some(Priority::BelowNormal),
        8 => Some(Priority::Normal),
        10 => Some(Priority::AboveNormal),
        13 => Some(Priority::High),
        24 => Some(Priority::Realtime),
        _ => None,
    };
    class.map(|p| p.label().to_string()).unwrap_or_else(|| base.to_string())
}

const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// "5 min ago", "5 h 02 m ago", "3 d 4 h ago".
fn ago_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h {:02} m ago", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h ago", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// "Today 09:14 \u{b7} 5 h 02 m ago" from a creation FILETIME (a dash when unknown).
fn started_text(create_time: i64, now: SystemTime) -> String {
    let dash = || "\u{2014}".to_string();
    let Some(ticks) = create_time.checked_sub(FILETIME_UNIX).filter(|t| *t > 0) else { return dash() };
    let Some(started) = UNIX_EPOCH.checked_add(Duration::from_nanos((ticks as u64).saturating_mul(100))) else { return dash() };
    let stamp = crate::pro::diskspace::local_stamp(started);
    if stamp.len() < 16 || !stamp.is_char_boundary(10) {
        return dash();
    }
    let (date, time) = stamp.split_at(10);
    let day_of = |t: SystemTime| crate::pro::diskspace::local_stamp(t).get(..10).map(str::to_string);
    let day = if day_of(now).as_deref() == Some(date) {
        "Today".to_string()
    } else if now.checked_sub(Duration::from_secs(86_400)).and_then(day_of).as_deref() == Some(date) {
        "Yesterday".to_string()
    } else {
        date.to_string()
    };
    match now.duration_since(started) {
        Ok(d) => format!("{day} {} \u{b7} {}", time.trim(), ago_text(d.as_secs())),
        Err(_) => format!("{day} {}", time.trim()),
    }
}

/// A column of the process table: a data column or a trend column next to CPU / Memory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PCol {
    Data(Column),
    CpuTrend,
    MemTrend,
}

impl PCol {
    fn id(self) -> &'static str {
        match self {
            PCol::Data(c) => c.id(),
            PCol::CpuTrend => "cpuTrend",
            PCol::MemTrend => "memoryTrend",
        }
    }
    /// The data column a click on the header sorts by.
    fn base(self) -> Column {
        match self {
            PCol::Data(c) => c,
            PCol::CpuTrend => Column::Cpu,
            PCol::MemTrend => Column::Memory,
        }
    }
    fn is_trend(self) -> bool {
        !matches!(self, PCol::Data(_))
    }
    fn right(self) -> bool {
        matches!(self, PCol::Data(c) if c.numeric() && c != Column::Pid)
    }
    fn title(self, span: &str) -> String {
        match self {
            PCol::Data(c) => c.title().to_string(),
            PCol::CpuTrend => format!("CPU \u{b7} {span}"),
            PCol::MemTrend => format!("Mem \u{b7} {span}"),
        }
    }
    fn width(self, saved: &std::collections::BTreeMap<String, u32>) -> f32 {
        match self {
            PCol::CpuTrend | PCol::MemTrend => SPARK_W + 20.0,
            PCol::Data(c) => saved.get(c.id()).map(|w| *w as f32).unwrap_or(match c {
                Column::Name => 300.0,
                Column::Pid => 64.0,
                Column::Status => 140.0,
                Column::User => 130.0,
                Column::Cpu | Column::Gpu => 80.0,
                Column::Memory | Column::Disk | Column::DiskRead | Column::DiskWrite => 100.0,
                Column::Description => 260.0,
                Column::Path | Column::CommandLine => 320.0,
                _ => c.default_width().max(80.0),
            }),
        }
    }
}

#[derive(Clone, Copy)]
struct CellCtx<'a> {
    max_mem: f32,
    max_io: f32,
    selected: bool,
    depth: usize,
    kids: usize,
    open: bool,
    /// Processes in the group (1 for a plain row).
    size: usize,
    agg: Option<Agg>,
    logical: usize,
    per_core: bool,
    trend: Option<(&'a Series, &'a Series)>,
    mem_color: Color32,
    st: theme::Status,
}

/// Draws one cell. Returns true when the tree chevron was clicked. `first` / `filler` tell the
/// selection frame which edges to close.
#[allow(clippy::too_many_arguments)]
fn cell_ui(ui: &mut egui::Ui, r: &ProcessRow, pc: PCol, pal: &Palette, cx: CellCtx<'_>, icons: &mut IconCache, first: bool, filler: bool) -> bool {
    let rect = ui.max_rect();
    let a = cx.agg.unwrap_or(Agg { cpu: r.cpu_pct, mem: r.private_working_set, read: r.read_bps, write: r.write_bps, gpu: r.gpu_pct });
    let cpu = if cx.per_core { a.cpu * cx.logical.max(1) as f32 } else { a.cpu };
    let c = pc.base();
    // Heat-map intensity for the activity columns.
    let heat = if filler || pc.is_trend() {
        0.0
    } else {
        match c {
            Column::Cpu => (a.cpu / 8.0).clamp(0.0, 1.0),
            Column::Memory => (a.mem as f32 / cx.max_mem).clamp(0.0, 1.0).sqrt() * 0.8,
            Column::DiskRead => (a.read as f32 / cx.max_io).clamp(0.0, 1.0).sqrt(),
            Column::DiskWrite => (a.write as f32 / cx.max_io).clamp(0.0, 1.0).sqrt(),
            Column::Disk => ((a.read + a.write) as f32 / (2.0 * cx.max_io)).clamp(0.0, 1.0).sqrt(),
            Column::Gpu => (a.gpu / 15.0).clamp(0.0, 1.0),
            _ => 0.0,
        }
    };
    {
        let p = ui.painter();
        if cx.selected {
            p.rect_filled(rect, 0.0, pal.selected_row());
            let s = Stroke::new(1.0_f32, pal.accent);
            p.line_segment([pos2(rect.left(), rect.top() + 0.5), pos2(rect.right(), rect.top() + 0.5)], s);
            p.line_segment([pos2(rect.left(), rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)], s);
            if first {
                p.line_segment([pos2(rect.left() + 0.5, rect.top()), pos2(rect.left() + 0.5, rect.bottom())], s);
            }
            if filler {
                p.line_segment([pos2(rect.right() - 0.5, rect.top()), pos2(rect.right() - 0.5, rect.bottom())], s);
            }
        } else {
            if cx.depth > 0 {
                p.rect_filled(rect, 0.0, with_alpha(pal.text, 0.025));
            } else {
                p.line_segment([pos2(rect.left(), rect.top() + 0.5), pos2(rect.right(), rect.top() + 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
            }
            if heat > 0.03 {
                p.rect_filled(rect, 0.0, with_alpha(pal.accent, heat * 0.18));
            }
        }
    }
    if filler {
        return false;
    }
    let child = cx.depth > 0;
    let mono = c.numeric() && !pc.is_trend();
    let font = if mono { theme::mono(12.0) } else { theme::ui(13.0) };
    let dimmed = matches!(c, Column::User | Column::Priority | Column::Architecture | Column::CommandLine | Column::Path | Column::Description);
    let base = if c == Column::Pid {
        pal.text_faint
    } else if dimmed {
        if child {
            pal.text_faint
        } else {
            pal.text_dim
        }
    } else if mono {
        pal.text_table()
    } else {
        pal.text
    };
    let hot = match c {
        Column::Cpu => a.cpu >= 10.0,
        Column::Memory => a.mem >= 1 << 30,
        Column::DiskRead => a.read >= 10.0 * 1048576.0,
        Column::DiskWrite => a.write >= 10.0 * 1048576.0,
        Column::Disk => a.read + a.write >= 10.0 * 1048576.0,
        Column::Gpu => a.gpu >= 20.0,
        _ => false,
    };
    let color = if hot { cx.st.yellow } else { base };
    let cy = rect.center().y;
    let mut toggled = false;
    match pc {
        PCol::CpuTrend | PCol::MemTrend => {
            let area = Rect::from_min_size(pos2(rect.left() + 10.0, cy - SPARK_H / 2.0), vec2(SPARK_W, SPARK_H));
            let p = ui.painter().with_clip_rect(rect);
            if r.status == Status::Suspended {
                p.extend(egui::Shape::dashed_line(&[pos2(area.left(), cy), pos2(area.right(), cy)], Stroke::new(1.2_f32, pal.raise(0.3)), 3.0, 3.0));
            } else if let Some((cpu_s, mem_s)) = cx.trend {
                let fill = if cx.depth == 0 { 0.16 } else { 0.0 };
                if pc == PCol::CpuTrend {
                    let color = if r.status == Status::NotResponding { cx.st.danger } else { pal.accent };
                    widgets::sparkline(&p, area, &[(cpu_s, color, (0.0, (cpu_s.max() * 1.2).max(2.0)))], fill, false);
                } else {
                    widgets::sparkline(&p, area, &[(mem_s, cx.mem_color, auto_range(mem_s, mem_s.max() * 0.02))], fill, false);
                }
            }
        }
        PCol::Data(Column::Name) => {
            let (icon_x, name_x);
            if child {
                let x = rect.left() + 56.0 + (cx.depth - 1) as f32 * 16.0;
                if cx.kids > 0 {
                    toggled = chevron(ui, r, pal, pos2(x + 2.0, cy), cx.open);
                    name_x = x + 18.0;
                } else {
                    name_x = x;
                }
                icon_x = None;
            } else {
                if cx.kids > 0 {
                    toggled = chevron(ui, r, pal, pos2(rect.left() + 16.0, cy), cx.open);
                }
                icon_x = Some(rect.left() + 28.0);
                name_x = rect.left() + 60.0;
            }
            let p = ui.painter().clone();
            if let Some(ix) = icon_x {
                let tex = icons.for_process(r.pid, r.path.as_deref());
                widgets::icon_box(&p, Rect::from_min_size(pos2(ix, cy - 12.0), vec2(24.0, 24.0)), pal, tex, r.name.chars().next().unwrap_or('?'));
            }
            let windows = !child && is_windows_process(r);
            let count = (cx.size > 1).then(|| cx.size.to_string());
            let eco = r.efficiency;
            let tail_w = count.as_ref().map(|c| widgets::text_width(&p, c, theme::mono(11.0)) + 8.0).unwrap_or(0.0) + if windows { widgets::badge_width(&p, "WINDOWS") + 8.0 } else { 0.0 } + if eco { 30.0 } else { 0.0 };
            let (nf, nc) = if child { (theme::ui(13.0), pal.text_dim) } else { (theme::semibold(13.0), pal.text) };
            let nr = widgets::text_fit(&p, pos2(name_x, cy), Align2::LEFT_CENTER, &r.name, nf, nc, rect.right() - name_x - 10.0 - tail_w);
            let mut x = nr.right() + 8.0;
            if let Some(c) = count {
                let tr = p.text(pos2(x, cy), Align2::LEFT_CENTER, &c, theme::mono(11.0), pal.text_faint);
                x = tr.right() + 8.0;
            }
            if windows {
                let br = widgets::badge(&p.with_clip_rect(rect), pos2(x, cy), "WINDOWS", pal);
                x = br.right() + 8.0;
            }
            if eco {
                p.with_clip_rect(rect).text(pos2(x, cy), Align2::LEFT_CENTER, "eco", theme::ui(11.0), pal.text_faint);
            }
        }
        PCol::Data(Column::Pid) => {
            let text = if cx.kids > 0 && !cx.open { "\u{2014}".to_string() } else { r.pid.to_string() };
            ui.painter().text(pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, text, font, color);
        }
        PCol::Data(Column::Status) => {
            let p = ui.painter().with_clip_rect(rect);
            match r.status {
                Status::Running => {
                    p.text(pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, r.status.label(), theme::ui(13.0), if child { pal.text_faint } else { pal.text_dim });
                }
                Status::NotResponding => {
                    widgets::chip(&p, pos2(rect.left() + 8.0, cy), r.status.label(), cx.st.danger_text, Some(Icon::Octagon));
                }
                Status::Suspended => {
                    widgets::draw_icon(&p, Rect::from_center_size(pos2(rect.left() + 17.0, cy), vec2(12.0, 12.0)), Icon::Pause, pal.text_faint);
                    p.text(pos2(rect.left() + 29.0, cy), Align2::LEFT_CENTER, r.status.label(), theme::ui(13.0), pal.text_faint);
                }
            }
        }
        PCol::Data(c) => {
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
            if pc.right() {
                p.with_clip_rect(rect).text(pos2(rect.right() - 10.0, cy), Align2::RIGHT_CENTER, &text, font, color);
            } else {
                widgets::text_fit(p, pos2(rect.left() + 10.0, cy), Align2::LEFT_CENTER, &text, font, color, rect.width() - 16.0);
            }
        }
    }
    toggled
}

/// Expand / collapse arrow of a group row; true when clicked.
fn chevron(ui: &mut egui::Ui, r: &ProcessRow, pal: &Palette, center: egui::Pos2, open: bool) -> bool {
    let hit = Rect::from_center_size(center, vec2(22.0, 26.0));
    let resp = ui.interact(hit, ui.id().with(("chev", r.pid, r.create_time)), Sense::click());
    let color = if resp.hovered() { pal.text } else { pal.text_faint };
    widgets::draw_icon(ui.painter(), Rect::from_center_size(center, vec2(12.0, 12.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, color);
    resp.clicked()
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

/// What a context-menu entry asks for.
enum MenuPick {
    Do(Action),
    Details,
}

fn action_menu(ui: &mut egui::Ui, r: &ProcessRow) -> Option<MenuPick> {
    let mut out = None;
    kit::menu_header(ui, &format!("{} \u{b7} PID {}", r.name, r.pid));
    if kit::menu_item(ui, "End task", Some("Del"), false) {
        out = Some(MenuPick::Do(Action::EndTask));
    }
    if kit::menu_item(ui, "End task tree\u{2026}", Some("\u{21e7} Del"), true) {
        out = Some(MenuPick::Do(Action::EndTree));
    }
    if r.status == Status::Suspended {
        if kit::menu_item(ui, "Resume", None, false) {
            out = Some(MenuPick::Do(Action::Resume));
        }
    } else if kit::menu_item(ui, "Suspend", None, false) {
        out = Some(MenuPick::Do(Action::Suspend));
    }
    if kit::menu_item(ui, if r.efficiency { "Efficiency mode: turn off" } else { "Efficiency mode" }, None, false) {
        out = Some(MenuPick::Do(Action::Efficiency(!r.efficiency)));
    }
    ui.separator();
    ui.menu_button(RichText::new("Set priority").font(theme::ui(13.0)), |ui| {
        for p in Priority::ALL {
            if kit::menu_item(ui, p.label(), None, false) {
                out = Some(MenuPick::Do(Action::SetPriority(p)));
            }
        }
    });
    ui.separator();
    if kit::menu_item(ui, "View details", Some("Enter"), false) {
        out = Some(MenuPick::Details);
    }
    if ui.add_enabled_ui(r.path.is_some(), |ui| kit::menu_item(ui, "Open file location", None, false)).inner {
        out = Some(MenuPick::Do(Action::OpenFileLocation));
    }
    ui.menu_button(RichText::new("Copy").font(theme::ui(13.0)), |ui| {
        let copy = |ui: &mut egui::Ui, label: &str, text: String| {
            if kit::menu_item(ui, label, None, false) {
                ui.ctx().copy_text(text);
            }
        };
        copy(ui, "PID", r.pid.to_string());
        if let Some(p) = &r.path {
            copy(ui, "Path", p.clone());
        }
        if let Some(c) = &r.command_line {
            copy(ui, "Command line", c.clone());
        }
        copy(ui, "Name", r.name.clone());
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, ct: i64, name: &str) -> ProcessRow {
        ProcessRow { pid, parent_pid: ppid, create_time: ct, name: name.into(), ..Default::default() }
    }

    #[test]
    fn column_order_lists_every_column_once() {
        let set: HashSet<Column> = ORDER.into_iter().collect();
        assert_eq!(set.len(), ORDER.len());
        assert!(Column::ALL.into_iter().all(|c| set.contains(&c)));
        assert!(!default_hidden(Column::Cpu) && !default_hidden(Column::Description) && default_hidden(Column::Threads));
    }

    #[test]
    fn system_accounts_and_windows_paths() {
        assert!(is_system_user(Some("SYSTEM")) && is_system_user(Some(r"NT AUTHORITY\Local Service")) && is_system_user(Some("network service")));
        assert!(!is_system_user(Some("alex")) && !is_system_user(None));
        assert!(path_under(r"C:\Windows\System32\svchost.exe", r"c:\windows"));
        assert!(!path_under(r"C:\Windows2\x.exe", r"c:\windows") && !path_under(r"C:\Program Files\x.exe", r"c:\windows"));
    }

    #[test]
    fn views_filter_rows() {
        let mut r = row(10, 4, 1, "svchost.exe");
        r.user = Some("SYSTEM".into());
        assert!(in_view(4, &r) && !in_view(5, &r));
        r.has_window = true;
        assert!(!in_view(4, &r));
        r.status = Status::Suspended;
        assert!(in_view(5, &r));
    }

    #[test]
    fn groups_apps_and_children() {
        let mut a = row(10, 1, 5, "chrome.exe");
        a.has_window = true;
        let rows = vec![row(1, 0, 1, "explorer.exe"), a, row(11, 10, 6, "chrome.exe"), row(12, 11, 7, "chrome.exe"), row(20, 1, 8, "svc.exe")];
        let visible: Vec<usize> = (0..rows.len()).collect();
        let g = group_rows(&rows, &visible, true, Column::Pid, false);
        assert_eq!(g.apps, vec![1]);
        assert_eq!(g.children[&1], vec![2]);
        assert_eq!(g.bg, vec![0]);
        assert_eq!(g.children[&0], vec![4]);
        let mut members = Vec::new();
        members_of(1, &g.children, &mut members, 0);
        assert_eq!(members, vec![1, 2, 3]);
        let flat = group_rows(&rows, &visible, false, Column::Pid, false);
        assert_eq!(flat.apps, vec![1]);
        assert_eq!(flat.bg.len(), 4);
    }

    #[test]
    fn open_group_lists_a_few_members_and_a_more_line() {
        let mut root = row(10, 1, 5, "app.exe");
        root.has_window = true;
        let mut rows = vec![root];
        for k in 0..8u32 {
            rows.push(row(11 + k, 10, 6 + k as i64, "child.exe"));
        }
        let visible: Vec<usize> = (0..rows.len()).collect();
        let g = group_rows(&rows, &visible, true, Column::Pid, false);
        let mut expanded = HashSet::new();
        expanded.insert((10, 5));
        let mut show_all = HashSet::new();
        let t = TreeCtx { rows: &rows, children: &g.children, expanded: &expanded, show_all: &show_all };
        let mut b = Built::default();
        push_root(0, &t, &mut b);
        assert_eq!(b.entries.len(), 1 + SHOWN_MEMBERS + 1);
        assert!(matches!(b.entries.last(), Some(Entry::More { n: 5, .. })));
        assert_eq!(b.size[&0], 9);
        show_all.insert((10, 5));
        let t = TreeCtx { rows: &rows, children: &g.children, expanded: &expanded, show_all: &show_all };
        let mut b = Built::default();
        push_root(0, &t, &mut b);
        assert_eq!(b.entries.len(), 9);
    }

    #[test]
    fn group_trend_adds_member_samples() {
        let rows = vec![row(1, 0, 1, "a"), row(2, 1, 2, "b")];
        let mut spark = HashMap::new();
        let mut a = (Series::new(SPARK_LEN), Series::new(SPARK_LEN));
        let mut b = (Series::new(SPARK_LEN), Series::new(SPARK_LEN));
        for v in [1.0, 2.0, 3.0] {
            a.0.push(v);
            a.1.push(10.0);
        }
        for v in [5.0, 5.0] {
            b.0.push(v);
            b.1.push(1.0);
        }
        spark.insert((1, 1), a);
        spark.insert((2, 2), b);
        let (cpu, mem) = summed_spark(&rows, &[0, 1], &spark);
        assert_eq!(cpu.values.iter().copied().collect::<Vec<_>>(), vec![1.0, 7.0, 8.0]);
        assert_eq!(mem.values.iter().copied().collect::<Vec<_>>(), vec![10.0, 11.0, 11.0]);
    }

    #[test]
    fn spans_and_ages() {
        assert_eq!(span_text(15.0), "15s");
        assert_eq!(span_text(7.5), "7.5s");
        assert_eq!(span_text(120.0), "2m");
        assert_eq!(samples_text(60, Some(Duration::from_millis(500))), "30s");
        assert_eq!(samples_text(60, None), "60 samples");
        assert_eq!(interval_text(Some(Duration::from_millis(500))), "0.5 s");
        assert_eq!(ago_text(30), "30 s ago");
        assert_eq!(ago_text(5 * 3600 + 2 * 60), "5 h 02 m ago");
        assert_eq!(ago_text(3 * 86_400 + 4 * 3600), "3 d 4 h ago");
    }

    #[test]
    fn start_time_from_a_filetime() {
        let now = SystemTime::now();
        assert_eq!(started_text(0, now), "\u{2014}");
        let ft = FILETIME_UNIX + (now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 600) * 10_000_000;
        let s = started_text(ft, now);
        assert!(s.ends_with("10 min ago"), "{s}");
    }

    /// The history strip, every kind of cell and the context menu draw without panicking.
    #[test]
    fn page_pieces_draw() {
        use crate::metrics::{History, Snapshot};
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut icons = IconCache::new(&ctx);
        let snap = Snapshot::default();
        let mut hist = History::new(64);
        for _ in 0..5 {
            hist.push(&snap);
        }
        let smooth = crate::ui::Smooth::default();
        let st = theme::status(&pal);
        let mut series = (Series::new(SPARK_LEN), Series::new(SPARK_LEN));
        for k in 0..10 {
            series.0.push(k as f32);
            series.1.push(1000.0 + k as f32);
        }
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let fd = FrameData { snap: &snap, hist: &hist, rows: &[], smooth: &smooth, pal, metric_colors: theme::MetricColors::default(), bloom: 0.0, phase: 0.0, glide: false, time: 0.0, show_self: true, lasting: Default::default() };
                    history_strip(ui.painter(), Rect::from_min_size(pos2(10.0, 10.0), vec2(900.0, STRIP_H)), &fd, &pal, Some(Duration::from_secs(1)));
                    history_strip(ui.painter(), Rect::from_min_size(pos2(10.0, 130.0), vec2(900.0, STRIP_H)), &fd, &pal, None);
                    let mut r = row(10, 4, 1, "chrome.exe");
                    r.path = None;
                    for (status, depth, kids, selected) in [(Status::Running, 0, 3, false), (Status::NotResponding, 0, 0, true), (Status::Suspended, 1, 0, false), (Status::Running, 2, 1, false)] {
                        r.status = status;
                        let cx = CellCtx {
                            max_mem: 1.0,
                            max_io: 1.0,
                            selected,
                            depth,
                            kids,
                            open: depth == 2,
                            size: kids + 1,
                            agg: (kids > 0).then(Agg::default),
                            logical: 8,
                            per_core: false,
                            trend: Some((&series.0, &series.1)),
                            mem_color: pal.accent,
                            st,
                        };
                        let (rect, _) = ui.allocate_exact_size(vec2(300.0, ROW_H), Sense::hover());
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                            for pc in [PCol::Data(Column::Name), PCol::Data(Column::Pid), PCol::Data(Column::Status), PCol::Data(Column::Cpu), PCol::CpuTrend, PCol::MemTrend, PCol::Data(Column::Description)] {
                                cell_ui(ui, &r, pc, &pal, cx, &mut icons, true, false);
                            }
                            cell_ui(ui, &r, PCol::Data(Column::Name), &pal, cx, &mut icons, false, true);
                        });
                    }
                    let mut out = None;
                    egui::Area::new(egui::Id::new("menu-smoke")).show(ctx, |ui| out = action_menu(ui, &r));
                    assert!(out.is_none());
                });
            });
        }
    }

    #[test]
    fn base_priority_names() {
        assert_eq!(priority_text(8), "Normal");
        assert_eq!(priority_text(13), "High");
        assert_eq!(priority_text(7), "7");
    }

    #[test]
    fn apps_are_named_by_a_short_description() {
        let mut r = row(1, 0, 1, "chrome.exe");
        assert_eq!(app_label(&r), "chrome.exe");
        r.description = Some("Google Chrome".into());
        assert_eq!(app_label(&r), "Google Chrome");
        r.description = Some("x".repeat(60));
        assert_eq!(app_label(&r), "chrome.exe");
    }
}
