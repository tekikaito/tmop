//! The GPU context menu and "GPU activity": each GPU's figures and what each process uses of it.

use super::kit;
use super::table::{self, Cell, Col, TableState, HEADER_H, ROW_H};
use super::widgets::{Button, ButtonSize};
use super::{App, Device, FrameData, Page};
use crate::metrics::{GpuAdapter, GpuProcessUsage};
use crate::process::format_bytes;
use eframe::egui::{self, pos2, vec2, Rect};
use std::collections::HashMap;

const DASH: &str = "\u{2014}";
const PROCESS_TABLE_H: f32 = 320.0;
const OVERVIEW_COLS: [Col; 6] = [Col::text("GPU", 300.0), Col::num("Utilization", 112.0), Col::num("Dedicated memory", 170.0), Col::num("Temperature", 100.0), Col::num("Power", 80.0), Col::num("Clock", 90.0)];
const PROCESS_COLS: [Col; 7] = [Col::text("Process", 190.0), Col::num("PID", 70.0), Col::text("GPU", 190.0), Col::num("Utilization", 112.0), Col::text("Engines", 170.0), Col::num("Dedicated", 90.0), Col::num("Shared", 90.0)];
/// For a detail area too small for the above (down to the minimum window): short titles, and the
/// figures that must stay readable keep their room.
const OVERVIEW_NARROW: [Col; 6] = [Col::text("GPU", 150.0), Col::num("Util.", 52.0), Col::num("Memory", 80.0), Col::num("Temp", 56.0), Col::num("Power", 52.0), Col::num("Clock", 72.0)];
const PROCESS_NARROW: [Col; 7] = [Col::text("Process", 92.0), Col::num("PID", 54.0), Col::text("GPU", 52.0), Col::num("Util.", 56.0), Col::text("Engines", 74.0), Col::num("Ded.", 68.0), Col::num("Shared", 68.0)];
/// Busiest first, as the demanding processes are what the table is opened for.
const UTILIZATION_COL: usize = 3;

/// What the GPU context menu asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMenu {
    Separated(bool),
    Activity(bool),
}

/// Fills a context menu of the GPU section.
pub fn gpu_menu(ui: &mut egui::Ui, fd: &FrameData<'_>, activity: bool) -> Option<GpuMenu> {
    let mut out = None;
    let label = if fd.gpus_separated { "Show GPUs grouped" } else { "Show GPUs separately" };
    let toggle = ui.add_enabled(fd.snap.gpu.adapters.len() >= 2, egui::Button::new(label)).on_disabled_hover_text("TMOP found only one GPU");
    if toggle.clicked() {
        out = Some(GpuMenu::Separated(!fd.gpus_separated));
    }
    if ui.button(if activity { "Hide GPU activity" } else { "View GPU activity" }).clicked() {
        out = Some(GpuMenu::Activity(!activity));
    }
    if out.is_some() {
        ui.close_menu();
    }
    out
}

/// "GPU 0 · NVIDIA GeForce RTX 4090".
pub fn adapter_title(index: usize, a: &GpuAdapter) -> String {
    format!("GPU {index} \u{b7} {}", a.name)
}

fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> String) -> String {
    v.map_or_else(|| DASH.to_string(), f)
}

/// "3D 85% · Copy 2%": the engines at work, or a dash when all are idle.
pub fn engines_text(p: &GpuProcessUsage) -> String {
    let busy: Vec<String> = p.engines.iter().filter(|e| e.1 >= 0.5).map(|(e, v)| format!("{e} {v:.0}%")).collect();
    if busy.is_empty() {
        DASH.to_string()
    } else {
        busy.join(" \u{b7} ")
    }
}

/// One line of the process table.
struct Line<'a> {
    usage: &'a GpuProcessUsage,
    name: String,
    gpu: String,
    created: Option<i64>,
}

fn compare(a: &Line<'_>, b: &Line<'_>, col: usize) -> std::cmp::Ordering {
    let (ua, ub) = (a.usage, b.usage);
    match col {
        0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        1 => ua.pid.cmp(&ub.pid),
        2 => a.gpu.cmp(&b.gpu),
        UTILIZATION_COL => ua.utilization_pct.total_cmp(&ub.utilization_pct),
        4 => engines_text(ua).cmp(&engines_text(ub)),
        5 => ua.dedicated.cmp(&ub.dedicated),
        _ => ua.shared.cmp(&ub.shared),
    }
}

impl App {
    pub fn apply_gpu_menu(&mut self, m: GpuMenu) {
        match m {
            GpuMenu::Separated(on) => {
                self.settings.window.gpus_separated = on;
                self.settings_dirty = true;
            }
            GpuMenu::Activity(on) => {
                self.perf.gpu_activity = on;
                if on {
                    self.device = Device::Gpu;
                    self.page = Page::Performance;
                }
            }
        }
    }

    /// The activity section. Returns its height and the process double-clicked, if any.
    pub fn gpu_activity(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32) -> (f32, Option<(u32, i64)>) {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let s = &fd.snap.gpu;
        let supported = self.caps.proc.gpu;
        let overview_h = HEADER_H + s.adapters.len().max(1) as f32 * ROW_H + 2.0;
        let body_h = overview_h + 14.0 + if supported { PROCESS_TABLE_H } else { 56.0 };
        let rect = Rect::from_min_size(pos2(x, y), vec2(w, 40.0 + body_h + 18.0));
        let c = kit::section_card(&p, rect, &pal, "GPU activity", "");
        let close = Rect::from_min_size(pos2(rect.right() - 22.0 - 68.0, rect.top() + 10.0), vec2(68.0, 26.0));
        if kit::row_ui(ui, close, |ui| Button::ghost("Hide").size(ButtonSize::Compact).show(ui, &pal).clicked()) {
            self.perf.gpu_activity = false;
        }

        let narrow = c.width() < PROCESS_COLS.iter().map(|c| c.width).sum::<f32>();
        let index: HashMap<&str, usize> = s.adapters.iter().enumerate().map(|(i, a)| (a.id.as_str(), i)).collect();
        let mut order: Vec<usize> = (0..s.adapters.len()).collect();
        self.perf.gpu_overview_table.sort(&s.adapters, &mut order, |a, b, col| match col {
            1 => a.utilization_pct.total_cmp(&b.utilization_pct),
            2 => a.dedicated_used.cmp(&b.dedicated_used),
            3 => a.temperature_c.unwrap_or(f32::MIN).total_cmp(&b.temperature_c.unwrap_or(f32::MIN)),
            4 => a.power_w.unwrap_or(f32::MIN).total_cmp(&b.power_w.unwrap_or(f32::MIN)),
            5 => a.clock_mhz.cmp(&b.clock_mhz),
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        let overview =Rect::from_min_size(c.min, vec2(c.width(), overview_h));
        let temps: Vec<String> = s.adapters.iter().map(|a| opt(a.temperature_c, |t| self.temp(t))).collect();
        let cell = |i: usize, col: usize| {
            let a = &s.adapters[i];
            Cell::new(match col {
                0 => adapter_title(i, a),
                1 => format!("{:.0}%", a.utilization_pct),
                2 if narrow => opt(a.dedicated_used, |u| format_bytes(u as f64)),
                2 => match (a.dedicated_used, a.dedicated_total) {
                    (Some(u), Some(t)) => format!("{} / {}", format_bytes(u as f64), format_bytes(t as f64)),
                    (u, _) => opt(u, |u| format_bytes(u as f64)),
                },
                3 => temps[i].clone(),
                4 => opt(a.power_w, |w| format!("{w:.0} W")),
                _ => opt(a.clock_mhz, |m| format!("{m} MHz")),
            })
        };
        let (cols, id) = if narrow { (&OVERVIEW_NARROW, "gpu-overview-narrow") } else { (&OVERVIEW_COLS, "gpu-overview") };
        table::show(ui, overview, id, &pal, cols, &order,&mut self.perf.gpu_overview_table, &|i| i.to_string(), &cell, &mut self.icons);

        let below = Rect::from_min_size(pos2(c.left(), overview.bottom() + 14.0), vec2(c.width(), body_h - overview_h - 14.0));
        if !supported {
            kit::empty_state(&p, below, &pal, "This system reports no per-process GPU usage");
            return (rect.height(), None);
        }
        let by_pid: HashMap<u32, (&str, i64)> = fd.rows.iter().map(|r| (r.pid, (r.name.as_str(), r.create_time))).collect();
        let lines: Vec<Line<'_>> = s
            .processes
            .iter()
            .map(|u| {
                let row = by_pid.get(&u.pid);
                let gpu = index.get(u.gpu_id.as_str()).map_or_else(|| "GPU".to_string(), |i| if narrow { i.to_string() } else { adapter_title(*i, &s.adapters[*i]) });
                Line { usage: u, name: row.map_or_else(|| format!("Process {}", u.pid), |r| r.0.to_string()), gpu, created: row.map(|r| r.1) }
            })
            .collect();
        let mut order: Vec<usize> = (0..lines.len()).collect();
        let state = self.perf.gpu_table.get_or_insert_with(|| TableState::sorted_by(UTILIZATION_COL, true));
        state.sort(&lines, &mut order, compare);
        let bytes = |v: Option<u64>| opt(v, |b| format_bytes(b as f64));
        let cell = |i: usize, col: usize| {
            let l = &lines[i];
            Cell::new(match col {
                0 => l.name.clone(),
                1 => l.usage.pid.to_string(),
                2 => l.gpu.clone(),
                UTILIZATION_COL => format!("{:.1}%", l.usage.utilization_pct),
                4 => engines_text(l.usage),
                5 => bytes(l.usage.dedicated),
                _ => bytes(l.usage.shared),
            })
        };
        let key = |i: usize| format!("{}:{}", lines[i].usage.pid, lines[i].usage.gpu_id);
        let (cols, id) = if narrow { (&PROCESS_NARROW, "gpu-processes-narrow") } else { (&PROCESS_COLS, "gpu-processes") };
        let opened = table::show(ui, below, id, &pal, cols, &order, state,&key, &cell, &mut self.icons);
        if lines.is_empty() {
            kit::empty_state(&p, below, &pal, "Waiting for GPU activity");
        }
        (rect.height(), opened.and_then(|i| lines[i].created.map(|c| (lines[i].usage.pid, c))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_text_lists_those_at_work() {
        let mut p = GpuProcessUsage { engines: vec![("3D".into(), 85.0), ("Copy".into(), 2.4), ("VideoDecode".into(), 0.1)], ..Default::default() };
        assert_eq!(engines_text(&p), "3D 85% \u{b7} Copy 2%");
        p.engines.clear();
        assert_eq!(engines_text(&p), DASH);
    }

    #[test]

    #[test]
    fn unknown_memory_sorts_below_known() {
        let (a, b) = (GpuProcessUsage { dedicated: None, ..Default::default() }, GpuProcessUsage { dedicated: Some(0), ..Default::default() });
        let line = |u| Line { usage: u, name: String::new(), gpu: String::new(), created: None };
        assert_eq!(compare(&line(&a), &line(&b), 5), std::cmp::Ordering::Less);
    }
}
