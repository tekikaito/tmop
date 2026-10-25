//! Benchmarks page: a run card with the live progress, one card per test with its latest
//! result and trend, and the result history.

use super::kit::{empty_state, hsplit};
use super::table::{self, cmp_f, Cell, Col, TableState};
use super::theme;
use super::widgets::{self, card, tracked_text, Button, Icon};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::pro::bench::{BenchResult, Progress, Test};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn test_color(fd: &FrameData<'_>, t: Test) -> Color32 {
    match t.group() {
        "cpu" => fd.metric("cpu"),
        "memory" => fd.metric("memory"),
        _ => fd.metric("disk"),
    }
}

pub fn format_value(t: Test, v: f64) -> String {
    match t {
        Test::MemLatency => format!("{v:.1}"),
        Test::MemBandwidth => format!("{v:.1}"),
        Test::DiskRandom => format!("{v:.0}"),
        _ => format!("{v:.0}"),
    }
}

/// Height of the run card and of a test card.
const RUN_H: f32 = 160.0;
const TEST_H: f32 = 124.0;
/// Narrowest test card (the design's `minmax(200px, 1fr)`) and the gap between cards.
const TEST_MIN_W: f32 = 200.0;
const CARD_GAP: f32 = 12.0;

#[derive(Default)]
pub struct BenchUi {
    pub folder: String,
    pub table: TableState,
    /// Tests in the run in progress (0 when idle); grows if the queue says it is longer.
    pub run_total: usize,
    /// When the run in progress was first seen.
    pub run_start: Option<Instant>,
}

/// `mm:ss` of a duration.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// A single-line field in the page's mono input style (36 px).
fn mono_input(ui: &mut egui::Ui, text: &mut String, width: f32, hint: &str, pal: &theme::Palette) -> egui::Response {
    egui::Frame::none()
        .fill(pal.control)
        .stroke(egui::Stroke::new(1.0_f32, pal.control_stroke))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(12.0, 9.0))
        .show(ui, |ui| ui.add(egui::TextEdit::singleline(text).hint_text(RichText::new(hint).color(pal.text_faint)).desired_width(width - 26.0).frame(false).font(theme::mono(12.0))))
        .inner
}

impl App {
    fn bench_dir(&self) -> PathBuf {
        let s = self.bench_ui.folder.trim();
        if s.is_empty() {
            std::env::temp_dir()
        } else {
            PathBuf::from(s)
        }
    }

    pub fn benchmarks_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.bench.ensure_loaded();
        if self.bench.poll() {
            ui.ctx().request_repaint();
        }
        let running = self.bench.is_running();
        let prog = self.bench.progress();
        if running {
            let in_run = prog.queue.len() + usize::from(prog.running.is_some());
            if self.bench_ui.run_total == 0 {
                self.bench_ui.run_total = Test::ALL.len();
            }
            self.bench_ui.run_total = self.bench_ui.run_total.max(in_run);
            self.bench_ui.run_start.get_or_insert_with(Instant::now);
        } else {
            self.bench_ui.run_total = 0;
            self.bench_ui.run_start = None;
        }
        let total = self.bench_ui.run_total.max(1);
        let index = total.saturating_sub(prog.queue.len()).max(1);
        let subtitle = if running { format!("running {index} of {total} \u{b7} Ctrl+9") } else { format!("{} results \u{b7} Ctrl+9", self.bench.history.len()) };
        let body = self.page_title(ui, &pal, "Benchmarks", &subtitle);
        if self.bench_ui.folder.is_empty() {
            self.bench_ui.folder = match self.settings.general.benchmark_folder.as_str() {
                "" => std::env::temp_dir().display().to_string(),
                f => f.to_string(),
            };
        }
        let mut run: Option<Vec<Test>> = None;
        let mut cancel = false;
        let mut clear = false;
        super::kit::header_toolbar(ui, body, 700.0, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if running {
                cancel = Button::danger("Cancel").show(ui, &pal).clicked();
            } else if Button::primary("Run all").show(ui, &pal).clicked() {
                run = Some(Test::ALL.to_vec());
            }
            ui.add_enabled_ui(!running, |ui| clear = Button::ghost("Clear history").show(ui, &pal).clicked());
            if mono_input(ui, &mut self.bench_ui.folder, 260.0, "Folder for the disk test file", &pal).changed() {
                self.settings.general.benchmark_folder = self.bench_ui.folder.clone();
                self.settings_dirty = true;
            }
            ui.label(RichText::new("Disk test folder").font(theme::ui(12.0)).color(pal.text_faint));
        });
        let area = Rect::from_min_max(pos2(body.left() + 12.0, body.top()), pos2(body.right() - 16.0, body.bottom()));
        let p = ui.painter().clone();

        // Run card.
        let run_rect = Rect::from_min_size(area.min, vec2(area.width(), RUN_H));
        self.run_card(&p, run_rect, fd, &prog, (index, total));

        // Test cards: as many columns as fit.
        let cols = (((area.width() + CARD_GAP) / (TEST_MIN_W + CARD_GAP)).floor() as usize).clamp(1, Test::ALL.len());
        let mut y = run_rect.bottom() + 16.0;
        for chunk in Test::ALL.chunks(cols) {
            let row = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), TEST_H));
            let mut cells = hsplit(row, &vec![1.0; cols], CARD_GAP);
            cells.truncate(chunk.len());
            for (t, r) in chunk.iter().zip(cells) {
                if let Some(start) = self.test_card(ui, r, fd, &prog, *t, running) {
                    run = Some(start);
                }
            }
            y += TEST_H + CARD_GAP;
        }

        // History table.
        let table_rect = Rect::from_min_max(pos2(area.left(), y + 4.0), pos2(area.right(), area.bottom().max(y + 164.0)));
        let hist: Vec<BenchResult> = self.bench.history.clone();
        if hist.is_empty() {
            card(&p, table_rect, &pal);
            empty_state(&p, table_rect, &pal, "No results yet");
        } else {
            let st = &self.bench_ui.table;
            let mut order: Vec<usize> = (0..hist.len()).collect();
            st.sort(&hist, &mut order, |a, b, col| match col {
                0 => a.when.cmp(&b.when),
                1 => a.test.title().cmp(b.test.title()),
                2 => cmp_f(a.value, b.value),
                _ => a.detail.cmp(&b.detail),
            });
            let cols = [Col::text("Finished", 170.0), Col::text("Test", 220.0), Col::num("Result", 140.0), Col::text("Details", 400.0)];
            let key = |i: usize| format!("{}{}", hist[i].when, hist[i].test.id());
            let cell = |i: usize, col: usize| -> Cell {
                let r = &hist[i];
                match col {
                    0 => Cell::new(&r.when).mono().color(pal.text_faint),
                    1 => Cell::new(r.test.title()).dot(test_color(fd, r.test)),
                    2 => Cell::new(format!("{} {}", format_value(r.test, r.value), r.test.unit())),
                    _ => Cell::new(&r.detail).dim(&pal),
                }
            };
            let icons = &mut self.icons;
            table::show(ui, table_rect, "bench-table", &pal, &cols, &order, &mut self.bench_ui.table, &key, &cell, icons);
        }
        if cancel {
            self.bench.cancel();
        }
        if clear {
            self.bench.clear_history();
        }
        if let Some(tests) = run {
            let dir = self.bench_dir();
            if dir.is_dir() {
                self.bench_ui.run_total = tests.len();
                self.bench_ui.run_start = Some(Instant::now());
                self.bench.start(tests, dir);
            } else {
                self.status = format!("{} is not a folder", dir.display());
            }
        }
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    /// The run card: overall progress ring, the test in progress with its progress and queue, and
    /// the timings. Idle, it holds the hint (and the outcome of the last run).
    fn run_card(&self, p: &egui::Painter, rect: Rect, fd: &FrameData<'_>, prog: &Progress, (index, total): (usize, usize)) {
        let pal = fd.pal;
        card(p, rect, &pal);
        let Some(t) = prog.running else {
            empty_state(p, rect, &pal, "Run all tests, or a single test from its card. Each test is time-bounded and can be cancelled.");
            if !prog.message.is_empty() {
                p.text(pos2(rect.center().x, rect.center().y + 24.0), Align2::CENTER_CENTER, &prog.message, theme::mono(12.0), pal.text_faint);
            }
            return;
        };
        let inner = rect.shrink2(vec2(22.0, 18.0));
        // Ring: progress through the whole run.
        let overall = ((index - 1) as f32 + prog.fraction) / total as f32;
        let center = pos2(inner.left() + 60.0, rect.center().y);
        widgets::ring(p, center, 52.0, 6.0, overall, pal.accent, pal.raise(0.07));
        let pct = format!("{:.0}", overall.clamp(0.0, 1.0) * 100.0);
        let r = p.text(center + vec2(-6.0, -6.0), Align2::CENTER_CENTER, &pct, theme::light(28.0), pal.text);
        p.text(pos2(r.right() + 1.0, r.bottom() - 6.0), Align2::LEFT_BOTTOM, "%", theme::ui(14.0), pal.text_faint);
        tracked_text(p, center + vec2(0.0, 18.0), Align2::CENTER_CENTER, &t.group().to_uppercase(), theme::ui(10.0), pal.text_faint, 0.1);

        // Timings at the right when there is room.
        let wide = inner.width() > 640.0;
        let text_right = if wide { inner.right() - 200.0 } else { inner.right() };
        if wide {
            let elapsed = self.bench_ui.run_start.map(|s| s.elapsed()).unwrap_or_default();
            let current_left = t.budget().mul_f32((1.0 - prog.fraction).clamp(0.0, 1.0));
            let remaining = prog.queue.iter().fold(current_left, |acc, q| acc + q.budget());
            let mut items = vec![("Elapsed", clock(elapsed)), ("Remaining", format!("~{}", clock(remaining)))];
            if let Some(c) = fd.snap.thermal.max_c() {
                items.push(("CPU temp", self.temp(c)));
            }
            let mut y = inner.top() + 6.0;
            for (k, v) in items {
                p.text(pos2(text_right + 24.0, y + 9.0), Align2::LEFT_CENTER, k, theme::ui(13.0), pal.text_faint);
                p.text(pos2(inner.right(), y + 9.0), Align2::RIGHT_CENTER, v, theme::mono(13.0), pal.text);
                y += 26.0;
            }
        }
        let left = center.x + 52.0 + 24.0;
        let col = Rect::from_min_max(pos2(left, inner.top()), pos2((text_right - 8.0).max(left + 120.0), inner.bottom()));
        let clip = p.with_clip_rect(col.expand2(vec2(0.0, 4.0)));
        let y0 = rect.center().y - 54.0;
        let title = clip.text(pos2(col.left(), y0), Align2::LEFT_TOP, t.title(), theme::light(22.0), pal.text);
        clip.text(pos2(title.right() + 10.0, y0 + 8.0), Align2::LEFT_TOP, format!("test {index} of {total}"), theme::mono(14.0), pal.text_faint);
        let live = prog.live.last().map(|v| format!(" \u{b7} {} {} so far", format_value(t, *v as f64), t.unit())).unwrap_or_default();
        let what = if prog.message.is_empty() { t.description() } else { prog.message.as_str() };
        widgets::text_fit(&clip, pos2(col.left(), y0 + 34.0), Align2::LEFT_TOP, &format!("{what}{live}"), theme::ui(13.0), pal.text_dim, col.width());
        tracked_text(&clip, pos2(col.left(), y0 + 58.0), Align2::LEFT_TOP, "TEST PROGRESS", theme::semibold(11.0), pal.text_faint, 0.12);
        widgets::bar(&clip, Rect::from_min_size(pos2(col.left(), y0 + 76.0), vec2(col.width(), 4.0)), prog.fraction, pal.accent);
        tracked_text(&clip, pos2(col.left(), y0 + 88.0), Align2::LEFT_TOP, "QUEUE", theme::semibold(11.0), pal.text_faint, 0.12);
        let next = if prog.queue.is_empty() { "Last test".to_string() } else { format!("Next: {}", prog.queue.iter().map(|q| q.title()).collect::<Vec<_>>().join(", ")) };
        widgets::text_fit(&clip, pos2(col.left(), y0 + 104.0), Align2::LEFT_TOP, &next, theme::ui(13.0), pal.text_table(), col.width());
    }

    /// One test's card. Returns the tests to start when its "Run" link was clicked.
    fn test_card(&self, ui: &mut egui::Ui, r: Rect, fd: &FrameData<'_>, prog: &Progress, t: Test, running: bool) -> Option<Vec<Test>> {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let c = test_color(fd, t);
        let p = ui.painter().clone();
        let is_running = prog.running == Some(t);
        let queued = prog.queue.contains(&t);
        if is_running {
            widgets::accent_card(&p, r, &pal, pal.accent, fd.bloom);
        } else {
            p.rect_filled(r, 8.0, pal.card);
            p.rect_stroke(r, 8.0, egui::Stroke::new(1.0_f32, pal.card_stroke));
        }
        let hist: Vec<&BenchResult> = self.bench.history.iter().filter(|h| h.test == t).collect();
        let latest = hist.last().copied();
        let clip = p.with_clip_rect(r.shrink(1.0));
        let (x, w) = (r.left() + 16.0, r.width() - 32.0);

        // Label and state.
        tracked_text(&clip, pos2(x, r.top() + 14.0), Align2::LEFT_TOP, &t.short().to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.14);
        let (state, state_color) = if is_running {
            ("running", pal.accent)
        } else if queued {
            ("queued", pal.text_faint)
        } else if latest.is_some() {
            ("done", st.ok)
        } else {
            ("", pal.text_faint)
        };
        clip.text(pos2(r.right() - 16.0, r.top() + 14.0), Align2::RIGHT_TOP, state, theme::ui(11.0), state_color);

        // Value: the live reading while running, else the latest result.
        let live = if is_running { prog.live.last().map(|v| format_value(t, *v as f64)) } else { None };
        let value = live.or_else(|| latest.map(|l| format_value(t, l.value)));
        let vy = r.top() + 32.0;
        match &value {
            Some(v) => {
                let vr = clip.text(pos2(x, vy), Align2::LEFT_TOP, v, theme::light(24.0), pal.text);
                clip.text(pos2(vr.right() + 5.0, vr.bottom() - 5.0), Align2::LEFT_BOTTOM, t.unit(), theme::ui(12.0), pal.text_faint);
            }
            None => {
                clip.text(pos2(x, vy), Align2::LEFT_TOP, "\u{2014}", theme::light(24.0), pal.text_faint);
            }
        }

        // Sub line: the change since the previous run, else what the test does.
        let sub = match hist.as_slice() {
            [.., prev, last] if prev.value.abs() > f64::EPSILON => {
                let pct = (last.value - prev.value) / prev.value * 100.0;
                let sign = if pct < 0.0 { "\u{2212}" } else { "+" };
                format!("last run {} \u{b7} {sign}{:.1}%", format_value(t, prev.value), pct.abs())
            }
            _ => t.description().to_string(),
        };
        widgets::text_fit(&clip, pos2(x, r.top() + 66.0), Align2::LEFT_TOP, &sub, theme::ui(12.0), pal.text_faint, w);

        // Trend: the live samples while running, else the history, oldest to newest.
        let values: Vec<f32> = if is_running { prog.live.clone() } else { hist.iter().map(|h| h.value as f32).collect() };
        if values.len() >= 2 {
            let mut s = Series::new(values.len());
            for v in &values {
                s.push(*v);
            }
            let (lo, hi) = values.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            let pad = ((hi - lo) * 0.15).max(hi.abs() * 0.02).max(1e-3);
            widgets::sparkline_static(&clip, Rect::from_min_size(pos2(x, r.bottom() - 14.0 - 24.0), vec2(w, 24.0)), &[(&s, c, (lo - pad, hi + pad))], 0.0);
        }

        // Run link next to the value.
        let btn = Rect::from_min_max(pos2(r.right() - 16.0 - 70.0, vy), pos2(r.right() - 16.0, vy + 28.0));
        let resp = ui.interact(btn, ui.id().with(("bench-run", t.id())), Sense::click());
        let label_c = if running { pal.text_faint } else if resp.hovered() { pal.text } else { pal.accent };
        if is_running {
            clip.text(btn.right_center(), Align2::RIGHT_CENTER, "Running\u{2026}", theme::semibold(12.5), label_c);
        } else {
            let tr = clip.text(btn.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, "Run", theme::semibold(12.5), label_c);
            widgets::draw_icon(&clip, Rect::from_center_size(pos2(tr.right() + 8.0, tr.center().y), vec2(9.0, 9.0)), Icon::Play, label_c);
        }
        let resp = match self.bench.best(t) {
            Some(b) if !running => resp.on_hover_text(format!("Best {} {}", format_value(t, b), t.unit())),
            _ => resp,
        };
        if !running && resp.clicked() {
            // Disk read and random tests need the file the write test creates.
            return Some(match t {
                Test::DiskRead | Test::DiskRandom => vec![Test::DiskWrite, t],
                _ => vec![t],
            });
        }
        None
    }
}
