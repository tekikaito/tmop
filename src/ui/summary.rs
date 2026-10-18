//! Summary page: system LED meters, CPU overview graph, top processes, memory and
//! the device tiles.

use super::theme::{self, mix, with_alpha};
use super::widgets::{self, card, draw_graph, led_column, led_panel, led_row, GraphLine, Icon, LedStyle, LED_PANEL_H};
use super::{App, Device, FrameData, Page};
use crate::metrics::Series;
use crate::process::{format_bytes, format_rate};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Vec2};

/// (label, colour, fill fraction, value text, LED style, device opened on click)
type MeterColumn<'a> = (&'a str, Color32, Option<f32>, String, LedStyle, Device);

const M: f32 = 12.0;
const GAP: f32 = 10.0;

impl App {
    pub fn summary_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Summary", Icon::Home);
        let area = body.shrink(M);
        let tiles_h = 86.0;
        let mem_h = 176.0;
        let top_h = (area.height() - tiles_h - mem_h - GAP * 2.0).max(260.0);
        let right_w = if area.width() > 1250.0 { 350.0 } else { 300.0 };
        let left_w = 220.0;
        let top = Rect::from_min_size(area.min, vec2(area.width(), top_h));
        let meters = Rect::from_min_size(top.min, vec2(left_w, top_h));
        let procs = Rect::from_min_size(pos2(top.right() - right_w, top.top()), vec2(right_w, top_h));
        let overview = Rect::from_min_max(pos2(meters.right() + GAP, top.top()), pos2(procs.left() - GAP, top.bottom()));
        let mem = Rect::from_min_size(pos2(area.left(), top.bottom() + GAP), vec2(area.width(), mem_h));
        let tiles = Rect::from_min_size(pos2(area.left(), mem.bottom() + GAP), vec2(area.width(), tiles_h));

        self.meter_card(ui, meters, fd);
        self.overview_card(ui, overview, fd);
        self.top_processes_card(ui, procs, fd);
        self.memory_card(ui, mem, fd);
        self.tiles_row(ui, tiles, fd);
    }

    fn meter_card(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let p = ui.painter();
        card(p, rect, &pal);
        let inner = rect.shrink(8.0);
        let clock = fd.smooth.clock;
        let ghz = if clock > 0.0 { format!("{:.1}GHZ", clock / 1000.0) } else { "---".into() };
        led_panel(p, Rect::from_min_size(inner.min, vec2(inner.width(), LED_PANEL_H)), "SYS", &ghz, &pal, fd.bloom);
        let s = fd.snap;
        let temp = s.thermal.max_c();
        let cols: [MeterColumn<'_>; 4] = [
            ("CPU", fd.metric("cpu"), Some(fd.smooth.cpu / 100.0), format!("{:.1}%", s.cpu.total_pct), LedStyle::traffic(fd.bloom), Device::Cpu),
            (
                "Clock",
                fd.metric("clock"),
                s.cpu.max_mhz.filter(|m| *m > 0).map(|m| fd.smooth.clock / m as f32),
                s.cpu.current_mhz.map(|m| format!("{:.1}G", m as f32 / 1000.0)).unwrap_or("\u{2014}".into()),
                LedStyle::new(fd.metric("clock"), fd.bloom),
                Device::Cpu,
            ),
            (
                "Temp",
                fd.metric("thermal"),
                temp.map(|_c| fd.smooth.thermal / 100.0),
                temp.map(|c| self.temp(c).replace(' ', "")).unwrap_or("\u{2014}".into()),
                LedStyle::new(fd.metric("thermal"), fd.bloom),
                Device::Thermal,
            ),
            (
                "GPU",
                fd.metric("gpu"),
                s.gpu.available.then_some(fd.smooth.gpu / 100.0),
                if s.gpu.available { format!("{:.1}%", s.gpu.utilization_pct) } else { "\u{2014}".into() },
                LedStyle::new(fd.metric("gpu"), fd.bloom),
                Device::Gpu,
            ),
        ];
        let top = inner.top() + LED_PANEL_H + 10.0;
        let col_w = inner.width() / 4.0;
        let meter_top = top + 20.0;
        let meter_bottom = inner.bottom() - 22.0;
        for (i, (label, color, frac, value, style, dev)) in cols.iter().enumerate() {
            let cx = inner.left() + col_w * (i as f32 + 0.5);
            p.text(pos2(cx, top), Align2::CENTER_TOP, label, theme::semibold(12.5), *color);
            let m = Rect::from_min_max(pos2(cx - 17.0, meter_top), pos2(cx + 17.0, meter_bottom));
            let segs = ((m.height() / 10.0).floor() as usize).clamp(8, 48);
            led_column(p, m, segs, frac.unwrap_or(0.0), style);
            p.text(pos2(cx, inner.bottom()), Align2::CENTER_BOTTOM, value, theme::ui(12.0), pal.text);
            let r = ui.interact(Rect::from_min_max(pos2(cx - col_w / 2.0, top), pos2(cx + col_w / 2.0, inner.bottom())), ui.id().with(("meter", i)), Sense::click());
            if r.clicked() {
                self.device = *dev;
                self.page = Page::Performance;
            }
        }
    }

    fn overview_card(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let p = ui.painter();
        card(p, rect, &pal);
        let inner = rect.shrink(8.0);
        let s = fd.snap;
        led_panel(p, Rect::from_min_size(inner.min, vec2(inner.width(), LED_PANEL_H)), "CPU OVERVIEW", &format!("{:.1}%", s.cpu.total_pct), &pal, fd.bloom);
        let mut x = inner.left() + 2.0;
        let ly = inner.top() + LED_PANEL_H + 14.0;
        let green = fd.metric("cpu");
        let red = fd.metric("kernel");
        let orange = fd.metric("thermal");
        let has_temp = !s.thermal.zones.is_empty();
        let mut legend = vec![("Utilization", green), ("Kernel", red)];
        if has_temp {
            legend.insert(1, ("Temperature", orange));
        }
        for (t, c) in legend {
            let r = p.text(pos2(x, ly), Align2::LEFT_CENTER, t, theme::ui(12.0), c);
            x += r.width() + 22.0;
        }
        let graph = Rect::from_min_max(pos2(inner.left() + 40.0, ly + 14.0), pos2(inner.right() - (if has_temp { 34.0 } else { 6.0 }), inner.bottom() - 22.0));
        let mut lines = vec![GraphLine { series: &fd.hist.cpu, color: green, range: (0.0, 100.0) }, GraphLine { series: &fd.hist.kernel, color: red, range: (0.0, 100.0) }];
        if has_temp {
            lines.push(GraphLine { series: &fd.hist.thermal, color: orange, range: (0.0, 110.0) });
        }
        let mut g = self.graph(lines, fd);
        g.border = Some(green);
        g.dashed = self.settings.graphs.dashed_summary;
        draw_graph(p, graph, &g);
        for (t, frac) in [("100%", 1.0), ("50%", 0.5), ("0%", 0.0)] {
            p.text(pos2(graph.left() - 6.0, graph.bottom() - graph.height() * frac), Align2::RIGHT_CENTER, t, theme::ui(11.5), green);
        }
        if has_temp {
            for (t, frac) in [("110\u{b0}", 1.0), ("55\u{b0}", 0.5), ("0\u{b0}", 0.0)] {
                p.text(pos2(graph.right() + 6.0, graph.bottom() - graph.height() * frac), Align2::LEFT_CENTER, t, theme::ui(11.5), orange);
            }
        }
        p.text(pos2(graph.left(), inner.bottom()), Align2::LEFT_BOTTOM, format!("{} logical processors", s.cpu.logical_count), theme::ui(12.0), pal.text_dim);
        let speed = s.cpu.current_mhz.map(|m| format!("Speed {:.2} GHz", m as f32 / 1000.0)).unwrap_or_default();
        p.text(pos2(graph.right(), inner.bottom()), Align2::RIGHT_BOTTOM, &speed, theme::ui(12.0), pal.text_dim);
        if ui.interact(graph, ui.id().with("overview-graph"), Sense::click()).clicked() {
            self.device = Device::Cpu;
            self.page = Page::Performance;
        }
    }

    fn top_processes_card(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let p = ui.painter();
        card(p, rect, &pal);
        let inner = rect.shrink(8.0);
        let row_h = 18.0;
        let list_top = inner.top() + LED_PANEL_H + 24.0;
        let n = ((inner.bottom() - list_top) / row_h).floor().max(0.0) as usize;
        led_panel(p, Rect::from_min_size(inner.min, vec2(inner.width(), LED_PANEL_H)), &format!("TOP {n} CPU PROCESSES"), "", &pal, fd.bloom);
        let mut rows: Vec<&crate::process::ProcessRow> = fd.rows.iter().filter(|r| r.pid != 0 && (self.settings.general.show_self_in_reports || r.pid != std::process::id())).collect();
        rows.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal).then(b.private_working_set.cmp(&a.private_working_set)));
        let hy = inner.top() + LED_PANEL_H + 12.0;
        let x_pid = inner.left() + 52.0;
        let x_name = inner.left() + 78.0;
        let x_cpu = inner.right() - 62.0;
        let x_mem = inner.right() - 4.0;
        let hf = theme::ui(12.0);
        p.text(pos2(x_pid, hy), Align2::RIGHT_CENTER, "PID", hf.clone(), pal.text_dim);
        p.text(pos2(x_name - 18.0, hy), Align2::LEFT_CENTER, "Name", hf.clone(), pal.text_dim);
        p.text(pos2(x_cpu, hy), Align2::RIGHT_CENTER, "CPU", hf.clone(), pal.text_dim);
        p.text(pos2(x_mem, hy), Align2::RIGHT_CENTER, "Memory", hf, pal.text_dim);
        let top_cpu = rows.first().map(|r| r.cpu_pct).unwrap_or(0.0).max(1.0);
        let heat = pal.heat();
        let mut clicked = None;
        for (i, r) in rows.iter().take(n).enumerate() {
            let y0 = list_top + i as f32 * row_h;
            let rr = Rect::from_min_size(pos2(inner.left(), y0), vec2(inner.width(), row_h - 1.0));
            if r.cpu_pct > 0.0 {
                let t = (0.35 + 0.65 * (r.cpu_pct / top_cpu)).min(1.0);
                p.rect_filled(rr, 2.0, mix(pal.card, heat, t));
            }
            let cy = rr.center().y;
            p.text(pos2(x_pid, cy), Align2::RIGHT_CENTER, r.pid.to_string(), theme::ui(12.0), pal.text_dim);
            let ic = Rect::from_center_size(pos2(x_name - 10.0, cy), vec2(16.0, 16.0));
            match self.icons.for_process(r.pid, r.path.as_deref()) {
                Some(t) => {
                    p.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => proc_icon(p, ic.shrink(2.0), r.has_window),
            }
            widgets::text_fit(p, pos2(x_name, cy), Align2::LEFT_CENTER, &r.name, theme::ui(12.5), pal.text, x_cpu - 44.0 - x_name);
            p.text(pos2(x_cpu, cy), Align2::RIGHT_CENTER, format!("{:.1}%", r.cpu_pct), theme::ui(12.0), if r.cpu_pct > 0.0 { pal.text } else { pal.text_dim });
            p.text(pos2(x_mem, cy), Align2::RIGHT_CENTER, format_bytes(r.private_working_set as f64), theme::ui(12.0), pal.text);
            if ui.interact(rr, ui.id().with(("top", r.pid)), Sense::click()).clicked() {
                clicked = Some((r.pid, r.create_time));
            }
        }
        if let Some(k) = clicked {
            self.selected = Some(k);
            self.page = Page::Processes;
        }
    }

    fn memory_card(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let p = ui.painter();
        card(p, rect, &pal);
        let inner = rect.shrink(8.0);
        let s = fd.snap;
        let purple = fd.metric("memory");
        let used = s.memory.total.saturating_sub(s.memory.available);
        p.text(pos2(inner.left() + 58.0, inner.top() + 10.0), Align2::LEFT_CENTER, "Memory Utilization", theme::display(14.0), pal.text);
        p.text(pos2(inner.right() - 2.0, inner.top() + 10.0), Align2::RIGHT_CENTER, format!("{} / {}", format_bytes(used as f64), format_bytes(s.memory.total as f64)), theme::ui(12.0), pal.text_dim);
        let meter = Rect::from_min_max(pos2(inner.left() + 10.0, inner.top() + 26.0), pos2(inner.left() + 42.0, inner.bottom() - 20.0));
        led_column(p, meter, 10, fd.smooth.memory / 100.0, &LedStyle::new(purple, fd.bloom));
        p.text(pos2(meter.center().x, inner.bottom()), Align2::CENTER_BOTTOM, format!("{:.1}%", s.memory.used_pct), theme::ui(12.0), pal.text);
        let graph = Rect::from_min_max(pos2(inner.left() + 58.0, inner.top() + 24.0), pos2(inner.right() - 2.0, inner.bottom() - 22.0));
        // Auto-ranged around the visible history (e.g. "20.3 GB .. 20.6 GB").
        let (lo, hi) = widgets::eased_range(ui.ctx(), "summary-memory", visible_range(&fd.hist.memory_bytes, 600));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.memory_bytes, color: purple, range: (lo, hi) }], fd);
        g.border = Some(with_alpha(purple, 0.8));
        g.dashed = self.settings.graphs.dashed_summary;
        draw_graph(p, graph, &g);
        p.text(pos2(graph.left() + 4.0, graph.top() + 2.0), Align2::LEFT_TOP, format_bytes(hi as f64), theme::ui(11.0), purple);
        p.text(pos2(graph.left() + 4.0, graph.bottom() - 2.0), Align2::LEFT_BOTTOM, format_bytes(lo as f64), theme::ui(11.0), purple);
        let fy = inner.bottom();
        p.text(pos2(graph.left(), fy), Align2::LEFT_BOTTOM, format!("Available {}", format_bytes(s.memory.available as f64)), theme::ui(12.0), pal.text_dim);
        p.text(pos2(graph.center().x, fy), Align2::CENTER_BOTTOM, format!("Cached {}", format_bytes(s.memory.cached as f64)), theme::ui(12.0), pal.text_dim);
        let pagefile = s.memory.commit_limit.saturating_sub(s.memory.total);
        let swap = s.memory.swap_used.map(|b| format!("Swap {}", format_bytes(b as f64))).unwrap_or_else(|| format!("Page file {}", format_bytes(pagefile as f64)));
        p.text(pos2(graph.right(), fy), Align2::RIGHT_BOTTOM, swap, theme::ui(12.0), pal.text_dim);
        if ui.interact(graph, ui.id().with("memory-graph"), Sense::click()).clicked() {
            self.device = Device::Memory;
            self.page = Page::Performance;
        }
    }

    fn tiles_row(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let s = fd.snap;
        let pal = fd.pal;
        let net_total = s.network.rx_bps + s.network.tx_bps;
        let net_peak = fd.hist.net_rx.values.iter().zip(&fd.hist.net_tx.values).map(|(a, b)| a + b).fold(0.0f32, f32::max).max(1024.0 * 100.0);
        let link = if s.network.link_bps > 0.0 { format!("{:.1} Gb/s combined  {:.1}%", s.network.link_bps / 1e9, s.network.utilization_pct) } else { "No active adapters".into() };
        let tiles: [(Device, &str, String, String, String, f32); 4] = [
            (Device::Network, "Network", format_rate(net_total), link, format!("Scale {}", format_rate(net_peak as f64)), (net_total as f32 / net_peak).clamp(0.0, 1.0)),
            (
                Device::Disk,
                "Disk",
                format!("{:.1}%", s.disk.active_pct),
                format!("R: {}  W: {}", format_rate(s.disk.read_bps), format_rate(s.disk.write_bps)),
                "PhysicalDisk _Total".into(),
                fd.smooth.disk / 100.0,
            ),
            (
                Device::Gpu,
                "GPUs",
                if s.gpu.available { format!("{:.1}%", s.gpu.utilization_pct) } else { "N/A".into() },
                if s.gpu.dedicated_used > 0 { format!("Dedicated memory {}", format_bytes(s.gpu.dedicated_used as f64)) } else { "No GPU engine counters".into() },
                s.gpu.engines.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(e, v)| format!("{e} {v:.0}%")).unwrap_or_default(),
                fd.smooth.gpu / 100.0,
            ),
            (
                Device::Thermal,
                "Thermals",
                s.thermal.max_c().map(|c| self.temp(c)).unwrap_or("\u{2014}".into()),
                if s.thermal.zones.is_empty() { "No ACPI thermal zones".into() } else { format!("{} ACPI zone(s), firmware sensor", s.thermal.zones.len()) },
                String::new(),
                fd.smooth.thermal / 100.0,
            ),
        ];
        let n = tiles.len() as f32;
        let w = (rect.width() - GAP * (n - 1.0)) / n;
        for (i, (dev, title, value, sub, sub_r, frac)) in tiles.iter().enumerate() {
            let r = Rect::from_min_size(pos2(rect.left() + i as f32 * (w + GAP), rect.top()), vec2(w, rect.height()));
            let c = fd.metric(dev.key());
            let p = ui.painter();
            widgets::accent_card(p, r, &pal, c, fd.bloom);
            let inner = r.shrink2(vec2(10.0, 6.0));
            p.text(pos2(inner.left(), inner.top() + 9.0), Align2::LEFT_CENTER, title, theme::display(14.0), pal.text);
            p.text(pos2(inner.right(), inner.top() + 9.0), Align2::RIGHT_CENTER, value, theme::ui(12.5), c);
            p.text(pos2(inner.left(), inner.top() + 30.0), Align2::LEFT_CENTER, sub, theme::ui(12.0), pal.text_dim);
            p.text(pos2(inner.right(), inner.top() + 30.0), Align2::RIGHT_CENTER, sub_r, theme::ui(12.0), pal.text_dim);
            let bar = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 16.0), pos2(inner.right(), inner.bottom() - 2.0));
            let segs = ((bar.width() / 11.0).floor() as usize).clamp(10, 60);
            led_row(p, bar, segs, *frac, &LedStyle { unlit_factor: 0.18, ..LedStyle::new(c, fd.bloom) });
            if ui.interact(r, ui.id().with(("tile", i)), Sense::click()).clicked() {
                self.device = *dev;
                self.page = Page::Performance;
            }
        }
    }
}

/// Min/max of the newest `n` values with a little headroom; a flat series gets a window.
pub fn visible_range(s: &Series, n: usize) -> (f32, f32) {
    let it = s.values.iter().rev().take(n);
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for v in it {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if lo == f32::MAX {
        return (0.0, 1.0);
    }
    let span = (hi - lo).max(hi.abs() * 0.01).max(1.0);
    (lo - span * 0.1, hi + span * 0.1)
}

/// Small generic process glyph: a window for apps, a cog-like square for background work.
pub fn proc_icon(p: &egui::Painter, r: Rect, app: bool) {
    let c = if app { Color32::from_rgb(0x4C, 0x9E, 0xF7) } else { Color32::from_rgb(0x8A, 0x8A, 0x8A) };
    p.rect_filled(r, 2.0, with_alpha(c, 0.25));
    p.rect_stroke(r, 2.0, egui::Stroke::new(1.0_f32, c));
    if app {
        p.line_segment([pos2(r.left() + 2.0, r.top() + 4.0), pos2(r.right() - 2.0, r.top() + 4.0)], egui::Stroke::new(1.0_f32, c));
    } else {
        p.circle_stroke(r.center(), 2.5, egui::Stroke::new(1.0_f32, c));
    }
}

#[allow(dead_code)]
fn _unused(_: Vec2) {}
