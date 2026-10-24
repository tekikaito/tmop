//! Flight Recorder page: live graph, process events and recordings.

use super::kit::{self, empty_state, hsplit, row_ui, titled_card, vsplit, GAP, MARGIN};
use super::pro_pages::LIMITS;
use super::theme::{self, with_alpha};
use super::widgets::{self, card, draw_graph, GraphLine, Icon};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::pro::recorder::{self, EventKind, Record};
use crate::process::format_bytes;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense, Stroke};
use std::path::PathBuf;
use std::time::Duration;

impl App {
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
}
