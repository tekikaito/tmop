//! Performance page: a card per device on the left; on the right the selected device's graph and
//! facts, the other resources as a strip and, for the CPU, the logical processors and the apps
//! using the most.

use super::disk_drives::{self, disk_menu, drive_subtitle};
use super::gpu_activity::{adapter_title, gpu_menu, GpuMenu};
use super::health::{app_groups, cpu_text, AppGroup};
use super::table::TableState;
use super::hardware::{self, Exploded};
use super::kit;
use super::theme::{self, lighten, mix, with_alpha, Palette};
use super::widgets::{self, draw_graph, Button, ButtonSize, Graph, GraphLine, Icon};
use super::{App, Device, FrameData};
use crate::docker::model::{Container, EngineStatus};
use crate::metrics::{DiskDrive, GpuAdapter, GpuTempKind, Series, SOURCES};
use crate::process::{format_bytes, format_rate};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense, Shape, Stroke};
use super::widgets::ScrollArea029 as _;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Top of the Thermals chart range in degrees C. A multiple of 4, so the quarter-step axis labels
/// are whole numbers (0, 30, 60, 90, 120), and above any plausible reading.
const THERMAL_MAX_C: f32 = 120.0;
const LIST_W: f32 = 236.0;
const GAP: f32 = 16.0;
const DEVICE_CARD_H: f32 = 76.0;
/// Height of the cards when the list is squeezed so every device stays in view; and the room the
/// "previews" line with the Refresh button below the cards takes.
const DEVICE_CARD_MIN_H: f32 = 50.0;
const DEVICE_FOOTER_H: f32 = 48.0;
/// Seconds the top-consumer sparklines cover.
const TOP_SPAN_SECS: f32 = 60.0;
/// Width kept free at the right of the CPU graph for the exploded CPU and its labels.
const FIGURE_W: f32 = 410.0;
/// Narrowest tile of the strip of other devices in which a value ("13.7 / 31.1 GB", both network
/// rates) still fits beside its label; in narrower tiles it goes below it.
const STRIP_INLINE_MIN_W: f32 = 240.0;
/// Narrowest tile of the stacked strip: the longest value ("down 9.6 up 1.7 KB/s") still fits.
const STRIP_STACKED_MIN_W: f32 = 140.0;

/// How the per-core block of the CPU page shows the logical processors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoreView {
    /// One small graph per core.
    #[default]
    Graphs,
    /// A colour per core, darker for idle.
    Heatmap,
}

/// State of the Performance page.
#[derive(Default)]
pub struct PerfState {
    pub core_view: CoreView,
    /// CPU use over time of the app groups that were busy lately, keyed by image name.
    pub top_spark: HashMap<String, Series>,
    /// Sampler generation the sparklines were last extended at.
    spark_seq: u64,
    /// The GPU shown while they are separated; the first one when this one is gone.
    pub gpu_selected: Option<String>,
    /// The disk shown while they are separated; the first one when this one is gone.
    pub disk_selected: Option<String>,
    /// The GPU activity section is open.
    pub gpu_activity: bool,
    pub gpu_overview_table: TableState,
    pub gpu_table: Option<TableState>,
    /// The highest temperature seen since TMOP started (°C) and when (`HH:MM`, local time).
    pub thermal_peak: Option<(f32, String)>,
}

impl PerfState {
    /// Records `now_c` (the hottest sensor of a sample) when it is a new peak.
    pub fn note_temperature(&mut self, now_c: Option<f32>) {
        let Some(t) = now_c.filter(|t| t.is_finite()) else { return };
        if self.thermal_peak.as_ref().is_none_or(|(peak, _)| t > *peak) {
            let (_, _, _, h, m, _) = crate::platform::local_time();
            self.thermal_peak = Some((t, format!("{h:02}:{m:02}")));
        }
    }
}

/// Docker series copied from the backend for the Performance page.
#[derive(Clone, Debug)]
pub struct DockerPerfCache {
    /// Backend `stats_seq` the copy was taken at.
    pub stats_seq: u64,
    pub totals_cpu: Series,
    /// Bytes.
    pub totals_mem: Series,
    /// CPU series of each tracked container, keyed by full id.
    pub per_container: Vec<(String, Series)>,
}

impl Default for DockerPerfCache {
    fn default() -> Self {
        Self { stats_seq: 0, totals_cpu: Series::new(2), totals_mem: Series::new(2), per_container: Vec::new() }
    }
}

/// How a device appears in the list and the strip.
struct DevView<'a> {
    value: String,
    lines: Vec<(&'a Series, Color32, (f32, f32))>,
    /// The second line is drawn dashed (network: receive solid, send dashed).
    dashed_second: bool,
}

/// One line of a [`plot`](App::plot).
struct PlotLine<'a> {
    series: &'a Series,
    color: Color32,
    range: (f32, f32),
    width: f32,
    fill: f32,
    dashed: bool,
}

impl<'a> PlotLine<'a> {
    fn new(series: &'a Series, color: Color32, range: (f32, f32)) -> Self {
        Self { series, color, range, width: 1.8, fill: 0.12, dashed: false }
    }
    fn dashed(mut self, color: Color32) -> Self {
        self.dashed = true;
        self.color = color;
        self.fill = 0.0;
        self.width = 1.4;
        self
    }
}

/// Title row of a detail card.
struct Head<'a> {
    title: &'a str,
    sub: String,
    /// Big number and its unit.
    value: (String, String),
    /// Small mono text beside the number.
    side: String,
}

/// What a click on the page asks for, applied after drawing.
enum Act {
    Device(Device),
    Gpu(String),
    GpuMenu(GpuMenu),
    Disk(String),
    /// Show the disks separately, or grouped.
    DisksSeparated(bool),
    Refresh,
    Cores(bool),
    View(CoreView),
    Select(u32, i64),
    SortCpu,
}

impl App {
    fn cpus_expanded(&self) -> bool {
        self.settings.window.performance_cpus_expanded
    }

    /// Copies the Docker series when the backend published new stats. The backend lock is
    /// held only for the copy.
    pub fn refresh_docker_perf(&mut self) {
        let Some(b) = self.docker.backend() else { return };
        let s = match b.shared.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        if s.stats_seq == self.perf_docker.stats_seq {
            return;
        }
        self.perf_docker = DockerPerfCache {
            stats_seq: s.stats_seq,
            totals_cpu: s.totals_hist.cpu.clone(),
            totals_mem: s.totals_hist.mem_used.clone(),
            per_container: s.stats.iter().map(|(id, st)| (id.clone(), st.cpu.clone())).collect(),
        };
    }

    /// The Docker device is listed while the engine is reachable and the Docker page is enabled.
    fn docker_listed(&self) -> bool {
        self.docker_visible() && self.docker.last_status == EngineStatus::Connected
    }

    /// Whether `d` is in the device list and the strip: Docker per [`App::docker_listed`],
    /// Thermals where this machine has a temperature source (or a hint for one).
    fn device_listed(&self, d: Device) -> bool {
        match d {
            Device::Docker => self.docker_listed(),
            Device::Thermal => self.caps.thermal,
            _ => true,
        }
    }

    pub fn performance_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        // Still connecting is not a reason to leave the page; a disabled or unreachable engine is.
        let gone = match self.device {
            Device::Docker => !self.docker_visible() || self.docker.last_status == EngineStatus::Unreachable,
            Device::Thermal => !self.caps.thermal,
            _ => false,
        };
        if gone {
            self.device = Device::Cpu;
        }
        if self.device == Device::Cpu {
            if self.pro.cpu_details.is_none() {
                self.pro.cpu_details = Some(crate::pro::inventory::cpu_details());
            }
            self.pro.power.poll();
            if self.pro.power.stale(Duration::from_secs(60)) {
                self.pro.power.start(crate::pro::power::read);
            }
        }
        // Averaged and in the calm order the Summary's top consumers use, sparklines included.
        let groups = self.summary.steady.update(fd.snap.seq, app_groups(fd.rows, fd.show_self));
        self.update_top_spark(&groups, fd.snap.seq);
        let area = ui.max_rect();
        let list = Rect::from_min_size(area.min, vec2(LIST_W, area.height()));
        let detail = Rect::from_min_max(pos2(list.right() + GAP, area.top()), area.max);
        let mut act = None;
        let scrolled = ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().id_salt("perf-list").auto_shrink([false, false]).show_029(ui, [false, true], |ui| self.device_list(ui, list, fd, &mut act))
        });
        // More cards below the edge: a small arrow says the list scrolls.
        let out = scrolled.inner;
        if out.state.clamped_offset().y + out.inner_rect.height() < out.content_size.y - 1.0 {
            let pal = fd.pal;
            let pill = Rect::from_center_size(pos2(list.center().x, list.bottom() - 12.0), vec2(34.0, 18.0));
            let p = ui.painter();
            p.rect_filled(pill, 9.0, pal.panel);
            p.rect_stroke(pill, 9.0, Stroke::new(1.0_f32, pal.accent), egui::StrokeKind::Outside);
            let c = pill.center();
            p.add(Shape::line(vec![pos2(c.x - 5.0, c.y - 2.5), pos2(c.x, c.y + 2.5), pos2(c.x + 5.0, c.y - 2.5)], Stroke::new(1.5_f32, pal.accent)));
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(detail), |ui| {
            egui::ScrollArea::vertical().id_salt("perf-detail").auto_shrink([false, false]).show_029(ui, [false, true], |ui| {
                let top = ui.cursor().min;
                let w = ui.available_width();
                let mut y = top.y;
                y += self.detail(ui, fd, top.x, y, w, area.width() >= 1100.0, &mut act) + GAP;
                if self.device == Device::Gpu && self.perf.gpu_activity {
                    let (h, opened) = self.gpu_activity(ui, fd, top.x, y, w);
                    y += h + GAP;
                    if let Some((pid, created)) = opened {
                        act = Some(Act::Select(pid, created));
                    }
                }
                y += self.other_resources(ui, fd, top.x, y, w, &mut act) + GAP;
                if self.device == Device::Cpu {
                    y += self.cpu_bottom(ui, fd, top.x, y, w, &groups, &mut act) + GAP;
                    y += self.cpu_facts(ui, fd, top.x, y, w);
                }
                ui.allocate_rect(Rect::from_min_size(top, vec2(w, y - top.y)), Sense::hover());
            });
        });
        match act {
            Some(Act::Device(d)) => self.device = d,
            Some(Act::Gpu(id)) => {
                self.device = Device::Gpu;
                self.perf.gpu_selected = Some(id);
            }
            Some(Act::GpuMenu(m)) => self.apply_gpu_menu(m),
            Some(Act::Disk(id)) => {
                self.device = Device::Disk;
                self.perf.disk_selected = Some(id);
            }
            Some(Act::DisksSeparated(on)) => self.apply_disk_menu(on),
            Some(Act::Refresh) => self.sampler.refresh(),
            Some(Act::Cores(on)) => {
                self.settings.window.performance_cpus_expanded = on;
                self.settings_dirty = true;
            }
            Some(Act::View(v)) => self.perf.core_view = v,
            Some(Act::Select(pid, created)) => {
                self.selected = Some((pid, created));
                self.page = super::Page::Processes;
            }
            Some(Act::SortCpu) => {
                self.sort = (crate::process::Column::Cpu, true);
                self.page = super::Page::Processes;
            }
            None => {}
        }
    }

    /// Extends the top-consumer sparklines once per sampler generation.
    fn update_top_spark(&mut self, groups: &[AppGroup], seq: u64) {
        if seq == self.perf.spark_seq {
            return;
        }
        self.perf.spark_seq = seq;
        let cap = self.spark_cap();
        let live: HashSet<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        self.perf.top_spark.retain(|k, _| live.contains(k.as_str()));
        for g in groups {
            match self.perf.top_spark.get_mut(&g.key) {
                Some(s) => {
                    s.resize(cap);
                    s.push(g.cpu);
                }
                None if g.cpu > 0.0 => {
                    let mut s = Series::new(cap);
                    s.push(g.cpu);
                    self.perf.top_spark.insert(g.key.clone(), s);
                }
                None => {}
            }
        }
    }

    /// Samples in the top-consumer sparklines: [`TOP_SPAN_SECS`] at the update interval.
    fn spark_cap(&self) -> usize {
        let secs = self.settings.update_speed().interval().map_or(1.0, |d| d.as_secs_f32());
        (TOP_SPAN_SECS / secs).ceil().clamp(8.0, 240.0) as usize
    }

    // ------------------------------------------------------------------ device list

    /// How `d` shows in the list and the strip: its value and the lines of its sparkline.
    fn dev_view<'a>(&'a self, d: Device, fd: &FrameData<'a>) -> DevView<'a> {
        let s = fd.snap;
        let c = fd.metric(d.key());
        match d {
            Device::Cpu => {
                let ghz = s.cpu.current_mhz.filter(|_| self.caps.clocks).map(|m| format!(" \u{b7} {:.2} GHz", m as f32 / 1000.0)).unwrap_or_default();
                DevView { value: format!("{:.0}%{ghz}", s.cpu.total_pct), lines: vec![(&fd.hist.cpu, c, (0.0, 100.0))], dashed_second: false }
            }
            Device::Memory => DevView {
                value: bytes_pair(s.memory.total.saturating_sub(s.memory.available), s.memory.total),
                lines: vec![(&fd.hist.memory, c, (0.0, 100.0))],
                dashed_second: false,
            },
            Device::Gpu => DevView {
                value: if s.gpu.available { format!("{:.0}%", s.gpu.utilization_pct) } else { "\u{2014}".to_string() },
                lines: vec![(&fd.hist.gpu, c, (0.0, 100.0))],
                dashed_second: false,
            },
            Device::Disk => DevView { value: if s.disk_known() { format!("{:.0}%", s.disk.active_pct) } else { "\u{2014}".to_string() }, lines: vec![(&fd.hist.disk, c, (0.0, 100.0))], dashed_second: false },
            Device::Network => {
                let peak = fd.hist.net_rx.max().max(fd.hist.net_tx.max()).max(1024.0);
                let value = if s.net_known() { rate_pair(s.network.rx_bps, s.network.tx_bps) } else { "\u{2014}".to_string() };
                DevView { value, lines: vec![(&fd.hist.net_rx, c, (0.0, peak)), (&fd.hist.net_tx, c, (0.0, peak))], dashed_second: true }
            }
            Device::Thermal => DevView {
                value: s.thermal.max_c().map(|t| self.temp(t)).unwrap_or_else(|| "\u{2014}".to_string()),
                lines: vec![(&fd.hist.thermal, c, (0.0, THERMAL_MAX_C))],
                dashed_second: false,
            },
            Device::Docker => DevView { value: format!("{} running", self.docker.ui.snap.totals.running), lines: vec![(&self.perf_docker.totals_cpu, c, (0.0, 100.0))], dashed_second: false },
        }
    }

    /// The GPU the page shows on its own, when they are separated: with its position in the list.
    fn selected_adapter<'a>(&self, fd: &FrameData<'a>) -> Option<(usize, &'a GpuAdapter)> {
        if !fd.gpus_separated {
            return None;
        }
        let adapters = &fd.snap.gpu.adapters;
        let chosen = self.perf.gpu_selected.as_ref().and_then(|id| adapters.iter().position(|a| a.id == *id));
        let i = chosen.unwrap_or(0);
        adapters.get(i).map(|a| (i, a))
    }

    /// The disk the page shows on its own, when they are separated: with its position in the list.
    fn selected_drive<'a>(&self, fd: &FrameData<'a>) -> Option<(usize, &'a DiskDrive)> {
        if !fd.disks_separated {
            return None;
        }
        let drives = &fd.snap.disk.drives;
        let chosen = self.perf.disk_selected.as_ref().and_then(|id| drives.iter().position(|d| d.id == *id));
        let i = chosen.unwrap_or(0);
        drives.get(i).map(|d| (i, d))
    }

    /// The device cards, the preview span and the Refresh button.
    fn device_list(&self, ui: &mut egui::Ui, list: Rect, fd: &FrameData<'_>, act: &mut Option<Act>) {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let top = ui.cursor().min;
        let mut y = top.y;
        // One card per entry: a separated GPU or disk is a device of its own here, told apart by
        // its position among its kind.
        let mut entries: Vec<(Device, Option<usize>)> = Vec::new();
        for d in Device::ALL.into_iter().filter(|d| self.device_listed(*d)) {
            if d == Device::Gpu && fd.gpus_separated {
                entries.extend((0..fd.snap.gpu.adapters.len()).map(|i| (d, Some(i))));
            } else if d == Device::Disk && fd.disks_separated {
                entries.extend((0..fd.snap.disk.drives.len()).map(|i| (d, Some(i))));
            } else {
                entries.push((d, None));
            }
        }
        let card_h = device_card_h(list.height(), entries.len());
        let title_y = if card_h < DEVICE_CARD_H { 15.0 } else { 19.0 };
        let selected_gpu = self.selected_adapter(fd).map(|(i, _)| i);
        let selected_disk = self.selected_drive(fd).map(|(i, _)| i);
        for (d, one) in entries {
            let gpu = one.filter(|_| d == Device::Gpu);
            let adapter = gpu.map(|i| &fd.snap.gpu.adapters[i]);
            let drive = one.filter(|_| d == Device::Disk).map(|i| &fd.snap.disk.drives[i]);
            let selected = if d == Device::Disk { selected_disk } else { selected_gpu };
            let r = Rect::from_min_size(pos2(list.left(), y), vec2(list.width(), card_h));
            let resp = ui.interact(r, ui.scope_id().with(("dev", d.key(), one)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if d == Device::Gpu {
                kit::context_menu(&resp, |ui| {
                    if let Some(m) = gpu_menu(ui, fd, self.perf.gpu_activity) {
                        *act = Some(Act::GpuMenu(m));
                    }
                });
            }
            if d == Device::Disk && disk_drives::offers_menu(fd) {
                kit::context_menu(&resp, |ui| {
                    if let Some(on) = disk_menu(ui, fd) {
                        *act = Some(Act::DisksSeparated(on));
                    }
                });
            }
            let active = self.device == d && (one.is_none() || one == selected);
            if active {
                p.rect_filled(r, 8.0, with_alpha(pal.accent, 0.1));
                p.rect_stroke(r, 8.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.5)), egui::StrokeKind::Outside);
                let bar = Rect::from_min_max(pos2(r.left() + 1.0, r.top() + 1.0), pos2(r.left() + 4.0, r.bottom() - 1.0));
                p.rect_filled(bar, egui::CornerRadius { nw: 7, sw: 7, ..Default::default() }, pal.accent);
            } else {
                p.rect_filled(r, 8.0, pal.raise(if resp.hovered() { 0.06 } else { 0.04 }));
                p.rect_stroke(r, 8.0, Stroke::new(1.0_f32, pal.raise(0.08)), egui::StrokeKind::Outside);
            }
            let view = match (adapter.and_then(|a| Some((a, fd.hist.gpu_adapter(&a.id)?))), drive.and_then(|v| Some((v, fd.hist.disk_drive(&v.id)?)))) {
                (Some((a, series)), _) => DevView { value: format!("{:.0}%", a.utilization_pct), lines: vec![(series, fd.metric("gpu"), (0.0, 100.0))], dashed_second: false },
                (_, Some((v, series))) => DevView { value: format!("{:.0}%", v.active_pct), lines: vec![(&series.active, fd.metric("disk"), (0.0, 100.0))], dashed_second: false },
                _ => self.dev_view(d, fd),
            };
            let gpu_title;
            let title: &str = match (gpu.zip(adapter), drive) {
                (Some((i, a)), _) => {
                    gpu_title = adapter_title(i, a);
                    &gpu_title
                }
                (_, Some(v)) => &v.title,
                _ => d.title(),
            };
            let value_w = widgets::text_width(&p, &view.value, theme::mono(12.0));
            let name_w = r.width() - 24.0 - value_w - 8.0;
            widgets::text_fit(&p, pos2(r.left() + 12.0, r.top() + title_y), Align2::LEFT_CENTER, title, theme::semibold(13.0), if active { pal.text } else { pal.text_table() }, name_w);
            widgets::text_fit(&p, pos2(r.right() - 12.0, r.top() + title_y), Align2::RIGHT_CENTER, &view.value, theme::mono(12.0), if active { pal.text } else { pal.text_dim }, r.width() - 24.0);
            let spark = Rect::from_min_size(pos2(r.left() + 12.0, r.top() + title_y + 15.0), vec2(r.width() - 24.0, (card_h - title_y - 23.0).min(32.0)));
            if d == Device::Docker {
                widgets::sparkline_unsynced(&p, spark, &view.lines, 0.12);
            } else {
                widgets::sparkline(&p, spark, &view.lines, 0.12, view.dashed_second);
            }
            if resp.clicked() {
                *act = Some(match (adapter, drive) {
                    (Some(a), _) => Act::Gpu(a.id.clone()),
                    (_, Some(v)) => Act::Disk(v.id.clone()),
                    _ => Act::Device(d),
                });
            }
            y += card_h + 6.0;
        }
        let span = span_text(widgets::spark_visible(list.width() - 24.0, fd.hist.cpu.values.len(), fd.hist.cpu.capacity), self.settings.update_speed().interval());
        p.text(pos2(list.left() + 2.0, y + 2.0), Align2::LEFT_TOP, format!("previews: last {span}"), theme::mono(11.0), pal.text_faint);
        let button = Rect::from_min_size(pos2(list.left(), y + 18.0), vec2(120.0, 28.0));
        if kit::row_ui(ui, button, |ui| Button::ghost("Refresh").size(ButtonSize::Compact).icon(Icon::Refresh).show(ui, &pal).clicked()) {
            *act = Some(Act::Refresh);
        }
        ui.allocate_rect(Rect::from_min_size(top, vec2(list.width(), y + DEVICE_FOOTER_H - top.y)), Sense::hover());
    }

    // ------------------------------------------------------------------ detail card

    /// The selected device's card. Returns its height.
    #[allow(clippy::too_many_arguments)]
    fn detail(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, figure: bool, act: &mut Option<Act>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let slot = p.add(Shape::Noop);
        let (ix, iw) = (x + 24.0, w - 48.0);
        let gw = if figure { iw - FIGURE_W } else { iw };
        let mut cy = y + 20.0;
        let iv = self.settings.update_speed().interval();
        let gs = &self.settings.graphs;
        let pc = PlotCtx { pal: &pal, smooth: gs.smooth_curves, bloom: fd.bloom, neutral: !gs.color_keyed, regions: widgets::regions_for(gs.history_multiplier, gs.compressed_time), px_per_update: gs.pixels_per_update.clamp(2, 64) as f32, interval: iv, phase: fd.phase, glide: fd.glide };
        let s = fd.snap;
        match self.device {
            Device::Cpu => {
                let det = self.pro.cpu_details.clone().unwrap_or_default();
                let name = if s.cpu.name.is_empty() { "CPU".to_string() } else { s.cpu.name.clone() };
                let threads = det.logical.max(s.cpu.logical_count);
                let sub = match (det.cores, s.cpu.base_mhz) {
                    (0, _) => format!("{threads} logical processors"),
                    (c, Some(b)) => format!("{c} cores \u{b7} {threads} threads \u{b7} base {:.2} GHz", b as f32 / 1000.0),
                    (c, None) => format!("{c} cores \u{b7} {threads} threads"),
                };
                let hd = Head { title: &name, sub, value: (format!("{:.0}", fd.smooth.cpu), "%".into()), side: format!("{:.0}% kernel", s.cpu.kernel_pct) };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let accent = pal.accent;
                let lines = [PlotLine::new(&fd.hist.cpu, accent, (0.0, 100.0)), PlotLine::new(&fd.hist.kernel, pal.text_faint, (0.0, 100.0)).dashed(pal.text_faint)];
                let readout = |age: usize| format!("{} \u{b7} {:.0}% \u{b7} {:.0}%", ago(iv, age), at(&fd.hist.cpu, age), at(&fd.hist.kernel, age));
                let (ph, hover) = plot(ui, &pc, "cpu", ix, cy, gw, 200.0, 36.0, &percent_labels(), &lines, &readout);
                if figure {
                    let temp = s.thermal.cpu.as_ref().map(|c| c.1).or_else(|| s.thermal.board_max_c()).map(|t| self.temp(t)).unwrap_or_else(|| "\u{2014}".to_string());
                    let ghz = s.cpu.current_mhz.filter(|_| self.caps.clocks).map(|m| format!(" \u{b7} {:.2} GHz", m as f32 / 1000.0)).unwrap_or_default();
                    let l3 = if det.l3 > 0 { format!(" \u{b7} L3 {}", format_bytes(det.l3 as f64)) } else { String::new() };
                    let die = format!("{}C / {}T{ghz}{l3}", det.cores, threads);
                    let ghz_of = |m: Option<u32>| m.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or_else(|| "\u{2014}".to_string());
                    let substrate = format!("base {} \u{b7} boost {}", ghz_of(s.cpu.base_mhz), ghz_of(s.cpu.boost_mhz));
                    let socket = format!("{} socket{}", det.sockets.max(1), if det.sockets.max(1) == 1 { "" } else { "s" });
                    figure_at(&p, &pal, Exploded::Cpu, ix + gw, cy, 200.0, &[("Temperature", temp), ("Cores", die), ("Clock speed", substrate), ("Sockets", socket)]);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("Total", accent, false), ("Kernel time", pal.text_faint, true)], hover.as_deref(), ix + gw);
                cy += 24.0;
            }
            Device::Memory => {
                let used = s.memory.total.saturating_sub(s.memory.available);
                let c = fd.metric("memory");
                let hd = Head { title: "Memory", sub: format!("{} total physical memory", format_bytes(s.memory.total as f64)), value: (format!("{:.0}", fd.smooth.memory), "%".into()), side: bytes_pair(used, s.memory.total) };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let lines = [PlotLine::new(&fd.hist.memory, c, (0.0, 100.0))];
                let readout = |age: usize| format!("{} \u{b7} {:.0}% \u{b7} {}", ago(iv, age), at(&fd.hist.memory, age), format_bytes(at(&fd.hist.memory_bytes, age) as f64));
                let (ph, hover) = plot(ui, &pc, "memory", ix, cy, gw, 200.0, 36.0, &percent_labels(), &lines, &readout);
                if figure {
                    let m = &s.memory;
                    let pair = |a: u64, b: u64| format!("{} / {}", format_bytes(a as f64), format_bytes(b as f64));
                    let labels = [("In use", pair(used, m.total)), ("Usage", format!("{:.0}%", m.used_pct)), ("Cached", format_bytes(m.cached as f64)), ("Committed", dash_or(m.commit_used.zip(m.commit_limit).map(|(u, l)| pair(u, l))))];
                    figure_at(&p, &pal, Exploded::Memory, ix + gw, cy, 200.0, &labels);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("In use", c, false)], hover.as_deref(), ix + gw);
                cy += 24.0;
                // In use / cached / free.
                let cached = s.memory.cached.min(s.memory.available);
                let free = s.memory.available.saturating_sub(cached);
                let bar = Rect::from_min_size(pos2(ix, cy + 6.0), vec2(iw, 14.0));
                widgets::share_bar(&p, bar, &[(used as f32, c), (cached as f32, with_alpha(c, 0.45)), (free as f32, with_alpha(pal.text, 0.12))], 2.0);
                let parts = [
                    (format!("In use {}", format_bytes(used as f64)), c),
                    (format!("Cached {}", format_bytes(cached as f64)), with_alpha(c, 0.45)),
                    (format!("Free {}", format_bytes(free as f64)), with_alpha(pal.text, 0.25)),
                ];
                let items: Vec<(&str, Color32)> = parts.iter().map(|(t, c)| (t.as_str(), *c)).collect();
                widgets::legend(&p, pos2(ix, bar.bottom() + 16.0), &items, theme::ui(12.0));
                cy = bar.bottom() + 34.0;
                let mut facts = vec![("In use".to_string(), format_bytes(used as f64)), ("Available".to_string(), format_bytes(s.memory.available as f64))];
                if let (Some(u), Some(l)) = (s.memory.commit_used, s.memory.commit_limit) {
                    facts.push(("Committed".to_string(), format!("{} / {}", format_bytes(u as f64), format_bytes(l as f64))));
                }
                facts.push(("Cached".to_string(), format_bytes(s.memory.cached as f64)));
                if let Some(mhz) = crate::pro::smbios::memory_speed_mhz() {
                    facts.push(("Speed".to_string(), crate::pro::smbios::format_speed(mhz)));
                }
                let swap = crate::platform::swap_word();
                if let Some(total) = s.memory.swap_total {
                    facts.push((swap.to_string(), format_bytes(total as f64)));
                }
                if let Some(used) = s.memory.swap_used {
                    facts.push((format!("{swap} in use"), format_bytes(used as f64)));
                }
                cy += dl(&p, &pal, ix, cy, iw, &facts);
            }
            Device::Gpu => {
                let c = fd.metric("gpu");
                // Under the card's content, so the plot keeps its hover readout: right-click only.
                let section = Rect::from_min_size(pos2(x, y), vec2(w, 300.0));
                kit::context_menu(&ui.interact(section, ui.scope_id().with("gpu-section"), Sense::click()), |ui| {
                    if let Some(m) = gpu_menu(ui, fd, self.perf.gpu_activity) {
                        *act = Some(Act::GpuMenu(m));
                    }
                });
                // The grouped figures, or those of the GPU shown on its own.
                let one = self.selected_adapter(fd);
                let title = one.map_or_else(|| "GPU".to_string(), |(i, a)| adapter_title(i, a));
                let series = one.and_then(|(_, a)| fd.hist.gpu_adapter(&a.id)).unwrap_or(&fd.hist.gpu);
                let (util, shown_util, engines, dedicated) = match one {
                    Some((_, a)) => (a.utilization_pct, a.utilization_pct, &a.engines, a.dedicated_used),
                    None => (s.gpu.utilization_pct, fd.smooth.gpu, &s.gpu.engines, Some(s.gpu.dedicated_used)),
                };
                // (used, total) of dedicated and of shared memory.
                let (ded_total, shared) = match one {
                    Some((_, a)) => (a.dedicated_total, (a.shared_used, a.shared_total)),
                    None => (s.gpu.dedicated_total, (s.gpu.shared_used, s.gpu.shared_total)),
                };
                let value = if s.gpu.available { (format!("{shown_util:.0}"), "%".into()) } else { ("\u{2014}".to_string(), String::new()) };
                let sub = if one.is_some() { "Busiest engine of this GPU" } else { SOURCES.gpu };
                // The GPUs this card speaks for: the one shown on its own, or all of them.
                let shown: Vec<&GpuAdapter> = match one {
                    Some((_, a)) => vec![a],
                    None => s.gpu.adapters.iter().collect(),
                };
                // Beside the utilisation, when it is one GPU's: the one shown, or the only one reporting.
                let mut reporting = shown.iter().filter_map(|a| a.temperature_c);
                let gpu_temp = match (reporting.next(), reporting.next()) {
                    (Some(t), None) => Some(self.temp(t)),
                    _ => None,
                };
                let hd = Head { title: &title, sub: sub.into(), value, side: gpu_temp.clone().unwrap_or_default() };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let lines = [PlotLine::new(series, c, (0.0, 100.0))];
                let readout = |age: usize| format!("{} \u{b7} {:.0}%", ago(iv, age), at(series, age));
                let (ph, hover) = plot(ui, &pc, "gpu", ix, cy, gw, 200.0, 36.0, &percent_labels(), &lines, &readout);
                if figure {
                    let g = &s.gpu;
                    let on = |v: String| dash_or(g.available.then_some(v));
                    let top = engines.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(e, v)| format!("{e} \u{b7} {v:.0}%"));
                    // The temperature where one GPU reports it, else the engine count as before.
                    let last = match gpu_temp.clone() {
                        Some(t) => ("Temperature", t),
                        None => ("Engines", on(format!("{} reported", engines.len()))),
                    };
                    let labels = [
                        ("Utilization", on(format!("{util:.0}%"))),
                        ("Busiest engine", dash_or(top.filter(|_| g.available))),
                        ("Dedicated memory", dash_or(gpu_memory(dedicated, ded_total).filter(|_| g.available))),
                        last,
                    ];
                    figure_at(&p, &pal, Exploded::Gpu, ix + gw, cy, 200.0, &labels);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("Utilization", c, false)], hover.as_deref(), ix + gw);
                cy += 30.0;
                if !s.gpu.available {
                    p.text(pos2(ix, cy), Align2::LEFT_TOP, SOURCES.no_gpu, theme::ui(12.0), theme::status(&pal).warn);
                    cy += 22.0;
                } else if let Some(note) = s.gpu.note.as_deref().filter(|_| one.is_none_or(|(_, a)| a.partial)) {
                    // The one place that says what the figures leave out.
                    cy += widgets::text_wrapped(&p, pos2(ix, cy), note, theme::ui(12.0), pal.text_faint, iw, 3).height() + 8.0;
                }
                for (e, v) in engines.iter().take(6) {
                    widgets::text_fit(&p, pos2(ix, cy + 8.0), Align2::LEFT_CENTER, e, theme::ui(12.0), pal.text_dim, 130.0);
                    widgets::bar(&p, Rect::from_min_size(pos2(ix + 140.0, cy + 6.5), vec2((iw - 140.0 - 60.0).max(20.0), 3.0)), v / 100.0, c);
                    p.text(pos2(ix + iw, cy + 8.0), Align2::RIGHT_CENTER, format!("{v:.0}%"), theme::mono(12.0), pal.text_table());
                    cy += 24.0;
                }
                let mut facts = vec![("Utilization".to_string(), format!("{util:.1}%")), ("Dedicated memory".to_string(), dash_or(gpu_memory(dedicated, ded_total)))];
                // Only where the system reports it (Windows, amdgpu): a dash would read as "none".
                if let Some(text) = gpu_memory(shared.0, shared.1) {
                    facts.push(("Shared memory".to_string(), text));
                }
                if self.caps.gpu_engines {
                    facts.push((if one.is_some() { "Engines" } else { "Engines reported" }.to_string(), engines.len().to_string()));
                }
                // Each temperature a source reports, named by what it measures and where it came
                // from; with several GPUs grouped, by GPU too. None reported: no row.
                facts.extend(gpu_temp_facts(&shown, one.is_none() && shown.len() > 1, |t| self.temp(t)));
                cy += dl(&p, &pal, ix, cy + 4.0, iw, &facts) + 4.0;
                if let Some((_, a)) = one {
                    let sensors = vec![
                        ("Power".to_string(), dash_or(a.power_w.map(|w| format!("{w:.0} W")))),
                        ("Clock".to_string(), dash_or(a.clock_mhz.map(|m| format!("{m} MHz")))),
                    ];
                    cy += dl(&p, &pal, ix, cy + 12.0, iw, &sensors) + 12.0;
                }
            }
            Device::Disk => {
                let c = fd.metric("disk");
                let write = theme::status(&pal).warn;
                if disk_drives::offers_menu(fd) {
                    // Under the card's content, so the plots keep their hover readout: right-click only.
                    let section = Rect::from_min_size(pos2(x, y), vec2(w, 300.0));
                    kit::context_menu(&ui.interact(section, ui.scope_id().with("disk-section"), Sense::click()), |ui| {
                        if let Some(on) = disk_menu(ui, fd) {
                            *act = Some(Act::DisksSeparated(on));
                        }
                    });
                }
                // The grouped figures, or those of the disk shown on its own.
                let one = self.selected_drive(fd).and_then(|(_, d)| Some((d, fd.hist.disk_drive(&d.id)?)));
                let (title, sub) = one.map_or(("Disk", SOURCES.disk), |(d, _)| (d.title.as_str(), drive_subtitle(d)));
                let (active, shown_active, read_bps, write_bps) = match one {
                    Some((d, _)) => (d.active_pct, d.active_pct, d.read_bps, d.write_bps),
                    None => (s.disk.active_pct, fd.smooth.disk, s.disk.read_bps, s.disk.write_bps),
                };
                let (h_active, h_read, h_write) = match one {
                    Some((_, h)) => (&h.active, &h.read, &h.write),
                    None => (&fd.hist.disk, &fd.hist.disk_read, &fd.hist.disk_write),
                };
                let value = if s.disk_known() { (format!("{shown_active:.0}"), "%".into()) } else { ("\u{2014}".to_string(), String::new()) };
                let hd = Head { title, sub: sub.into(), value, side: format!("R {} \u{b7} W {}", format_rate(read_bps), format_rate(write_bps)) };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let lines = [PlotLine::new(h_active, c, (0.0, 100.0))];
                let readout = |age: usize| format!("{} \u{b7} {:.0}%", ago(iv, age), at(h_active, age));
                let (ph, hover) = plot(ui, &pc, "disk", ix, cy, gw, 150.0, 36.0, &percent_labels(), &lines, &readout);
                if figure {
                    let which = match one {
                        Some((d, _)) => ("Disk", d.name.clone()),
                        None => ("Disks", "all physical disks".to_string()),
                    };
                    let labels = [which, ("Read", format_rate(read_bps)), ("Write", format_rate(write_bps)), ("Active time", format!("{active:.1}%"))];
                    figure_at(&p, &pal, Exploded::Disk, ix + gw, cy, 194.0, &labels);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("Active time", c, false)], hover.as_deref(), ix + gw);
                cy += 34.0;
                let m = widgets::eased(ui.ctx(), "perf-disk-rate", h_read.max().max(h_write.max()).max(1024.0 * 1024.0));
                let lines = [PlotLine::new(h_read, c, (0.0, m)), PlotLine::new(h_write, write, (0.0, m))];
                let readout = |age: usize| format!("{} \u{b7} R {} \u{b7} W {}", ago(iv, age), format_rate(at(h_read, age) as f64), format_rate(at(h_write, age) as f64));
                let (ph, hover) = plot(ui, &pc, "disk-rate", ix, cy, iw, 130.0, 64.0, &rate_labels(m), &lines, &readout);
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 64.0, cy + 12.0, &[("Read", c, false), ("Write", write, false)], hover.as_deref(), ix + iw);
                cy += 24.0;
                let mut facts = vec![("Active time".to_string(), format!("{active:.1}%")), ("Read speed".to_string(), format_rate(read_bps)), ("Write speed".to_string(), format_rate(write_bps))];
                // Only when the disk reports one.
                if let Some((d, _)) = one.filter(|(d, _)| !d.firmware.is_empty()) {
                    facts.push(("Firmware".to_string(), d.firmware.clone()));
                }
                // How full the volumes are: those wholly on this disk, or all of them once.
                let space = match one {
                    Some((d, _)) => d.space,
                    None => s.disk.space,
                };
                if let Some(space) = space {
                    facts.push(("Storage used".to_string(), disk_drives::space_text(space)));
                }
                cy += dl(&p, &pal, ix, cy, iw, &facts);
            }
            Device::Network => {
                let c = fd.metric("network");
                let (num, unit) = if s.net_known() { split_unit(&format_rate(s.network.rx_bps + s.network.tx_bps)) } else { ("\u{2014}".to_string(), String::new()) };
                let hd = Head { title: "Network", sub: SOURCES.network.into(), value: (num, unit), side: if s.network.link_bps > 0.0 { format!("{:.2}% of link", s.network.utilization_pct) } else { String::new() } };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let m = widgets::eased(ui.ctx(), "perf-net-rate", fd.hist.net_rx.max().max(fd.hist.net_tx.max()).max(100.0 * 1024.0));
                let lines = [PlotLine::new(&fd.hist.net_rx, c, (0.0, m)), PlotLine::new(&fd.hist.net_tx, c, (0.0, m)).dashed(c)];
                let readout = |age: usize| format!("{} \u{b7} \u{2193} {} \u{b7} \u{2191} {}", ago(iv, age), format_rate(at(&fd.hist.net_rx, age) as f64), format_rate(at(&fd.hist.net_tx, age) as f64));
                let (ph, hover) = plot(ui, &pc, "network", ix, cy, gw, 200.0, 64.0, &rate_labels(m), &lines, &readout);
                if figure {
                    let n = &s.network;
                    let link = link_speed_text(n.link_bps);
                    let labels = [("Receive", format!("\u{2193} {}", format_rate(n.rx_bps))), ("Send", format!("\u{2191} {}", format_rate(n.tx_bps))), ("Utilization", link_use_text(n.link_bps, n.utilization_pct)), ("Link speed", link)];
                    figure_at(&p, &pal, Exploded::Network, ix + gw, cy, 200.0, &labels);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 64.0, cy + 12.0, &[("Receive", c, false), ("Send", c, true)], hover.as_deref(), ix + gw);
                cy += 24.0;
                let facts = vec![
                    ("Receive".to_string(), format_rate(s.network.rx_bps)),
                    ("Send".to_string(), format_rate(s.network.tx_bps)),
                    ("Link speed".to_string(), link_speed_text(s.network.link_bps)),
                    ("Utilization".to_string(), link_use_text(s.network.link_bps, s.network.utilization_pct)),
                ];
                cy += dl(&p, &pal, ix, cy, iw, &facts);
            }
            Device::Thermal => {
                let c = fd.metric("thermal");
                let src = if s.thermal.sources.is_empty() { SOURCES.thermal.to_string() } else { s.thermal.sources.clone() };
                let value = match s.thermal.max_c() {
                    Some(t) => split_unit(&self.temp(t)),
                    None => ("\u{2014}".to_string(), String::new()),
                };
                // Which reading the headline is: "GPU · WDDM", "CPU · PawnIO", "TZ00 · ACPI zone".
                let hottest = s.thermal.hottest();
                let side = hottest.as_ref().map(|h| format!("{} \u{b7} {}", h.what, h.source)).unwrap_or_default();
                let hd = Head { title: "Thermals", sub: src, value, side };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let lines = [PlotLine::new(&fd.hist.thermal, c, (0.0, THERMAL_MAX_C))];
                let readout = |age: usize| format!("{} \u{b7} {}", ago(iv, age), temp_or_none(at(&fd.hist.thermal, age), |t| self.temp(t)));
                let labels = thermal_axis_labels(self.settings.fahrenheit());
                let (ph, hover) = plot(ui, &pc, "thermal", ix, cy, gw, 200.0, 36.0, &labels, &lines, &readout);
                if figure {
                    let th = &s.thermal;
                    let zones = th.zones.len();
                    let labels = [
                        ("Hottest now", dash_or(hottest.as_ref().map(|h| format!("{} {}", h.what, self.temp(h.c))))),
                        ("Peak since start", dash_or(self.perf.thermal_peak.as_ref().map(|(t, at)| format!("{} at {at}", self.temp(*t))))),
                        ("CPU", dash_or(th.cpu.as_ref().map(|(l, c)| format!("{l} {}", self.temp(*c))))),
                        ("Zones", dash_or((zones > 0).then(|| format!("{zones} thermal zone{}", if zones == 1 { "" } else { "s" })))),
                    ];
                    figure_at(&p, &pal, Exploded::Thermal, ix + gw, cy, 200.0, &labels);
                }
                cy += ph + 10.0;
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("Hottest sensor", c, false)], hover.as_deref(), ix + gw);
                cy += 30.0;
                if s.thermal.max_c().is_none() && s.thermal.gpu.is_empty() {
                    // Wraps inside the card (the hint is long); more than three lines end in an ellipsis.
                    let hint = format!("No temperature source available. {}.", SOURCES.thermal_hint);
                    cy += widgets::text_wrapped(&p, pos2(ix, cy), &hint, theme::ui(12.0), theme::status(&pal).warn, iw, 3).height() + 8.0;
                } else {
                    let mut facts: Vec<(String, String)> = s.thermal.cpu.iter().map(|(l, c)| (l.clone(), self.temp(*c))).collect();
                    // Every GPU reading is listed; only the GPU's own (not hot spot or memory)
                    // counts toward the hottest figure above.
                    facts.extend(s.thermal.gpu.iter().map(|g| (g.label.clone(), self.temp(g.c))));
                    facts.extend(s.thermal.zones.iter().map(|(z, c)| (z.clone(), self.temp(*c))));
                    cy += dl(&p, &pal, ix, cy, iw, &facts);
                }
            }
            Device::Docker => {
                let t = self.docker.ui.snap.totals;
                let c = fd.metric("docker");
                let mem_c = fd.metric("memory");
                let snap = &self.docker.ui.snap;
                let host = if snap.active.is_empty() { "N/A".to_string() } else { snap.active.clone() };
                let engine = snap.info.as_ref().map(|i| i.server_version.clone()).filter(|v| !v.is_empty()).unwrap_or_else(|| "N/A".into());
                let hd = Head { title: "Docker", sub: format!("Host {host} \u{b7} Engine {engine}"), value: (format!("{:.1}", t.cpu_pct), "% CPU".into()), side: format!("{} / {} running", t.running, t.containers) };
                cy += head(&p, &pal, ix, cy, iw, &hd) + 10.0;
                let mem_max = snap.info.as_ref().map(|i| i.mem_total as f32).filter(|m| *m > 0.0).unwrap_or(self.perf_docker.totals_mem.max()).max(1.0);
                let lines = [PlotLine::new(&self.perf_docker.totals_cpu, c, (0.0, 100.0)), PlotLine::new(&self.perf_docker.totals_mem, mem_c, (0.0, mem_max))];
                let readout = |age: usize| format!("{} \u{b7} {:.1}% \u{b7} {}", ago(iv, age), at(&self.perf_docker.totals_cpu, age), format_bytes(at(&self.perf_docker.totals_mem, age) as f64));
                // Docker stats arrive on their own clock, so the plot does not glide on the sampler's.
                let still = PlotCtx { phase: 0.0, glide: false, ..pc };
                let (ph, hover) = plot(ui, &still, "docker", ix, cy, gw, 160.0, 36.0, &percent_labels(), &lines, &readout);
                if figure {
                    let known = |v: &str| dash_or((v != "N/A").then(|| v.to_string()));
                    let labels = [
                        ("Containers", format!("{} / {} running", t.running, t.containers)),
                        ("Engine", known(&engine)),
                        ("Runtime", format!("{:.1}% CPU \u{b7} {}", t.cpu_pct, format_bytes(t.mem_used as f64))),
                        ("Host", known(&host)),
                    ];
                    figure_at(&p, &pal, Exploded::Docker, ix + gw, cy, 194.0, &labels);
                }
                cy += ph + 10.0;
                let scale = format!("Memory (scale {})", format_bytes(mem_max as f64));
                legend_row(&p, &pal, ix + 36.0, cy + 12.0, &[("CPU", c, false), (scale.as_str(), mem_c, false)], hover.as_deref(), ix + gw);
                cy += 24.0;
                let facts = vec![
                    ("Containers".to_string(), format!("{} / {}", t.running, t.containers)),
                    ("CPU".to_string(), format!("{:.1}%", t.cpu_pct)),
                    ("Memory".to_string(), format_bytes(t.mem_used as f64)),
                    ("Net rx".to_string(), format_rate(t.net_rx_bps)),
                    ("Net tx".to_string(), format_rate(t.net_tx_bps)),
                    ("Disk read".to_string(), format_rate(t.blk_read_bps)),
                    ("Disk write".to_string(), format_rate(t.blk_write_bps)),
                ];
                cy += dl(&p, &pal, ix, cy, iw, &facts);
            }
        }
        cy += 18.0;
        let rect = Rect::from_min_max(pos2(x, y), pos2(x + w, cy));
        p.set(slot, Shape::Vec(vec![Shape::rect_filled(rect, 10.0, pal.card), Shape::rect_stroke(rect, 10.0, Stroke::new(1.0_f32, pal.card_stroke), egui::StrokeKind::Outside)]));
        let mut h = rect.height();
        if self.device == Device::Docker {
            // The container cards sit under the card.
            let cards = self.docker.ui.snap.containers.len();
            let cols = if w >= 720.0 { 3 } else { 2 };
            let content = cards.div_ceil(cols).max(1) as f32 * 120.0;
            let area = Rect::from_min_size(pos2(x, rect.bottom() + GAP), vec2(w, content.min(440.0)));
            self.docker_cards(ui, area, fd);
            h += GAP + area.height();
        }
        h
    }

    // ------------------------------------------------------------------ other resources

    /// The devices other than the selected one as a strip of mini graphs. Returns its height.
    fn other_resources(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, act: &mut Option<Act>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let others: Vec<Device> = Device::ALL.into_iter().filter(|d| *d != self.device && self.device_listed(*d)).collect();
        if others.is_empty() {
            return 0.0;
        }
        // Label and value share the first line only where the longest pair ("NETWORK" and both
        // rates) fits; in narrower tiles the value gets a line of its own instead of running
        // into the label. The width decides for the whole strip, so the tiles stay alike and the
        // layout does not flip when a value grows by a digit.
        let stacked = strip_stacked(w, others.len());
        let tile_h = if stacked { 90.0 } else { 80.0 };
        // Tiles that would be too narrow for their values go on further rows (balanced).
        let mut tiles = Vec::new();
        let mut row_y = y;
        for n in strip_rows(w, others.len(), stacked) {
            tiles.extend(kit::tile_strip(&p, Rect::from_min_size(pos2(x, row_y), vec2(w, tile_h)), &pal, n));
            row_y += tile_h + 8.0;
        }
        let strip_h = row_y - 8.0 - y;
        for (d, t) in others.iter().zip(tiles) {
            let resp = ui.interact(t, ui.scope_id().with(("other", d.key())), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() {
                p.rect_filled(t, 0.0, pal.raise(0.03));
            }
            let view = self.dev_view(*d, fd);
            widgets::tracked_text(&p, pos2(t.left() + 14.0, t.top() + 10.0), Align2::LEFT_TOP, &d.title().to_uppercase(), theme::ui(11.0), pal.text_faint, 0.14);
            let spark = if stacked {
                widgets::text_fit(&p, pos2(t.left() + 14.0, t.top() + 27.0), Align2::LEFT_TOP, &view.value, theme::mono(12.0), pal.text, t.width() - 28.0);
                Rect::from_min_size(pos2(t.left() + 14.0, t.top() + 46.0), vec2(t.width() - 28.0, 34.0))
            } else {
                widgets::text_fit(&p, pos2(t.right() - 14.0, t.top() + 9.0), Align2::RIGHT_TOP, &view.value, theme::mono(12.0), pal.text, t.width() * 0.6);
                Rect::from_min_size(pos2(t.left() + 14.0, t.top() + 28.0), vec2(t.width() - 28.0, 40.0))
            };
            if *d == Device::Docker {
                widgets::sparkline_unsynced(&p, spark, &view.lines, 0.12);
            } else {
                widgets::sparkline(&p, spark, &view.lines, 0.12, view.dashed_second);
            }
            if resp.clicked() {
                *act = Some(Act::Device(*d));
            }
        }
        strip_h
    }

    // ------------------------------------------------------------------ CPU: processors and consumers

    /// "Logical processors" and "Top CPU consumers" side by side. Returns the height.
    #[allow(clippy::too_many_arguments)]
    fn cpu_bottom(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32, groups: &[AppGroup], act: &mut Option<Act>) -> f32 {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let s = fd.snap;
        let iv = self.settings.update_speed().interval();
        let wide = w >= 760.0;
        let (lw, rw) = if wide { ((w - GAP) * 0.6, (w - GAP) * 0.4) } else { (w, w) };

        // Facts under the cores.
        let facts: Vec<(String, String)> = [
            self.caps.clocks.then(|| ("Speed".to_string(), s.cpu.current_mhz.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or_else(|| "N/A".into()))),
            self.caps.thermal.then(|| ("Temp".to_string(), s.thermal.cpu.as_ref().map(|c| c.1).or_else(|| s.thermal.board_max_c()).map(|t| self.temp(t)).unwrap_or_else(|| "\u{2014}".into()))),
            Some(("Processes".to_string(), s.process_count.to_string())),
            Some(("Threads".to_string(), s.thread_count.to_string())),
            s.handle_count.map(|h| (SOURCES.handles.to_string(), h.to_string())),
            Some(("Up time".to_string(), super::fmt_hms(s.uptime))),
        ]
        .into_iter()
        .flatten()
        .collect();
        let c_w = lw - 44.0;
        let n = fd.hist.per_core.len();
        let show_cores = self.cpus_expanded() && n > 0;
        let cols = (((c_w + 6.0) / (96.0 + 6.0)).floor() as usize).clamp(1, n.max(1));
        let rows = n.div_ceil(cols);
        let cells_h = if show_cores { rows as f32 * 56.0 + (rows.max(1) - 1) as f32 * 6.0 + 14.0 } else { 0.0 };
        let dl_cols = (((c_w + 12.0) / 112.0).floor() as usize).clamp(1, facts.len());
        let dl_h = 14.0 + facts.len().div_ceil(dl_cols) as f32 * 48.0 - 12.0;
        let left_h = 40.0 + cells_h + dl_h + 18.0;

        // The busiest apps, already in the calm order of `Steady`.
        let mut order: Vec<&AppGroup> = groups.iter().collect();
        order.truncate(5);
        let right_natural = 40.0 + order.len().max(1) as f32 * 32.0 + 14.0 + 18.0 + 18.0;
        let h = if wide { left_h.max(right_natural) } else { left_h };

        let left = Rect::from_min_size(pos2(x, y), vec2(lw, h));
        // What a core's sparkline (the cell less its 1 px inset) shows of the history.
        let spark_w = (c_w - 6.0 * (cols - 1) as f32) / cols as f32 - 2.0;
        let span = span_text(fd.hist.per_core.first().map_or(0, |s| widgets::spark_visible(spark_w, s.values.len(), s.capacity)), iv);
        let c = kit::section_card(&p, left, &pal, &format!("Logical processors \u{b7} last {span}"), "");
        // View toggle and fold button at the right of the heading.
        let mut tx = left.right() - 22.0;
        let fold = Rect::from_center_size(pos2(tx - 10.0, left.top() + 25.0), vec2(24.0, 24.0));
        let fr = ui.interact(fold, ui.scope_id().with("cores-fold"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if self.cpus_expanded() { "Hide the processors" } else { "Show the processors" });
        if fr.hovered() {
            p.rect_filled(fold, 4.0, pal.raise(0.06));
        }
        widgets::draw_icon(&p, Rect::from_center_size(fold.center(), vec2(12.0, 12.0)), if self.cpus_expanded() { Icon::ChevronDown } else { Icon::ChevronRight }, pal.text_faint);
        if fr.clicked() {
            *act = Some(Act::Cores(!self.cpus_expanded()));
        }
        tx = fold.left() - 8.0;
        if show_cores {
            let labels = [("graphs", CoreView::Graphs), ("heatmap", CoreView::Heatmap)];
            for (label, view) in labels.iter().rev() {
                let tw = widgets::text_width(&p, label, theme::mono(11.0)) + 16.0;
                let r = Rect::from_min_size(pos2(tx - tw, left.top() + 13.0), vec2(tw, 24.0));
                let resp = ui.interact(r, ui.scope_id().with(("core-view", *label)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                let on = self.perf.core_view == *view;
                if on {
                    p.rect_filled(r, 3.0, pal.raise(0.12));
                } else if resp.hovered() {
                    p.rect_filled(r, 3.0, pal.raise(0.05));
                }
                p.text(r.center(), Align2::CENTER_CENTER, label, theme::mono(11.0), if on { pal.text } else { pal.text_faint });
                if resp.clicked() {
                    *act = Some(Act::View(*view));
                }
                tx = r.left() - 2.0;
            }
        }
        let mut cy = c.top();
        if show_cores {
            let cw = (c.width() - 6.0 * (cols - 1) as f32) / cols as f32;
            for (i, series) in fd.hist.per_core.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let cell = Rect::from_min_size(pos2(c.left() + col as f32 * (cw + 6.0), cy + row as f32 * 62.0), vec2(cw, 56.0));
                let pct = fd.smooth.per_core.get(i).copied().unwrap_or_else(|| series.last());
                let f = (pct / 100.0).clamp(0.0, 1.0);
                match self.perf.core_view {
                    CoreView::Graphs => {
                        p.rect_filled(cell, 4.0, pal.well());
                        widgets::sparkline(&p, Rect::from_min_max(pos2(cell.left() + 1.0, cell.top() + 18.0), cell.max - vec2(1.0, 1.0)), &[(series, pal.accent, (0.0, 100.0))], 0.2, false);
                        p.rect_stroke(cell, 4.0, Stroke::new(1.0_f32, pal.row_rule()), egui::StrokeKind::Outside);
                        p.text(cell.left_top() + vec2(6.0, 5.0), Align2::LEFT_TOP, i.to_string(), theme::mono(10.0), pal.text_faint);
                        p.text(cell.right_top() + vec2(-6.0, 4.0), Align2::RIGHT_TOP, format!("{pct:.0}%"), theme::mono(11.0), pal.text_dim);
                    }
                    CoreView::Heatmap => {
                        p.rect_filled(cell, 4.0, mix(pal.well(), pal.accent, f));
                        p.rect_stroke(cell, 4.0, Stroke::new(1.0_f32, pal.row_rule()), egui::StrokeKind::Outside);
                        let ink = if f > 0.55 { pal.body } else { pal.text };
                        p.text(cell.center(), Align2::CENTER_CENTER, format!("{pct:.0}%"), theme::mono(11.0), ink);
                    }
                }
            }
            cy += cells_h;
        }
        dl(&p, &pal, c.left(), cy, c.width(), &facts);

        // Top CPU consumers.
        let right = if wide { Rect::from_min_size(pos2(x + lw + GAP, y), vec2(rw, h)) } else { Rect::from_min_size(pos2(x, y + h + GAP), vec2(rw, right_natural)) };
        let secs = self.perf.top_spark.values().map(|s| s.values.len()).max().unwrap_or(0);
        let tc = kit::section_card(&p, right, &pal, &format!("Top CPU consumers \u{b7} {}", span_text(secs, iv)), "");
        let cap = self.spark_cap();
        let hi = order.iter().filter_map(|g| self.perf.top_spark.get(&g.key)).map(|s| s.max()).fold(5.0_f32, f32::max);
        for (i, g) in order.iter().enumerate() {
            let ry = tc.top() + i as f32 * 32.0;
            let row = Rect::from_min_size(pos2(tc.left(), ry), vec2(tc.width(), 32.0));
            let resp = ui.interact(row, ui.scope_id().with(("top-cpu", &g.key)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() {
                p.rect_filled(row.shrink2(vec2(-6.0, 1.0)), 4.0, pal.raise(0.04));
            }
            let cy = row.center().y;
            let pct_w = 52.0;
            let spark = Rect::from_min_size(pos2(row.right() - pct_w - 12.0 - 84.0, cy - 10.0), vec2(84.0, 20.0));
            let mut name_w = spark.left() - row.left() - 12.0;
            if g.system {
                let bw = widgets::badge_width(&p, crate::process::os_badge());
                name_w -= bw + 8.0;
                let nr = widgets::text_fit(&p, pos2(row.left(), cy), Align2::LEFT_CENTER, &g.name, theme::ui(13.0), pal.text_table(), name_w);
                widgets::badge(&p, pos2(nr.right() + 8.0, cy), crate::process::os_badge(), &pal);
            } else {
                widgets::text_fit(&p, pos2(row.left(), cy), Align2::LEFT_CENTER, &g.name, theme::ui(13.0), pal.text_table(), name_w);
            }
            if let Some(series) = self.perf.top_spark.get(&g.key) {
                spark_fixed(&p, spark, series, pal.accent, (0.0, hi), cap, 0.0, self.settings.graphs.smooth_curves);
            }
            p.text(pos2(row.right(), cy), Align2::RIGHT_CENTER, cpu_text(g.cpu), theme::mono(12.0), pal.text_table());
            if resp.clicked() {
                if let Some(r) = fd.rows.get(g.top) {
                    *act = Some(Act::Select(r.pid, r.create_time));
                }
            }
        }
        if order.is_empty() {
            kit::empty_state(&p, tc, &pal, "No processes");
        }
        let st = theme::status(&pal);
        let link = p.text(pos2(tc.left(), right.bottom() - 18.0 - 18.0), Align2::LEFT_TOP, "Sort processes by CPU \u{2192}", theme::ui(13.0), st.info);
        let lr = ui.interact(link.expand(4.0), ui.scope_id().with("sort-cpu"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if lr.hovered() {
            p.text(link.left_top(), Align2::LEFT_TOP, "Sort processes by CPU \u{2192}", theme::ui(13.0), lighten(st.info, 0.25));
        }
        if lr.clicked() {
            *act = Some(Act::SortCpu);
        }
        if wide {
            h
        } else {
            h + GAP + right_natural
        }
    }

    /// The CPU facts the graph cards leave out. Returns the height.
    fn cpu_facts(&self, ui: &mut egui::Ui, fd: &FrameData<'_>, x: f32, y: f32, w: f32) -> f32 {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let s = fd.snap;
        let det = self.pro.cpu_details.clone().unwrap_or_default();
        let yes_no = |b: Option<bool>| b.map(|v| if v { "Yes" } else { "No" }.to_string()).unwrap_or_else(|| "Unknown".into());
        let plan = self.pro.power.ok().map(|i| i.scheme_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "\u{2014}".into());
        let ghz = |m: Option<u32>| m.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or_else(|| "N/A".into());
        let clocks = [
            ("Base speed", ghz(s.cpu.base_mhz)),
            ("Boost speed", ghz(s.cpu.boost_mhz)),
            ("Fastest core", s.cpu.per_core_mhz.iter().max().map(|m| format!("{:.2} GHz", *m as f32 / 1000.0)).unwrap_or_else(|| "N/A".into())),
        ];
        let items: Vec<(String, String, bool, Option<Color32>)> = clocks
            .into_iter()
            .filter(|_| self.caps.clocks)
            .chain([
            ("Kernel time", format!("{:.1}%", s.cpu.kernel_pct)),
            (crate::pro::power::scheme_words().0, plan),
            ("Sockets", det.sockets.max(1).to_string()),
            ("Physical cores", det.cores.to_string()),
            ("Logical processors", s.cpu.logical_count.to_string()),
            ("Virtual machine", if det.virtual_machine { "Yes" } else { "No" }.into()),
            ("Virtualization", det.virtualization.map(|v| if v { "Enabled" } else { "Disabled" }.to_string()).unwrap_or_else(|| "Unknown".into())),
            ("L1 cache", format_bytes(det.l1 as f64)),
            ("L2 cache", format_bytes(det.l2 as f64)),
            ("L3 cache", format_bytes(det.l3 as f64)),
        ])
        // Fast Startup is a Windows setting; other OSes report None and get no row.
        .chain((cfg!(windows) || det.fast_startup.is_some()).then(|| ("Fast Startup", yes_no(det.fast_startup))))
        .map(|(k, v)| (k.to_string(), v, false, None))
        .collect();
        let cols = if w >= 900.0 { 3 } else if w >= 560.0 { 2 } else { 1 };
        let per = items.len().div_ceil(cols);
        let h = 40.0 + per as f32 * 24.0 + 18.0;
        let rect = Rect::from_min_size(pos2(x, y), vec2(w, h));
        let c = kit::section_card(&p, rect, &pal, "CPU details", "");
        let cw = (c.width() - 24.0 * (cols - 1) as f32) / cols as f32;
        for (i, chunk) in items.chunks(per).enumerate() {
            kit::kv_grid(&p, Rect::from_min_size(pos2(c.left() + i as f32 * (cw + 24.0), c.top()), vec2(cw, c.height())), &pal, chunk, 130.0, 24.0);
        }
        h
    }

    // ------------------------------------------------------------------ Docker containers

    /// Scrollable grid of container cards: name, state, CPU and memory, and a CPU graph for
    /// containers with a series. Containers without one (not running) come last, dimmed.
    fn docker_cards(&self, ui: &mut egui::Ui, area: Rect, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let blue = fd.metric("docker");
        let snap = &self.docker.ui.snap;
        let mut cards: Vec<(&Container, Option<&Series>)> = snap
            .containers
            .iter()
            .map(|c| (c, self.perf_docker.per_container.iter().find(|(id, _)| *id == c.id).map(|(_, s)| s)))
            .collect();
        cards.sort_by_key(|(_, s)| s.is_none());
        if cards.is_empty() {
            ui.painter().text(area.left_top() + vec2(0.0, 8.0), Align2::LEFT_TOP, "No containers on this engine.", theme::ui(12.0), pal.text_dim);
            return;
        }
        let cols = if area.width() >= 720.0 { 3 } else { 2 };
        let (gap, ch) = (10.0, 110.0);
        let cw = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let content_h = cards.len().div_ceil(cols) as f32 * (ch + gap) - gap;
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            egui::ScrollArea::vertical().id_salt("docker-cards").auto_shrink([false, false]).show_029(ui, [false, true], |ui| {
                let (content, _) = ui.allocate_exact_size(vec2(area.width(), content_h), Sense::hover());
                let p = ui.painter().clone();
                for (i, (c, series)) in cards.iter().enumerate() {
                    let (col, row) = (i % cols, i / cols);
                    let r = Rect::from_min_size(pos2(content.left() + col as f32 * (cw + gap), content.top() + row as f32 * (ch + gap)), vec2(cw, ch));
                    let running = c.state.is_running();
                    if running {
                        widgets::tinted_card(&p, r, &pal, blue);
                    } else {
                        widgets::card(&p, r, &pal);
                    }
                    let name_color = if running { pal.text } else { pal.text_dim };
                    widgets::text_fit(&p, pos2(r.left() + 12.0, r.top() + 16.0), Align2::LEFT_CENTER, &c.name, theme::semibold(13.0), name_color, cw - 110.0);
                    p.text(pos2(r.right() - 12.0, r.top() + 16.0), Align2::RIGHT_CENTER, c.state.label(), theme::ui(11.5), if running { blue } else { pal.text_dim });
                    let line = match snap.stats_last.get(&c.id) {
                        Some(s) if running => format!("CPU {:.1}%   Mem {}", s.cpu_pct, format_bytes(s.mem_used as f64)),
                        _ => c.status.clone(),
                    };
                    widgets::text_fit(&p, pos2(r.left() + 12.0, r.top() + 38.0), Align2::LEFT_CENTER, &line, theme::mono(11.0), pal.text_faint, cw - 24.0);
                    if let (true, Some(s)) = (running, *series) {
                        let gr = Rect::from_min_max(pos2(r.left() + 12.0, r.top() + 54.0), pos2(r.right() - 12.0, r.bottom() - 12.0));
                        widgets::sparkline_unsynced(&p, gr, &[(s, blue, (0.0, 100.0))], 0.12);
                    }
                }
            });
        });
    }
}

/// Title row: name and facts at the left, the big value at the right. Returns its height.
fn head(p: &egui::Painter, pal: &Palette, x: f32, y: f32, w: f32, h: &Head<'_>) -> f32 {
    let (num, unit) = &h.value;
    let right = x + w;
    let mut edge = right;
    if !h.side.is_empty() {
        let r = p.text(pos2(right, y + 36.0), Align2::RIGHT_BOTTOM, &h.side, theme::mono(12.0), pal.text_faint);
        edge = r.left() - 10.0;
    }
    if !unit.is_empty() {
        let r = p.text(pos2(edge, y + 35.0), Align2::RIGHT_BOTTOM, unit, theme::ui(20.0), pal.text_faint);
        edge = r.left() - 2.0;
    }
    let n = p.text(pos2(edge, y + 41.0), Align2::RIGHT_BOTTOM, num, theme::light(40.0), pal.text);
    let text_w = (n.left() - x - 16.0).max(80.0);
    widgets::text_fit(p, pos2(x, y), Align2::LEFT_TOP, h.title, theme::light(20.0), pal.text, text_w);
    widgets::text_fit(p, pos2(x, y + 28.0), Align2::LEFT_TOP, &h.sub, theme::ui(12.0), pal.text_faint, text_w);
    48.0
}

/// How a [`plot`] is set up.
struct PlotCtx<'a> {
    pal: &'a Palette,
    /// Smooth the curves (`graphs.smoothCurves`).
    smooth: bool,
    /// Glow of the traces (the appearance bloom, 0 = none).
    bloom: f32,
    /// Every trace in one neutral blue (`graphs.colorKeyed` off).
    neutral: bool,
    /// History compression: the regions and the px per update of the live part
    /// (`graphs.compressedTime`, `historyMultiplier`, `pixelsPerUpdate`); 1 region fits the whole
    /// history across the width.
    regions: u32,
    px_per_update: f32,
    /// The update interval, for the time labels.
    interval: Option<Duration>,
    /// The newest sample scrolls in over the update interval (`FrameData::phase`, `glide`).
    phase: f32,
    glide: bool,
}

/// A history graph with y labels, time labels and a readout on hover. `gutter` is the width
/// of the y labels. Returns the height used and the hover readout.
#[allow(clippy::too_many_arguments)]
fn plot(ui: &mut egui::Ui, cx: &PlotCtx<'_>, id: &str, x: f32, y: f32, w: f32, h: f32, gutter: f32, ylabels: &[String; 5], lines: &[PlotLine<'_>], readout: &dyn Fn(usize) -> String) -> (f32, Option<String>) {
    let pal = cx.pal;
    let p = ui.painter().clone();
    let gr = Rect::from_min_size(pos2(x + gutter, y), vec2((w - gutter).max(40.0), h));
    for (i, label) in ylabels.iter().enumerate() {
        let ly = gr.top() + gr.height() * i as f32 / 4.0;
        p.text(pos2(gr.left() - 8.0, ly), Align2::RIGHT_CENTER, label, theme::mono(10.0), pal.text_faint);
        let ly = (ly.round() + 0.5).clamp(gr.top(), gr.bottom());
        p.line_segment([pos2(gr.left(), ly), pos2(gr.right(), ly)], Stroke::new(1.0_f32, pal.grid));
    }
    let cap = lines.first().map_or(2, |l| l.series.capacity).max(2);
    // Uniform time fits the whole history across the width; compressed time keeps the configured
    // px per update in the live part and squeezes the older history into the rest.
    let (regions, ppu) = if cx.regions > 1 { (cx.regions, cx.px_per_update) } else { (1, gr.width() / (cap - 1) as f32) };
    let ppu = ppu.max(widgets::MIN_PX_PER_UPDATE);
    let axis = widgets::TimeAxis::new(gr.right(), gr.width(), ppu, regions);
    for (i, l) in lines.iter().enumerate() {
        let color = if cx.neutral { widgets::neutral_graph_color(i) } else { l.color };
        let mut g = Graph::new(vec![GraphLine { series: l.series, color, range: l.range }]);
        g.regions = regions;
        g.px_per_update = ppu;
        g.bloom = cx.bloom;
        g.markers = false;
        g.smooth = cx.smooth;
        g.line_width = l.width;
        g.fill = l.fill;
        g.dashed = l.dashed;
        g.phase = cx.phase;
        g.glide = cx.glide;
        draw_graph(&p, gr, &g);
    }
    // Time labels from the history length and the update interval.
    let label_y = gr.bottom() + 6.0;
    if let Some(iv) = cx.interval {
        for i in 0..6 {
            let secs = (axis.age_at(gr.width() * (5 - i) as f32 / 5.0) * iv.as_secs_f32()).round() as u64;
            let text = if i == 5 { "now".to_string() } else { format!("\u{2212}{}:{:02}", secs / 60, secs % 60) };
            let lx = gr.left() + gr.width() * i as f32 / 5.0;
            let anchor = match i {
                0 => Align2::LEFT_TOP,
                5 => Align2::RIGHT_TOP,
                _ => Align2::CENTER_TOP,
            };
            p.text(pos2(lx, label_y), anchor, text, theme::mono(10.0), pal.text_faint);
        }
    }
    // Cursor.
    let resp = ui.interact(gr, ui.scope_id().with(("plot", id)), Sense::hover());
    let mut text = None;
    if let (Some(pos), Some(first)) = (resp.hover_pos(), lines.first()) {
        let n = first.series.values.len();
        // The samples sit where draw_graph put them this frame (shifted by the glide).
        let offset = if cx.glide && n >= 2 { cx.phase.clamp(0.0, 1.0) - 1.0 } else { cx.phase.clamp(0.0, 1.0) };
        let age = (axis.age_at(gr.right() - pos.x) - offset).round().max(0.0) as usize;
        if age < n {
            let cx = axis.x_of(age as f32 + offset).min(gr.right());
            p.line_segment([pos2(cx, gr.top()), pos2(cx, gr.bottom())], Stroke::new(1.0_f32, pal.raise(0.3)));
            for l in lines {
                // A sample without a reading (NaN) has no point on the trace.
                if let Some(v) = l.series.values.len().checked_sub(1 + age).and_then(|i| l.series.values.get(i)).filter(|v| !v.is_nan()) {
                    let (lo, hi) = l.range;
                    let ty = gr.bottom() - ((v - lo) / if hi > lo { hi - lo } else { 1.0 }).clamp(0.0, 1.0) * gr.height();
                    p.circle_filled(pos2(cx, ty), 3.5, l.color);
                    p.circle_stroke(pos2(cx, ty), 3.5, Stroke::new(1.0_f32, pal.card));
                }
            }
            text = Some(readout(age));
        }
    }
    (h + 6.0 + 14.0, text)
}

/// Legend of the plot's lines at the left and the hover readout at the right.
#[allow(clippy::too_many_arguments)]
fn legend_row(p: &egui::Painter, pal: &Palette, x: f32, cy: f32, items: &[(&str, Color32, bool)], readout: Option<&str>, right: f32) {
    // The readout box, if any; legend items it would cover give way while it shows.
    let boxed = readout.map(|t| {
        let w = widgets::text_width(p, t, theme::mono(12.0)) + 16.0;
        (t, Rect::from_min_size(pos2(right - w, cy - 12.0), vec2(w, 24.0)))
    });
    let mut lx = x;
    for (label, color, dashed) in items {
        if boxed.is_some_and(|(_, r)| lx + 22.0 + widgets::text_width(p, label, theme::ui(12.0)) > r.left() - 12.0) {
            break;
        }
        let a = pos2(lx, cy);
        let b = pos2(lx + 14.0, cy);
        if *dashed {
            p.extend(Shape::dashed_line(&[a, b], Stroke::new(2.0_f32, *color), 3.0, 2.0));
        } else {
            p.line_segment([a, b], Stroke::new(2.0_f32, *color));
        }
        let r = p.text(pos2(lx + 22.0, cy), Align2::LEFT_CENTER, label, theme::ui(12.0), pal.text_dim);
        lx = r.right() + 18.0;
    }
    if let Some((t, r)) = boxed {
        p.rect_filled(r, 4.0, pal.raise(0.06));
        p.rect_stroke(r, 4.0, Stroke::new(1.0_f32, pal.raise(0.08)), egui::StrokeKind::Outside);
        p.text(r.center(), Align2::CENTER_CENTER, t, theme::mono(12.0), pal.text);
    }
}

/// Columns of a [`dl`] `w` px wide: as many as keep every column at least 100 px and as wide as
/// the widest label (`label_ws`), so one heading never runs into the next.
fn dl_columns(w: f32, label_ws: &[f32]) -> usize {
    let min_col = label_ws.iter().copied().fold(100.0_f32, f32::max).ceil();
    (((w + 12.0) / (min_col + 12.0)).floor() as usize).clamp(1, label_ws.len().max(1))
}

/// Label / value facts in auto-fitting columns under a hairline. Returns the height used.
fn dl(p: &egui::Painter, pal: &Palette, x: f32, y: f32, w: f32, items: &[(String, String)]) -> f32 {
    p.line_segment([pos2(x, y + 0.5), pos2(x + w, y + 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
    let labels: Vec<String> = items.iter().map(|(k, _)| k.to_uppercase()).collect();
    let label_ws: Vec<f32> = labels.iter().map(|l| widgets::tracked_width(p, l, theme::ui(11.0), 0.12)).collect();
    let cols = dl_columns(w, &label_ws);
    let cw = (w - 12.0 * (cols - 1) as f32) / cols as f32;
    for (i, (label, (_, v))) in labels.iter().zip(items).enumerate() {
        let (c, r) = (i % cols, i / cols);
        let ix = x + c as f32 * (cw + 12.0);
        let iy = y + 14.0 + r as f32 * 48.0;
        widgets::tracked_text(p, pos2(ix, iy), Align2::LEFT_TOP, label, theme::ui(11.0), pal.text_faint, 0.12);
        widgets::text_fit(p, pos2(ix, iy + 15.0), Align2::LEFT_TOP, v, theme::light(17.0), pal.text, cw);
    }
    let rows = items.len().div_ceil(cols).max(1);
    14.0 + rows as f32 * 48.0 - 12.0
}

/// A sparkline that keeps the time scale of a history of `cap` samples: a shorter series
/// fills only the right part.
#[allow(clippy::too_many_arguments)]
fn spark_fixed(p: &egui::Painter, rect: Rect, s: &Series, color: Color32, range: (f32, f32), cap: usize, fill: f32, smooth: bool) {
    if s.values.len() < 2 {
        return;
    }
    let mut g = Graph::new(vec![GraphLine { series: s, color: widgets::spark_color(0, color), range }]);
    g.regions = 1;
    g.px_per_update = rect.width() / cap.saturating_sub(1).max(1) as f32;
    g.bloom = 0.0;
    g.markers = false;
    g.smooth = smooth;
    g.line_width = 1.4;
    g.fill = fill;
    (g.phase, g.glide) = widgets::spark_glide();
    draw_graph(p, rect, &g);
}

/// Value `age` samples ago (0 = newest).
fn at(s: &Series, age: usize) -> f32 {
    s.values.len().checked_sub(1 + age).and_then(|i| s.values.get(i)).copied().unwrap_or(0.0)
}

/// "-m:ss" for a sample `age` updates old.
fn ago(iv: Option<Duration>, age: usize) -> String {
    match iv {
        Some(iv) => {
            let secs = (age as f32 * iv.as_secs_f32()).round() as u64;
            format!("\u{2212}{}:{:02}", secs / 60, secs % 60)
        }
        None => format!("\u{2212}{age}"),
    }
}

/// How long `samples` updates cover: "45 s", "16 min", "2.5 h".
fn span_text(samples: usize, iv: Option<Duration>) -> String {
    let Some(iv) = iv else { return "history".to_string() };
    let secs = (samples.saturating_sub(1)) as f32 * iv.as_secs_f32();
    if secs < 90.0 {
        format!("{:.0} s", secs.max(1.0))
    } else if secs < 5400.0 {
        format!("{:.0} min", secs / 60.0)
    } else {
        format!("{:.1} h", secs / 3600.0)
    }
}

fn percent_labels() -> [String; 5] {
    ["100%".into(), "75".into(), "50".into(), "25".into(), "0".into()]
}

/// Y labels of a rate axis from `max` bytes/s down to zero.
fn rate_labels(max: f32) -> [String; 5] {
    [1.0, 0.75, 0.5, 0.25, 0.0].map(|f| if f == 0.0 { "0".to_string() } else { format_rate((max * f) as f64) })
}

/// "11.8 GB" -> ("11.8", "GB").
fn split_unit(s: &str) -> (String, String) {
    match s.split_once(' ') {
        Some((n, u)) => (n.to_string(), u.to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// The exploded device right of a graph ending at `gx` (top `y`, `h` tall): a divider, the
/// drawing fitted to `h - 20` and its layer labels, which keep their column whatever `h` is.
#[allow(clippy::too_many_arguments)]
fn figure_at(p: &egui::Painter, pal: &Palette, kind: Exploded, gx: f32, y: f32, h: f32, labels: &[(&str, String); 4]) {
    let fx = gx + 28.0;
    p.line_segment([pos2(fx - 14.0, y), pos2(fx - 14.0, y + h)], Stroke::new(1.0_f32, pal.card_stroke));
    let fh = (h - 20.0).min(180.0);
    let fw = fh * 5.0 / 6.0;
    let labels = labels.each_ref().map(|(t, s)| (*t, s.as_str()));
    hardware::exploded_view(p, Rect::from_min_size(pos2(fx + 150.0 - fw, y + (h - fh) / 2.0), vec2(fw, fh)), kind, pal.text_code(), pal.accent, &labels, FIGURE_W - 28.0 - 150.0 - 18.0 + 16.0, pal);
}

/// "0.2 / 6.0 GB" with a known size, else what is known of use and size.
fn gpu_memory(used: Option<u64>, total: Option<u64>) -> Option<String> {
    match (used, total) {
        (Some(u), Some(t)) => Some(bytes_pair(u, t)),
        (Some(u), None) => Some(format!("{} in use", format_bytes(u as f64))),
        (None, Some(t)) => Some(format!("{} total", format_bytes(t as f64))),
        (None, None) => None,
    }
}

/// One row per temperature the GPUs report: "Temperature" / "Hot spot" / "Memory junction" ...
/// with "52°C · WDDM"; with `named`, the GPU's name in front. Nothing for a GPU that reports none.
fn gpu_temp_facts(adapters: &[&GpuAdapter], named: bool, fmt: impl Fn(f32) -> String) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for a in adapters {
        for t in &a.temps {
            let what = match t.kind {
                GpuTempKind::Gpu => "Temperature",
                GpuTempKind::HotSpot => "Hot spot",
                GpuTempKind::Memory => "Memory temperature",
                GpuTempKind::MemoryJunction => "Memory junction",
            };
            let label = if named { format!("{} \u{b7} {what}", a.name) } else { what.to_string() };
            rows.push((label, format!("{} \u{b7} {}", fmt(t.c), t.source)));
        }
    }
    rows
}

/// A temperature of the history for the hover readout; a sample without a reading (NaN) says so.
fn temp_or_none(c: f32, fmt: impl Fn(f32) -> String) -> String {
    if c.is_nan() {
        "no reading".to_string()
    } else {
        fmt(c)
    }
}

/// "1.0 Gb/s" for a link speed in bit/s; 0 is how a system that does not report it shows up.
fn link_speed_text(bps: f64) -> String {
    if bps > 0.0 {
        format!("{:.1} Gb/s", bps / 1e9)
    } else {
        "Not reported".to_string()
    }
}

/// Utilization of the link; unknown (as the link speed is) when the speed is.
fn link_use_text(bps: f64, pct: f32) -> String {
    if bps > 0.0 {
        format!("{pct:.2}%")
    } else {
        "Not reported".to_string()
    }
}

fn dash_or(s: Option<String>) -> String {
    s.unwrap_or_else(|| "\u{2014}".to_string())
}

/// "19.0 / 31.1 GB": both in the unit of the total.
pub(super) fn bytes_pair(used: u64, total: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut i, mut t) = (0, total as f64);
    while t >= 1024.0 && i < U.len() - 1 {
        t /= 1024.0;
        i += 1;
    }
    format!("{:.1} / {t:.1} {}", used as f64 / 1024f64.powi(i as i32), U[i])
}

/// Height of the device cards in a list `avail` px high with `n` cards: the usual height, less when
/// that would push the last card (and the Refresh button) out of view, but never below the least
/// that still shows a title and a trace (the list then scrolls).
fn device_card_h(avail: f32, n: usize) -> f32 {
    let n = n.max(1) as f32;
    ((avail - DEVICE_FOOTER_H) / n - 6.0).floor().clamp(DEVICE_CARD_MIN_H, DEVICE_CARD_H)
}

/// Whether a strip `w` wide with `tiles` tiles puts each value below its label.
fn strip_stacked(w: f32, tiles: usize) -> bool {
    w / (tiles.max(1) as f32) < STRIP_INLINE_MIN_W
}

/// Tiles per row of the strip of other devices: one row while the tiles stay at least
/// `STRIP_STACKED_MIN_W` wide (or sit inline), else as few rows as the width allows, filled evenly.
fn strip_rows(w: f32, tiles: usize, stacked: bool) -> Vec<usize> {
    if tiles == 0 {
        return Vec::new();
    }
    let cols = if stacked { (((w + 1.0) / (STRIP_STACKED_MIN_W + 1.0)).floor() as usize).clamp(1, tiles) } else { tiles };
    let rows = tiles.div_ceil(cols);
    let per = tiles.div_ceil(rows);
    (0..rows).map(|i| per.min(tiles - i * per)).collect()
}

/// "\u{2193}1.2 \u{2191}0.2 MB/s": both rates in the unit of the larger.
fn rate_pair(rx: f64, tx: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut i, mut big) = (0, rx.max(tx));
    while big >= 1024.0 && i < U.len() - 1 {
        big /= 1024.0;
        i += 1;
    }
    let scale = 1024f64.powi(i as i32);
    format!("\u{2193}{:.1} \u{2191}{:.1} {}/s", rx / scale, tx / scale, U[i])
}

/// The Thermals chart's vertical axis, top to bottom: the gridlines (quarters of the °C range) in
/// the temperature unit, "120°" ... "0°" or "248°" ... "32°".
fn thermal_axis_labels(fahrenheit: bool) -> [String; 5] {
    std::array::from_fn(|i| {
        let c = THERMAL_MAX_C * (4 - i) as f32 / 4.0;
        format!("{:.0}\u{b0}", if fahrenheit { super::to_fahrenheit(c) } else { c })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_axis_labels_follow_the_unit() {
        assert_eq!(thermal_axis_labels(false), ["120\u{b0}", "90\u{b0}", "60\u{b0}", "30\u{b0}", "0\u{b0}"]);
        assert_eq!(thermal_axis_labels(true), ["248\u{b0}", "194\u{b0}", "140\u{b0}", "86\u{b0}", "32\u{b0}"]);
    }

    #[test]
    fn thermal_peak_keeps_the_highest_reading() {
        let mut p = PerfState::default();
        p.note_temperature(None);
        assert!(p.thermal_peak.is_none());
        for t in [47.0, 58.5, 51.0, f32::NAN] {
            p.note_temperature(Some(t));
        }
        assert_eq!(p.thermal_peak.as_ref().map(|(t, _)| *t), Some(58.5));
        assert_eq!(p.thermal_peak.as_ref().map(|(_, at)| at.len()), Some(5), "HH:MM");
    }

    /// Runs `add` in a few frames of a headless context; the pointer rests at (320, 120) from the
    /// second frame on.
    fn frames(mut add: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for i in 0..3 {
            let mut input = egui::RawInput::default();
            if i > 0 {
                input.events.push(egui::Event::PointerMoved(pos2(320.0, 120.0)));
            }
            ctx.run_ui(input, |root| {
                egui::CentralPanel::default().show(root, |ui| add(ui));
            }).drop_without_applying_deltas();
        }
    }

    #[test]
    fn the_shared_parts_draw_and_read_out_on_hover() {
        let pal = theme::DARK;
        let mut a = Series::new(60);
        let mut b = Series::new(60);
        for i in 0..40 {
            a.push((i * 3 % 100) as f32);
            b.push((i * 7 % 100) as f32);
        }
        let mut hovered = false;
        frames(|ui| {
            let p = ui.painter().clone();
            let head = Head { title: "Intel(R) Core(TM) Ultra 9 285K Processor", sub: "8 cores".into(), value: ("23".into(), "%".into()), side: "8% kernel".into() };
            assert_eq!(super::head(&p, &pal, 20.0, 20.0, 600.0, &head), 48.0);
            let cx = PlotCtx { pal: &pal, smooth: true, bloom: 0.0, neutral: false, regions: 1, px_per_update: 16.0, interval: Some(Duration::from_secs(1)), phase: 0.0, glide: false };
            let lines = [PlotLine::new(&a, pal.accent, (0.0, 100.0)), PlotLine::new(&b, pal.text_faint, (0.0, 100.0)).dashed(pal.text_faint)];
            let (h, text) = plot(ui, &cx, "t", 20.0, 20.0, 600.0, 200.0, 36.0, &percent_labels(), &lines, &|age| ago(cx.interval, age));
            assert_eq!(h, 220.0);
            hovered |= text.is_some();
            legend_row(&p, &pal, 56.0, 240.0, &[("Total", pal.accent, false), ("Kernel", pal.text_faint, true)], text.as_deref(), 620.0);
            let facts = vec![("A".to_string(), "1".to_string()), ("B".to_string(), "2".to_string())];
            assert!(dl(&p, &pal, 20.0, 260.0, 600.0, &facts) > 0.0);
            spark_fixed(&p, Rect::from_min_size(pos2(20.0, 340.0), vec2(84.0, 20.0)), &a, pal.accent, (0.0, 100.0), 60, 0.0, true);
        });
        assert!(hovered, "the plot reads out the sample under the pointer");
    }

    #[test]
    fn samples_without_a_reading_are_gaps_that_draw_and_read_out_as_such() {
        let pal = theme::DARK;
        // Never a sensor, a sensor that appears late, and one that drops out and returns.
        let mut never = Series::new(60);
        let mut late = Series::new(60);
        let mut gappy = Series::new(60);
        for i in 0..40 {
            never.push(f32::NAN);
            late.push(if i < 30 { f32::NAN } else { 40.0 + i as f32 });
            gappy.push(if (10..20).contains(&i) { f32::NAN } else { 50.0 });
        }
        assert_eq!(never.max(), 0.0, "an absent reading does not count as a value");
        let mut texts = Vec::new();
        frames(|ui| {
            let cx = PlotCtx { pal: &pal, smooth: true, bloom: 0.5, neutral: false, regions: 1, px_per_update: 16.0, interval: Some(Duration::from_secs(1)), phase: 0.0, glide: false };
            for s in [&never, &late, &gappy] {
                let lines = [PlotLine::new(s, pal.accent, (0.0, 110.0))];
                let (_, text) = plot(ui, &cx, "gap", 20.0, 20.0, 600.0, 200.0, 36.0, &percent_labels(), &lines, &|age| temp_or_none(at(s, age), |t| format!("{t:.0}")));
                texts.extend(text);
            }
        });
        assert!(texts.iter().any(|t| t == "no reading"), "{texts:?}");
    }

    #[test]
    fn an_empty_history_draws_too() {
        let pal = theme::LIGHT;
        let empty = Series::new(4);
        frames(|ui| {
            let cx = PlotCtx { pal: &pal, smooth: false, bloom: 0.0, neutral: false, regions: 1, px_per_update: 16.0, interval: None, phase: 0.0, glide: false };
            let lines = [PlotLine::new(&empty, pal.accent, (0.0, 100.0))];
            let (_, text) = plot(ui, &cx, "empty", 20.0, 20.0, 300.0, 100.0, 36.0, &percent_labels(), &lines, &|_| String::new());
            assert!(text.is_none());
        });
    }

    #[test]
    fn pairs_share_one_unit() {
        assert_eq!(bytes_pair(19 << 30, 32 << 30), "19.0 / 32.0 GB");
        assert_eq!(rate_pair(1.2 * 1024.0 * 1024.0, 0.2 * 1024.0 * 1024.0), "\u{2193}1.2 \u{2191}0.2 MB/s");
        assert_eq!(rate_pair(0.0, 0.0), "\u{2193}0.0 \u{2191}0.0 B/s");
    }

    /// The strip stacks label and value when its tiles are too narrow for both on one line.
    #[test]
    fn narrow_strip_tiles_stack_the_value() {
        assert!(strip_stacked(945.0, 5), "the default window: 189 px tiles");
        assert!(!strip_stacked(1500.0, 5));
        assert!(!strip_stacked(945.0, 3));
        assert!(!strip_stacked(300.0, 0), "no tiles, nothing to stack");
    }

    #[test]
    fn a_narrow_strip_wraps_onto_rows_of_usable_tiles() {
        // 6 other devices in 520 px: 3 per row, not 6 tiles of 86 px.
        assert_eq!(strip_rows(520.0, 6, true), vec![3, 3]);
        assert_eq!(strip_rows(520.0, 5, true), vec![3, 2]);
        assert_eq!(strip_rows(945.0, 5, true), vec![5], "the default window keeps one row");
        assert_eq!(strip_rows(1500.0, 5, false), vec![5]);
        assert_eq!(strip_rows(100.0, 3, true), vec![1, 1, 1]);
        assert!(strip_rows(500.0, 0, true).is_empty());
    }

    #[test]
    fn device_cards_shrink_to_keep_every_device_in_view() {
        assert_eq!(device_card_h(900.0, 7), DEVICE_CARD_H);
        // 462 px, 7 devices at the 900x600 window: smaller cards, all in view.
        let h = device_card_h(462.0, 7);
        assert!((DEVICE_CARD_MIN_H..DEVICE_CARD_H).contains(&h));
        assert!(7.0 * (h + 6.0) + DEVICE_FOOTER_H <= 462.0);
        // Far too little room: the minimum, and the list scrolls.
        assert_eq!(device_card_h(200.0, 7), DEVICE_CARD_MIN_H);
    }

    #[test]
    fn facts_columns_leave_room_for_the_widest_heading() {
        // 480 px with a 135 px heading: 3 columns, not the 4 a 100 px minimum allows.
        assert_eq!(dl_columns(480.0, &[135.0, 90.0, 120.0, 130.0]), 3);
        assert_eq!(dl_columns(480.0, &[80.0, 90.0, 60.0, 70.0]), 4);
        assert_eq!(dl_columns(100.0, &[135.0, 90.0]), 1);
        assert_eq!(dl_columns(500.0, &[]), 1);
    }

    #[test]
    fn unknown_values_read_as_not_reported() {
        assert_eq!(link_speed_text(0.0), "Not reported");
        assert_eq!(link_speed_text(1e9), "1.0 Gb/s");
        assert_eq!(link_use_text(0.0, 0.0), "Not reported");
        assert_eq!(link_use_text(1e9, 2.5), "2.50%");
        assert_eq!(temp_or_none(f32::NAN, |t| format!("{t}")), "no reading");
        assert_eq!(temp_or_none(41.0, |t| format!("{t:.0} C")), "41 C");
    }

    #[test]
    fn gpu_memory_reads_use_and_size() {
        assert_eq!(gpu_memory(Some(237 << 20), Some(6 << 30)).as_deref(), Some("0.2 / 6.0 GB"));
        assert_eq!(gpu_memory(Some(237 << 20), None).as_deref(), Some("237.0 MB in use"));
        assert_eq!(gpu_memory(None, Some(16 << 30)).as_deref(), Some("16.0 GB total"));
        assert_eq!(gpu_memory(None, None), None);
    }

    #[test]
    fn gpu_temperatures_read_with_meaning_and_source() {
        let mut nv = GpuAdapter { name: "RTX 5070".into(), ..Default::default() };
        nv.offer_temp(GpuTempKind::Gpu, 52.0, "WDDM");
        nv.offer_temp(GpuTempKind::HotSpot, 64.0, "HWiNFO");
        let igpu = GpuAdapter { name: "Radeon".into(), ..Default::default() };
        let fmt = |t: f32| format!("{t:.0}\u{b0}C");
        let s = |rows: Vec<(String, String)>| rows.into_iter().map(|(l, v)| format!("{l}={v}")).collect::<Vec<_>>();
        assert_eq!(s(gpu_temp_facts(&[&nv], false, fmt)), ["Temperature=52\u{b0}C \u{b7} WDDM", "Hot spot=64\u{b0}C \u{b7} HWiNFO"]);
        assert_eq!(s(gpu_temp_facts(&[&igpu, &nv], true, fmt))[0], "RTX 5070 \u{b7} Temperature=52\u{b0}C \u{b7} WDDM");
        assert!(gpu_temp_facts(&[&igpu], false, fmt).is_empty(), "no reading, no row");
    }

    #[test]
    fn spans_read_naturally() {
        let s = Duration::from_secs(1);
        assert_eq!(span_text(61, Some(s)), "60 s");
        assert_eq!(span_text(961, Some(s)), "16 min");
        assert_eq!(span_text(10_000, Some(s)), "2.8 h");
        assert_eq!(span_text(10, None), "history");
        assert_eq!(ago(Some(s), 185), "\u{2212}3:05");
    }

    #[test]
    fn the_newest_sample_is_age_zero() {
        let mut s = Series::new(8);
        for v in [1.0, 2.0, 3.0] {
            s.push(v);
        }
        assert_eq!((at(&s, 0), at(&s, 2), at(&s, 3)), (3.0, 1.0, 0.0));
    }

    #[test]
    fn rate_axes_end_at_zero() {
        let l = rate_labels(1024.0 * 1024.0);
        assert_eq!((l[0].as_str(), l[4].as_str()), ("1.0 MB/s", "0"));
    }
}
