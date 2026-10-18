//! Performance page: device list on the left, detail graphs and statistics on the right
//! (title, segmented utilization bar, per-core graph grid, stats).

use super::theme::{self, mix, with_alpha, Palette};
use super::widgets::{self, draw_graph, draw_icon, led_gradient_row, led_row, Graph, GraphLine, Icon, LedStyle};
use super::{App, Device, FrameData};
use crate::metrics::Series;
use crate::process::{format_bytes, format_rate};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Stroke};

const LIST_W: f32 = 236.0;

/// Background of the per-core graph cells and the memory composition bar.
fn cell_bg(pal: &Palette) -> Color32 {
    mix(pal.graph_bg(), pal.body, 0.5)
}

impl App {
    fn cpus_expanded(&self) -> bool {
        self.settings.window.performance_cpus_expanded
    }

    pub fn performance_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Performance", Icon::Chart);
        let mut refresh = false;
        super::kit::header_toolbar(ui, body, 200.0, |ui| refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked());
        if refresh {
            self.sampler.refresh();
        }
        let list = Rect::from_min_size(body.min, vec2(LIST_W, body.height()));
        let detail = Rect::from_min_max(pos2(list.right(), body.top()), body.max);
        ui.painter().rect_filled(list, 0.0, pal.sidebar);
        self.device_list(ui, list, fd);
        let inner = detail.shrink2(vec2(28.0, 10.0));
        match self.device {
            Device::Cpu => self.cpu_detail(ui, inner, fd),
            Device::Memory => self.memory_detail(ui, inner, fd),
            Device::Gpu => self.gpu_detail(ui, inner, fd),
            Device::Disk => self.disk_detail(ui, inner, fd),
            Device::Network => self.network_detail(ui, inner, fd),
            Device::Thermal => self.thermal_detail(ui, inner, fd),
        }
    }

    fn device_list(&mut self, ui: &mut egui::Ui, list: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let mut net = Series::new(fd.hist.net_rx.capacity);
        for (a, b) in fd.hist.net_rx.values.iter().zip(&fd.hist.net_tx.values) {
            net.push(a + b);
        }
        net.pushed = fd.hist.net_rx.pushed;
        let net_peak = net.max().max(1024.0);
        let mut y = list.top() + 6.0;
        for d in Device::ALL {
            let r = Rect::from_min_size(pos2(list.left() + 4.0, y), vec2(list.width() - 8.0, 58.0));
            let resp = ui.interact(r, ui.id().with(("dev", d.key())), Sense::click());
            let p = ui.painter();
            let selected = self.device == d;
            if selected {
                p.rect_filled(r, 4.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(r, 4.0, pal.hover);
            }
            let c = fd.metric(d.key());
            draw_icon(p, Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(15.0, 15.0)), d.icon(), c);
            let (name, sub1, sub2, series, range): (String, String, String, &Series, (f32, f32)) = match d {
                Device::Cpu => ("CPU".into(), format!("{:.1}%", s.cpu.total_pct), format!("{} logical processors", s.cpu.logical_count), &fd.hist.cpu, (0.0, 100.0)),
                Device::Memory => (
                    "Memory".into(),
                    format!("{} / {}", format_bytes(s.memory.total.saturating_sub(s.memory.available) as f64), format_bytes(s.memory.total as f64)),
                    format!("{:.1}%", s.memory.used_pct),
                    &fd.hist.memory,
                    (0.0, 100.0),
                ),
                Device::Gpu => ("GPU".into(), "GPU adapter".into(), if s.gpu.available { format!("{:.1}%", s.gpu.utilization_pct) } else { "no counters".into() }, &fd.hist.gpu, (0.0, 100.0)),
                Device::Disk => ("Disk".into(), "Physical disks".into(), format!("{:.1}%", s.disk.active_pct), &fd.hist.disk, (0.0, 100.0)),
                Device::Network => ("Network".into(), "All network adapters".into(), format!("S: {}  R: {}", format_rate(s.network.tx_bps), format_rate(s.network.rx_bps)), &net, (0.0, net_peak)),
                Device::Thermal => ("Thermals".into(), "ACPI thermal zones".into(), s.thermal.max_c().map(|c| self.temp(c)).unwrap_or("no sensor".into()), &fd.hist.thermal, (0.0, 110.0)),
            };
            let tx = r.left() + 42.0;
            let tw = r.right() - 90.0 - tx;
            let fg = if selected { pal.on_selection() } else { pal.text };
            widgets::text_fit(p, pos2(tx, r.top() + 13.0), Align2::LEFT_CENTER, &name, theme::display(14.0), fg, tw);
            widgets::text_fit(p, pos2(tx, r.top() + 31.0), Align2::LEFT_CENTER, &sub1, theme::ui(11.5), fg, tw);
            widgets::text_fit(p, pos2(tx, r.top() + 45.0), Align2::LEFT_CENTER, &sub2, theme::ui(11.5), fg, tw);
            let thumb = Rect::from_min_size(pos2(r.right() - 82.0, r.top() + 7.0), vec2(76.0, 44.0));
            let mut g = Graph::new(vec![GraphLine { series, color: c, range }]);
            g.px_per_update = 1.5;
            g.phase = fd.phase;
            g.glide = fd.glide;
            g.regions = 1;
            g.smooth = self.settings.graphs.smooth_curves;
            g.bloom = fd.bloom * 0.5;
            g.markers = false;
            g.line_width = 1.4;
            g.fill = 0.10;
            g.background = Some(if selected { pal.selection } else { mix(pal.card, Color32::BLACK, 0.1) });
            g.border = Some(if selected { with_alpha(c, 0.9) } else { with_alpha(pal.card_stroke, 0.9) });
            draw_graph(p, thumb, &g);
            if resp.clicked() {
                self.device = d;
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            y += 64.0;
        }
    }

    /// Title row: device name, then the segmented utilization bar with the value at the
    /// right. Returns the y below it.
    #[allow(clippy::too_many_arguments)]
    fn detail_title(&self, ui: &egui::Ui, rect: Rect, title: &str, value: &str, frac: f32, colors: [Color32; 3], fd: &FrameData<'_>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter();
        widgets::text_fit(p, pos2(rect.left(), rect.top() + 22.0), Align2::LEFT_CENTER, title, theme::display(29.0), pal.text, rect.width() - 140.0);
        let bar = Rect::from_min_max(pos2(rect.left(), rect.top() + 44.0), pos2(rect.right() - 128.0, rect.top() + 60.0));
        let segs = ((bar.width() / 9.0).floor() as usize).clamp(20, 200);
        led_gradient_row(p, bar, segs, frac, colors, fd.bloom);
        p.text(pos2(rect.right(), bar.center().y + 1.0), Align2::RIGHT_CENTER, value, theme::semibold(21.0), pal.accent);
        rect.top() + 64.0
    }

    /// Statistics block: a row of label/value pairs.
    fn stat_row(&self, ui: &egui::Ui, rect: Rect, y: f32, items: &[(&str, String)], fd: &FrameData<'_>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter();
        let w = rect.width() / items.len().max(4) as f32;
        for (i, (label, value)) in items.iter().enumerate() {
            let x = rect.left() + i as f32 * w;
            p.text(pos2(x, y), Align2::LEFT_TOP, label, theme::ui(11.5), pal.text_dim);
            widgets::text_fit(p, pos2(x, y + 15.0), Align2::LEFT_TOP, value, theme::ui(12.5), pal.text, w - 12.0);
        }
        y + 34.0
    }

    fn kv_grid(&self, ui: &egui::Ui, rect: Rect, y: f32, items: &[(&str, String)], fd: &FrameData<'_>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter();
        let cols = 2;
        let rows = items.len().div_ceil(cols);
        let col_w = rect.width() / cols as f32;
        for (i, (label, value)) in items.iter().enumerate() {
            let (c, r) = (i / rows, i % rows);
            let x = rect.left() + c as f32 * col_w;
            let yy = y + r as f32 * 19.0;
            p.text(pos2(x, yy), Align2::LEFT_TOP, format!("{label}:"), theme::ui(11.5), pal.text_dim);
            widgets::text_fit(p, pos2(x + 126.0, yy), Align2::LEFT_TOP, value, theme::ui(11.5), pal.text, col_w - 140.0);
        }
        y + rows as f32 * 19.0
    }

    fn cpu_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let green = fd.metric("cpu");
        if self.pro.cpu_details.is_none() {
            self.pro.cpu_details = Some(crate::pro::inventory::cpu_details());
        }
        self.pro.power.poll();
        if self.pro.power.stale(std::time::Duration::from_secs(60)) {
            self.pro.power.start(crate::pro::power::read);
        }
        let det = self.pro.cpu_details.clone().unwrap_or_default();
        let name = if s.cpu.name.is_empty() { "CPU".to_string() } else { s.cpu.name.clone() };
        let mut y = self.detail_title(ui, rect, &name, &format!("{:.1}%", s.cpu.total_pct), fd.smooth.cpu / 100.0, [green, fd.metric("yellow"), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, format!("{} logical processors", s.cpu.logical_count), theme::ui(12.0), pal.text_dim);
        y += 24.0;
        let expanded = self.cpus_expanded() && !fd.hist.per_core.is_empty();
        let graph_bottom = rect.bottom() - 246.0;
        let label = if expanded { "% Utilization by logical processor" } else { "% Utilization over time" };
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, label, theme::ui(12.0), pal.text_dim);
        let toggle_text = if expanded { "CPUs Expanded  \u{02C5}" } else { "CPUs Overall  \u{02C5}" };
        let tr = p.text(pos2(rect.right(), y), Align2::RIGHT_TOP, toggle_text, theme::ui(12.0), pal.accent);
        if ui.interact(tr.expand(3.0), ui.id().with("cpus-expanded"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.settings.window.performance_cpus_expanded = !expanded;
            self.settings_dirty = true;
        }
        y += 19.0;
        if expanded {
            let n = fd.hist.per_core.len();
            let cols = if n > 32 { 8 } else if n > 12 { 6 } else if n > 6 { 4 } else { n.max(1) };
            let rows = n.div_ceil(cols);
            let gap = 4.0;
            let cw = (rect.width() - gap * (cols as f32 - 1.0)) / cols as f32;
            let ch = ((graph_bottom - y - gap * (rows as f32 - 1.0)) / rows as f32).max(36.0);
            for (i, sr) in fd.hist.per_core.iter().enumerate() {
                let (c, r) = (i % cols, i / cols);
                let cell = Rect::from_min_size(pos2(rect.left() + c as f32 * (cw + gap), y + r as f32 * (ch + gap)), vec2(cw, ch));
                p.rect_filled(cell, 3.0, cell_bg(&pal));
                let mut lines = vec![GraphLine { series: sr, color: green, range: (0.0, 100.0) }];
                if let Some(k) = fd.hist.per_core_kernel.get(i) {
                    lines.push(GraphLine { series: k, color: Color32::from_rgb(0xFF, 0x6B, 0x5E), range: (0.0, 100.0) });
                }
                let mut g = self.graph(lines, fd);
                g.background = None;
                g.grid = Some(with_alpha(pal.grid, 0.9));
                g.px_per_update = (self.settings.graphs.pixels_per_update as f32 / 2.0).max(2.0);
                g.line_width = 1.6;
                g.fill = 0.10;
                draw_graph(&p, cell.shrink2(vec2(4.0, 4.0)).with_min_y(cell.top() + 18.0), &g);
                if fd.bloom > 0.0 {
                    p.rect_stroke(cell.expand(1.0), 4.0, Stroke::new(2.0_f32, with_alpha(pal.accent, 0.12 * fd.bloom)));
                }
                p.rect_stroke(cell, 3.0, Stroke::new(1.0_f32, pal.accent));
                p.text(cell.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("CPU {i}"), theme::ui(11.5), pal.text_dim);
            }
        } else {
            let gr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), graph_bottom));
            let mut g = self.graph(vec![GraphLine { series: &fd.hist.cpu, color: green, range: (0.0, 100.0) }, GraphLine { series: &fd.hist.kernel, color: fd.metric("kernel"), range: (0.0, 100.0) }], fd);
            g.border = Some(green);
            g.fill = 0.08;
            draw_graph(&p, gr, &g);
            p.text(pos2(gr.left() + 6.0, gr.top() + 4.0), Align2::LEFT_TOP, "100%", theme::ui(11.0), green);
            p.text(pos2(gr.left() + 6.0, gr.bottom() - 4.0), Align2::LEFT_BOTTOM, "0%", theme::ui(11.0), green);
            p.text(pos2(gr.right() - 6.0, gr.top() + 4.0), Align2::RIGHT_TOP, "Utilization  \u{2022}  Kernel", theme::ui(11.0), pal.text_dim);
        }
        y = graph_bottom + 24.0;
        let items = [
            ("Utilization", format!("{:.1}%", s.cpu.total_pct)),
            ("Speed", s.cpu.current_mhz.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or("N/A".into())),
            ("Processes", s.process_count.to_string()),
            ("Threads", s.thread_count.to_string()),
        ];
        y = self.stat_row(ui, rect, y, &items, fd);
        let p = ui.painter();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Up time", theme::ui(11.5), pal.text_dim);
        p.text(pos2(rect.left(), y + 15.0), Align2::LEFT_TOP, super::fmt_hms(s.uptime), theme::ui(12.5), pal.text);
        y += 38.0;
        let yes_no = |b: Option<bool>| b.map(|v| if v { "Yes" } else { "No" }.to_string()).unwrap_or("Unknown".into());
        let plan = self.pro.power.ok().map(|i| i.scheme_name.clone()).filter(|n| !n.is_empty()).unwrap_or("\u{2014}".into());
        let kv = [
            ("Fast Startup", yes_no(det.fast_startup)),
            ("Sockets", det.sockets.max(1).to_string()),
            ("Logical processors", s.cpu.logical_count.to_string()),
            ("Virtual machine", if det.virtual_machine { "Yes" } else { "No" }.into()),
            ("L2 cache", format_bytes(det.l2 as f64)),
            ("Kernel time", format!("{:.1}%", s.cpu.kernel_pct)),
            ("Power plan", plan),
            ("Base speed", s.cpu.max_mhz.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or("N/A".into())),
            ("Physical cores", det.cores.to_string()),
            ("Virtualization", det.virtualization.map(|v| if v { "Enabled" } else { "Disabled" }.to_string()).unwrap_or("Unknown".into())),
            ("L1 cache", format_bytes(det.l1 as f64)),
            ("L3 cache", format_bytes(det.l3 as f64)),
            ("Fastest core", s.cpu.per_core_mhz.iter().max().map(|m| format!("{:.2} GHz", *m as f32 / 1000.0)).unwrap_or("N/A".into())),
            ("Handles", s.handle_count.to_string()),
        ];
        self.kv_grid(ui, rect, y, &kv, fd);
    }

    fn memory_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let purple = fd.metric("memory");
        let mut y = self.detail_title(ui, rect, "Memory", &format!("{:.1}%", s.memory.used_pct), fd.smooth.memory / 100.0, [purple, theme::lighten(purple, 0.2), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, format!("{} total physical memory", format_bytes(s.memory.total as f64)), theme::ui(12.0), pal.text_dim);
        y += 24.0;
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Memory usage", theme::ui(12.0), pal.text_dim);
        p.text(pos2(rect.right(), y), Align2::RIGHT_TOP, format_bytes(s.memory.total as f64), theme::ui(12.0), pal.text_dim);
        y += 19.0;
        let gr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - 250.0));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.memory_bytes, color: purple, range: (0.0, s.memory.total.max(1) as f32) }], fd);
        g.border = Some(purple);
        g.fill = 0.14;
        draw_graph(&p, gr, &g);
        y = gr.bottom() + 16.0;
        // Composition bar: in use / cached / free.
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Memory composition", theme::ui(12.0), pal.text_dim);
        y += 18.0;
        let bar = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + 34.0));
        let total = s.memory.total.max(1) as f32;
        let used = s.memory.total.saturating_sub(s.memory.available) as f32;
        let cached = (s.memory.cached as f32).min(s.memory.available as f32);
        p.rect_filled(bar, 3.0, cell_bg(&pal));
        let wu = bar.width() * used / total;
        let wc = bar.width() * cached / total;
        p.rect_filled(Rect::from_min_size(bar.min, vec2(wu, bar.height())), 3.0, with_alpha(purple, 0.55));
        p.rect_filled(Rect::from_min_size(bar.min + vec2(wu, 0.0), vec2(wc, bar.height())), 0.0, with_alpha(purple, 0.22));
        p.rect_stroke(bar, 3.0, Stroke::new(1.0_f32, purple));
        p.line_segment([pos2(bar.left() + wu, bar.top()), pos2(bar.left() + wu, bar.bottom())], Stroke::new(1.0_f32, purple));
        y = bar.bottom() + 16.0;
        let items = [
            ("In use", format_bytes(used as f64)),
            ("Available", format_bytes(s.memory.available as f64)),
            ("Committed", format!("{} / {}", format_bytes(s.memory.commit_used as f64), format_bytes(s.memory.commit_limit as f64))),
            ("Cached", format_bytes(s.memory.cached as f64)),
        ];
        y = self.stat_row(ui, rect, y, &items, fd);
        let kv = [
            ("Page file size", format_bytes(s.memory.commit_limit.saturating_sub(s.memory.total) as f64)),
            ("Page file in use", s.memory.swap_used.map(|b| format_bytes(b as f64)).unwrap_or("N/A".into())),
            ("Source", "GlobalMemoryStatusEx, GetPerformanceInfo".into()),
            ("Compression", "not reported".into()),
        ];
        self.kv_grid(ui, rect, y + 6.0, &kv, fd);
    }

    fn gpu_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let blue = fd.metric("gpu");
        let mut y = self.detail_title(ui, rect, "GPU", &if s.gpu.available { format!("{:.1}%", s.gpu.utilization_pct) } else { "N/A".into() }, fd.smooth.gpu / 100.0, [blue, theme::lighten(blue, 0.3), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Busiest engine of the busiest adapter (PDH GPU Engine counters)", theme::ui(12.0), pal.text_dim);
        y += 24.0;
        let engines_h = (s.gpu.engines.len().min(6) as f32) * 26.0;
        let gr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - 110.0 - engines_h));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.gpu, color: blue, range: (0.0, 100.0) }], fd);
        g.border = Some(blue);
        g.fill = 0.10;
        draw_graph(&p, gr, &g);
        p.text(pos2(gr.left() + 6.0, gr.top() + 4.0), Align2::LEFT_TOP, "100%", theme::ui(11.0), blue);
        y = gr.bottom() + 14.0;
        if !s.gpu.available {
            p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "No GPU Engine performance counters were reported.", theme::ui(12.0), pal.warn);
            y += 20.0;
        }
        for (e, v) in s.gpu.engines.iter().take(6) {
            p.text(pos2(rect.left(), y + 6.0), Align2::LEFT_CENTER, e, theme::ui(12.0), pal.text_dim);
            let bar = Rect::from_min_max(pos2(rect.left() + 130.0, y), pos2(rect.right() - 60.0, y + 12.0));
            led_row(&p, bar, ((bar.width() / 9.0) as usize).clamp(10, 100), v / 100.0, &LedStyle { rim: false, rounding: 1.0, ..LedStyle::new(blue, fd.bloom) });
            p.text(pos2(rect.right(), y + 6.0), Align2::RIGHT_CENTER, format!("{v:.0}%"), theme::ui(12.0), pal.text);
            y += 26.0;
        }
        let items = [("Utilization", format!("{:.1}%", s.gpu.utilization_pct)), ("Dedicated memory in use", format_bytes(s.gpu.dedicated_used as f64)), ("Engines reported", s.gpu.engines.len().to_string())];
        self.stat_row(ui, rect, y, &items, fd);
    }

    fn disk_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let green = fd.metric("disk");
        let mut y = self.detail_title(ui, rect, "Disk", &format!("{:.1}%", s.disk.active_pct), fd.smooth.disk / 100.0, [green, fd.metric("yellow"), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Active time (all physical disks)", theme::ui(12.0), pal.text_dim);
        y += 19.0;
        let mid = y + (rect.bottom() - 110.0 - y) * 0.55;
        let g1 = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), mid - 24.0));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.disk, color: green, range: (0.0, 100.0) }], fd);
        g.border = Some(green);
        g.fill = 0.10;
        draw_graph(&p, g1, &g);
        p.text(pos2(g1.left() + 6.0, g1.top() + 4.0), Align2::LEFT_TOP, "100%", theme::ui(11.0), green);
        y = g1.bottom() + 8.0;
        let m = widgets::eased(ui.ctx(), "perf-disk-rate", fd.hist.disk_read.max().max(fd.hist.disk_write.max()).max(1024.0 * 1024.0));
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Transfer rate", theme::ui(12.0), pal.text_dim);
        p.text(pos2(rect.right(), y), Align2::RIGHT_TOP, format!("Read  \u{2022}  Write    scale {}", format_rate(m as f64)), theme::ui(11.5), pal.text_dim);
        y += 18.0;
        let g2 = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - 110.0));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.disk_read, color: green, range: (0.0, m) }, GraphLine { series: &fd.hist.disk_write, color: fd.metric("thermal"), range: (0.0, m) }], fd);
        g.border = Some(with_alpha(green, 0.7));
        draw_graph(&p, g2, &g);
        y = g2.bottom() + 14.0;
        let items = [("Active time", format!("{:.1}%", s.disk.active_pct)), ("Read speed", format_rate(s.disk.read_bps)), ("Write speed", format_rate(s.disk.write_bps)), ("Source", "PDH PhysicalDisk".into())];
        self.stat_row(ui, rect, y, &items, fd);
    }

    fn network_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let blue = fd.metric("network");
        let mut y = self.detail_title(ui, rect, "Network", &format_rate(s.network.rx_bps + s.network.tx_bps), fd.smooth.net / 100.0, [blue, theme::lighten(blue, 0.3), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        let m = widgets::eased(ui.ctx(), "perf-net-rate", fd.hist.net_rx.max().max(fd.hist.net_tx.max()).max(100.0 * 1024.0));
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "Throughput (GetIfTable2, all adapters)", theme::ui(12.0), pal.text_dim);
        p.text(pos2(rect.right(), y), Align2::RIGHT_TOP, format!("Receive  \u{2022}  Send    scale {}", format_rate(m as f64)), theme::ui(11.5), pal.text_dim);
        y += 19.0;
        let gr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - 110.0));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.net_rx, color: blue, range: (0.0, m) }, GraphLine { series: &fd.hist.net_tx, color: fd.metric("thermal"), range: (0.0, m) }], fd);
        g.border = Some(blue);
        g.fill = 0.10;
        draw_graph(&p, gr, &g);
        y = gr.bottom() + 14.0;
        let items = [
            ("Receive", format_rate(s.network.rx_bps)),
            ("Send", format_rate(s.network.tx_bps)),
            ("Link speed", format!("{:.1} Gb/s", s.network.link_bps / 1e9)),
            ("Utilization", format!("{:.2}%", s.network.utilization_pct)),
        ];
        self.stat_row(ui, rect, y, &items, fd);
    }

    fn thermal_detail(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let s = fd.snap;
        let orange = fd.metric("thermal");
        let mut y = self.detail_title(ui, rect, "Thermals", &s.thermal.max_c().map(|c| self.temp(c)).unwrap_or("N/A".into()), fd.smooth.thermal / 110.0, [fd.metric("cpu"), fd.metric("yellow"), fd.metric("clock")], fd);
        let p = ui.painter().clone();
        p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "ACPI thermal zones (firmware sensors, not the CPU package)", theme::ui(12.0), pal.text_dim);
        y += 19.0;
        let gr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - 120.0));
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.thermal, color: orange, range: (0.0, 110.0) }], fd);
        g.border = Some(orange);
        g.fill = 0.10;
        draw_graph(&p, gr, &g);
        p.text(pos2(gr.left() + 6.0, gr.top() + 4.0), Align2::LEFT_TOP, "110\u{b0}", theme::ui(11.0), orange);
        y = gr.bottom() + 14.0;
        if s.thermal.zones.is_empty() {
            p.text(pos2(rect.left(), y), Align2::LEFT_TOP, "This machine's firmware publishes no ACPI thermal zones. Vendor SDK sensors are not implemented.", theme::ui(12.0), pal.warn);
        } else {
            let items: Vec<(&str, String)> = s.thermal.zones.iter().map(|(z, c)| (z.as_str(), self.temp(*c))).collect();
            self.stat_row(ui, rect, y, &items, fd);
        }
    }
}
