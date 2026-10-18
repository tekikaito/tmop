//! Benchmarks page: one card per test with the latest and best result, a live graph of
//! the running test, and the result history.

use super::kit::{empty_state, hsplit, titled_card, vsplit, GAP, MARGIN};
use super::table::{self, cmp_f, Cell, Col, TableState};
use super::theme::{self, with_alpha};
use super::widgets::{self, card, draw_graph, led_row, GraphLine, Icon, LedStyle};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::pro::bench::{BenchResult, Test};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense};
use std::path::PathBuf;

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

#[derive(Default)]
pub struct BenchUi {
    pub folder: String,
    pub table: TableState,
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
        let body = self.page_header(ui, &pal, "Benchmarks", Icon::Gauge);
        self.bench.ensure_loaded();
        if self.bench.poll() {
            ui.ctx().request_repaint();
        }
        if self.bench_ui.folder.is_empty() {
            self.bench_ui.folder = match self.settings.general.benchmark_folder.as_str() {
                "" => std::env::temp_dir().display().to_string(),
                f => f.to_string(),
            };
        }
        let running = self.bench.is_running();
        let prog = self.bench.progress();
        let mut run: Option<Vec<Test>> = None;
        let mut cancel = false;
        let mut clear = false;
        super::kit::header_toolbar(ui, body, 760.0, |ui| {
            if running {
                cancel = widgets::button(ui, "Cancel", &pal, Some(Icon::Stop)).clicked();
            } else if widgets::button(ui, "Run all", &pal, Some(Icon::Play)).clicked() {
                run = Some(Test::ALL.to_vec());
            }
            if !running && widgets::button(ui, "Clear history", &pal, None).clicked() {
                clear = true;
            }
            let r = widgets::search_box(ui, &mut self.bench_ui.folder, "Folder for the disk test file", 300.0, &pal);
            if r.changed() {
                self.settings.general.benchmark_folder = self.bench_ui.folder.clone();
                self.settings_dirty = true;
            }
            ui.label(RichText::new("Disk test folder").font(theme::ui(12.5)).color(pal.text_dim));
        });
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[190.0, 330.0, -1.0], GAP);
        let p = ui.painter().clone();

        // Live card.
        let title = match prog.running {
            Some(t) => format!("Running {}", t.title()),
            None => "Benchmark".into(),
        };
        let right = match prog.running {
            Some(t) => prog.live.last().map(|v| format!("{} {}", format_value(t, *v as f64), t.unit())).unwrap_or_default(),
            None => prog.message.clone(),
        };
        let content = titled_card(&p, rows[0], &pal, fd.bloom, &title, &right);
        let split = hsplit(content, &[3.0, 1.2], GAP * 2.0);
        match prog.running {
            Some(t) => {
                let mut s = Series::new(prog.live.len().max(2));
                for v in &prog.live {
                    s.push(*v);
                }
                let c = test_color(fd, t);
                let top = s.max().max(1e-3) * 1.15;
                let mut g = widgets::Graph::new(vec![GraphLine { series: &s, color: c, range: (0.0, top) }]);
                g.px_per_update = (split[0].width() / (t.budget().as_secs_f32() * 10.0)).max(2.0);
                g.bloom = fd.bloom;
                g.smooth = self.settings.graphs.smooth_curves;
                g.grid = Some(pal.grid);
                g.background = Some(pal.graph_bg());
                g.border = Some(c);
                g.fill = 0.12;
                draw_graph(&p, split[0], &g);
                p.text(split[0].left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("{} {}", format_value(t, top as f64), t.unit()), theme::ui(11.0), c);
            }
            None => {
                p.rect_stroke(split[0], 0.0, egui::Stroke::new(1.0_f32, pal.grid));
                empty_state(&p, split[0], &pal, "Run all tests, or a single test from its card. Each test is time-bounded and can be cancelled.");
            }
        }
        let side = split[1];
        let total = Test::ALL.len() as f32;
        let done = Test::ALL.len() - prog.queue.len() - prog.running.map(|_| 1).unwrap_or(0);
        let overall = if running { (done as f32 + prog.fraction) / total } else { 0.0 };
        p.text(side.left_top(), Align2::LEFT_TOP, "Test progress", theme::ui(12.0), pal.text_dim);
        led_row(&p, Rect::from_min_size(side.left_top() + vec2(0.0, 20.0), vec2(side.width(), 12.0)), 24, prog.fraction, &LedStyle { rim: false, rounding: 1.0, ..LedStyle::new(theme::BLUE, fd.bloom) });
        p.text(side.left_top() + vec2(0.0, 44.0), Align2::LEFT_TOP, "Queue", theme::ui(12.0), pal.text_dim);
        led_row(&p, Rect::from_min_size(side.left_top() + vec2(0.0, 64.0), vec2(side.width(), 12.0)), 24, if running { overall } else { 0.0 }, &LedStyle { rim: false, rounding: 1.0, ..LedStyle::new(theme::GREEN, fd.bloom) });
        let next = if prog.queue.is_empty() { if running { "Last test".to_string() } else { String::new() } } else { format!("Next: {}", prog.queue.iter().map(|t| t.title()).collect::<Vec<_>>().join(", ")) };
        let gal = p.layout(next, theme::ui(12.0), pal.text_dim, side.width());
        p.galley(side.left_top() + vec2(0.0, 90.0), gal, pal.text_dim);

        // Test cards: 4 + 3.
        let grid = vsplit(rows[1], &[-1.0, -1.0], GAP);
        let mut cards: Vec<Rect> = hsplit(grid[0], &[1.0; 4], GAP);
        cards.extend(hsplit(grid[1], &[1.0; 4], GAP).into_iter().take(3));
        for (t, r) in Test::ALL.iter().zip(cards) {
            let c = test_color(fd, *t);
            let latest = self.bench.latest(*t).cloned();
            let best = self.bench.best(*t);
            let is_running = prog.running == Some(*t);
            let content = titled_card(&p, r, &pal, fd.bloom, t.short(), t.unit());
            if is_running && fd.bloom > 0.0 {
                p.rect_stroke(r.expand(1.0), 7.0, egui::Stroke::new(2.0_f32, with_alpha(c, 0.5 * fd.bloom)));
            }
            let value = latest.as_ref().map(|l| format_value(*t, l.value)).unwrap_or_else(|| "\u{2014}".into());
            let vr = p.text(content.left_top() + vec2(2.0, 0.0), Align2::LEFT_TOP, &value, theme::display(22.0), c);
            p.text(pos2(vr.right() + 6.0, vr.bottom() - 4.0), Align2::LEFT_BOTTOM, t.unit(), theme::ui(12.0), pal.text_dim);
            let sub = match (&latest, best) {
                (Some(l), Some(b)) => format!("Best {} \u{2022} {}", format_value(*t, b), &l.when[5.min(l.when.len())..]),
                _ => t.description().to_string(),
            };
            let clip = p.with_clip_rect(content);
            clip.text(content.left_top() + vec2(2.0, 34.0), Align2::LEFT_TOP, &sub, theme::ui(11.5), pal.text_dim);
            if let (Some(l), Some(b)) = (&latest, best) {
                let frac = if t.higher_is_better() { l.value / b } else { b / l.value };
                let bar = Rect::from_min_max(pos2(content.left(), content.bottom() - 34.0), pos2(content.right(), content.bottom() - 24.0));
                led_row(&p, bar, 20, frac as f32, &LedStyle { rim: false, rounding: 1.0, ..LedStyle::new(c, fd.bloom) });
            }
            let btn_rect = Rect::from_min_max(pos2(content.right() - 70.0, content.bottom() - 20.0), content.right_bottom());
            let resp = ui.interact(btn_rect, ui.id().with(("bench-run", t.id())), Sense::click());
            let enabled = !running;
            let label_c = if !enabled { pal.text_faint } else if resp.hovered() { pal.text } else { pal.accent };
            let tr = p.text(btn_rect.right_center() - vec2(16.0, 0.0), Align2::RIGHT_CENTER, if is_running { "Running\u{2026}" } else { "Run" }, theme::semibold(12.5), label_c);
            if !is_running {
                widgets::draw_icon(&p, Rect::from_center_size(pos2(tr.right() + 9.0, tr.center().y), vec2(10.0, 10.0)), Icon::Play, label_c);
            }
            p.text(content.left_top() + vec2(2.0, 54.0), Align2::LEFT_TOP, t.title(), theme::ui(11.5), pal.text_faint);
            if enabled && resp.clicked() {
                // Disk read and random tests need the file the write test creates.
                run = Some(match t {
                    Test::DiskRead | Test::DiskRandom => vec![Test::DiskWrite, *t],
                    _ => vec![*t],
                });
            }
        }

        // History table.
        let hist: Vec<BenchResult> = self.bench.history.clone();
        if hist.is_empty() {
            card(&p, rows[2], &pal);
            empty_state(&p, rows[2], &pal, "No results yet");
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
                    0 => Cell::new(&r.when).dim(&pal),
                    1 => Cell::new(r.test.title()).dot(test_color(fd, r.test)),
                    2 => Cell::new(format!("{} {}", format_value(r.test, r.value), r.test.unit())),
                    _ => Cell::new(&r.detail).dim(&pal),
                }
            };
            let icons = &mut self.icons;
            table::show(ui, rows[2], "bench-table", &pal, &cols, &order, &mut self.bench_ui.table, &key, &cell, icons);
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
                self.bench.start(tests, dir);
            } else {
                self.status = format!("{} is not a folder", dir.display());
            }
        }
        if running {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
