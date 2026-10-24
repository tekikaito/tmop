//! Power & Freq page: clocks, battery and the active power plan.

use super::kit::{empty_state, header_toolbar, hsplit, stat_row, titled_card, vsplit, GAP, MARGIN};
use super::theme::{self, with_alpha};
use super::widgets::{self, draw_graph, led_column, GraphLine, Icon, LedStyle};
use super::{App, FrameData};
use eframe::egui::{self, pos2, Align2, Rect, Stroke};
use std::time::Duration;

impl App {
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
}
