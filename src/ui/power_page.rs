//! Power & Freq page: clocks, battery and the active power plan.

use super::hardware::Iso;
use super::kit::{empty_state, hsplit, section_card, stat_tile, vsplit, StatTile};
use super::pro_pages::{fmt_span, list_row, page_layout, swatch_legend, tile_rects, toolbar, RowLook, Swatch};
use super::theme::{self, with_alpha};
use super::widgets::{self, draw_graph, tracked_text, Graph, GraphLine, Icon, ScrollArea029 as _};
use super::{App, FrameData};
use crate::metrics::Series;
use crate::platform::Os;
use crate::pro::power::{Battery, PowerInfo};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense};
use std::time::Duration;

/// Tiles are at least this wide.
const TILE_W: f32 = 190.0;

impl App {
    pub fn power_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let body = self.page_title(ui, &pal, "Power & Freq", &format!("clocks, battery and the active {}", crate::pro::power::scheme_words().0.to_lowercase()));
        let (tools, area) = page_layout(body, 100.0);
        let mut refresh = false;
        toolbar(ui, tools, |ui| refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked());
        self.pro.power.poll();
        if refresh || self.pro.power.stale(Duration::from_secs(10)) {
            self.pro.power.start(crate::pro::power::read);
        }
        let s = fd.snap;
        let fastest = fastest_core(&s.cpu.per_core_mhz);
        let max = s.cpu.max_mhz.unwrap_or(0) as f32;
        let base = s.cpu.base_mhz.or(s.cpu.max_mhz).unwrap_or(0) as f32;
        let cur = fd.smooth.clock;
        let info = self.pro.power.ok().cloned();
        let battery = info.as_ref().and_then(|i| i.battery.clone());
        let ac = info.as_ref().and_then(|i| i.ac_online);

        // Key values.
        let ghz = |mhz: f32| format!("{:.2} GHz", mhz / 1000.0);
        let fastest_mhz = fastest.map(|f| f.1 as f32).unwrap_or(0.0);
        // The battery tile draws its own icon and (coloured) sub line.
        let mut tiles = battery_tiles(battery.as_ref(), ac);
        // No clock tiles or cards where the OS reports no clocks (Apple Silicon).
        let clocks = self.caps.clocks;
        if clocks {
            let mut clock = StatTile::new("Clock", if cur > 0.0 { ghz(cur) } else { "N/A".into() }, "Average of all logical processors");
            if max > 0.0 && cur > 0.0 {
                clock = clock.bar((cur / max).min(1.0), pal.accent);
            }
            tiles.push(clock);
            let mut core = StatTile::new("Fastest core", if fastest.is_some() { ghz(fastest_mhz) } else { "N/A".into() }, fastest_sub(fastest.map(|f| f.0), base as u32));
            if max > 0.0 && fastest_mhz > 0.0 {
                core = core.bar((fastest_mhz / max).min(1.0), st.warn);
            }
            tiles.push(core);
        }
        let (scheme, schemes) = crate::pro::power::scheme_words();
        tiles.push(StatTile::new(
            scheme,
            info.as_ref().map(|i| i.scheme_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "\u{2014}".into()),
            info.as_ref().map(|i| if i.schemes.is_empty() { format!("No {schemes} offered") } else { format!("{} {schemes} installed", i.schemes.len()) }).unwrap_or_else(|| "Reading power policy\u{2026}".into()),
        ));
        let (_, tiles_h) = tile_rects(area, tiles.len(), TILE_W);
        // A window too low for the cards scrolls instead of squeezing them to nothing.
        let content_h = area.height().max(min_content_height(tiles_h, clocks));
        ui.scope_builder(egui::UiBuilder::new().max_rect(area).id_salt("power-scroll"), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_029(ui, [false, true], |ui| {
                let (area, _) = ui.allocate_exact_size(vec2(area.width(), content_h), Sense::hover());
                self.power_body(ui, fd, area, &tiles, battery.as_ref(), &info, base, max);
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn power_body(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>, area: Rect, tiles: &[StatTile], battery: Option<&Battery>, info: &Option<PowerInfo>, base: f32, max: f32) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let clocks = self.caps.clocks;
        let (scheme, schemes) = crate::pro::power::scheme_words();
        let (rects, tiles_h) = tile_rects(area, tiles.len(), TILE_W);
        for (i, (r, t)) in rects.iter().zip(tiles).enumerate() {
            stat_tile(&p, *r, &pal, &t.label, &t.value, &t.sub, t.bar, None);
            if let (0, Some(b)) = (i, battery) {
                battery_icon(&p, Rect::from_min_size(pos2(r.right() - 16.0 - 54.0, r.top() + 14.0), vec2(54.0, 36.0)), pal.text_rail(), st.ok, b.percent as f32 / 100.0);
                let sub = if b.charging && b.rate_mw <= 0 {
                    "Charging".to_string()
                } else if b.charging {
                    format!("Charging {:.1} W", b.rate_mw as f32 / 1000.0)
                } else if b.rate_mw != 0 {
                    format!("Discharging {:.1} W", -(b.rate_mw as f32) / 1000.0)
                } else {
                    "Idle".to_string()
                };
                widgets::text_fit(&p, pos2(r.left() + 16.0, r.top() + 62.0), Align2::LEFT_TOP, &sub, theme::ui(12.0), st.ok, r.width() - 32.0);
            }
        }

        // Clock history and per-core clocks, then policy and plans.
        let rest = Rect::from_min_max(pos2(area.left(), area.top() + tiles_h + 14.0), area.max);
        let low_h = (rest.height() * 0.45).clamp(LOW_MIN_H, 300.0);
        let low_row = if clocks {
            let rows = vsplit(rest, &[-1.0, low_h], 14.0);
            let mid = hsplit(rows[0], &[3.0, 2.0], 14.0);
            self.clock_history(ui, mid[0], fd, base);
            let n = fd.smooth.per_core_mhz.len();
            // Without a reported maximum the bars scale to the fastest core seen.
            let scale = bar_scale(max, &fd.smooth.per_core_mhz);
            // The title and the right-hand text share the card's width: the long form only where it fits.
            let fits = |s: &str| widgets::text_width(&p, "PER-CORE CLOCK", theme::semibold(11.0)) * 1.3 + widgets::text_width(&p, s, theme::mono(12.0)) + 44.0 + 24.0 <= mid[1].width();
            let long = format!("{n} cores \u{b7} max {:.1} GHz", max / 1000.0);
            let short = format!("{n} cores");
            let right = match (n, scale) {
                (0, _) => String::new(),
                (_, Some(_)) if max > 0.0 && fits(&long) => long,
                _ if fits(&short) => short,
                _ => String::new(),
            };
            let content = section_card(&p, mid[1], &pal, "Per-core clock", &right);
            match scale {
                Some(scale) if n > 0 => self.core_bars(ui, content, fd, scale),
                _ => empty_state(&p, content, &pal, "This system does not report per-core clocks"),
            }
            rows[1]
        } else {
            Rect::from_min_size(rest.min, vec2(rest.width(), low_h))
        };

        let low = hsplit(low_row, &[3.0, 2.0], 14.0);
        let policy_title = match crate::platform::OS {
            Os::Linux => "CPU frequency policy",
            Os::MacOs => "Power settings",
            _ => "Processor power policy",
        };
        let content = section_card(&p, low[0], &pal, policy_title, info.as_ref().map(|i| i.scheme_name.as_str()).unwrap_or(""));
        match &info {
            Some(i) if !i.policy.is_empty() => {
                // One value column when the OS does not split by power source.
                let split = i.policy.iter().any(|r| !r.dc.is_empty());
                let x_dc = content.right();
                // The value columns are as wide as their widest text; the setting names get
                // what is left and are cut with an ellipsis, never drawn into a value.
                let text_w = |text: &str, font: egui::FontId| p.layout_no_wrap(text.to_string(), font, pal.text_table()).size().x;
                let col_w = |f: &dyn Fn(&crate::pro::power::PolicyRow) -> &str, head: &str| i.policy.iter().map(|r| text_w(f(r), theme::mono(12.0))).fold(head.len() as f32 * 8.0, f32::max);
                let dc_w = if split { col_w(&|r| r.dc.as_str(), "On battery") } else { 0.0 };
                let ac_w = col_w(&|r| r.ac.as_str(), if split { "Plugged in" } else { "Current" });
                let x_ac = if split { x_dc - dc_w - 24.0 } else { x_dc };
                let name_w = (x_ac - ac_w - 16.0 - content.left()).max(40.0);
                let head = |x: f32, anchor: Align2, text: &str| {
                    tracked_text(&p, pos2(x, content.top() + 10.0), anchor, &text.to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.08);
                };
                head(content.left(), Align2::LEFT_CENTER, "Setting");
                if split {
                    head(x_ac, Align2::RIGHT_CENTER, "Plugged in");
                    head(x_dc, Align2::RIGHT_CENTER, "On battery");
                } else {
                    head(x_ac, Align2::RIGHT_CENTER, "Current");
                }
                for (k, row) in i.policy.iter().enumerate() {
                    let top = content.top() + 24.0 + k as f32 * 30.0;
                    if top + 30.0 > content.bottom() + 1.0 {
                        break;
                    }
                    p.hline(content.x_range(), top, egui::Stroke::new(1.0_f32, pal.row_rule()));
                    let y = top + 15.0;
                    widgets::text_fit(&p, pos2(content.left(), y), Align2::LEFT_CENTER, row.name, theme::ui(13.0), pal.text_table(), name_w);
                    p.text(pos2(x_ac, y), Align2::RIGHT_CENTER, &row.ac, theme::mono(12.0), pal.text_table());
                    if split {
                        p.text(pos2(x_dc, y), Align2::RIGHT_CENTER, &row.dc, theme::mono(12.0), pal.text_table());
                    }
                }
            }
            // The read finished with no policy rows: say so instead of loading for ever.
            Some(_) => empty_state(&p, content, &pal, policy_empty_text()),
            None => empty_state(&p, content, &pal, self.pro.power.error().unwrap_or("Reading power policy\u{2026}")),
        }

        let content = section_card(&p, low[1], &pal, &format!("{scheme}s"), "");
        let list = Rect::from_min_max(content.min, pos2(content.right(), content.bottom() - 20.0));
        if info.as_ref().is_some_and(|i| i.schemes.is_empty()) {
            empty_state(&p, list, &pal, &format!("No {schemes} offered"));
        }
        if let Some(i) = &info {
            for (k, (guid, name, active)) in i.schemes.iter().enumerate() {
                let r = Rect::from_min_size(pos2(list.left(), list.top() + k as f32 * 46.0), vec2(list.width(), 40.0));
                if r.bottom() > list.bottom() + 1.0 {
                    break;
                }
                list_row(&p, r, &pal, if *active { RowLook::Marked(pal.accent) } else { RowLook::Plain }, false);
                widgets::text_fit(&p, pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, if name.is_empty() { guid } else { name }, theme::ui(13.0), pal.text, r.width() - 100.0);
                if *active {
                    tracked_text(&p, pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, "ACTIVE", theme::ui(11.0), pal.accent, 0.1);
                }
            }
        }
        let note = info.as_ref().map(|i| i.note.as_str()).filter(|n| !n.is_empty()).unwrap_or(crate::pro::power::DEFAULT_NOTE);
        widgets::text_fit(&p, pos2(content.left(), content.bottom()), Align2::LEFT_BOTTOM, note, theme::ui(11.0), pal.text_faint, content.width());
    }

    /// The "Clock history" card: average clock and fastest core over the history, the base clock
    /// as a dashed line.
    fn clock_history(&self, ui: &egui::Ui, rect: Rect, fd: &FrameData<'_>, base: f32) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let n = fd.hist.clock.values.len();
        let span = self.settings.update_speed().interval().map(|d| fmt_span((d.as_secs_f32() * n as f32).round() as u64)).filter(|_| n > 1);
        let title = match span {
            Some(s) => format!("Clock history \u{b7} {s}"),
            None => "Clock history".into(),
        };
        let content = section_card(&p, rect, &pal, &title, &format!("{:.2} GHz", fd.smooth.clock / 1000.0));
        let graph = Rect::from_min_max(pos2(content.left() + 38.0, content.top() + 4.0), pos2(content.right(), content.bottom() - 28.0));
        if n < 2 || graph.height() < 20.0 {
            empty_state(&p, content, &pal, "Collecting samples\u{2026}");
            return;
        }
        let fastest = Some(&fd.hist.clock_max).filter(|s| s.max() > 0.0);
        let hi = fd.hist.clock.max().max(fastest.map_or(0.0, |s| s.max())).max(base).max(1000.0);
        let step = if hi > 7000.0 { 2000.0 } else { 1000.0 };
        let top = widgets::eased(ui.ctx(), "power-top", (hi / step).ceil() * step);
        let lines = (top / step).round().max(1.0) as usize;
        for k in 0..=lines {
            let y = graph.bottom() - graph.height() * (k as f32 * step / top).min(1.0);
            if k > 0 {
                p.hline(graph.x_range(), y, egui::Stroke::new(1.0_f32, pal.grid));
            }
            p.text(pos2(graph.left() - 8.0, y), Align2::RIGHT_CENTER, if k == 0 { "0".to_string() } else { format!("{:.1}", k as f32 * step / 1000.0) }, theme::mono(10.0), pal.text_faint);
        }
        // The scale of the full history, so the trace grows in from the right while it fills.
        let px = graph.width() / (fd.hist.clock.capacity.max(n).max(2) - 1) as f32;
        let smooth = self.settings.graphs.smooth_curves;
        let trace = |series: &Series, color: Color32, width: f32, fill: f32| {
            let mut g = Graph::new(vec![GraphLine { series, color, range: (0.0, top) }]);
            g.regions = 1;
            g.px_per_update = px;
            g.bloom = 0.0;
            g.markers = false;
            g.smooth = smooth;
            g.line_width = width;
            g.fill = fill;
            (g.phase, g.glide) = (fd.phase, fd.glide);
            draw_graph(&p, graph, &g);
        };
        if base > 0.0 {
            widgets::baseline(&p, graph, base / top, pal.text_faint);
        }
        trace(&fd.hist.clock, pal.accent, 1.8, 0.12);
        if let Some(f) = fastest {
            trace(f, st.warn, 1.2, 0.0);
        }
        let mut legend = vec![("Average", Swatch::Line(pal.accent)), ("Fastest core", Swatch::Line(st.warn))];
        let base_label = format!("Base {:.1} GHz", base / 1000.0);
        if base > 0.0 {
            legend.push((base_label.as_str(), Swatch::Dashed(pal.text_faint)));
        }
        swatch_legend(&p, &pal, pos2(graph.left(), content.bottom() - 8.0), &legend);
    }

    /// One bar per logical processor (the fastest in orange) with a sparse axis below.
    fn core_bars(&self, ui: &mut egui::Ui, content: Rect, fd: &FrameData<'_>, max: f32) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        let clocks = &fd.smooth.per_core_mhz;
        let n = clocks.len();
        let region = Rect::from_min_max(pos2(content.left(), content.top() + 4.0), pos2(content.right(), (content.bottom() - 24.0).min(content.top() + 4.0 + 150.0)));
        let gap = if (region.width() / n as f32) < 8.0 { 1.0 } else { 4.0 };
        let w = ((region.width() - gap * (n - 1) as f32) / n as f32).max(1.0);
        let hot = clocks.iter().enumerate().fold(0, |best, (i, m)| if *m > clocks[best] { i } else { best });
        for (i, mhz) in clocks.iter().enumerate() {
            let x = region.left() + i as f32 * (w + gap);
            let col = Rect::from_min_size(pos2(x, region.top()), vec2(w, region.height()));
            let h = (mhz / (max * 1.25)).clamp(0.0, 1.0) * region.height();
            let bar = Rect::from_min_max(pos2(x, region.bottom() - h), pos2(x + w, region.bottom()));
            p.rect_filled(bar, 1.0, if i == hot { with_alpha(st.warn, 0.9) } else { pal.accent });
            ui.interact(col, ui.scope_id().with(("core-bar", i)), Sense::hover()).on_hover_text(format!("Core {i}: {:.2} GHz", mhz / 1000.0));
        }
        let label = |i: usize| {
            let x = region.left() + i as f32 * (w + gap) + w / 2.0;
            (x, if i == 0 || i == n - 1 { format!("{i} \u{b7} {:.2}", clocks[i] / 1000.0) } else { i.to_string() })
        };
        let y = region.bottom() + 12.0;
        for i in (4..n.saturating_sub(1)).step_by(4) {
            let (x, text) = label(i);
            if x - region.left() > 60.0 && region.right() - x > 60.0 {
                p.text(pos2(x, y), Align2::CENTER_CENTER, text, theme::mono(10.0), pal.text_faint);
            }
        }
        p.text(pos2(region.left(), y), Align2::LEFT_CENTER, label(0).1, theme::mono(10.0), pal.text_faint);
        if n > 1 {
            p.text(pos2(region.right(), y), Align2::RIGHT_CENTER, label(n - 1).1, theme::mono(10.0), pal.text_faint);
        }
    }
}

/// Least height of the clock history / per-core row and of the policy / plans row (the policy
/// table's header and six rows); below that the page scrolls.
const MID_MIN_H: f32 = 170.0;
const LOW_MIN_H: f32 = 262.0;

/// The page height that fits all cards: the tiles, then the clock row (where the OS has clocks)
/// and the policy row, with the 14 px gaps.
fn min_content_height(tiles_h: f32, clocks: bool) -> f32 {
    tiles_h + 14.0 + if clocks { MID_MIN_H + 14.0 } else { 0.0 } + LOW_MIN_H
}

/// The "Fastest core" tile's sub line. A base clock of 0 is "not reported", never "0.00 GHz".
fn fastest_sub(core: Option<usize>, base_mhz: u32) -> String {
    let mut parts = Vec::new();
    if base_mhz > 0 {
        parts.push(format!("Base {:.2} GHz", base_mhz as f32 / 1000.0));
    }
    if let Some(c) = core {
        parts.push(format!("core {c}"));
    }
    if parts.is_empty() {
        "Base clock not reported".into()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// The MHz the per-core bars are scaled to: the reported maximum, else the fastest core seen;
/// `None` when no core has a clock.
fn bar_scale(max_mhz: f32, clocks: &[f32]) -> Option<f32> {
    if max_mhz > 0.0 {
        return Some(max_mhz);
    }
    let seen = clocks.iter().copied().fold(0.0, f32::max);
    (seen > 0.0).then_some(seen)
}

/// What the policy card says when the read finished without any policy rows.
fn policy_empty_text() -> &'static str {
    match crate::platform::OS {
        Os::Linux => "Not reported by this system (no cpufreq driver)",
        _ => "Not reported by this system",
    }
}

/// The logical processor with the highest clock (the first of equals) and its MHz.
fn fastest_core(clocks: &[u32]) -> Option<(usize, u32)> {
    clocks.iter().enumerate().fold(None, |best, (i, &m)| match best {
        Some((_, b)) if b >= m => best,
        _ => Some((i, m)),
    })
}

/// The battery tile's drawing, like [`super::hardware::Hardware::Battery`] but with the cell filled
/// to `frac` in `fill`.
fn battery_icon(p: &egui::Painter, rect: Rect, color: Color32, fill: Color32, frac: f32) {
    let (vw, vh) = (144.0, 96.0);
    let s = (rect.width() / vw).min(rect.height() / vh);
    let origin = rect.center() - vec2(vw, vh) * s / 2.0 + vec2(72.0, 14.0) * s;
    let g = Iso { p, origin, kx: 1.04, ky: 0.6, scale: s, width: 1.2, color };
    g.rrect(8.0, 22.0, 46.0, 22.0, 2.0, 1.0, 0.0);
    g.rect(54.0, 29.0, 4.0, 8.0, 0.7, 0.0);
    let w = 30.0 * frac.clamp(0.0, 1.0);
    if w >= 1.0 {
        Iso { color: fill, ..g }.rect(12.0, 26.0, w, 14.0, 1.0, 0.35);
    }
}

/// The battery tiles in front of the clock tiles: "Battery" (first, the page draws its icon),
/// "Power source" and, where the health is known, "Battery health".
fn battery_tiles(battery: Option<&Battery>, ac: Option<bool>) -> Vec<StatTile> {
    let Some(b) = battery else {
        return vec![StatTile::new("Power source", if ac == Some(false) { "Battery" } else { "AC" }, "No battery reported")];
    };
    let source = match ac {
        Some(true) => "Plugged in",
        Some(false) => "On battery",
        None => "\u{2014}",
    };
    let mut tiles = vec![StatTile::new("Battery", format!("{}%", b.percent), ""), StatTile::new("Power source", source, battery_time(b))];
    if let Some((value, sub)) = health_tile(b) {
        tiles.push(StatTile::new("Battery health", value, sub));
    }
    tiles
}

/// The "Battery health" tile's value and sub line, `None` (no tile) when the design capacity
/// (or the driver's relative health) is unknown. Over 100% is shown as it is.
fn health_tile(b: &Battery) -> Option<(String, String)> {
    let percent = b.health()?;
    let capacities = b.design_mwh.map(|d| format!("{:.1} of {:.1} Wh design", b.full_mwh as f64 / 1000.0, d as f64 / 1000.0));
    let cycles = b.cycles.map(|c| if c == 1 { "1 cycle".to_string() } else { format!("{c} cycles") });
    let sub: Vec<String> = capacities.into_iter().chain(cycles).collect();
    Some((format!("{percent}%"), sub.join(" \u{b7} ")))
}

/// What the "Power source" tile says under "Plugged in" / "On battery".
fn battery_time(b: &Battery) -> String {
    if b.charging {
        let minutes = b.seconds_left.map(|s| s / 60).or_else(|| (b.rate_mw > 0 && b.full_mwh > b.remaining_mwh).then(|| ((b.full_mwh - b.remaining_mwh) as u64 * 60 / b.rate_mw as u64) as u32));
        match minutes {
            Some(m) => format!("full in {m} min"),
            None => "Charging".into(),
        }
    } else if let Some(s) = b.seconds_left {
        format!("{} h {} min left", s / 3600, s / 60 % 60)
    } else if b.percent >= 100 {
        "Fully charged".into()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_icon_draws_empty_half_and_full() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                for frac in [0.0, 0.5, 1.0] {
                    battery_icon(ui.painter(), Rect::from_min_size(pos2(10.0, 10.0), vec2(54.0, 36.0)), pal.text_rail(), pal.ok, frac);
                }
            });
        }).drop_without_applying_deltas();
    }

    #[test]
    fn unreported_values_are_not_zero() {
        assert_eq!(fastest_sub(Some(3), 0), "core 3");
        assert_eq!(fastest_sub(Some(2), 2600), "Base 2.60 GHz \u{b7} core 2");
        assert_eq!(fastest_sub(None, 0), "Base clock not reported");
        assert_eq!(bar_scale(0.0, &[0.0, 0.0]), None);
        assert_eq!(bar_scale(0.0, &[2100.0, 2400.0]), Some(2400.0));
        assert_eq!(bar_scale(3000.0, &[2100.0]), Some(3000.0));
        assert!(min_content_height(205.0, true) > min_content_height(205.0, false));
    }

    #[test]
    fn fastest_core_is_the_first_of_equals() {
        assert_eq!(fastest_core(&[3000, 4200, 4200, 3900]), Some((1, 4200)));
        assert_eq!(fastest_core(&[]), None);
    }

    #[test]
    fn battery_text() {
        let b = Battery { percent: 82, charging: true, rate_mw: 41_200, remaining_mwh: 40_000, full_mwh: 50_000, seconds_left: None, ..Default::default() };
        assert_eq!(battery_time(&b), "full in 14 min");
        assert_eq!(battery_time(&Battery { seconds_left: Some(1500), ..b.clone() }), "full in 25 min");
        assert_eq!(battery_time(&Battery { charging: false, seconds_left: Some(5400), ..b.clone() }), "1 h 30 min left");
        assert_eq!(battery_time(&Battery { charging: false, percent: 100, ..b }), "Fully charged");
    }

    fn labels(tiles: &[StatTile]) -> Vec<&str> {
        tiles.iter().map(|t| t.label.as_str()).collect()
    }

    #[test]
    fn health_tile_only_with_a_design_capacity() {
        let worn = Battery { percent: 64, full_mwh: 41_200, design_mwh: Some(47_500), cycles: Some(312), ..Default::default() };
        let tiles = battery_tiles(Some(&worn), Some(false));
        assert_eq!(labels(&tiles), ["Battery", "Power source", "Battery health"]);
        assert_eq!((tiles[2].value.as_str(), tiles[2].sub.as_str()), ("87%", "41.2 of 47.5 Wh design \u{b7} 312 cycles"));
        // No cycle count: the cycle part is left out, no "0 cycles".
        assert_eq!(health_tile(&Battery { cycles: None, ..worn.clone() }), Some(("87%".into(), "41.2 of 47.5 Wh design".into())));
        assert_eq!(health_tile(&Battery { cycles: Some(1), ..worn.clone() }).unwrap().1, "41.2 of 47.5 Wh design \u{b7} 1 cycle");
        // A new cell above its design is shown as it is.
        assert_eq!(health_tile(&Battery { full_mwh: 52_000, design_mwh: Some(50_000), ..worn.clone() }).unwrap().0, "104%");
        // Unknown design (or no full charge to compare): no tile at all.
        let unknown = Battery { design_mwh: None, ..worn.clone() };
        assert_eq!(health_tile(&unknown), None);
        assert_eq!(labels(&battery_tiles(Some(&unknown), Some(true))), ["Battery", "Power source"]);
        assert_eq!(health_tile(&Battery { full_mwh: 0, ..worn.clone() }), None);
        assert_eq!(health_tile(&Battery { design_mwh: Some(0), ..worn.clone() }), None);
        // Relative capacities (Windows): the percent only, plus the cycles where known.
        let relative = Battery { full_mwh: 0, design_mwh: None, relative_health: Some(93), cycles: None, ..worn };
        assert_eq!(health_tile(&relative), Some(("93%".into(), String::new())));
        // No battery: only the power source.
        assert_eq!(labels(&battery_tiles(None, Some(true))), ["Power source"]);
    }
}
