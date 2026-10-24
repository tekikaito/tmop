//! Flight Recorder page: live graph, process events and recordings.

use super::kit::{self, empty_state, hsplit, section_card, vsplit};
use super::pro_pages::{clock_of, fmt_span, hm_of, list_row, mono_input, page_layout, stamp_secs, swatch_legend, swatch_legend_width, toolbar, RowLook, Swatch, LIMITS};
use super::theme::{self, with_alpha};
use super::widgets::{self, draw_graph, Button, ButtonKind, Graph, GraphLine, Icon};
use super::{fmt_hms, App, FrameData};
use crate::metrics::Series;
use crate::pro::recorder::{self, EventKind, Record};
use crate::process::format_bytes;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense, Stroke};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Width the toolbar needs: folder field, three buttons, the limit box and the two main buttons.
const TOOLBAR_W: f32 = 1060.0;
/// Space left of the graph for the mono percentage labels.
const LABEL_W: f32 = 38.0;

impl App {
    pub fn recorder_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        if self.pro.rec_dir_text.is_empty() {
            self.pro.rec_dir_text = self.recorder.dir.display().to_string();
        }
        if !self.pro.recordings_listed {
            self.pro.recordings = recorder::list(&self.recorder.dir);
            self.pro.recordings_listed = true;
        }
        let recording = self.recorder.is_recording();
        let (subtitle, sub_color) = match (self.recorder.recording_path(), &self.recorder.last_error) {
            (Some(path), _) => (format!("REC {} \u{b7} {} samples \u{b7} Recording to {}", fmt_hms(self.recorder.elapsed().unwrap_or_default()), self.recorder.recorded_rows(), path.display()), pal.text_faint),
            (None, Some(e)) => (e.clone(), st.warn),
            (None, None) => match &self.recorder.last_saved {
                Some(sv) => (format!("Saved {}", sv.display()), pal.text_faint),
                None => (format!("Not recording \u{b7} {} samples in the flight ring", self.recorder.ring.len()), pal.text_faint),
            },
        };
        // The subtitle is drawn here (it needs a colour and the recording dot); the title block only
        // gets a blank one so it keeps its two-line layout.
        let body = self.page_title(ui, &pal, "Flight Recorder", " ");
        let (tools, area) = page_layout(body, TOOLBAR_W);
        let p = ui.painter().clone();
        {
            let (x, y) = (body.left() + 13.0, body.top() - super::HEADER_H + 44.0);
            let on_title_row = tools.bottom() <= body.top();
            let max_w = if on_title_row { tools.left() - x - 16.0 } else { body.right() - x - 8.0 }.max(120.0);
            let mut text_x = x;
            if recording {
                let c = pos2(x + 4.0, y + 8.0);
                let pulse = 0.5 + 0.5 * (fd.time * 3.0).sin() as f32;
                p.circle_filled(c, 8.0, with_alpha(st.danger, 0.10 + 0.14 * pulse * fd.bloom.max(0.4)));
                p.circle_filled(c, 4.0, st.danger);
                text_x += 16.0;
            }
            widgets::text_fit(&p, pos2(text_x, y), Align2::LEFT_TOP, &subtitle, theme::mono(12.0), sub_color, max_w - (text_x - x));
        }

        let mut toggle = false;
        let mut save_flight = false;
        let mut open_folder = false;
        let mut apply_dir = false;
        let mut default_dir = false;
        let ring_len = self.recorder.ring.len();
        toolbar(ui, tools, |ui| {
            save_flight = Button::new(&format!("Save flight recorder ({ring_len})")).show(ui, &pal).clicked();
            toggle = if recording { Button::new("\u{25a0} Stop recording").kind(ButtonKind::DangerFill) } else { Button::primary("\u{25cf} Start recording") }.show(ui, &pal).clicked();
            egui::ComboBox::from_id_salt("rec-limit").width(130.0).selected_text(LIMITS[self.pro.rec_limit].0).show_ui(ui, |ui| {
                for (i, (l, _)) in LIMITS.iter().enumerate() {
                    ui.selectable_value(&mut self.pro.rec_limit, i, *l);
                }
            });
            ui.label(RichText::new("Stop after").font(theme::ui(12.0)).color(pal.text_faint));
            open_folder = Button::ghost("Open folder").show(ui, &pal).clicked();
            default_dir = Button::ghost("Default").show(ui, &pal).clicked();
            apply_dir = mono_input(ui, &mut self.pro.rec_dir_text, "Recording folder", 220.0, &pal).lost_focus();
            ui.label(RichText::new("Folder").font(theme::ui(12.0)).color(pal.text_faint));
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

        let low_h = (area.height() * 0.36).clamp(170.0, 250.0);
        let rows = vsplit(area, &[-1.0, low_h], 14.0);
        let close_playback = self.flight_graph(ui, rows[0], fd);

        // Process events and saved recordings.
        let low = hsplit(rows[1], &[3.0, 2.0], 14.0);
        let content = section_card(&p, low[0], &pal, "Process events", &self.recorder.events.len().to_string());
        let row_h = 32.0;
        let max_rows = (content.height() / row_h).floor() as usize;
        if self.recorder.events.is_empty() {
            empty_state(&p, content, &pal, "Process starts and exits appear here");
        }
        for (i, e) in self.recorder.events.iter().rev().take(max_rows).enumerate() {
            let top = content.top() + i as f32 * row_h;
            let y = top + row_h / 2.0;
            let (tag, c) = match e.kind {
                EventKind::Started => ("Start", st.ok),
                EventKind::Exited => ("Exit", st.danger_text),
                EventKind::Note => ("Note", st.info),
            };
            p.hline(content.x_range(), top, Stroke::new(1.0_f32, pal.row_rule()));
            p.text(pos2(content.left(), y), Align2::LEFT_CENTER, clock_of(&e.time), theme::mono(12.0), pal.text_faint);
            p.text(pos2(content.left() + 84.0, y), Align2::LEFT_CENTER, tag, theme::ui(13.0), c);
            widgets::text_fit(&p, pos2(content.left() + 140.0, y), Align2::LEFT_CENTER, &e.text, theme::ui(13.0), pal.text_table(), content.width() - 140.0);
        }

        let content = section_card(&p, low[1], &pal, "Recordings", &format!("{} \u{b7} click to play back", self.pro.recordings.len()));
        let list = Rect::from_min_max(content.min, pos2(content.right(), content.bottom() - 20.0));
        if self.pro.recordings.is_empty() {
            empty_state(&p, list, &pal, "No recordings in this folder yet");
        }
        let writing = self.recorder.recording_path().and_then(|w| w.file_name()).map(|n| n.to_os_string());
        let elapsed = self.recorder.elapsed().map(|d| fmt_span(d.as_secs())).unwrap_or_default();
        let mut open: Option<PathBuf> = None;
        let mut reveal: Option<PathBuf> = None;
        for (i, r) in self.pro.recordings.iter().take((list.height() / 42.0).floor() as usize).enumerate() {
            let rr = Rect::from_min_size(pos2(list.left(), list.top() + i as f32 * 42.0), vec2(list.width(), 38.0));
            let resp = ui.interact(rr, ui.id().with(("recording", i)), Sense::click());
            let playing = self.pro.playback.as_ref().map(|(pp, _)| *pp == r.path).unwrap_or(false);
            let live = writing.as_deref().is_some_and(|n| r.path.file_name() == Some(n));
            if live {
                widgets::tinted_card(&p, rr, &pal, st.danger);
                if playing {
                    p.rect_stroke(rr, 10.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.8)));
                }
            } else {
                list_row(&p, rr, &pal, if playing { RowLook::Marked(pal.accent) } else { RowLook::Plain }, resp.hovered());
            }
            let (right, right_color) = if live { (format!("recording \u{b7} {elapsed}"), st.danger_text) } else { (format_bytes(r.size as f64), pal.text_faint) };
            let rt = p.text(pos2(rr.right() - 12.0, rr.center().y), Align2::RIGHT_CENTER, right, theme::ui(12.0), right_color);
            widgets::text_fit(&p, pos2(rr.left() + 12.0, rr.center().y), Align2::LEFT_CENTER, &r.name, theme::mono(12.0), pal.text, rr.width() - 36.0 - rt.width());
            if resp.clicked() {
                open = Some(r.path.clone());
            }
            if resp.secondary_clicked() {
                reveal = Some(r.path.clone());
            }
            resp.on_hover_text("Click to play back, right-click to show in Explorer");
        }
        widgets::text_fit(&p, pos2(content.left(), content.bottom()), Align2::LEFT_BOTTOM, "Right-click shows the file in Explorer.", theme::ui(11.0), pal.text_faint, content.width());
        if close_playback {
            self.pro.playback = None;
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

    /// The graph card of the flight ring (or of an opened recording). True when "Close playback"
    /// was clicked.
    fn flight_graph(&self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) -> bool {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let playing = self.pro.playback.is_some();
        let records: Vec<&Record> = match &self.pro.playback {
            Some((_, recs)) => recs.iter().collect(),
            None => self.recorder.ring.iter().collect(),
        };
        let n = records.len();
        // Span of the samples and the time between two of them.
        let span_secs = match (records.first(), records.last()) {
            (Some(a), Some(b)) if n > 1 => stamp_secs(&b.time).zip(stamp_secs(&a.time)).map(|(b, a)| (b - a).max(0) as u64),
            _ => None,
        };
        let interval = if playing {
            span_secs.filter(|_| n > 1).map(|s| fmt_interval(Duration::from_secs_f32(s as f32 / (n - 1) as f32)))
        } else {
            self.settings.update_speed().interval().map(fmt_interval)
        };
        let title = match (&self.pro.playback, span_secs) {
            (Some((path, _)), _) => format!("Playback {}", path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()),
            (None, Some(s)) => format!("Flight recorder \u{b7} last {}{}", fmt_span(s), interval.map(|i| format!(" \u{b7} 1 sample / {i}")).unwrap_or_default()),
            (None, None) => "Flight recorder".to_string(),
        };
        let content = section_card(&p, rect, &pal, &title, "");
        let mut legend: Vec<(&str, Swatch)> = vec![("CPU", Swatch::Line(fd.metric("cpu"))), ("Memory", Swatch::Line(fd.metric("memory"))), ("Disk", Swatch::Line(fd.metric("disk"))), ("GPU", Swatch::Line(fd.metric("gpu")))];
        if !playing {
            legend.push(("Process event", Swatch::Tick(st.warn)));
        }
        let lw = swatch_legend_width(&p, &legend);
        swatch_legend(&p, &pal, pos2(rect.right() - 22.0 - lw, rect.top() + 25.0), &legend);

        let foot_h = 16.0 + 8.0 + 28.0;
        let graph = Rect::from_min_max(pos2(content.left() + LABEL_W, content.top() + 2.0), pos2(content.right(), (content.bottom() - foot_h).max(content.top() + 60.0)));
        let mut close = false;
        // Readout row under the time labels.
        let readout_y = graph.bottom() + 16.0 + 8.0 + 14.0;
        if n < 2 {
            p.rect_stroke(graph, 0.0, Stroke::new(1.0_f32, pal.grid));
            empty_state(&p, graph, &pal, "Collecting samples\u{2026}");
        } else {
            let px = (graph.width() / (n - 1) as f32).clamp(0.25, 16.0);
            let x_of = |idx: usize| graph.right() - (n - 1 - idx) as f32 * px;
            // Time labels (six, evenly spaced) with a faint vertical line each.
            for k in 0..6 {
                let x = graph.left() + graph.width() * k as f32 / 5.0;
                let idx = (n - 1).saturating_sub(((graph.right() - x) / px).round() as usize);
                if x_of(idx) - x > px + 0.5 {
                    continue; // left of the oldest sample
                }
                p.vline(x, graph.y_range(), Stroke::new(1.0_f32, with_alpha(pal.text, 0.05)));
                let anchor = match k {
                    0 => Align2::LEFT_CENTER,
                    5 => Align2::RIGHT_CENTER,
                    _ => Align2::CENTER_CENTER,
                };
                p.text(pos2(x, graph.bottom() + 12.0), anchor, hm_of(&records[idx].time), theme::mono(10.0), pal.text_faint);
            }
            for (k, label) in ["100%", "75", "50", "25", "0"].iter().enumerate() {
                let y = graph.top() + graph.height() * k as f32 / 4.0;
                match k {
                    0 | 4 => {}
                    _ => {
                        p.hline(graph.x_range(), y, Stroke::new(1.0_f32, pal.grid));
                    }
                }
                p.text(pos2(graph.left() - 8.0, y), Align2::RIGHT_CENTER, label, theme::mono(10.0), pal.text_faint);
            }
            p.hline(graph.x_range(), graph.bottom(), Stroke::new(1.0_f32, with_alpha(pal.text, 0.14)));
            let series = |f: &dyn Fn(&Record) -> f32| {
                let mut s = Series::new(n.max(2));
                for r in &records {
                    s.push(f(r));
                }
                s
            };
            let (cpu, mem, disk, gpu) = (series(&|r| r.cpu), series(&|r| r.mem_pct), series(&|r| r.disk_pct), series(&|r| r.gpu));
            let smooth = self.settings.graphs.smooth_curves;
            let trace = |s: &Series, color: Color32, width: f32, fill: f32| {
                let mut g = Graph::new(vec![GraphLine { series: s, color, range: (0.0, 100.0) }]);
                g.regions = 1;
                g.px_per_update = px;
                g.bloom = 0.0;
                g.markers = false;
                g.smooth = smooth;
                g.line_width = width;
                g.fill = fill;
                draw_graph(&p, graph, &g);
            };
            trace(&mem, fd.metric("memory"), 1.6, 0.0);
            trace(&disk, fd.metric("disk"), 1.4, 0.0);
            trace(&gpu, fd.metric("gpu"), 1.4, 0.0);
            trace(&cpu, fd.metric("cpu"), 1.8, 0.10);
            // Process starts and exits of the flight ring, at the sample of their second.
            if !playing && !self.recorder.events.is_empty() {
                let mut first: HashMap<&str, usize> = HashMap::new();
                for (i, r) in records.iter().enumerate() {
                    first.entry(r.time.as_str()).or_insert(i);
                }
                let mut xs: Vec<f32> = self.recorder.events.iter().filter_map(|e| first.get(e.time.as_str())).map(|i| x_of(*i)).collect();
                xs.dedup_by(|a, b| (*a - *b).abs() < 1.0);
                for x in xs {
                    p.extend(egui::Shape::dashed_line(&[pos2(x, graph.top()), pos2(x, graph.bottom())], Stroke::new(1.0_f32, with_alpha(st.warn, 0.7)), 3.0, 3.0));
                }
            }
            // Hover: a line over the graph, its sample in the readout below.
            let hovered = ui.ctx().pointer_hover_pos().filter(|h| graph.contains(*h)).and_then(|hp| {
                let age = ((graph.right() - hp.x) / px).round() as usize;
                (age < n).then_some((hp.x, n - 1 - age))
            });
            if let Some((x, _)) = hovered {
                p.vline(x, graph.y_range(), Stroke::new(1.0_f32, with_alpha(pal.text, 0.5)));
            }
            let r = records[hovered.map(|h| h.1).unwrap_or(n - 1)];
            let top = r.top.split('|').next().unwrap_or("");
            let mut text = format!("{} \u{b7} CPU {:.0}% \u{b7} RAM {:.0}% \u{b7} Disk {:.0}% \u{b7} GPU {:.0}%", clock_of(&r.time), r.cpu, r.mem_pct, r.disk_pct, r.gpu);
            if !top.is_empty() {
                text.push_str(&format!(" \u{b7} {top}"));
            }
            let w = widgets::text_width(&p, &text, theme::mono(12.0)).min(content.width() - 20.0);
            let chip = Rect::from_min_size(pos2(content.left(), readout_y - 13.0), vec2(w + 16.0, 26.0));
            p.rect_filled(chip, 4.0, pal.raise(0.06));
            p.rect_stroke(chip, 4.0, Stroke::new(1.0_f32, pal.raise(0.08)));
            widgets::text_fit(&p, chip.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, &text, theme::mono(12.0), pal.text, w);
            let hint_x = chip.right() + 14.0;
            if content.right() - hint_x > 300.0 {
                p.text(pos2(hint_x, readout_y), Align2::LEFT_CENTER, "Hover for details \u{b7} click a recording to play it back", theme::ui(12.0), pal.text_faint);
            }
        }
        if playing {
            let r = p.text(pos2(content.right() - 18.0, readout_y), Align2::RIGHT_CENTER, "Close playback", theme::ui(12.0), pal.accent);
            widgets::draw_icon(&p, Rect::from_center_size(pos2(content.right() - 7.0, readout_y), vec2(11.0, 11.0)), Icon::Close, pal.accent);
            close = ui.interact(Rect::from_min_max(r.min, pos2(content.right(), r.max.y)).expand(4.0), ui.id().with("close-playback"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
        }
        close
    }
}

/// "250 ms" or "1 s": the time between two samples.
fn fmt_interval(d: Duration) -> String {
    if d.as_millis() < 1000 {
        format!("{} ms", d.as_millis())
    } else if d.subsec_millis() == 0 {
        format!("{} s", d.as_secs())
    } else {
        format!("{:.1} s", d.as_secs_f32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_interval_text() {
        assert_eq!(fmt_interval(Duration::from_millis(250)), "250 ms");
        assert_eq!(fmt_interval(Duration::from_secs(1)), "1 s");
        assert_eq!(fmt_interval(Duration::from_millis(1500)), "1.5 s");
    }
}
