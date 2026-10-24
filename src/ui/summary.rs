//! Summary page: is my PC healthy? A hero with the verdict, one card per resource, the apps
//! using the most and what started or ended lately. When the PC is under pressure it also says
//! what is going on and where the memory went.

use super::hardware::Hardware;
use super::health::{self, app_groups, level_of, AppGroup, Bottleneck, Health, Level, CPU_LEVELS, DISK_LEVELS, GPU_LEVELS, MEMORY_LEVELS};
use super::theme::{self, lighten, mix, with_alpha, Status};
use super::widgets::{self, Button, ButtonSize, Icon};
use super::{kit, App, Device, FrameData, Page};
use crate::metrics::{History, Series};
use crate::pro::recorder::EventKind;
use crate::process::{format_bytes, format_rate, Action, Column};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Space between the page's sections and between side-by-side cards.
const GAP_Y: f32 = 22.0;
const GAP_X: f32 = 14.0;
/// How long "This is expected" keeps the hero neutral.
const DISMISS: Duration = Duration::from_secs(30 * 60);
const RESOURCE_CARD_H: f32 = 163.0;
const CONSUMER_ROW_H: f32 = 44.0;
const EVENT_H: f32 = 46.0;

/// State of the Summary page.
#[derive(Default)]
pub struct SummaryState {
    /// The "What is my PC doing?" explanation is open.
    pub explain: bool,
    /// "This is expected": the hero stays neutral until then.
    pub dismissed_until: Option<Instant>,
    /// When the verdict became Critical.
    pub since: Option<Instant>,
    /// Level of CPU, memory, disk and GPU, and since when it has been that.
    levels: [(Level, Option<Instant>); 4],
}

/// Where a click on the page leads, applied after the page is drawn.
enum Go {
    Device(Device),
    Processes,
    /// Open Processes with this process selected.
    Select(u32, i64),
    /// Open Processes sorted by this column, busiest first.
    Sort(Column),
    /// Open Processes filtered by this image name.
    See(String),
    /// Ask to end the process tree of this row.
    End(usize),
    ToggleExplain,
    Dismiss,
}

/// Everything the sections need to know about the verdict.
struct Verdict {
    health: Health,
    /// Level of CPU, memory, disk and GPU.
    levels: [Level; 4],
    /// Critical and not dismissed: tinted hero, warm backdrop.
    pressure: bool,
    /// The explanation sections are shown.
    explain: bool,
}

impl App {
    pub fn summary_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let now = Instant::now();
        let v = self.verdict(fd, now);
        let area = ui.max_rect();
        if let (true, Some([hi, mid, edge])) = (v.pressure, pal.backdrop) {
            // The page paints its own, warmer backdrop over the whole panel (margins included).
            let panel = egui::Rect::from_min_max(area.min - vec2(8.0, 12.0), area.max + vec2(32.0, 36.0));
            widgets::fill_radial_backdrop(ui.painter(), panel, mix(hi, theme::RED, 0.25), mid, edge);
        }
        let groups = app_groups(fd.rows, fd.show_self);
        let n_rows = ((area.height() - 540.0) / CONSUMER_ROW_H).floor().clamp(4.0, 6.0) as usize;
        let mut go = None;
        egui::ScrollArea::vertical().id_salt("summary-scroll").auto_shrink([false, false]).show(ui, |ui| {
            let top = ui.cursor().min;
            let w = ui.available_width();
            let mut y = top.y;
            y += self.hero(ui, fd, top.x, y, w, &v, &mut go) + GAP_Y;
            // When something is wrong the four tiles of the explanation stand in for the cards.
            let tiles = v.health.level != Level::Good;
            if v.explain {
                y += self.doing_card(ui, fd, top.x, y, w, &v, &groups, &mut go) + GAP_Y;
                y += self.memory_row(ui, fd, top.x, y, w, &v, &groups, tiles, &mut go) + GAP_Y;
            }
            if !(v.explain && tiles) {
                y += self.resource_cards(ui, fd, top.x, y, w, &v, &mut go) + GAP_Y;
            }
            y += self.consumers_row(ui, fd, top.x, y, w, &groups, n_rows, &mut go);
            ui.allocate_rect(Rect::from_min_size(top, vec2(w, y - top.y)), Sense::hover());
        });
        self.go(go, fd, now);
    }

    /// The verdict, with the bookkeeping of how long each level has lasted.
    fn verdict(&mut self, fd: &FrameData<'_>, now: Instant) -> Verdict {
        let s = fd.snap;
        let levels = [
            health::settled(level_of(s.cpu.total_pct, CPU_LEVELS), fd.lasting.cpu),
            level_of(s.memory.used_pct, MEMORY_LEVELS),
            health::settled(level_of(s.disk.active_pct, DISK_LEVELS), fd.lasting.disk),
            if s.gpu.available { level_of(s.gpu.utilization_pct, GPU_LEVELS) } else { Level::Good },
        ];
        let st = &mut self.summary;
        for (slot, level) in st.levels.iter_mut().zip(levels) {
            if slot.0 != level || slot.1.is_none() {
                *slot = (level, Some(now));
            }
        }
        let health = health::health(s, fd.rows, st.since, fd.lasting);
        if health.level == Level::Critical {
            st.since.get_or_insert(now);
        } else {
            st.since = None;
        }
        if st.dismissed_until.is_some_and(|t| now >= t) {
            st.dismissed_until = None;
        }
        let dismissed = st.dismissed_until.is_some();
        let pressure = health.level == Level::Critical && !dismissed;
        Verdict { health, levels, pressure, explain: st.explain || pressure }
    }

    /// Applies the click the page recorded.
    fn go(&mut self, go: Option<Go>, fd: &FrameData<'_>, now: Instant) {
        let Some(go) = go else { return };
        match go {
            Go::Device(d) => {
                self.device = d;
                self.page = Page::Performance;
            }
            Go::Processes => self.page = Page::Processes,
            Go::Select(pid, created) => {
                self.selected = Some((pid, created));
                self.page = Page::Processes;
            }
            Go::Sort(col) => {
                self.sort = (col, true);
                self.page = Page::Processes;
            }
            Go::See(name) => {
                self.filter = name;
                self.page = Page::Processes;
            }
            Go::End(i) => {
                if let Some(row) = fd.rows.get(i) {
                    self.request(Action::EndTree, row.clone());
                }
            }
            Go::ToggleExplain => self.summary.explain = !self.summary.explain,
            Go::Dismiss => {
                self.summary.dismissed_until = Some(now + DISMISS);
                self.summary.explain = false;
            }
        }
    }

    // ------------------------------------------------------------------ hero

    /// The health hero. Returns its height.
    #[allow(clippy::too_many_arguments)]
    fn hero(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, v: &Verdict, go: &mut Option<Go>) -> f32 {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let h = &v.health;
        let p = ui.painter().clone();
        let (pad_x, pad_y, ring) = (36.0, 28.0, 150.0);
        let inner_w = w - 2.0 * pad_x;
        let key_color = level_color(&st, h.level);
        // What the button does.
        let (label, primary) = match (v.pressure, h.bottleneck) {
            (true, Some(b)) => (format!("Show {} users \u{2192}", noun(b)), true),
            _ => ("What is my PC doing? \u{2192}".to_string(), false),
        };
        let bw = widgets::text_width(&p, &label, theme::semibold(13.0)) + 40.0;
        let wide = inner_w - ring - 36.0 - bw - 36.0 >= 320.0;
        let text_w = if wide { inner_w - ring - 36.0 - bw - 36.0 } else { (inner_w - ring - 36.0).max(200.0) };

        // Headline with the key word highlighted, the detail and the legend.
        let (pre, post) = match h.headline.find(h.emphasis) {
            Some(i) => (&h.headline[..i], &h.headline[i + h.emphasis.len()..]),
            None => (h.headline.as_str(), ""),
        };
        let h1 = rich(&p, &[(pre, theme::light(44.0), pal.text), (h.emphasis, theme::ui(44.0), key_color), (post, theme::light(44.0), pal.text)], text_w, 46.0);
        let para = rich(&p, &[(h.detail.as_str(), theme::ui(15.0), pal.text_dim)], text_w.min(580.0), 22.5);
        let cpu_c = ring_color(&st, pal.accent, v.levels[0]);
        let mem_c = ring_color(&st, fd.metric("memory"), v.levels[1]);
        let disk_c = ring_color(&st, fd.metric("disk"), v.levels[2]);
        let gpu_c = ring_color(&st, fd.metric("gpu"), v.levels[3]);
        let s = fd.snap;
        let gpu_text = if s.gpu.available { format!("GPU {:.0}%", s.gpu.utilization_pct) } else { "GPU \u{2014}".to_string() };
        let items = [
            (format!("CPU {:.0}%", s.cpu.total_pct), cpu_c, v.levels[0]),
            (format!("Memory {:.0}%", s.memory.used_pct), mem_c, v.levels[1]),
            (format!("Disk {:.0}%", s.disk.active_pct), disk_c, v.levels[2]),
            (gpu_text, gpu_c, v.levels[3]),
        ];
        let font = theme::ui(12.0);
        let widths: Vec<f32> = items.iter().map(|(t, _, l)| 16.0 + widgets::text_width(&p, t, font.clone()) + if *l == Level::Good { 0.0 } else { 14.0 }).collect();
        let legend_w = widths.iter().sum::<f32>() + 22.0 * 3.0;
        let legend_rows = if legend_w <= text_w { 1 } else { 2 };
        let legend_h = legend_rows as f32 * 16.0 + (legend_rows - 1) as f32 * 6.0;

        let text_h = 14.0 + 8.0 + h1.size().y + 12.0 + para.size().y + 18.0 + legend_h;
        let main_h = ring.max(text_h);
        let content_h = main_h + if wide { 0.0 } else { 16.0 + 44.0 };
        let rect = Rect::from_min_size(pos2(x, y), vec2(w, content_h + 2.0 * pad_y));
        if v.pressure {
            widgets::tinted_card(&p, rect, &pal, st.danger);
        } else {
            widgets::card(&p, rect, &pal);
        }

        // The four rings with the verdict's glyph in the middle.
        let cx = rect.left() + pad_x + ring / 2.0;
        let cy = rect.top() + pad_y + main_h / 2.0;
        let k = ring / 120.0;
        let frac = |f: f32| (f / 100.0).clamp(0.0, 1.0);
        let fracs = [(frac(fd.smooth.cpu), cpu_c), (frac(fd.smooth.memory), mem_c), (frac(fd.smooth.disk), disk_c), (frac(fd.smooth.gpu), gpu_c)];
        widgets::rings(&p, pos2(cx, cy), &[52.0 * k, 44.0 * k, 36.0 * k, 28.0 * k], 5.0 * k, &fracs, with_alpha(pal.text, 0.07));
        let (glyph, glyph_c, glyph_s) = match h.level {
            Level::Good => (Icon::Check, st.ok, 26.0),
            Level::Elevated => (Icon::Alert, st.warn, 30.0),
            Level::Critical => (Icon::Alert, st.danger, 30.0),
        };
        widgets::draw_icon(&p, Rect::from_center_size(pos2(cx, cy), vec2(glyph_s, glyph_s)), glyph, glyph_c);

        // Text column.
        let tx = rect.left() + pad_x + ring + 36.0;
        let mut ty = rect.top() + pad_y + (main_h - text_h) / 2.0;
        let label_text = if v.pressure { "SYSTEM HEALTH \u{b7} PRESSURE" } else { "SYSTEM HEALTH" };
        widgets::tracked_text(&p, pos2(tx, ty), Align2::LEFT_TOP, label_text, theme::ui(11.0), pal.text_faint, 0.16);
        ty += 14.0 + 8.0;
        p.galley(pos2(tx, ty), h1.clone(), pal.text);
        ty += h1.size().y + 12.0;
        p.galley(pos2(tx, ty), para.clone(), pal.text_dim);
        ty += para.size().y + 18.0;
        let mut lx = tx;
        let mut ly = ty + 8.0;
        for (i, ((text, color, level), iw)) in items.iter().zip(&widths).enumerate() {
            if legend_rows == 2 && i == 2 {
                lx = tx;
                ly += 22.0;
            }
            p.circle_filled(pos2(lx + 4.0, ly), 4.0, *color);
            let r = p.text(pos2(lx + 16.0, ly), Align2::LEFT_CENTER, text, font.clone(), pal.text_dim);
            marker(&p, pos2(r.right() + 10.0, ly), *level, *color);
            lx += iw + 22.0;
        }

        // The button: at the right, or under the text when the card is narrow.
        let at = if wide { pos2(rect.right() - pad_x - bw, cy - 22.0) } else { pos2(tx, rect.top() + pad_y + main_h + 16.0) };
        let clicked = kit::row_ui(ui, Rect::from_min_size(at, vec2(bw + 4.0, 44.0)), |ui| {
            let b = if primary { Button::primary(&label) } else { Button::new(&label) };
            b.size(ButtonSize::Hero).show(ui, &pal).clicked()
        });
        if clicked {
            *go = Some(match (primary, h.bottleneck) {
                (true, Some(Bottleneck::Memory)) => Go::Sort(Column::Memory),
                (true, Some(Bottleneck::Cpu)) => Go::Sort(Column::Cpu),
                (true, Some(Bottleneck::Disk)) => Go::Sort(Column::Disk),
                _ => Go::ToggleExplain,
            });
        }
        rect.height()
    }

    // ------------------------------------------------------------------ resource cards

    /// One card per resource; a click opens its Performance page. Returns the height.
    #[allow(clippy::too_many_arguments)]
    fn resource_cards(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, v: &Verdict, go: &mut Option<Go>) -> f32 {
        let pal = fd.pal;
        let s = fd.snap;
        let p = ui.painter().clone();
        let net = net_total(fd.hist);
        let net_peak = net.max().max(1024.0 * 100.0);
        let frac = |f: f32| (f / 100.0).clamp(0.0, 1.0);
        let temp = s.thermal.max_c().map(|c| self.temp(c));
        let ghz = s.cpu.current_mhz.map(|m| format!("{:.2} GHz", m as f32 / 1000.0));
        let cpu_footer = match (ghz, temp) {
            (Some(g), Some(t)) => format!("{g} \u{b7} {t}"),
            (Some(g), None) => g,
            (None, Some(t)) => t,
            (None, None) => "\u{2014}".to_string(),
        };
        let gpu_footer = if s.gpu.available || s.gpu.dedicated_used > 0 { format!("VRAM {}", format_bytes(s.gpu.dedicated_used as f64)) } else { "No GPU engine counters".to_string() };
        let (net_value, net_unit) = split_unit(&format_rate(s.network.rx_bps + s.network.tx_bps));
        let gpu_value = if s.gpu.available { (format!("{:.0}", s.gpu.utilization_pct), "%".to_string()) } else { ("\u{2014}".to_string(), String::new()) };
        let st = theme::status(&pal);
        let lc = |base: Color32, l: Level| ring_color(&st, base, l);
        let cards = [
            Res {
                label: "CPU",
                device: Device::Cpu,
                frac: frac(fd.smooth.cpu),
                color: lc(pal.accent, v.levels[0]),
                value: (format!("{:.0}", s.cpu.total_pct), "%".into()),
                hardware: Hardware::Cpu,
                lines: vec![(&fd.hist.cpu, lc(pal.accent, v.levels[0]), (0.0, 100.0))],
                footer: cpu_footer,
            },
            Res {
                label: "Memory",
                device: Device::Memory,
                frac: frac(fd.smooth.memory),
                color: lc(fd.metric("memory"), v.levels[1]),
                value: (format!("{:.0}", s.memory.used_pct), "%".into()),
                hardware: Hardware::Memory,
                lines: vec![(&fd.hist.memory, lc(fd.metric("memory"), v.levels[1]), (0.0, 100.0))],
                footer: format!("{} / {}", format_bytes(s.memory.total.saturating_sub(s.memory.available) as f64), format_bytes(s.memory.total as f64)),
            },
            Res {
                label: "Disk",
                device: Device::Disk,
                frac: frac(fd.smooth.disk),
                color: lc(fd.metric("disk"), v.levels[2]),
                value: (format!("{:.0}", s.disk.active_pct), "%".into()),
                hardware: Hardware::Disk,
                lines: vec![(&fd.hist.disk, lc(fd.metric("disk"), v.levels[2]), (0.0, 100.0))],
                footer: format!("R {} \u{b7} W {}", format_rate(s.disk.read_bps), format_rate(s.disk.write_bps)),
            },
            Res {
                label: "GPU",
                device: Device::Gpu,
                frac: frac(fd.smooth.gpu),
                color: lc(fd.metric("gpu"), v.levels[3]),
                value: gpu_value,
                hardware: Hardware::Gpu,
                lines: vec![(&fd.hist.gpu, lc(fd.metric("gpu"), v.levels[3]), (0.0, 100.0))],
                footer: gpu_footer,
            },
            Res {
                label: "Network",
                device: Device::Network,
                frac: frac(fd.smooth.net),
                color: fd.metric("network"),
                value: (net_value, net_unit),
                hardware: Hardware::Network,
                lines: vec![(&net, fd.metric("network"), (0.0, net_peak))],
                footer: format!("\u{2193} {} \u{b7} \u{2191} {}", format_rate(s.network.rx_bps), format_rate(s.network.tx_bps)),
            },
        ];
        let cols = (((w + GAP_X) / (200.0 + GAP_X)).floor() as usize).clamp(1, cards.len());
        let cw = (w - GAP_X * (cols - 1) as f32) / cols as f32;
        let rows = cards.len().div_ceil(cols);
        for (i, c) in cards.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let r = Rect::from_min_size(pos2(x + col as f32 * (cw + GAP_X), y + row as f32 * (RESOURCE_CARD_H + GAP_X)), vec2(cw, RESOURCE_CARD_H));
            let resp = ui.interact(r, ui.id().with(("res", c.label)), Sense::click());
            widgets::card(&p, r, &pal);
            if resp.hovered() {
                p.rect_stroke(r, 10.0, Stroke::new(1.0_f32, pal.raise(0.14)));
            }
            let ring_c = pos2(r.left() + 20.0 + 26.0, r.top() + 20.0 + 26.0);
            widgets::ring(&p, ring_c, 27.0 * 52.0 / 64.0, 4.0 * 52.0 / 64.0, c.frac, c.color, with_alpha(pal.text, 0.08));
            let tx = r.left() + 20.0 + 52.0 + 14.0;
            widgets::tracked_text(&p, pos2(tx, r.top() + 21.0), Align2::LEFT_TOP, &c.label.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.14);
            let num = p.text(pos2(tx, r.top() + 34.0), Align2::LEFT_TOP, &c.value.0, theme::light(30.0), pal.text);
            let mut end = num.right();
            if !c.value.1.is_empty() {
                end = p.text(pos2(num.right() + 3.0, num.bottom() - 7.0), Align2::LEFT_BOTTOM, &c.value.1, theme::ui(16.0), pal.text_faint).right();
            }
            let icon = Rect::from_min_size(pos2(r.right() - 20.0 - 72.0, r.top() + 22.0), vec2(72.0, 48.0));
            if end + 4.0 < icon.left() + 14.0 {
                super::hardware::draw(&p, icon, c.hardware, pal.text_rail());
            }
            let spark = Rect::from_min_size(pos2(r.left() + 20.0, r.top() + 20.0 + 52.0 + 10.0), vec2(r.width() - 40.0, 40.0));
            if self.settings.graphs.dashed_summary {
                widgets::sparkline_dashed(&p, spark, &c.lines);
            } else {
                widgets::sparkline(&p, spark, &c.lines, 0.12, false);
            }
            widgets::text_fit(&p, pos2(r.left() + 20.0, spark.bottom() + 10.0), Align2::LEFT_TOP, &c.footer, theme::mono(12.0), pal.text_faint, r.width() - 40.0);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *go = Some(Go::Device(c.device));
            }
        }
        rows as f32 * RESOURCE_CARD_H + (rows - 1) as f32 * GAP_X
    }

    // ------------------------------------------------------------------ top consumers and timeline

    /// Top consumers and the timeline, side by side when there is room. Returns the height.
    #[allow(clippy::too_many_arguments)]
    fn consumers_row(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, groups: &[AppGroup], n_rows: usize, go: &mut Option<Go>) -> f32 {
        let mut order: Vec<&AppGroup> = groups.iter().collect();
        order.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
        order.truncate(n_rows);
        let wide = w >= 760.0;
        let (lw, rw) = if wide { ((w - GAP_X) * 0.6, (w - GAP_X) * 0.4) } else { (w, w) };
        let top_h = 40.0 + 32.0 + order.len().max(1) as f32 * CONSUMER_ROW_H + 18.0;
        let events: Vec<_> = self.recorder.events.iter().rev().take(n_rows).cloned().collect();
        let fit = (((if wide { top_h } else { 40.0 + n_rows as f32 * EVENT_H + 18.0 }) - 58.0) / EVENT_H).floor().max(1.0) as usize;
        let shown = events.len().min(fit);
        let tl_h = if wide { top_h } else { 40.0 + shown.max(1) as f32 * EVENT_H + 18.0 };
        let top = Rect::from_min_size(pos2(x, y), vec2(lw, top_h));
        self.top_consumers(ui, fd, top, &order, go);
        let tl = if wide { Rect::from_min_size(pos2(x + lw + GAP_X, y), vec2(rw, tl_h)) } else { Rect::from_min_size(pos2(x, y + top_h + GAP_X), vec2(rw, tl_h)) };
        self.timeline(ui, fd, tl, &events[..shown]);
        if wide {
            top_h
        } else {
            top_h + GAP_X + tl_h
        }
    }

    fn top_consumers(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>, rect: Rect, order: &[&AppGroup], go: &mut Option<Go>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let c = kit::section_card(&p, rect, &pal, "Top consumers", "");
        // "All processes" link at the right of the heading.
        let link = p.text(pos2(rect.right() - 22.0, rect.top() + 15.0), Align2::RIGHT_TOP, "All processes \u{2192}", theme::ui(13.0), st.info);
        let lr = ui.interact(link.expand(4.0), ui.id().with("all-processes"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if lr.hovered() {
            p.text(link.right_top(), Align2::RIGHT_TOP, "All processes \u{2192}", theme::ui(13.0), lighten(st.info, 0.25));
        }
        if lr.clicked() {
            *go = Some(Go::Processes);
        }
        // Columns from the right: GPU, Disk, Memory, CPU; the narrow card drops the first two.
        let mut cols: Vec<(&str, f32)> = vec![("CPU", 150.0), ("Memory", 92.0)];
        if c.width() >= 520.0 {
            cols.push(("Disk", 92.0));
            cols.push(("GPU", 64.0));
        }
        let table_w: f32 = cols.iter().map(|(_, w)| w).sum();
        let mut edges = Vec::new(); // right edge of each column
        let mut re = c.right();
        for (_, cw) in cols.iter().rev() {
            edges.push(re);
            re -= cw;
        }
        edges.reverse();
        let app_w = (c.width() - table_w).max(120.0);
        tracked_header(&p, pos2(c.left(), c.top() + 16.0), "APP", Align2::LEFT_CENTER, &pal);
        for ((title, _), e) in cols.iter().zip(&edges) {
            tracked_header(&p, pos2(e - 12.0, c.top() + 16.0), &title.to_uppercase(), Align2::RIGHT_CENTER, &pal);
        }
        if order.is_empty() {
            kit::empty_state(&p, Rect::from_min_max(pos2(c.left(), c.top() + 32.0), c.max), &pal, "No processes");
            return;
        }
        let busy_total = fd.snap.cpu.total_pct;
        for (i, g) in order.iter().enumerate() {
            let ry = c.top() + 32.0 + i as f32 * CONSUMER_ROW_H;
            let row = Rect::from_min_size(pos2(c.left(), ry), vec2(c.width(), CONSUMER_ROW_H));
            let resp = ui.interact(row, ui.id().with(("consumer", &g.key)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() {
                p.rect_filled(row.shrink2(vec2(-8.0, 1.0)), 6.0, pal.raise(0.04));
            }
            p.line_segment([pos2(row.left(), ry + 0.5), pos2(row.right(), ry + 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
            let cy = row.center().y;
            let tex = g.icon.and_then(|i| fd.rows.get(i)).and_then(|r| self.icons.for_process(r.pid, r.path.as_deref()));
            let icon_rect = Rect::from_center_size(pos2(row.left() + 14.0, cy), vec2(28.0, 28.0));
            if g.system && tex.is_none() {
                p.rect_filled(icon_rect, 6.0, pal.raise(0.08));
            }
            widgets::icon_box(&p, icon_rect, &pal, tex, g.name.chars().next().unwrap_or('?'));
            let sub = if g.count == 1 { "1 process".to_string() } else { format!("{} processes", g.count) };
            let sub_w = widgets::text_width(&p, &sub, theme::ui(12.0));
            let name = widgets::text_fit(&p, pos2(row.left() + 40.0, cy), Align2::LEFT_CENTER, &g.name, theme::semibold(13.0), pal.text, (app_w - 40.0 - sub_w - 20.0).max(40.0));
            p.text(pos2(name.right() + 8.0, cy), Align2::LEFT_CENTER, sub, theme::ui(12.0), pal.text_faint);
            for (ci, ((title, _), e)) in cols.iter().zip(&edges).enumerate() {
                let (text, dim) = match *title {
                    "CPU" => (format!("{:.1}%", g.cpu), false),
                    "Memory" => (format_bytes(g.memory as f64), false),
                    "Disk" if g.io_bps >= 1.0 => (format_rate(g.io_bps), false),
                    "GPU" if g.gpu >= 0.05 => (format!("{:.0}%", g.gpu), false),
                    _ => ("\u{2014}".to_string(), true),
                };
                let color = if dim { pal.text_faint } else { pal.text_table() };
                let tr = p.text(pos2(e - 12.0, cy), Align2::RIGHT_CENTER, text, theme::mono(12.0), color);
                if ci == 0 {
                    let share = (g.cpu / busy_total.max(g.cpu).max(0.1)).clamp(0.0, 1.0);
                    widgets::bar(&p, Rect::from_min_size(pos2(tr.left() - 10.0 - 60.0, cy - 1.5), vec2(60.0, 3.0)), share, pal.accent);
                }
            }
            if resp.clicked() {
                if let Some(r) = fd.rows.get(g.top) {
                    *go = Some(Go::Select(r.pid, r.create_time));
                }
            }
        }
    }

    fn timeline(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, rect: Rect, events: &[crate::pro::recorder::Event]) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let c = kit::section_card(&p, rect, &pal, "Timeline", "");
        if events.is_empty() {
            kit::empty_state(&p, c, &pal, "Process starts and exits appear here");
            return;
        }
        for (i, e) in events.iter().enumerate() {
            let ey = c.top() + 4.0 + i as f32 * EVENT_H;
            let (color, text) = match e.kind {
                EventKind::Started => (st.ok, format!("{} started", strip_pid(&e.text))),
                EventKind::Exited => (st.danger, format!("{} exited", strip_pid(&e.text))),
                EventKind::Note => (st.info, e.text.clone()),
            };
            p.circle_filled(pos2(c.left() + 4.0, ey + 9.0), 4.0, color);
            widgets::text_fit(&p, pos2(c.left() + 22.0, ey), Align2::LEFT_TOP, &text, theme::ui(13.0), pal.text_table(), c.width() - 22.0);
            let time = e.time.rsplit(' ').next().unwrap_or(&e.time);
            p.text(pos2(c.left() + 22.0, ey + 20.0), Align2::LEFT_TOP, time, theme::mono(11.0), pal.text_faint);
        }
    }

    // ------------------------------------------------------------------ pressure: what is going on

    /// "What is my PC doing?": the app behind the pressure and what to do about it.
    #[allow(clippy::too_many_arguments)]
    fn doing_card(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, v: &Verdict, groups: &[AppGroup], go: &mut Option<Go>) -> f32 {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let s = fd.snap;
        let lead = leader(groups, v.health.bottleneck);
        let hot = if v.levels[1] == Level::Critical { st.danger_text } else { st.warn };
        let (pad_x, pad_y) = (32.0, 26.0);
        let text_w = (w - 2.0 * pad_x).min(860.0);
        let l = theme::light(22.0);
        let m = theme::semibold(22.0);
        let procs = |g: &AppGroup| if g.count == 1 { "1 process".to_string() } else { format!("{} processes", g.count) };
        let sentence = match lead {
            Some(g) => match v.health.bottleneck {
                Some(Bottleneck::Memory) => vec![
                    (g.name.clone(), m.clone(), pal.text),
                    (" is holding ".into(), l.clone(), pal.text),
                    (format_bytes(g.memory as f64), m.clone(), hot),
                    (" of memory".into(), l.clone(), pal.text),
                    (if g.count > 1 { format!(" across {}", procs(g)) } else { String::new() }, l.clone(), pal.text),
                    (".".into(), l.clone(), pal.text),
                ],
                Some(Bottleneck::Cpu) => vec![
                    (g.name.clone(), m.clone(), pal.text),
                    (" is using ".into(), l.clone(), pal.text),
                    (format!("{:.0}%", g.cpu), m.clone(), hot),
                    (" of the processor".into(), l.clone(), pal.text),
                    (if g.count > 1 { format!(" across {}", procs(g)) } else { String::new() }, l.clone(), pal.text),
                    (".".into(), l.clone(), pal.text),
                ],
                Some(Bottleneck::Disk) => vec![
                    (g.name.clone(), m.clone(), pal.text),
                    (" is reading and writing ".into(), l.clone(), pal.text),
                    (format_rate(g.io_bps), m.clone(), hot),
                    (if g.count > 1 { format!(" across {}", procs(g)) } else { String::new() }, l.clone(), pal.text),
                    (".".into(), l.clone(), pal.text),
                ],
                None => vec![
                    (g.name.clone(), m.clone(), pal.text),
                    (" is the busiest app, using ".into(), l.clone(), pal.text),
                    (format!("{:.0}%", g.cpu), m.clone(), pal.text),
                    (" of the processor and ".into(), l.clone(), pal.text),
                    (format_bytes(g.memory as f64), m.clone(), pal.text),
                    (" of memory.".into(), l.clone(), pal.text),
                ],
            },
            None => vec![("No app is using noticeable processor time right now.".into(), l.clone(), pal.text)],
        };
        let parts: Vec<(&str, FontId, Color32)> = sentence.iter().map(|(t, f, c)| (t.as_str(), f.clone(), *c)).collect();
        let big = rich(&p, &parts, text_w, 31.0);
        let second = match v.health.bottleneck {
            Some(Bottleneck::Memory) if v.levels[2] != Level::Good => Some(format!("Disk activity is {:.0}% because Windows is writing memory to disk.", s.disk.active_pct)),
            None => Some("No resource is under pressure.".to_string()),
            _ => None,
        };
        let para = second.as_deref().map(|t| rich(&p, &[(t, theme::ui(15.0), pal.text_dim)], text_w.min(820.0), 22.5));
        let h = pad_y + 13.0 + 16.0 + big.size().y + para.as_ref().map_or(0.0, |g| 16.0 + g.size().y) + 20.0 + 44.0 + pad_y;
        let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
        widgets::card(&p, rect, &pal);
        let mut cy = rect.top() + pad_y;
        widgets::tracked_text(&p, pos2(rect.left() + pad_x, cy), Align2::LEFT_TOP, "WHAT IS MY PC DOING?", theme::semibold(11.0), pal.text_faint, 0.16);
        cy += 13.0 + 16.0;
        p.galley(pos2(rect.left() + pad_x, cy), big.clone(), pal.text);
        cy += big.size().y;
        if let Some(g) = &para {
            cy += 16.0;
            p.galley(pos2(rect.left() + pad_x, cy), g.clone(), pal.text_dim);
            cy += g.size().y;
        }
        cy += 20.0;
        let row = Rect::from_min_size(pos2(rect.left() + pad_x, cy), vec2(rect.width() - 2.0 * pad_x, 44.0));
        kit::row_ui(ui, row, |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if let Some(g) = lead {
                let short = clip(&g.name, 26);
                if Button::new(&format!("See {short}'s processes")).size(ButtonSize::Hero).show(ui, &pal).clicked() {
                    *go = Some(Go::See(g.key.clone()));
                }
                if v.health.level != Level::Good && Button::new(&format!("End {short}\u{2026}")).size(ButtonSize::Hero).show(ui, &pal).clicked() {
                    *go = Some(Go::End(g.root));
                }
            }
            let (label, action) = if v.health.level == Level::Good { ("Hide", Go::ToggleExplain) } else { ("This is expected", Go::Dismiss) };
            if Button::ghost(label).size(ButtonSize::Hero).show(ui, &pal).clicked() {
                *go = Some(action);
            }
        });
        h
    }

    /// "Where the memory went" and the four resource tiles. Returns the height.
    #[allow(clippy::too_many_arguments)]
    fn memory_row(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, v: &Verdict, groups: &[AppGroup], with_tiles: bool, go: &mut Option<Go>) -> f32 {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let s = fd.snap;
        let wide = w >= 760.0 && with_tiles;
        let (lw, rw) = if wide { ((w - GAP_X) * 0.6, (w - GAP_X) * 0.4) } else { (w, w) };

        // Memory by who holds it.
        let top = leader(groups, Some(Bottleneck::Memory));
        let held = |pred: &dyn Fn(&AppGroup) -> bool| groups.iter().filter(|g| top.is_none_or(|t| t.key != g.key) && pred(g)).map(|g| g.memory).sum::<u64>();
        let apps = held(&|g| g.has_window);
        let windows = held(&|g| !g.has_window);
        let cached = s.memory.cached.min(s.memory.available);
        let free = s.memory.available.saturating_sub(cached);
        let top_color = match v.levels[1] {
            Level::Critical => st.danger,
            Level::Elevated => st.warn,
            Level::Good => st.info,
        };
        let parts = [
            (top.map_or("Largest app".to_string(), |g| clip(&g.name, 22)), top.map_or(0, |g| g.memory), top_color, pal.text),
            ("Other apps".to_string(), apps, theme::darken_for(&pal, theme::INDIGO), pal.text),
            ("Windows".to_string(), windows, theme::darken_for(&pal, theme::PURPLE), pal.text),
            ("Cache".to_string(), cached, with_alpha(pal.text, 0.22), pal.text),
            ("Free".to_string(), free, with_alpha(st.ok, 0.4), st.ok),
        ];
        let inner_w = lw - 52.0;
        let gcols = (((inner_w + GAP_X) / (120.0 + GAP_X)).floor() as usize).clamp(1, parts.len());
        let grows = parts.len().div_ceil(gcols);
        let left_h = 22.0 + 13.0 + 16.0 + 14.0 + 16.0 + grows as f32 * 48.0 + (grows - 1) as f32 * GAP_X + 22.0;
        let tile_h = 113.0;
        let right_h = 2.0 * tile_h + GAP_X;
        let h = if wide { left_h.max(right_h) } else { left_h };

        let left = Rect::from_min_size(pos2(x, y), vec2(lw, h));
        widgets::card(&p, left, &pal);
        widgets::tracked_text(&p, pos2(left.left() + 26.0, left.top() + 22.0), Align2::LEFT_TOP, "WHERE THE MEMORY WENT", theme::semibold(11.0), pal.text_faint, 0.16);
        p.text(pos2(left.right() - 26.0, left.top() + 22.0), Align2::RIGHT_TOP, format!("{} installed", format_bytes(s.memory.total as f64)), theme::mono(12.0), pal.text_faint);
        let bar = Rect::from_min_size(pos2(left.left() + 26.0, left.top() + 22.0 + 13.0 + 16.0), vec2(inner_w, 14.0));
        let weights: Vec<(f32, Color32)> = parts.iter().map(|(_, b, c, _)| (*b as f32, *c)).collect();
        widgets::share_bar(&p, bar, &weights, 3.0);
        let cw = (inner_w - GAP_X * (gcols - 1) as f32) / gcols as f32;
        for (i, (label, bytes, color, value_color)) in parts.iter().enumerate() {
            let (col, row) = (i % gcols, i / gcols);
            let cx = left.left() + 26.0 + col as f32 * (cw + GAP_X);
            let cy = bar.bottom() + 16.0 + row as f32 * (48.0 + GAP_X);
            p.circle_filled(pos2(cx + 4.0, cy + 8.0), 4.0, if i == 4 { with_alpha(st.ok, 0.6) } else { *color });
            widgets::text_fit(&p, pos2(cx + 16.0, cy + 8.0), Align2::LEFT_CENTER, label, theme::ui(12.0), pal.text_dim, cw - 16.0);
            let (num, unit) = split_unit(&format_bytes(*bytes as f64));
            let nr = p.text(pos2(cx, cy + 16.0), Align2::LEFT_TOP, num, theme::light(22.0), *value_color);
            p.text(pos2(nr.right() + 4.0, nr.bottom() - 4.0), Align2::LEFT_BOTTOM, unit, theme::ui(13.0), pal.text_faint);
        }

        if !with_tiles {
            return h;
        }
        // The four tiles.
        let (tiles_x, tiles_y, tiles_w) = if wide { (x + lw + GAP_X, y, rw) } else { (x, y + h + GAP_X, rw) };
        let th = if wide { (h - GAP_X) / 2.0 } else { tile_h };
        let tw = (tiles_w - GAP_X) / 2.0;
        let gpu_value = if s.gpu.available { format!("{:.0}", s.gpu.utilization_pct) } else { "\u{2014}".to_string() };
        let tiles = [
            ("CPU", Device::Cpu, format!("{:.0}", s.cpu.total_pct), 0usize, String::new()),
            ("Memory", Device::Memory, format!("{:.0}", s.memory.used_pct), 1, format!("{} free", format_bytes(s.memory.available as f64))),
            ("Disk", Device::Disk, format!("{:.0}", s.disk.active_pct), 2, String::new()),
            ("GPU", Device::Gpu, gpu_value, 3, String::new()),
        ];
        for (i, (label, dev, value, li, free_text)) in tiles.iter().enumerate() {
            let (col, row) = (i % 2, i / 2);
            let r = Rect::from_min_size(pos2(tiles_x + col as f32 * (tw + GAP_X), tiles_y + row as f32 * (th + GAP_X)), vec2(tw, th));
            let level = v.levels[*li];
            let resp = ui.interact(r, ui.id().with(("tile", *label)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if level == Level::Critical {
                widgets::tinted_card(&p, r, &pal, st.danger);
            } else {
                widgets::card(&p, r, &pal);
            }
            if resp.hovered() {
                p.rect_stroke(r, 10.0, Stroke::new(1.0_f32, pal.raise(0.14)));
            }
            widgets::tracked_text(&p, pos2(r.left() + 18.0, r.top() + 18.0), Align2::LEFT_TOP, &label.to_uppercase(), theme::ui(11.0), pal.text_faint, 0.14);
            let nr = p.text(pos2(r.left() + 18.0, r.top() + 18.0 + 13.0 + 8.0), Align2::LEFT_TOP, value, theme::light(28.0), pal.text);
            if value != "\u{2014}" {
                p.text(pos2(nr.right() + 3.0, nr.bottom() - 6.0), Align2::LEFT_BOTTOM, "%", theme::ui(15.0), pal.text_faint);
            }
            let (line, color, bold) = match level {
                Level::Good if *li == 3 && !s.gpu.available => ("No GPU counters".to_string(), pal.text_faint, false),
                Level::Good => ("Normal".to_string(), pal.text_faint, false),
                Level::Elevated => (format!("Elevated \u{b7} {}", age(self.summary.levels[*li].1)), st.warn, false),
                Level::Critical if !free_text.is_empty() => (format!("Critical \u{b7} {free_text}"), st.danger_text, true),
                Level::Critical => (format!("Critical \u{b7} {}", age(self.summary.levels[*li].1)), st.danger_text, true),
            };
            let ly = r.bottom() - 18.0 - 7.0;
            let mut lx = r.left() + 18.0;
            if level != Level::Good {
                marker(&p, pos2(lx + 4.0, ly), level, color);
                lx += 14.0;
            }
            let font = if bold { theme::semibold(12.0) } else { theme::ui(12.0) };
            widgets::text_fit(&p, pos2(lx, ly), Align2::LEFT_CENTER, &line, font, color, r.right() - lx - 12.0);
            if resp.clicked() {
                *go = Some(Go::Device(*dev));
            }
        }
        if wide {
            h
        } else {
            h + GAP_X + 2.0 * tile_h + GAP_X
        }
    }
}

/// One resource card.
struct Res<'a> {
    label: &'static str,
    device: Device,
    frac: f32,
    color: Color32,
    /// Number and unit.
    value: (String, String),
    hardware: Hardware,
    lines: Vec<(&'a Series, Color32, (f32, f32))>,
    footer: String,
}

/// The app group most responsible for `b` (memory by bytes, disk by transfer rate, else CPU).
fn leader(groups: &[AppGroup], b: Option<Bottleneck>) -> Option<&AppGroup> {
    match b {
        Some(Bottleneck::Memory) => groups.iter().max_by_key(|g| g.memory),
        Some(Bottleneck::Disk) => groups.iter().max_by(|a, b| a.io_bps.total_cmp(&b.io_bps)),
        _ => groups.iter().filter(|g| g.cpu > 0.0).max_by(|a, b| a.cpu.total_cmp(&b.cpu)),
    }
}

fn noun(b: Bottleneck) -> &'static str {
    match b {
        Bottleneck::Memory => "memory",
        Bottleneck::Cpu => "CPU",
        Bottleneck::Disk => "disk",
    }
}

/// The colour that says how the verdict is: good, elevated or critical.
fn level_color(st: &Status, level: Level) -> Color32 {
    match level {
        Level::Good => st.ok,
        Level::Elevated => st.warn,
        Level::Critical => st.danger_text,
    }
}

/// Ring and legend colour of a resource: its own colour, or the warning colours when it is
/// under pressure.
fn ring_color(st: &Status, base: Color32, level: Level) -> Color32 {
    match level {
        Level::Good => base,
        Level::Elevated => st.warn,
        Level::Critical => st.danger,
    }
}

/// Triangle for elevated, square for critical: a shape, so the state does not depend on colour.
fn marker(p: &egui::Painter, c: Pos2, level: Level, color: Color32) {
    match level {
        Level::Good => {}
        Level::Elevated => {
            p.add(Shape::convex_polygon(vec![pos2(c.x, c.y - 4.0), pos2(c.x + 4.0, c.y + 3.5), pos2(c.x - 4.0, c.y + 3.5)], color, Stroke::NONE));
        }
        Level::Critical => {
            p.rect_filled(Rect::from_center_size(c, vec2(7.0, 7.0)), 1.0, color);
        }
    }
}

fn tracked_header(p: &egui::Painter, pos: Pos2, text: &str, anchor: Align2, pal: &super::theme::Palette) {
    widgets::tracked_text(p, pos, anchor, text, theme::ui(11.0), pal.text_faint, 0.08);
}

/// Wrapped text in several fonts and colours; every row `line_h` high.
fn rich(p: &egui::Painter, parts: &[(&str, FontId, Color32)], wrap: f32, line_h: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap.max(40.0);
    for (text, font, color) in parts {
        if text.is_empty() {
            continue;
        }
        job.append(text, 0.0, egui::TextFormat { font_id: font.clone(), color: *color, line_height: Some(line_h), ..Default::default() });
    }
    p.layout_job(job)
}

/// "11.8 GB" -> ("11.8", "GB").
fn split_unit(s: &str) -> (String, String) {
    match s.split_once(' ') {
        Some((n, u)) => (n.to_string(), u.to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// "24 s" or "6 min" since `t`.
fn age(t: Option<Instant>) -> String {
    let s = t.map_or(0, |t| t.elapsed().as_secs());
    if s < 60 {
        format!("{s} s")
    } else {
        format!("{} min", s / 60)
    }
}

/// "chrome.exe (1234)" -> "chrome.exe".
fn strip_pid(text: &str) -> &str {
    match text.rsplit_once(" (") {
        Some((name, rest)) if rest.ends_with(')') => name,
        _ => text,
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}\u{2026}", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// Network receive plus send as one series (the Summary card and the Performance preview).
pub fn net_total(hist: &History) -> Series {
    let mut net = Series::new(hist.net_rx.capacity);
    for (a, b) in hist.net_rx.values.iter().zip(&hist.net_tx.values) {
        net.push(a + b);
    }
    net
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
    let c = Color32::from_rgb(if app { 0x4C } else { 0x8A }, if app { 0x9E } else { 0x8A }, if app { 0xF7 } else { 0x8A });
    p.rect_filled(r, 2.0, with_alpha(c, 0.25));
    p.rect_stroke(r, 2.0, Stroke::new(1.0_f32, c));
    if app {
        p.line_segment([pos2(r.left() + 2.0, r.top() + 4.0), pos2(r.right() - 2.0, r.top() + 4.0)], Stroke::new(1.0_f32, c));
    } else {
        p.circle_stroke(r.center(), 2.5, Stroke::new(1.0_f32, c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapped multi-colour text and the level markers lay out and draw.
    #[test]
    fn rich_text_and_markers_draw() {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = ui.painter().clone();
                    let parts = [("Memory is ", theme::light(44.0), Color32::WHITE), ("almost full", theme::ui(44.0), Color32::RED), (".", theme::light(44.0), Color32::WHITE)];
                    let one = rich(&p, &parts, 2000.0, 46.0);
                    let wrapped = rich(&p, &parts, 200.0, 46.0);
                    assert!(wrapped.size().y > one.size().y, "narrow text wraps onto more rows");
                    p.galley(pos2(20.0, 20.0), wrapped, Color32::WHITE);
                    for (i, level) in [Level::Good, Level::Elevated, Level::Critical].into_iter().enumerate() {
                        marker(&p, pos2(20.0 + i as f32 * 20.0, 300.0), level, Color32::RED);
                    }
                });
            });
        }
    }

    #[test]
    fn units_split_off_numbers() {
        assert_eq!(split_unit("11.8 GB"), ("11.8".to_string(), "GB".to_string()));
        assert_eq!(split_unit("0 B/s"), ("0".to_string(), "B/s".to_string()));
        assert_eq!(split_unit("\u{2014}"), ("\u{2014}".to_string(), String::new()));
    }

    #[test]
    fn event_texts_lose_their_pid() {
        assert_eq!(strip_pid("chrome.exe (1234)"), "chrome.exe");
        assert_eq!(strip_pid("Recording started"), "Recording started");
    }

    #[test]
    fn long_names_are_clipped() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("Microsoft Edge WebView2", 10).chars().count(), 10);
    }

    #[test]
    fn the_leader_follows_the_bottleneck() {
        let g = |key: &str, cpu: f32, memory: u64| AppGroup { key: key.into(), name: key.into(), count: 1, cpu, memory, io_bps: 0.0, gpu: 0.0, has_window: true, system: false, top: 0, root: 0, icon: None };
        let groups = [g("a", 50.0, 10), g("b", 5.0, 900)];
        assert_eq!(leader(&groups, Some(Bottleneck::Memory)).map(|g| g.key.as_str()), Some("b"));
        assert_eq!(leader(&groups, Some(Bottleneck::Cpu)).map(|g| g.key.as_str()), Some("a"));
        assert_eq!(leader(&groups, None).map(|g| g.key.as_str()), Some("a"));
        assert!(leader(&[], None).is_none());
    }
}
