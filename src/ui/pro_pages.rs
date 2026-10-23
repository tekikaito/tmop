//! Advanced pages (Power & Freq, Flight Recorder, Connections, Installed Apps, Drivers,
//! Disk Space) on the `pro` back ends, in the style of the Summary page.

use super::kit::{self, empty_state, header_toolbar, hsplit, row_ui, stat_row, titled_card, vsplit, GAP, MARGIN};
use super::table::{self, cmp_str, Cell, Col, TableState};
use super::theme::{self, mix, with_alpha, Palette};
use super::widgets::{self, card, draw_graph, led_column, GraphLine, Icon, LedStyle};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::pro::apps::App as InstalledApp;
use crate::pro::connections::{Conn, Proto};
use crate::pro::diskdelete::{self, Target};
use crate::pro::diskspace::{self, DirFile, Scanner, Tree, Volume};
use crate::pro::drivers::Service;
use crate::pro::power::PowerInfo;
use crate::pro::recorder::{self, EventKind, Record, RecordingInfo};
use crate::pro::Loader;
use crate::process::{format_bytes, format_rate};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// UI state of the advanced and inventory pages.
#[derive(Default)]
pub struct ProState {
    pub power: Loader<PowerInfo>,
    pub conns: Loader<Vec<Conn>>,
    pub conns_table: TableState,
    pub conns_view: usize,
    pub apps: Loader<Vec<InstalledApp>>,
    pub apps_table: TableState,
    pub drivers: Loader<Vec<Service>>,
    pub drivers_table: TableState,
    pub volumes: Loader<Vec<Volume>>,
    pub scanner: Scanner,
    pub disk_root: String,
    pub disk_tree: Option<(Arc<Tree>, u64)>,
    pub disk_cursor: u32,
    pub disk_tab: usize,
    /// Files tab sort column; direction in `disk_files_asc` (false = descending).
    pub disk_files_sort: FileCol,
    pub disk_files_asc: bool,
    /// Folder the Files tab listing was requested for (None = nothing requested yet).
    pub disk_files_for: Option<PathBuf>,
    pub disk_files: Loader<Vec<DirFile>>,
    /// Sorted index order of `disk_files.ok()`, valid for the (col, asc) it was built with.
    pub disk_files_order: Option<(FileCol, bool, Vec<usize>)>,
    /// Files tab "Include subfolders": on, and the sorted index order (file indices, biggest size) of the
    /// last finished sort with the key it was requested for (`disk_sub_key`) and the key shown (`disk_sub_ready`).
    pub disk_files_sub: bool,
    pub disk_sub_order: Loader<(Vec<u32>, u64)>,
    pub disk_sub_key: Option<SubKey>,
    pub disk_sub_ready: Option<SubKey>,
    /// Largest-files indices into `Tree::files` for (tree generation, cursor).
    pub disk_largest: Option<(u64, u32, Vec<usize>)>,
    pub disk_confirm_stop: bool,
    /// Deletions applied to the scan tree (part of the tree version, see `disk_epoch`).
    pub disk_edits: u64,
    pub del: super::disk_delete::DiskDeleteUi,
    pub disk_mode: super::disk_cleanup::DiskMode,
    pub clean: super::disk_cleanup::CleanupUi,
    pub recordings: Vec<RecordingInfo>,
    pub recordings_listed: bool,
    pub playback: Option<(PathBuf, Vec<Record>)>,
    pub rec_dir_text: String,
    pub rec_limit: usize,
    pub inv: super::inventory_pages::InventoryState,
    pub cpu_details: Option<crate::pro::inventory::CpuDetails>,
}

/// (scan generation, folder, sort column, ascending) a sorted listing was built for.
pub type SubKey = (u64, u32, FileCol, bool);

impl ProState {
    /// Version of the scan tree shown: scan generation and the deletions applied to it.
    pub fn disk_epoch(&self) -> u64 {
        self.disk_tree.as_ref().map(|(_, g)| *g).unwrap_or(0) << 20 | (self.disk_edits & 0xF_FFFF)
    }
}

/// Column of the Disk Space Files tab (in-memory only, never saved).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileCol {
    Name,
    #[default]
    Size,
    Modified,
}

const DISK_ROW_H: f32 = 24.0;
const DISK_BIG_ROW_H: f32 = 40.0;
const DISK_DATE_W: f32 = 78.0;
/// Share of the Name+Folder width given to the name when the Folder column shows.
const FOLDER_SPLIT: f32 = 0.55;
const DISK_NARROW_W: f32 = 330.0;
const DISK_TINY_W: f32 = 240.0;

const LIMITS: [(&str, Option<u64>); 6] = [("Until stopped", None), ("1 minute", Some(60)), ("5 minutes", Some(300)), ("15 minutes", Some(900)), ("1 hour", Some(3600)), ("8 hours", Some(28800))];

impl App {
    // ------------------------------------------------------------------ Power & Freq

    pub fn power_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Power & Freq", Icon::Bolt);
        let mut refresh = false;
        header_toolbar(ui, body, 200.0, |ui| refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked());
        self.pro.power.poll();
        if refresh || self.pro.power.stale(Duration::from_secs(10)) {
            self.pro.power.start(crate::pro::power::read);
        }
        let s = fd.snap;
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[150.0, -1.0, 250.0], GAP);
        let p = ui.painter().clone();
        let max = s.cpu.max_mhz.unwrap_or(0) as f32;
        let cur = fd.smooth.clock;
        let red = fd.metric("clock");
        let info = self.pro.power.ok().cloned();
        let fastest = s.cpu.per_core_mhz.iter().max().copied().unwrap_or(0);
        let (batt_title, batt_value, batt_sub, batt_frac) = match info.as_ref().and_then(|i| i.battery.clone()) {
            Some(b) => (
                "Battery".to_string(),
                format!("{}%", b.percent),
                if b.charging { format!("Charging {:.1} W", b.rate_mw as f32 / 1000.0) } else if b.rate_mw != 0 { format!("Discharging {:.1} W", -(b.rate_mw as f32) / 1000.0) } else { "Idle".to_string() },
                Some(b.percent as f32 / 100.0),
            ),
            None => (
                "Power source".to_string(),
                match info.as_ref().and_then(|i| i.ac_online) {
                    Some(true) | None => "AC".to_string(),
                    Some(false) => "Battery".to_string(),
                },
                "No battery reported".to_string(),
                None,
            ),
        };
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Clock".into(), if cur > 0.0 { format!("{:.2} GHz", cur / 1000.0) } else { "N/A".into() }, "Average of all logical processors".into(), red, (max > 0.0).then(|| cur / (max * 1.25))),
                ("Fastest core".into(), format!("{:.2} GHz", fastest as f32 / 1000.0), format!("Base {:.2} GHz", max / 1000.0), theme::ORANGE, (max > 0.0).then(|| fastest as f32 / (max * 1.25))),
                (
                    "Power plan".into(),
                    info.as_ref().map(|i| i.scheme_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "\u{2014}".into()),
                    info.as_ref().map(|i| format!("{} plans installed", i.schemes.len())).unwrap_or_else(|| "Reading power policy\u{2026}".into()),
                    theme::BLUE,
                    None,
                ),
                (batt_title, batt_value, batt_sub, theme::GREEN, batt_frac),
            ],
        );
        // Clock history + per-core meters.
        let mid = hsplit(rows[1], &[3.0, 2.0], GAP);
        let content = titled_card(&p, mid[0], &pal, fd.bloom, "Clock history", &format!("{:.2} GHz", cur / 1000.0));
        let graph = Rect::from_min_max(pos2(content.left() + 44.0, content.top() + 4.0), pos2(content.right() - 4.0, content.bottom() - 4.0));
        let top = (max * 1.25).max(fd.hist.clock.max() * 1.05).max(1000.0);
        let mut g = self.graph(vec![GraphLine { series: &fd.hist.clock, color: red, range: (0.0, top) }], fd);
        g.border = Some(red);
        g.fill = 0.10;
        draw_graph(&p, graph, &g);
        for (frac, label) in [(1.0, format!("{:.1}G", top / 1000.0)), (0.5, format!("{:.1}G", top / 2000.0)), (0.0, "0".to_string())] {
            p.text(pos2(graph.left() - 6.0, graph.bottom() - graph.height() * frac), Align2::RIGHT_CENTER, &label, theme::ui(11.5), red);
        }
        if max > 0.0 {
            let y = graph.bottom() - graph.height() * (max / top);
            p.add(egui::Shape::dashed_line(&[pos2(graph.left(), y), pos2(graph.right(), y)], Stroke::new(1.0_f32, with_alpha(pal.text_dim, 0.6)), 4.0, 4.0));
            p.text(pos2(graph.right() - 6.0, y - 3.0), Align2::RIGHT_BOTTOM, "base", theme::ui(11.0), pal.text_dim);
        }
        let content = titled_card(&p, mid[1], &pal, fd.bloom, "Per-core clock", &format!("{} cores", fd.smooth.per_core_mhz.len()));
        let n = fd.smooth.per_core_mhz.len();
        if n == 0 || max <= 0.0 {
            empty_state(&p, content, &pal, "Per-core clocks are not reported");
        } else {
            let col_w = content.width() / n as f32;
            let meter_w = (col_w * 0.55).clamp(4.0, 22.0);
            let mbottom = content.bottom() - 34.0;
            for (i, mhz) in fd.smooth.per_core_mhz.iter().enumerate() {
                let cx = content.left() + col_w * (i as f32 + 0.5);
                let m = Rect::from_min_max(pos2(cx - meter_w / 2.0, content.top() + 4.0), pos2(cx + meter_w / 2.0, mbottom));
                let segs = ((m.height() / 9.0).floor() as usize).clamp(6, 40);
                led_column(&p, m, segs, mhz / (max * 1.25), &LedStyle::traffic(fd.bloom));
                if col_w >= 18.0 {
                    p.text(pos2(cx, mbottom + 4.0), Align2::CENTER_TOP, i.to_string(), theme::ui(11.0), pal.text_dim);
                    p.text(pos2(cx, content.bottom()), Align2::CENTER_BOTTOM, format!("{:.1}", mhz / 1000.0), theme::ui(10.5), pal.text);
                }
            }
        }
        // Policy and plans.
        let low = hsplit(rows[2], &[3.0, 2.0], GAP);
        let content = titled_card(&p, low[0], &pal, fd.bloom, "Processor power policy", info.as_ref().map(|i| i.scheme_name.as_str()).unwrap_or(""));
        match &info {
            Some(i) if !i.policy.is_empty() => {
                let x_ac = content.left() + content.width() * 0.55;
                let x_dc = content.left() + content.width() * 0.80;
                p.text(pos2(x_ac, content.top() + 8.0), Align2::LEFT_CENTER, "Plugged in", theme::semibold(12.0), pal.text_dim);
                p.text(pos2(x_dc, content.top() + 8.0), Align2::LEFT_CENTER, "On battery", theme::semibold(12.0), pal.text_dim);
                for (k, row) in i.policy.iter().enumerate() {
                    let y = content.top() + 30.0 + k as f32 * 26.0;
                    if y > content.bottom() - 8.0 {
                        break;
                    }
                    if k % 2 == 0 {
                        p.rect_filled(Rect::from_min_max(pos2(content.left(), y - 12.0), pos2(content.right(), y + 12.0)), 3.0, with_alpha(pal.text, 0.03));
                    }
                    p.text(pos2(content.left() + 8.0, y), Align2::LEFT_CENTER, row.name, theme::ui(12.5), pal.text);
                    p.text(pos2(x_ac, y), Align2::LEFT_CENTER, &row.ac, theme::ui(12.5), theme::RED);
                    p.text(pos2(x_dc, y), Align2::LEFT_CENTER, &row.dc, theme::ui(12.5), theme::ORANGE);
                }
            }
            _ => empty_state(&p, content, &pal, self.pro.power.error().unwrap_or("Reading power policy\u{2026}")),
        }
        let content = titled_card(&p, low[1], &pal, fd.bloom, "Power plans", "");
        if let Some(i) = &info {
            for (k, (guid, name, active)) in i.schemes.iter().enumerate() {
                let y = content.top() + 12.0 + k as f32 * 30.0;
                if y > content.bottom() - 10.0 {
                    break;
                }
                let r = Rect::from_min_max(pos2(content.left(), y - 13.0), pos2(content.right(), y + 13.0));
                if *active {
                    p.rect_filled(r, 4.0, pal.selection);
                }
                p.circle_filled(pos2(r.left() + 12.0, y), 4.0, if *active { theme::GREEN } else { pal.text_faint });
                p.text(pos2(r.left() + 26.0, y), Align2::LEFT_CENTER, if name.is_empty() { guid } else { name }, theme::ui(12.5), if *active { pal.on_selection() } else { pal.text });
                if *active {
                    p.text(pos2(r.right() - 8.0, y), Align2::RIGHT_CENTER, "Active", theme::ui(11.5), pal.on_selection());
                }
            }
        }
    }

    // ------------------------------------------------------------------ Flight Recorder

    pub fn recorder_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Flight Recorder", Icon::Record);
        if self.pro.rec_dir_text.is_empty() {
            self.pro.rec_dir_text = self.recorder.dir.display().to_string();
        }
        if !self.pro.recordings_listed {
            self.pro.recordings = recorder::list(&self.recorder.dir);
            self.pro.recordings_listed = true;
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[128.0, -1.0, 230.0], GAP);
        let p = ui.painter().clone();
        let red = Color32::from_rgb(0xFF, 0x3B, 0x3B);
        // Control card.
        card(&p, rows[0], &pal);
        let inner = rows[0].shrink(14.0);
        let recording = self.recorder.is_recording();
        let btn = Rect::from_center_size(pos2(inner.left() + 46.0, inner.center().y), vec2(84.0, 84.0));
        let resp = ui.interact(btn, ui.id().with("rec-button"), Sense::click());
        let pulse = if recording { 0.6 + 0.4 * ((fd.time * 3.0).sin() as f32 * 0.5 + 0.5) } else { 0.0 };
        p.circle_filled(btn.center(), 40.0, if resp.hovered() { pal.hover } else { pal.panel });
        p.circle_stroke(btn.center(), 40.0, Stroke::new(2.0_f32, if recording { red } else { pal.control_stroke }));
        if recording {
            if fd.bloom > 0.0 {
                p.circle_filled(btn.center(), 30.0, with_alpha(red, 0.18 * fd.bloom * pulse));
            }
            p.rect_filled(Rect::from_center_size(btn.center(), vec2(26.0, 26.0)), 3.0, red);
        } else {
            p.circle_filled(btn.center(), 18.0, theme::dim(red, 0.85));
        }
        resp.clone().on_hover_text(if recording { "Stop recording" } else { "Start recording" });
        let mut toggle = resp.clicked();
        // Stopwatch + status.
        let clock = super::fmt_clock(self.recorder.elapsed().unwrap_or_default());
        let h = 26.0;
        let lcd = Rect::from_min_size(pos2(inner.left() + 110.0, inner.top()), vec2(widgets::seven_segment_width(&clock, h) + 20.0, h + 12.0));
        let lcd_pal = pal.lcd();
        p.rect_filled(lcd, 4.0, lcd_pal.bg);
        p.rect_stroke(lcd, 4.0, Stroke::new(1.0_f32, lcd_pal.rim));
        let lit = if recording { Color32::from_rgb(0xFF, 0x6A, 0x6A) } else { lcd_pal.lit };
        widgets::seven_segment(&p, lcd.min + vec2(10.0, 6.0), &clock, h, lit, lcd_pal.unlit, if recording { fd.bloom } else { 0.0 });
        let status = match (self.recorder.recording_path(), &self.recorder.last_error) {
            (Some(path), _) => format!("Recording {} samples to {}", self.recorder.recorded_rows(), path.display()),
            (None, Some(e)) => e.clone(),
            (None, None) => match &self.recorder.last_saved {
                Some(sv) => format!("Saved {}", sv.display()),
                None => "Not recording. Every sample is also kept in the flight recorder below.".into(),
            },
        };
        let clip = p.with_clip_rect(Rect::from_min_max(pos2(lcd.right() + 16.0, inner.top()), pos2(inner.right() - 390.0, inner.bottom())));
        clip.text(pos2(lcd.right() + 16.0, lcd.center().y), Align2::LEFT_CENTER, &status, theme::ui(12.5), if self.recorder.last_error.is_some() && !recording { pal.warn } else { pal.text_dim });
        let mut save_flight = false;
        let mut open_folder = false;
        let mut apply_dir = false;
        let mut default_dir = false;
        row_ui(ui, Rect::from_min_max(pos2(inner.left() + 110.0, inner.bottom() - 32.0), pos2(inner.right() - 390.0, inner.bottom())), |ui| {
            ui.label(RichText::new("Folder").font(theme::ui(12.5)).color(pal.text_dim));
            let w = (ui.available_width() - 260.0).max(160.0);
            let r = widgets::search_box(ui, &mut self.pro.rec_dir_text, "Recording folder", w, &pal);
            apply_dir |= r.lost_focus();
            default_dir |= widgets::button(ui, "Default", &pal, None).clicked();
            open_folder |= widgets::button(ui, "Open folder", &pal, Some(Icon::Folder)).clicked();
        });
        row_ui(ui, Rect::from_min_max(pos2(inner.right() - 370.0, inner.top()), pos2(inner.right(), inner.top() + 32.0)), |ui| {
            ui.label(RichText::new("Stop after").font(theme::ui(12.5)).color(pal.text_dim));
            egui::ComboBox::from_id_salt("rec-limit").width(150.0).selected_text(LIMITS[self.pro.rec_limit].0).show_ui(ui, |ui| {
                for (i, (l, _)) in LIMITS.iter().enumerate() {
                    ui.selectable_value(&mut self.pro.rec_limit, i, *l);
                }
            });
        });
        row_ui(ui, Rect::from_min_max(pos2(inner.right() - 370.0, inner.bottom() - 32.0), inner.right_bottom()), |ui| {
            let label = if recording { "Stop recording" } else { "Start recording" };
            toggle |= widgets::button(ui, label, &pal, Some(if recording { Icon::Stop } else { Icon::Record })).clicked();
            save_flight |= widgets::button(ui, &format!("Save flight recorder ({})", self.recorder.ring.len()), &pal, None).clicked();
        });
        if default_dir {
            self.pro.rec_dir_text = recorder::default_dir().display().to_string();
            apply_dir = true;
        }
        if apply_dir {
            let d = PathBuf::from(self.pro.rec_dir_text.trim());
            if d.as_os_str().is_empty() || !d.is_absolute() {
                self.status = "The recording folder must be an absolute path".into();
            } else if d != self.recorder.dir {
                self.recorder.dir = d.clone();
                self.settings.general.recording_folder = d.display().to_string();
                self.settings_dirty = true;
                self.pro.recordings_listed = false;
            }
        }
        if toggle {
            if recording {
                self.recorder.stop();
            } else {
                let limit = LIMITS[self.pro.rec_limit].1.map(Duration::from_secs);
                if let Err(e) = self.recorder.start(limit) {
                    self.recorder.last_error = Some(e);
                }
            }
            self.pro.recordings_listed = false;
        }
        if save_flight {
            match self.recorder.save_flight() {
                Ok(_) => self.pro.recordings_listed = false,
                Err(e) => self.recorder.last_error = Some(e),
            }
        }
        if open_folder {
            let _ = std::fs::create_dir_all(&self.recorder.dir);
            let _ = kit::reveal(&self.recorder.dir);
        }

        // Graph of the flight ring (or of an opened recording).
        let (title, records): (String, Vec<&Record>) = match &self.pro.playback {
            Some((path, recs)) => (format!("Playback {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()), recs.iter().collect()),
            None => ("Flight recorder".into(), self.recorder.ring.iter().collect()),
        };
        let span = match (records.first(), records.last()) {
            (Some(a), Some(b)) if records.len() > 1 => format!("{} \u{2192} {}", &a.time[11.min(a.time.len())..], &b.time[11.min(b.time.len())..]),
            _ => String::new(),
        };
        let content = titled_card(&p, rows[1], &pal, fd.bloom, &title, &span);
        let mut close_playback = false;
        let mk = |f: &dyn Fn(&Record) -> f32| {
            let mut s = Series::new(records.len().max(2));
            for r in &records {
                s.push(f(r));
            }
            s
        };
        let cpu = mk(&|r| r.cpu);
        let mem = mk(&|r| r.mem_pct);
        let disk = mk(&|r| r.disk_pct);
        let gpu = mk(&|r| r.gpu);
        let legend = [("CPU", fd.metric("cpu")), ("Memory", fd.metric("memory")), ("Disk", fd.metric("disk")), ("GPU", fd.metric("gpu"))];
        let mut lx = content.left() + 44.0;
        for (t, c) in legend {
            p.circle_filled(pos2(lx + 4.0, content.top() + 8.0), 4.0, c);
            let r = p.text(pos2(lx + 12.0, content.top() + 8.0), Align2::LEFT_CENTER, t, theme::ui(12.0), c);
            lx = r.right() + 18.0;
        }
        if self.pro.playback.is_some() {
            let close = Rect::from_min_size(pos2(content.right() - 120.0, content.top()), vec2(120.0, 18.0));
            let r = p.text(close.right_center(), Align2::RIGHT_CENTER, "Close playback \u{2715}", theme::ui(12.0), pal.accent);
            close_playback = ui.interact(r, ui.id().with("close-playback"), Sense::click()).clicked();
        }
        let graph = Rect::from_min_max(pos2(content.left() + 44.0, content.top() + 22.0), pos2(content.right() - 4.0, content.bottom() - 4.0));
        if records.len() < 2 {
            p.rect_stroke(graph, 0.0, Stroke::new(1.0_f32, pal.grid));
            empty_state(&p, graph, &pal, "Collecting samples\u{2026}");
        } else {
            let mut g = self.graph(
                vec![
                    GraphLine { series: &cpu, color: fd.metric("cpu"), range: (0.0, 100.0) },
                    GraphLine { series: &mem, color: fd.metric("memory"), range: (0.0, 100.0) },
                    GraphLine { series: &disk, color: fd.metric("disk"), range: (0.0, 100.0) },
                    GraphLine { series: &gpu, color: fd.metric("gpu"), range: (0.0, 100.0) },
                ],
                fd,
            );
            // Fit the whole recording into the graph width.
            g.px_per_update = (graph.width() / records.len().max(1) as f32).clamp(0.25, 16.0);
            g.regions = 1;
            g.phase = if self.pro.playback.is_some() { 0.0 } else { fd.phase };
            g.glide = self.pro.playback.is_none() && fd.glide;
            g.border = Some(pal.card_stroke);
            g.line_width = 1.6;
            draw_graph(&p, graph, &g);
            for (frac, l) in [(1.0, "100%"), (0.5, "50%"), (0.0, "0%")] {
                p.text(pos2(graph.left() - 6.0, graph.bottom() - graph.height() * frac), Align2::RIGHT_CENTER, l, theme::ui(11.5), pal.text_dim);
            }
            // Hover readout.
            if let Some(hp) = ui.ctx().pointer_hover_pos().filter(|h| graph.contains(*h)) {
                let n = records.len();
                let px = g.px_per_update;
                let age = ((graph.right() - hp.x) / px).round() as usize;
                if age < n {
                    let r = records[n - 1 - age];
                    p.line_segment([pos2(hp.x, graph.top()), pos2(hp.x, graph.bottom())], Stroke::new(1.0_f32, with_alpha(pal.text, 0.4)));
                    let text = format!("{}\nCPU {:.1}%  Mem {:.1}%  Disk {:.1}%  GPU {:.1}%\n{}", r.time, r.cpu, r.mem_pct, r.disk_pct, r.gpu, r.top.replace('|', ", "));
                    let gal = p.layout(text, theme::ui(12.0), pal.text, 420.0);
                    let at = pos2((hp.x + 12.0).min(graph.right() - gal.size().x - 12.0), graph.top() + 8.0);
                    let bg = Rect::from_min_size(at, gal.size()).expand(6.0);
                    p.rect_filled(bg, 4.0, with_alpha(pal.panel, 0.95));
                    p.rect_stroke(bg, 4.0, Stroke::new(1.0_f32, pal.card_stroke));
                    p.galley(at, gal, pal.text);
                }
            }
        }
        if close_playback {
            self.pro.playback = None;
        }
        // Events and saved recordings.
        let low = hsplit(rows[2], &[1.0, 1.0], GAP);
        let content = titled_card(&p, low[0], &pal, fd.bloom, "Process events", &format!("{}", self.recorder.events.len()));
        let row_h = 19.0;
        let max_rows = (content.height() / row_h).floor() as usize;
        if self.recorder.events.is_empty() {
            empty_state(&p, content, &pal, "Process starts and exits appear here");
        }
        for (i, e) in self.recorder.events.iter().rev().take(max_rows).enumerate() {
            let y = content.top() + i as f32 * row_h + row_h / 2.0;
            let (tag, c) = match e.kind {
                EventKind::Started => ("\u{25b2}", theme::GREEN),
                EventKind::Exited => ("\u{25bc}", theme::RED),
                EventKind::Note => ("\u{25cf}", theme::BLUE),
            };
            p.text(pos2(content.left() + 2.0, y), Align2::LEFT_CENTER, &e.time[11.min(e.time.len())..], theme::mono(11.5), pal.text_dim);
            p.text(pos2(content.left() + 70.0, y), Align2::LEFT_CENTER, tag, theme::ui(11.0), c);
            let clip = p.with_clip_rect(Rect::from_min_max(pos2(content.left() + 86.0, y - 10.0), pos2(content.right(), y + 10.0)));
            clip.text(pos2(content.left() + 86.0, y), Align2::LEFT_CENTER, &e.text, theme::ui(12.0), pal.text);
        }
        let content = titled_card(&p, low[1], &pal, fd.bloom, "Recordings", &format!("{}", self.pro.recordings.len()));
        if self.pro.recordings.is_empty() {
            empty_state(&p, content, &pal, "No recordings in this folder yet");
        }
        let mut open: Option<PathBuf> = None;
        let mut reveal: Option<PathBuf> = None;
        for (i, r) in self.pro.recordings.iter().take(((content.height()) / 26.0).floor() as usize).enumerate() {
            let rr = Rect::from_min_size(pos2(content.left(), content.top() + i as f32 * 26.0), vec2(content.width(), 24.0));
            let resp = ui.interact(rr, ui.id().with(("recording", i)), Sense::click());
            let playing = self.pro.playback.as_ref().map(|(pp, _)| *pp == r.path).unwrap_or(false);
            if playing {
                p.rect_filled(rr, 3.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(rr, 3.0, pal.hover);
            }
            widgets::draw_icon(&p, Rect::from_center_size(pos2(rr.left() + 12.0, rr.center().y), vec2(13.0, 13.0)), Icon::Chart, theme::GREEN);
            let clip = p.with_clip_rect(Rect::from_min_max(rr.min, pos2(rr.right() - 90.0, rr.bottom())));
            let (fg, fg_dim) = if playing { (pal.on_selection(), pal.on_selection()) } else { (pal.text, pal.text_dim) };
            clip.text(pos2(rr.left() + 26.0, rr.center().y), Align2::LEFT_CENTER, &r.name, theme::ui(12.5), fg);
            p.text(pos2(rr.right() - 6.0, rr.center().y), Align2::RIGHT_CENTER, format_bytes(r.size as f64), theme::ui(12.0), fg_dim);
            if resp.clicked() {
                open = Some(r.path.clone());
            }
            if resp.secondary_clicked() {
                reveal = Some(r.path.clone());
            }
            resp.on_hover_text("Click to play back, right-click to show in Explorer");
        }
        if let Some(path) = open {
            match recorder::load(&path) {
                Ok(recs) => self.pro.playback = Some((path, recs)),
                Err(e) => self.status = e,
            }
        }
        if let Some(path) = reveal {
            let _ = kit::reveal(&path);
        }
    }

    // ------------------------------------------------------------------ Connections

    pub fn connections_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Connections", Icon::Globe);
        let mut refresh = false;
        header_toolbar(ui, body, 640.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.conns_table.filter, "Filter by process, address, port or state", 300.0, &pal);
            egui::ComboBox::from_id_salt("conn-view").width(140.0).selected_text(["All", "Established", "Listening", "UDP", "Internet"][self.pro.conns_view]).show_ui(ui, |ui| {
                for (i, l) in ["All", "Established", "Listening", "UDP", "Internet"].iter().enumerate() {
                    ui.selectable_value(&mut self.pro.conns_view, i, *l);
                }
            });
        });
        self.pro.conns.poll();
        if refresh || self.pro.conns.stale(Duration::from_secs(3)) {
            self.pro.conns.start(crate::pro::connections::read);
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let names: HashMap<u32, &str> = fd.rows.iter().map(|r| (r.pid, r.name.as_str())).collect();
        let paths: HashMap<u32, &str> = fd.rows.iter().filter_map(|r| r.path.as_deref().map(|p| (r.pid, p))).collect();
        let name_of = |pid: u32| -> String {
            match pid {
                0 => "System Idle".into(),
                4 => "System".into(),
                _ => names.get(&pid).map(|s| s.to_string()).unwrap_or_else(|| format!("PID {pid}")),
            }
        };
        let conns: Vec<Conn> = self.pro.conns.ok().cloned().unwrap_or_default();
        let est = conns.iter().filter(|c| c.is_established()).count();
        let listen = conns.iter().filter(|c| c.is_listen()).count();
        let udp = conns.iter().filter(|c| c.proto == Proto::Udp).count();
        let internet = conns.iter().filter(|c| c.is_established() && c.scope() == "Internet").count();
        let procs: HashSet<u32> = conns.iter().filter(|c| c.is_established()).map(|c| c.pid).collect();
        let total = conns.len().max(1) as f32;
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Established".into(), est.to_string(), format!("{} to the internet", internet), theme::GREEN, Some(est as f32 / total)),
                ("Listening".into(), listen.to_string(), "TCP ports accepting connections".into(), theme::BLUE, Some(listen as f32 / total)),
                ("UDP".into(), udp.to_string(), "Open UDP endpoints".into(), theme::PURPLE, Some(udp as f32 / total)),
                ("Processes".into(), procs.len().to_string(), "With established connections".into(), theme::ORANGE, None),
                ("Throughput".into(), format_rate(fd.snap.network.rx_bps + fd.snap.network.tx_bps), format!("\u{2193} {}  \u{2191} {}", format_rate(fd.snap.network.rx_bps), format_rate(fd.snap.network.tx_bps)), fd.metric("network"), Some(fd.smooth.net / 100.0)),
            ],
        );
        let view = self.pro.conns_view;
        let st = &self.pro.conns_table;
        let mut order: Vec<usize> = (0..conns.len())
            .filter(|i| {
                let c = &conns[*i];
                let keep = match view {
                    1 => c.is_established(),
                    2 => c.is_listen(),
                    3 => c.proto == Proto::Udp,
                    4 => c.is_established() && c.scope() == "Internet",
                    _ => true,
                };
                keep && table::matches(&st.filter, &[&name_of(c.pid), &c.local_text(), &c.remote_text(), c.state_name(), &c.pid.to_string()])
            })
            .collect();
        st.sort(&conns, &mut order, |a, b, col| match col {
            0 => cmp_str(&name_of(a.pid), &name_of(b.pid)),
            1 => a.pid.cmp(&b.pid),
            2 => a.proto.cmp(&b.proto),
            3 => a.local.cmp(&b.local).then(a.local_port.cmp(&b.local_port)),
            4 => a.remote.cmp(&b.remote).then(a.remote_port.cmp(&b.remote_port)),
            5 => a.state_name().cmp(b.state_name()),
            _ => a.scope().cmp(b.scope()),
        });
        if conns.is_empty() {
            empty_state(&p, rows[1], &pal, self.pro.conns.error().unwrap_or("Reading connection tables\u{2026}"));
            return;
        }
        let cols = [
            Col::text("Process", 220.0),
            Col::num("PID", 70.0),
            Col::text("Protocol", 80.0),
            Col::text("Local address", 230.0),
            Col::text("Remote address", 260.0),
            Col::text("State", 120.0),
            Col::text("Scope", 120.0),
        ];
        let key = |i: usize| {
            let c = &conns[i];
            format!("{:?}{}{}{}", c.proto, c.local_text(), c.remote_text(), c.pid)
        };
        let cell = |i: usize, col: usize| -> Cell {
            let c = &conns[i];
            match col {
                0 => Cell::new(name_of(c.pid)).icon(paths.get(&c.pid).copied()),
                1 => Cell::new(c.pid.to_string()).dim(&pal),
                2 => Cell::new(if c.proto == Proto::Tcp { if c.local.is_ipv6() { "TCPv6" } else { "TCP" } } else if c.local.is_ipv6() { "UDPv6" } else { "UDP" }).dim(&pal),
                3 => Cell::new(c.local_text()),
                4 => Cell::new(c.remote_text()),
                5 => {
                    let color = if c.is_established() {
                        theme::GREEN
                    } else if c.is_listen() {
                        theme::BLUE
                    } else if c.proto == Proto::Udp {
                        theme::PURPLE
                    } else {
                        theme::ORANGE
                    };
                    Cell::new(c.state_name()).dot(color)
                }
                _ => {
                    let s = c.scope();
                    Cell::new(s).color(if s == "Internet" { theme::BLUE } else { pal.text_dim })
                }
            }
        };
        let icons = &mut self.icons;
        table::show(ui, rows[1], "conn-table", &pal, &cols, &order, &mut self.pro.conns_table, &key, &cell, icons);
    }

    // ------------------------------------------------------------------ Installed Apps

    pub fn apps_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Installed Apps", Icon::Apps);
        let mut refresh = false;
        header_toolbar(ui, body, 480.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.apps_table.filter, "Filter by name, publisher or version", 300.0, &pal);
        });
        self.pro.apps.poll();
        if refresh || (self.pro.apps.get().is_none() && !self.pro.apps.is_loading()) {
            self.pro.apps.start(crate::pro::apps::read);
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let apps: &[InstalledApp] = self.pro.apps.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let total: u64 = apps.iter().map(|a| a.size).sum();
        let user = apps.iter().filter(|a| a.scope == "User").count();
        let x86 = apps.iter().filter(|a| a.scope == "Machine (32-bit)").count();
        let publishers: HashSet<&str> = apps.iter().map(|a| a.publisher.as_str()).filter(|p| !p.is_empty()).collect();
        let biggest = apps.iter().max_by_key(|a| a.size);
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Apps".into(), apps.len().to_string(), format!("{user} per-user, {x86} 32-bit"), theme::BLUE, None),
                ("Reported size".into(), format_bytes(total as f64), "Sum of EstimatedSize".into(), theme::PURPLE, None),
                ("Publishers".into(), publishers.len().to_string(), "Distinct publishers".into(), theme::GREEN, None),
                ("Largest".into(), biggest.map(|b| format_bytes(b.size as f64)).unwrap_or_default(), biggest.map(|b| b.name.clone()).unwrap_or_default(), theme::ORANGE, biggest.map(|b| b.size as f32 / total.max(1) as f32)),
            ],
        );
        if apps.is_empty() {
            empty_state(&p, rows[1], &pal, self.pro.apps.error().unwrap_or("Reading the Uninstall registry keys\u{2026}"));
            return;
        }
        let max_size = apps.iter().map(|a| a.size).max().unwrap_or(1).max(1) as f32;
        let st = &self.pro.apps_table;
        let mut order: Vec<usize> = (0..apps.len()).filter(|i| table::matches(&st.filter, &[&apps[*i].name, &apps[*i].publisher, &apps[*i].version])).collect();
        st.sort(apps, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.publisher, &b.publisher),
            2 => a.version.cmp(&b.version),
            3 => a.install_date.cmp(&b.install_date),
            4 => a.size.cmp(&b.size),
            5 => a.scope.cmp(b.scope),
            _ => cmp_str(&a.location, &b.location),
        });
        let cols = [Col::text("Name", 320.0), Col::text("Publisher", 220.0), Col::text("Version", 130.0), Col::text("Installed", 100.0), Col::num("Size", 90.0), Col::text("Scope", 130.0), Col::text("Location", 300.0)];
        let key = |i: usize| format!("{}|{}", apps[i].key, apps[i].scope);
        let cell = |i: usize, col: usize| -> Cell {
            let a = &apps[i];
            match col {
                0 => Cell::new(&a.name),
                1 => Cell::new(&a.publisher).dim(&pal),
                2 => Cell::new(&a.version).dim(&pal),
                3 => Cell::new(&a.install_date).dim(&pal),
                4 => Cell::new(if a.size > 0 { format_bytes(a.size as f64) } else { String::new() }).heat((a.size as f32 / max_size).sqrt()),
                5 => Cell::new(a.scope).dim(&pal),
                _ => Cell::new(&a.location).dim(&pal),
            }
        };
        let icons = &mut self.icons;
        if let Some(i) = table::show(ui, rows[1], "apps-table", &pal, &cols, &order, &mut self.pro.apps_table, &key, &cell, icons) {
            let loc = PathBuf::from(&apps[i].location);
            if loc.exists() {
                let _ = kit::reveal(&loc);
            }
        }
    }

    // ------------------------------------------------------------------ Drivers

    pub fn drivers_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Drivers", Icon::Driver);
        let mut refresh = false;
        header_toolbar(ui, body, 480.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.drivers_table.filter, "Filter by name, company or path", 300.0, &pal);
        });
        self.pro.drivers.poll();
        if refresh || (self.pro.drivers.get().is_none() && !self.pro.drivers.is_loading()) {
            self.pro.drivers.start(|| crate::pro::drivers::read(crate::pro::drivers::Kind::Drivers));
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let list: &[Service] = self.pro.drivers.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        driver_like_page(ui, &p, rows, fd, list, &mut self.pro.drivers_table, self.pro.drivers.error(), true, &mut self.icons);
    }

    // ------------------------------------------------------------------ Disk Space

    pub fn diskspace_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Disk Space", Icon::Disk);
        self.pro.volumes.poll();
        if self.pro.volumes.stale(Duration::from_secs(30)) {
            self.pro.volumes.start(|| Ok(diskspace::volumes()));
        }
        let progress = self.pro.scanner.progress();
        let (tree, gen) = self.pro.scanner.tree();
        if let Some(t) = tree {
            if self.pro.disk_tree.as_ref().map(|(_, g)| *g != gen).unwrap_or(true) {
                if (self.pro.disk_cursor as usize) >= t.nodes.len() {
                    self.pro.disk_cursor = 0;
                }
                self.pro.disk_tree = Some((t, gen));
            }
        }
        if let Some((t, _)) = &self.pro.disk_tree {
            if !t.nodes.get(self.pro.disk_cursor as usize).is_some_and(|n| !n.removed) {
                self.pro.disk_cursor = 0;
            }
        }
        if self.pro.disk_root.is_empty() {
            self.pro.disk_root = std::env::var("SystemDrive").map(|d| format!("{d}\\")).unwrap_or_else(|_| "C:\\".into());
        }
        let mut start_scan: Option<String> = None;
        let mut stop = false;
        let mut mode_click = None;
        header_toolbar(ui, body, 760.0, |ui| {
            if progress.running {
                stop = widgets::button(ui, "Stop scan", &pal, Some(Icon::Stop)).clicked();
            } else if widgets::button(ui, "Scan", &pal, Some(Icon::Play)).clicked() {
                start_scan = Some(self.pro.disk_root.clone());
            }
            let r = widgets::search_box(ui, &mut self.pro.disk_root, "Folder or drive to scan", 300.0, &pal);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                start_scan = Some(self.pro.disk_root.clone());
            }
            mode_click = super::disk_cleanup::segmented(ui, &["Analyze", "Clean up"], self.pro.disk_mode as usize, &pal);
        });
        match mode_click {
            Some(1) => {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Cleanup;
                self.pro.clean.started = false;
            }
            Some(_) => self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze,
            None => {}
        }
        let area = body.shrink(MARGIN);
        let elevated = *self.pro.del.elevated.get_or_insert_with(crate::pro::diskdelete::is_elevated);
        let rows = if elevated {
            vsplit(area, &[112.0, 30.0, -1.0], GAP)
        } else {
            let r = vsplit(area, &[36.0, 112.0, 30.0, -1.0], GAP);
            self.disk_admin_panel(ui, r[0], &pal);
            r[1..].to_vec()
        };
        let p = ui.painter().clone();
        // Volume cards.
        let vols: Vec<Volume> = self.pro.volumes.ok().cloned().unwrap_or_default();
        if !vols.is_empty() {
            let shown = vols.len().min(5);
            let strip = Rect::from_min_size(rows[0].min, vec2(rows[0].width().min(shown as f32 * 440.0), rows[0].height()));
            let rects = hsplit(strip, &vec![1.0; shown], GAP);
            for (r, v) in rects.iter().zip(vols.iter()) {
                let frac = v.used_fraction();
                let c = if frac > 0.9 { theme::RED } else if frac > 0.75 { theme::ORANGE } else { theme::GREEN };
                let title = format!("{} {}", v.root.trim_end_matches('\\'), if v.label.is_empty() { v.kind } else { v.label.as_str() });
                kit::stat_card(&p, *r, &pal, fd.bloom, &title, &format!("{} free", format_bytes(v.free as f64)), &format!("{} of {} used \u{2022} {}", format_bytes(v.used() as f64), format_bytes(v.total as f64), v.fs), c, Some(frac));
                let resp = ui.interact(*r, ui.id().with(("vol", &v.root)), Sense::click());
                if resp.clicked() && !progress.running {
                    self.pro.disk_root = v.root.clone();
                    start_scan = Some(v.root.clone());
                }
                resp.on_hover_text(format!("Scan {}", v.root));
            }
        }
        // Progress / breadcrumb line.
        let line = rows[1];
        let tree = self.pro.disk_tree.as_ref().map(|(t, _)| t.clone());
        if progress.running {
            let txt = format!("Scanning\u{2026} {} files, {} folders, {} \u{2022} {:.0} s \u{2022} {}", progress.files, progress.dirs, format_bytes(progress.bytes as f64), progress.elapsed.as_secs_f32(), progress.current);
            let clip = p.with_clip_rect(line);
            clip.text(pos2(line.left() + 2.0, line.center().y), Align2::LEFT_CENTER, &txt, theme::ui(12.5), pal.text_dim);
            let bar = Rect::from_min_max(pos2(line.left(), line.bottom() - 3.0), pos2(line.right(), line.bottom()));
            let t = (fd.time as f32 * 0.6).fract();
            p.rect_filled(bar, 1.0, pal.grid);
            p.rect_filled(Rect::from_min_max(pos2(bar.left() + bar.width() * t * 0.8, bar.top()), pos2(bar.left() + bar.width() * (t * 0.8 + 0.2), bar.bottom())), 1.0, theme::BLUE);
        } else if let Some(t) = &tree {
            let mut x = line.left() + 2.0;
            let mut go = None;
            for (k, idx) in t.ancestry(self.pro.disk_cursor).into_iter().enumerate() {
                if k > 0 {
                    let r = p.text(pos2(x, line.center().y), Align2::LEFT_CENTER, "\u{203a}", theme::ui(14.0), pal.text_faint);
                    x = r.right() + 6.0;
                }
                let name = &t.nodes[idx as usize].name;
                let r = p.text(pos2(x, line.center().y), Align2::LEFT_CENTER, name, theme::semibold(13.0), if idx == self.pro.disk_cursor { pal.text } else { pal.accent });
                if ui.interact(r, ui.id().with(("crumb", idx)), Sense::click()).clicked() {
                    go = Some(idx);
                }
                x = r.right() + 6.0;
            }
            let n = &t.nodes[self.pro.disk_cursor as usize];
            let mut summary = format!("{} in {} files", format_bytes(n.size as f64), n.files);
            if self.pro.del.freed > 0 {
                summary.push_str(&format!(" \u{2022} {} freed", format_bytes(self.pro.del.freed as f64)));
            }
            if progress.cancelled {
                summary.push_str(" \u{2022} partial (scan stopped)");
            } else if progress.finished {
                summary.push_str(&format!(" \u{2022} scanned in {:.1} s ({}), {} folders unreadable", progress.elapsed.as_secs_f32(), progress.method.label(), progress.denied));
                if let Some(why) = &progress.fell_back {
                    summary.push_str(&format!(" \u{2022} MFT unavailable: {why}, used parallel walker"));
                }
            }
            p.text(pos2(line.right(), line.center().y), Align2::RIGHT_CENTER, &summary, theme::ui(12.5), pal.text_dim);
            if let Some(g) = go {
                self.pro.disk_cursor = g;
            }
        }
        // Treemap + side list.
        let main = hsplit(rows[2], &[3.0, 2.0], GAP);
        if self.pro.disk_mode == super::disk_cleanup::DiskMode::Cleanup {
            if self.cleanup_page(ui, rows[2], fd, tree.as_ref(), progress.running, elevated) && !progress.running {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze;
                start_scan = Some(self.pro.disk_root.clone());
            }
        } else {
            match &tree {
                None => {
                    card(&p, rows[2], &pal);
                    empty_state(&p, rows[2], &pal, if progress.running { "Building the folder map\u{2026}" } else { "Pick a drive above or enter a folder, then Scan" });
                }
                Some(t) => {
                    let cursor = self.pro.disk_cursor;
                    if let Some(next) = self.treemap(ui, main[0], t, cursor, fd) {
                        self.pro.disk_cursor = next;
                    }
                    self.disk_side(ui, main[1], t, fd);
                }
            }
        }
        if stop {
            if self.settings.general.disk_space_suppress_stop_confirm {
                self.pro.scanner.cancel();
            } else {
                self.pro.disk_confirm_stop = true;
            }
        }
        if self.pro.disk_confirm_stop {
            let mut answer = None;
            let mut dont_ask = false;
            egui::Window::new("Stop scan").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ui.ctx(), |ui| {
                ui.label("Stop the Disk Space scan? The results so far are kept.");
                ui.checkbox(&mut dont_ask, "Don't ask again");
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Stop", &pal, None).clicked() {
                        answer = Some(true);
                    }
                    if widgets::button(ui, "Keep scanning", &pal, None).clicked() {
                        answer = Some(false);
                    }
                });
            });
            if dont_ask {
                self.settings.general.disk_space_suppress_stop_confirm = true;
                self.settings_dirty = true;
            }
            if let Some(a) = answer {
                self.pro.disk_confirm_stop = false;
                if a {
                    self.pro.scanner.cancel();
                }
            }
        }
        if let Some(root) = start_scan {
            let root = diskdelete::canonical_root(PathBuf::from(root.trim()));
            if root.is_dir() {
                self.pro.disk_cursor = 0;
                self.pro.disk_tree = None;
                self.pro.disk_largest = None;
                self.pro.disk_files_for = None;
                self.pro.del.sel = Default::default();
                self.pro.del.freed = 0;
                self.pro.scanner.start(root);
            } else {
                self.status = format!("{} is not a folder", root.display());
            }
        }
    }

    fn treemap(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Tree, cursor: u32, fd: &FrameData<'_>) -> Option<u32> {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let node = &t.nodes[cursor as usize];
        let content = titled_card(&p, rect, &pal, fd.bloom, &format!("Folder map {}", node.name), &format_bytes(node.size as f64));
        let kids: Vec<u32> = node.children.iter().copied().filter(|c| t.nodes[*c as usize].size > 0).take(60).collect();
        let mut sizes: Vec<f64> = kids.iter().map(|c| t.nodes[*c as usize].size as f64).collect();
        let shown: u64 = sizes.iter().sum::<f64>() as u64;
        let rest = node.own; // files directly here
        let more = node.size.saturating_sub(shown).saturating_sub(rest); // folders past the 60th
        if more > 0 {
            sizes.push(more as f64);
        }
        if rest > 0 {
            sizes.push(rest as f64);
        }
        if sizes.is_empty() {
            empty_state(&p, content, &pal, "This folder is empty");
            return None;
        }
        let rects = diskspace::squarify(&sizes, content.left(), content.top(), content.width(), content.height());
        let mut go = None;
        let hover = ui.ctx().pointer_hover_pos();
        // Registered first so the tiles sit on top; right-click on empty map goes up one level.
        let bg = ui.interact(content, ui.id().with("tm-bg"), Sense::click());
        let mut tiles: Vec<Rect> = Vec::new();
        let rules = self.pro.del.rules.ok().cloned();
        let mut menu = super::disk_delete::MenuOut::default();
        for (i, (x, y, w, h)) in rects.iter().enumerate() {
            let r = Rect::from_min_size(pos2(*x, *y), vec2(*w, *h)).shrink(1.0);
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            tiles.push(r);
            let is_rest = i >= kids.len();
            let c = if is_rest { pal.text_faint } else { kit::category_color(i) };
            let hovered = hover.map(|h| r.contains(h)).unwrap_or(false);
            let fill = mix(pal.card, c, if hovered { 0.42 } else { 0.24 });
            p.rect_filled(r, 3.0, fill);
            if fd.bloom > 0.0 && hovered {
                p.rect_stroke(r.expand(1.5), 4.0, Stroke::new(3.0_f32, with_alpha(c, 0.25 * fd.bloom)));
            }
            p.rect_stroke(r, 3.0, Stroke::new(1.0_f32, with_alpha(c, 0.85)));
            let is_more = is_rest && more > 0 && i == kids.len();
            let (name, size) = if is_more { ("More folders".to_string(), more) } else if is_rest { ("Files here".to_string(), rest) } else { let n = &t.nodes[kids[i] as usize]; (n.name.clone(), n.size) };
            if r.width() > 46.0 && r.height() > 22.0 {
                let clip = p.with_clip_rect(r.shrink(4.0));
                clip.text(r.left_top() + vec2(6.0, 5.0), Align2::LEFT_TOP, &name, theme::semibold(12.5), pal.text);
                if r.height() > 40.0 {
                    clip.text(r.left_top() + vec2(6.0, 22.0), Align2::LEFT_TOP, format_bytes(size as f64), theme::ui(11.5), lighten_text(c));
                }
            }
            if !is_rest {
                let resp = ui.interact(r, ui.id().with(("tm", kids[i])), Sense::click());
                if resp.clicked() && !t.nodes[kids[i] as usize].children.is_empty() {
                    go = Some(kids[i]);
                }
                resp.context_menu(|ui| super::disk_delete::item_menu(ui, &diskdelete::folder_target(t, kids[i]), rules.as_deref(), &mut menu));
                resp.on_hover_text(format!("{}\n{}  \u{2022}  {} files", t.path_of(kids[i]).display(), format_bytes(size as f64), t.nodes[kids[i] as usize].files));
            } else if is_more {
                ui.interact(r, ui.id().with("tm-more"), Sense::hover()).on_hover_text(format!("{} in smaller folders not drawn here\nSee the Folders tab", format_bytes(more as f64)));
            } else {
                let resp = ui.interact(r, ui.id().with("tm-rest"), Sense::click());
                if resp.clicked() {
                    self.pro.disk_tab = 1;
                }
                resp.on_hover_text(format!("{} in files directly in this folder\nClick to list them", format_bytes(rest as f64)));
            }
        }
        if bg.secondary_clicked() && node.parent != diskspace::NO_PARENT && !hover.is_some_and(|h| tiles.iter().any(|r| r.contains(h))) {
            go = Some(node.parent);
        }
        if menu.want_rules {
            self.pro.del.ensure_rules(fd.rows);
        }
        if let (Some(mode), Some(target)) = (menu.mode, menu.target.take()) {
            let epoch = self.pro.disk_epoch();
            self.pro.del.request(vec![target], mode, epoch, fd.rows);
        }
        go
    }

    fn disk_side(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Tree, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let p = ui.painter().clone();
        card(&p, rect, &pal);
        let inner = rect.shrink(8.0);
        // Tabs.
        let tabs = ["Folders", "Files", "Largest files", "File types"];
        let mut x = inner.left();
        for (i, name) in tabs.iter().enumerate() {
            let r = Rect::from_min_size(pos2(x, inner.top()), vec2(widgets::text_width(&p, name, theme::ui(13.0)) + 24.0, 28.0));
            let resp = ui.interact(r, ui.id().with(("disk-tab", i)), Sense::click());
            let sel = self.pro.disk_tab == i;
            if sel {
                p.rect_filled(r, 4.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(r, 4.0, pal.hover);
            }
            p.text(r.center(), Align2::CENTER_CENTER, *name, theme::ui(13.0), if sel { pal.on_selection() } else { pal.text });
            if resp.clicked() {
                if i == 1 && self.pro.disk_tab != 1 {
                    self.pro.disk_files_for = None; // re-list on entering the Files tab
                }
                self.pro.disk_tab = i;
            }
            x = r.right() + 6.0;
        }
        // Selection is per list: a different tab, sort, subfolder mode, folder or tree version clears it.
        let epoch = self.pro.disk_epoch();
        let view = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.pro.disk_tab, self.pro.disk_files_sub, self.pro.disk_files_sort as u8, self.pro.disk_files_asc, epoch).hash(&mut h);
            h.finish()
        };
        self.pro.del.sel.reset(view, self.pro.disk_cursor);
        let bar_h = if self.pro.disk_tab < 3 && !self.pro.del.sel.rows.is_empty() { super::disk_delete::BAR_H } else { 0.0 };
        let bar = Rect::from_min_max(pos2(inner.left(), inner.bottom() - super::disk_delete::BAR_H), inner.max);
        let list = Rect::from_min_max(pos2(inner.left(), inner.top() + 38.0), pos2(inner.right(), inner.bottom() - bar_h));
        let rules = self.pro.del.rules.ok().cloned();
        let rules = rules.as_deref();
        let del = &mut self.pro.del;
        let mut menu = super::disk_delete::MenuOut::default();
        let node = &t.nodes[self.pro.disk_cursor as usize];
        let total = node.size.max(1) as f32;
        let mut go = None;
        let mut reveal = None;
        let now = SystemTime::now();
        let narrow = list.width() < DISK_NARROW_W;
        let tiny = list.width() < DISK_TINY_W;
        let stamp = |m: Option<SystemTime>| m.map(diskspace::local_stamp).filter(|s| !s.is_empty()).unwrap_or_else(|| "\u{2014}".into());
        // Date cell text; a dash keeps the size column aligned when there is no timestamp.
        let date_of = |m: Option<SystemTime>| if tiny { None } else { Some(m.map(|m| diskspace::relative_time(now, m)).unwrap_or_else(|| "\u{2014}".into())) };
        match self.pro.disk_tab {
            0 => {
                let mk = |i: usize| Some(diskdelete::folder_target(t, node.children[i]));
                let size_of = |i: usize| t.nodes[node.children[i] as usize].size;
                disk_list(ui, list, ("disk-folders", self.pro.disk_cursor), &pal, DISK_ROW_H, node.children.len(), |ui, i, r, resp| {
                    let c = node.children[i];
                    let n = &t.nodes[c as usize];
                    if del.sel.rows.contains(&i) {
                        super::disk_delete::paint_selected(ui.painter(), r, &pal);
                    }
                    disk_row(ui.painter(), r, &pal, &n.name, false, n.size, n.size as f32 / total, kit::category_color(i), !narrow, None, None);
                    if resp.clicked() && del.on_click(ui, i) && !n.children.is_empty() {
                        go = Some(c);
                    }
                    del.row_menu(&resp, i, &mk, rules, &mut menu);
                });
                del.finish(ui, &pal, bar, node.children.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                if node.children.is_empty() {
                    empty_state(&p, list, &pal, "No sub-folders");
                }
            }
            1 => {
                let sub = self.pro.disk_files_sub;
                let cur = self.pro.disk_cursor;
                let gen = epoch;
                let (col, asc) = (self.pro.disk_files_sort, self.pro.disk_files_asc);
                let here = t.path_of(cur);
                let mut loading;
                let mut fresh = false;
                let mut order: &[u32] = &[];
                if sub {
                    // Every file below this folder, from the scan index, sorted on a worker thread.
                    if self.pro.disk_sub_order.poll() {
                        self.pro.disk_sub_ready = self.pro.disk_sub_key;
                    }
                    let want: SubKey = (gen, cur, col, asc);
                    if self.pro.disk_sub_ready != Some(want) && !self.pro.disk_sub_order.is_loading() {
                        if let Some((tree, _)) = &self.pro.disk_tree {
                            let tree = tree.clone();
                            self.pro.disk_sub_key = Some(want);
                            self.pro.disk_sub_order.start(move || Ok(sorted_files(&tree, cur, col, asc)));
                        }
                    }
                    // A newer scan generation may keep showing the previous order meanwhile.
                    let usable = self.pro.disk_sub_ready.map(|r| (r.1, r.2, r.3) == (cur, col, asc)).unwrap_or(false);
                    fresh = self.pro.disk_sub_ready == Some(want);
                    loading = !fresh;
                    if usable {
                        order = self.pro.disk_sub_order.ok().map(|o| o.0.as_slice()).unwrap_or(&[]);
                        loading = false;
                    }
                    if self.pro.disk_sub_order.is_loading() {
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                } else {
                    // Keep the listing current: one background read per visited folder.
                    if self.pro.disk_files.poll() {
                        self.pro.disk_files_order = None;
                    }
                    if self.pro.disk_files_for.as_deref() != Some(here.as_path()) && !self.pro.disk_files.is_loading() {
                        self.pro.disk_files_for = Some(here.clone());
                        self.pro.disk_files_order = None;
                        let h = here.clone();
                        self.pro.disk_files.start(move || diskspace::list_files(&h));
                    }
                    loading = self.pro.disk_files.is_loading() || self.pro.disk_files_for.as_deref() != Some(here.as_path());
                    if loading {
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                }
                // Include-subfolders switch, then the header strip with sortable columns.
                let tgl = Rect::from_min_size(list.min, vec2(list.width(), 24.0));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tgl).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                    ui.label(RichText::new("Include subfolders").font(theme::ui(12.5)).color(pal.text));
                    ui.add_space(4.0);
                    widgets::toggle(ui, &mut self.pro.disk_files_sub, &pal);
                });
                let hdr = Rect::from_min_size(pos2(list.left(), tgl.bottom()), vec2(list.width(), 22.0));
                let body = Rect::from_min_max(pos2(list.left(), hdr.bottom()), list.max);
                let date_w = if tiny { 0.0 } else { DISK_DATE_W };
                let cols = [(FileCol::Name, "Name", Align2::LEFT_CENTER, hdr.left() + 4.0), (FileCol::Size, "Size", Align2::RIGHT_CENTER, hdr.right() - 4.0 - date_w), (FileCol::Modified, "Modified", Align2::RIGHT_CENTER, hdr.right() - 4.0)];
                for (k, (c, label, anchor, x)) in cols.iter().enumerate() {
                    if *c == FileCol::Modified && tiny {
                        continue;
                    }
                    let active = *c == self.pro.disk_files_sort;
                    let arrow = if !active { "" } else if self.pro.disk_files_asc { "  \u{25b2}" } else { "  \u{25bc}" };
                    let r = p.text(pos2(*x, hdr.center().y), *anchor, format!("{label}{arrow}"), theme::ui(12.0), if active { pal.text } else { pal.text_dim });
                    if ui.interact(r.expand2(vec2(4.0, 3.0)), ui.id().with(("files-hdr", k)), Sense::click()).clicked() {
                        if active {
                            self.pro.disk_files_asc = !self.pro.disk_files_asc;
                        } else {
                            self.pro.disk_files_sort = *c;
                            self.pro.disk_files_asc = *c == FileCol::Name;
                        }
                        self.pro.disk_files_order = None;
                    }
                }
                if sub && !narrow {
                    p.text(pos2(folder_x(hdr, true, !tiny), hdr.center().y), Align2::LEFT_CENTER, "Folder", theme::ui(12.0), pal.text_dim);
                }
                p.line_segment([pos2(hdr.left(), hdr.bottom()), pos2(hdr.right(), hdr.bottom())], Stroke::new(1.0_f32, pal.panel_stroke));
                if sub {
                    if loading {
                        empty_state(&p, body, &pal, &format!("Sorting {} files\u{2026}", t.nodes[cur as usize].files));
                    } else if order.is_empty() {
                        empty_state(&p, body, &pal, "No files in this folder or below");
                    } else {
                        let biggest = self.pro.disk_sub_order.ok().map(|o| o.1).unwrap_or(1).max(1) as f32;
                        let mk = |i: usize| if fresh { super::disk_delete::file_target(t, order[i] as usize) } else { None };
                        let size_of = |i: usize| t.files.get(order[i] as usize).map_or(0, |f| f.size);
                        disk_list(ui, body, ("disk-files-sub", cur), &pal, DISK_ROW_H, order.len(), |ui, i, r, resp| {
                            let Some(f) = t.files.get(order[i] as usize) else { return };
                            let rel = t.relative_dir(cur, f.dir);
                            let date = date_of(f.modified());
                            let folder = if rel.is_empty() { "." } else { rel.as_str() };
                            if del.sel.rows.contains(&i) {
                                super::disk_delete::paint_selected(ui.painter(), r, &pal);
                            }
                            disk_row(ui.painter(), r, &pal, t.file_name(f), f.hidden(), f.size, f.size as f32 / biggest, theme::BLUE, !narrow, date.as_deref(), (!narrow).then_some(folder));
                            if resp.clicked() && del.on_click(ui, i) {
                                reveal = Some(t.file_path(f));
                            }
                            del.row_menu(&resp, i, &mk, rules, &mut menu);
                            if resp.hovered() {
                                resp.on_hover_text(format!("{}\n{}\nModified {}", t.file_path(f).display(), format_bytes(f.size as f64), stamp(f.modified())));
                            }
                        });
                        del.finish(ui, &pal, bar, order.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                    }
                } else {
                    // Sorted order, rebuilt only when the data or sort changed.
                    if self.pro.disk_files_order.as_ref().map(|o| (o.0, o.1) != (col, asc)).unwrap_or(true) {
                        if let Some(files) = self.pro.disk_files.ok() {
                            let mut v: Vec<usize> = (0..files.len()).collect();
                            v.sort_by(|a, b| {
                                let (a, b) = (&files[*a], &files[*b]);
                                let o = match col {
                                    FileCol::Name => cmp_str(&a.name, &b.name),
                                    FileCol::Size => a.size.cmp(&b.size),
                                    FileCol::Modified => a.modified.cmp(&b.modified),
                                };
                                if asc { o } else { o.reverse() }
                            });
                            self.pro.disk_files_order = Some((col, asc, v));
                        }
                    }
                    let order: &[usize] = self.pro.disk_files_order.as_ref().map(|o| o.2.as_slice()).unwrap_or(&[]);
                    if loading {
                        empty_state(&p, body, &pal, "Reading folder\u{2026}");
                    } else if let Some(e) = self.pro.disk_files.error() {
                        empty_state(&p, body, &pal, e);
                    } else if let Some(files) = self.pro.disk_files.ok() {
                        if files.is_empty() {
                            empty_state(&p, body, &pal, "No files directly in this folder");
                        } else {
                            let biggest = files.iter().map(|f| f.size).max().unwrap_or(1).max(1) as f32;
                            // Listed live: no file index; the tree update finds the record by name.
                            let mk = |i: usize| Some(Target { path: here.join(&files[order[i]].name), size: files[order[i]].size, is_dir: false, node: None, file: None });
                            let size_of = |i: usize| files[order[i]].size;
                            disk_list(ui, body, ("disk-files", cur), &pal, DISK_ROW_H, order.len(), |ui, i, r, resp| {
                                let f = &files[order[i]];
                                let date = date_of(f.modified);
                                if del.sel.rows.contains(&i) {
                                    super::disk_delete::paint_selected(ui.painter(), r, &pal);
                                }
                                disk_row(ui.painter(), r, &pal, &f.name, f.hidden, f.size, f.size as f32 / biggest, theme::BLUE, !narrow, date.as_deref(), None);
                                let path = here.join(&f.name);
                                if resp.clicked() && del.on_click(ui, i) {
                                    reveal = Some(path.clone());
                                }
                                del.row_menu(&resp, i, &mk, rules, &mut menu);
                                resp.on_hover_text(format!("{}\n{}\nModified {}", path.display(), format_bytes(f.size as f64), stamp(f.modified)));
                            });
                            del.finish(ui, &pal, bar, order.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                        }
                    }
                }
            }
            2 => {
                let gen = epoch;
                let cursor = self.pro.disk_cursor;
                if self.pro.disk_largest.as_ref().map(|(g, c, _)| (*g, *c) != (gen, cursor)).unwrap_or(true) {
                    self.pro.disk_largest = Some((gen, cursor, t.largest_under(cursor, 100)));
                }
                let big: &[usize] = self.pro.disk_largest.as_ref().map(|l| l.2.as_slice()).unwrap_or(&[]);
                if big.is_empty() {
                    empty_state(&p, list, &pal, "No files in this folder");
                }
                let top = big.first().map(|i| t.files[*i].size).unwrap_or(1).max(1) as f32;
                let mk = |i: usize| super::disk_delete::file_target(t, big[i]);
                let size_of = |i: usize| t.files[big[i]].size;
                disk_list(ui, list, ("disk-largest", cursor), &pal, DISK_BIG_ROW_H, big.len(), |ui, i, r, resp| {
                    let f = &t.files[big[i]];
                    let folder = t.path_of(f.dir);
                    let name = t.file_name(f);
                    let line1 = Rect::from_min_size(r.min, vec2(r.width(), 22.0));
                    let date = date_of(f.modified());
                    if del.sel.rows.contains(&i) {
                        super::disk_delete::paint_selected(ui.painter(), r, &pal);
                    }
                    disk_row(ui.painter(), line1, &pal, name, false, f.size, f.size as f32 / top, theme::ORANGE, !narrow, date.as_deref(), None);
                    widgets::text_fit(ui.painter(), pos2(r.left() + 4.0, r.bottom() - 9.0), Align2::LEFT_CENTER, &folder.display().to_string(), theme::ui(11.0), pal.text_dim, r.width() - 8.0);
                    let path = folder.join(name);
                    if resp.clicked() && del.on_click(ui, i) {
                        reveal = Some(path.clone());
                    }
                    del.row_menu(&resp, i, &mk, rules, &mut menu);
                    resp.on_hover_text(format!("{}\n{}\nModified {}", path.display(), format_bytes(f.size as f64), stamp(f.modified())));
                });
                del.finish(ui, &pal, bar, big.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
            }
            _ => {
                let all: u64 = t.extensions.iter().map(|e| e.1).sum::<u64>().max(1);
                disk_list(ui, list, "disk-types", &pal, DISK_ROW_H, t.extensions.len(), |ui, i, r, _| {
                    let (ext, bytes, files) = &t.extensions[i];
                    disk_row(ui.painter(), r, &pal, &format!(".{ext}  ({files} files)"), false, *bytes, *bytes as f32 / all as f32, kit::category_color(i), !narrow, None, None);
                });
            }
        }
        if menu.want_rules {
            self.pro.del.ensure_rules(fd.rows);
        }
        if let Some(g) = go {
            self.pro.disk_cursor = g;
        }
        if let Some(path) = reveal {
            let _ = kit::reveal(&path);
        }
    }
}

/// Virtualised list in `rect`: `row(ui, i, r, resp)` draws item i into r (use `ui.painter()`).
/// `id` is salted with the folder so each one starts scrolled to the top.
pub(super) fn disk_list(ui: &mut egui::Ui, rect: Rect, id: impl std::hash::Hash + Copy, pal: &Palette, row_h: f32, n: usize, mut row: impl FnMut(&mut egui::Ui, usize, Rect, egui::Response)) {
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).id_salt(id), |ui| {
        ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let resp = ui.interact(r, ui.id().with(("row", i)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 3.0, with_alpha(pal.text, 0.05));
                }
                row(ui, i, r, resp);
            }
        });
    });
}

/// One list row: name, thin % bar, size and optional date (right-aligned). `pct` prints the
/// percentage text (dropped on narrow panels); `date` is skipped when `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn disk_row(p: &egui::Painter, r: Rect, pal: &Palette, name: &str, dim: bool, size: u64, frac: f32, color: Color32, pct: bool, date: Option<&str>, folder: Option<&str>) {
    let bar = Rect::from_min_max(pos2(r.left(), r.bottom() - 4.0), pos2(r.left() + r.width() * frac.clamp(0.0, 1.0), r.bottom() - 1.0));
    p.rect_filled(bar, 1.0, with_alpha(color, 0.75));
    let y = r.center().y - 2.0;
    let mut x = r.right() - 4.0;
    if let Some(d) = date {
        p.text(pos2(x, y), Align2::RIGHT_CENTER, d, theme::ui(11.5), pal.text_dim);
        x -= DISK_DATE_W;
    }
    if pct {
        p.text(pos2(x, y), Align2::RIGHT_CENTER, format!("{:.1}%", frac * 100.0), theme::ui(12.0), pal.text_dim);
        x -= 56.0;
    }
    p.text(pos2(x, y), Align2::RIGHT_CENTER, format_bytes(size as f64), theme::ui(12.0), pal.text);
    x -= 70.0;
    let avail = x - r.left() - 8.0;
    let name_w = if folder.is_some() { avail * FOLDER_SPLIT } else { avail };
    widgets::text_fit(p, pos2(r.left() + 4.0, y), Align2::LEFT_CENTER, name, theme::ui(12.5), if dim { pal.text_faint } else { pal.text }, name_w);
    if let Some(f) = folder {
        widgets::text_fit(p, pos2(r.left() + 4.0 + name_w + 8.0, y), Align2::LEFT_CENTER, f, theme::ui(11.5), pal.text_dim, avail - name_w - 8.0);
    }
}

/// Left edge of the Folder column in a row or header `r`, mirroring [`disk_row`].
fn folder_x(r: Rect, pct: bool, date: bool) -> f32 {
    let x = r.right() - 4.0 - if date { DISK_DATE_W } else { 0.0 } - if pct { 56.0 } else { 0.0 } - 70.0;
    r.left() + 4.0 + (x - r.left() - 8.0) * FOLDER_SPLIT + 8.0
}

/// Case-insensitive name order without allocating.
fn cmp_name_nocase(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// File indices below `base` in the requested order, and the biggest file size.
fn sorted_files(t: &Tree, base: u32, col: FileCol, asc: bool) -> (Vec<u32>, u64) {
    let mut v: Vec<u32> = t.files_under(base).into_iter().map(|i| i as u32).collect();
    let biggest = v.iter().map(|i| t.files[*i as usize].size).max().unwrap_or(0);
    v.sort_by(|a, b| {
        let (a, b) = (&t.files[*a as usize], &t.files[*b as usize]);
        let o = match col {
            FileCol::Name => cmp_name_nocase(t.file_name(a), t.file_name(b)),
            FileCol::Size => a.size.cmp(&b.size),
            FileCol::Modified => a.mtime.cmp(&b.mtime),
        };
        if asc { o } else { o.reverse() }
    });
    (v, biggest)
}

fn lighten_text(c: Color32) -> Color32 {
    theme::lighten(c, 0.35)
}

/// Shared body of the Drivers and Services pages: stat strip + table.
#[allow(clippy::too_many_arguments)]
pub fn driver_like_page(ui: &mut egui::Ui, p: &egui::Painter, rows: Vec<Rect>, fd: &FrameData<'_>, list: &[Service], state: &mut TableState, error: Option<&str>, drivers: bool, icons: &mut super::icons::IconCache) {
    let (pal, bloom) = (&fd.pal, fd.bloom);
    let running = list.iter().filter(|s| s.running).count();
    let auto = list.iter().filter(|s| matches!(s.start, "Boot" | "System" | "Automatic")).count();
    let disabled = list.iter().filter(|s| s.start == "Disabled").count();
    let third_party = list.iter().filter(|s| !s.company.is_empty() && !s.company.contains("Microsoft")).count();
    let n = list.len().max(1) as f32;
    let mut cards = vec![
        ("Running".to_string(), running.to_string(), format!("of {} {}", list.len(), if drivers { "drivers" } else { "services" }), theme::GREEN, Some(running as f32 / n)),
        ("Stopped".to_string(), (list.len() - running).to_string(), format!("{disabled} disabled"), theme::ORANGE, Some((list.len() - running) as f32 / n)),
        ("Start at boot".to_string(), auto.to_string(), "Boot, system or automatic".into(), theme::BLUE, Some(auto as f32 / n)),
    ];
    if drivers {
        cards.push(("Third party".into(), third_party.to_string(), "Not signed by Microsoft (company name)".into(), theme::PURPLE, Some(third_party as f32 / n)));
    } else {
        let shared = list.iter().filter(|s| s.kind == "Shared process" && s.running).count();
        cards.push(("Shared hosts".into(), shared.to_string(), "Running in svchost groups".into(), theme::PURPLE, Some(shared as f32 / n)));
    }
    stat_row(p, rows[0], pal, bloom, &cards);
    if list.is_empty() {
        empty_state(p, rows[1], pal, error.unwrap_or("Reading the Service Control Manager\u{2026}"));
        return;
    }
    let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&state.filter, &[&list[*i].name, &list[*i].display, &list[*i].company, &list[*i].path])).collect();
    // Services have no version column: their column 6 is the image path.
    let map = |col: usize| if !drivers && col >= 6 { col + 1 } else { col };
    state.sort(list, &mut order, |a, b, col| match map(col) {
        0 => cmp_str(&a.name, &b.name),
        1 => cmp_str(&a.display, &b.display),
        2 => a.state.cmp(b.state),
        3 => a.start.cmp(b.start),
        4 => a.kind.cmp(b.kind),
        5 => {
            if drivers {
                cmp_str(&a.company, &b.company)
            } else {
                a.pid.cmp(&b.pid)
            }
        }
        6 => a.version.cmp(&b.version),
        _ => cmp_str(&a.path, &b.path),
    });
    let cols: Vec<Col> = if drivers {
        vec![Col::text("Name", 170.0), Col::text("Description", 280.0), Col::text("Status", 100.0), Col::text("Start", 90.0), Col::text("Type", 100.0), Col::text("Company", 200.0), Col::text("Version", 150.0), Col::text("Image path", 320.0)]
    } else {
        vec![Col::text("Name", 190.0), Col::text("Display name", 330.0), Col::text("Status", 100.0), Col::text("Start", 100.0), Col::text("Type", 120.0), Col::num("PID", 70.0), Col::text("Image path", 360.0)]
    };
    let cell = |i: usize, col: usize| -> Cell {
        let s = &list[i];
        match map(col) {
            0 => Cell::new(&s.name),
            1 => Cell::new(if drivers && !s.description.is_empty() { &s.description } else { &s.display }).dim(pal),
            2 => Cell::new(s.state).dot(if s.running { theme::GREEN } else { pal.text_faint }),
            3 => Cell::new(s.start).color(if s.start == "Disabled" { theme::RED } else { pal.text_dim }),
            4 => Cell::new(s.kind).dim(pal),
            5 => {
                if drivers {
                    Cell::new(&s.company).dim(pal)
                } else {
                    Cell::new(if s.pid != 0 { s.pid.to_string() } else { String::new() }).dim(pal)
                }
            }
            6 => Cell::new(&s.version).dim(pal),
            _ => Cell::new(&s.path).dim(pal),
        }
    };
    let key = |i: usize| list[i].name.clone();
    if let Some(i) = table::show(ui, rows[1], if drivers { "drivers-table" } else { "services-table" }, pal, &cols, &order, state, &key, &cell, icons) {
        let path = PathBuf::from(&list[i].path);
        if path.exists() {
            let _ = kit::reveal(&path);
        }
    }
}
