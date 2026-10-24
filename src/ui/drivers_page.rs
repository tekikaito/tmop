//! Drivers page, and the shared body of the Drivers and Services pages.

mod service_info;

pub use service_info::InfoCache;

use super::kit::{self, empty_state, filter_box, header_toolbar, GAP};
use super::table::{self, cmp_str, Cell, Col, TableState};
use super::theme::{self};
use super::widgets::{self, Button, ButtonSize};
use super::{App, FrameData};
use crate::pro::drivers::Service;
use crate::process::{format_bytes, format_rate};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, Sense};
use std::path::{Path, PathBuf};

/// Gap between the stat tiles and what is below them.
const BLOCK_GAP: f32 = 16.0;
/// Page width from which the service panel sits right of the table.
const ASIDE_MIN_PAGE_W: f32 = 1100.0;
/// Height of the selected-driver bar.
const DRIVER_BAR_H: f32 = 62.0;

/// The rect of a page body: inset to line up with the title (left) and the toolbar (right).
pub(super) fn page_area(body: Rect) -> Rect {
    Rect::from_min_max(pos2(body.left() + 12.0, body.top()), pos2(body.right() - 16.0, body.bottom()))
}

impl App {
    pub fn drivers_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.pro.drivers.poll();
        let subtitle = match self.pro.drivers.ok() {
            Some(l) => format!("kernel drivers from the Service Control Manager \u{b7} {} entries", l.len()),
            None => "kernel drivers from the Service Control Manager".to_string(),
        };
        let body = self.page_title(ui, &pal, "Drivers", &subtitle);
        let mut refresh = false;
        header_toolbar(ui, body, 340.0, |ui| {
            refresh = Button::new("Refresh").show(ui, &pal).clicked();
            filter_box(ui, &mut self.pro.drivers_table.filter, "Filter by name, company or path", 240.0, &pal);
        });
        if refresh || (self.pro.drivers.get().is_none() && !self.pro.drivers.is_loading()) {
            self.pro.drivers.start(|| crate::pro::drivers::read(crate::pro::drivers::Kind::Drivers));
        }
        let list: &[Service] = self.pro.drivers.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let mut info = InfoCache::default();
        driver_like_page(ui, page_area(body), fd, list, &mut self.pro.drivers_table, self.pro.drivers.error(), true, &mut self.icons, &mut info);
    }
}

/// True for a company other than Microsoft.
fn third_party(company: &str) -> bool {
    !company.is_empty() && !company.contains("Microsoft")
}

/// Shared body of the Drivers and Services pages: stat tiles, the table and, for drivers, the bar
/// of the selected driver or, for services, the panel that explains the selected service. Returns
/// the process to open when the panel's process link was clicked.
#[allow(clippy::too_many_arguments)]
pub fn driver_like_page(
    ui: &mut egui::Ui,
    area: Rect,
    fd: &FrameData<'_>,
    list: &[Service],
    state: &mut TableState,
    error: Option<&str>,
    drivers: bool,
    icons: &mut super::icons::IconCache,
    info: &mut InfoCache,
) -> Option<(u32, i64)> {
    let pal = &fd.pal;
    let st = theme::status(pal);
    let p = ui.painter().clone();
    let running = list.iter().filter(|s| s.running).count();
    let auto = list.iter().filter(|s| matches!(s.start, "Boot" | "System" | "Automatic")).count();
    let disabled = list.iter().filter(|s| s.start == "Disabled").count();
    let n = list.len().max(1) as f32;
    let what = if drivers { "drivers" } else { "services" };
    let mut running_tile = kit::StatTile::new("Running", running.to_string(), format!("of {} {what}", list.len())).bar(running as f32 / n, st.ok);
    if drivers {
        running_tile = running_tile.icon(super::hardware::Hardware::Cpu);
    }
    let mut tiles = vec![
        running_tile,
        kit::StatTile::new("Stopped", (list.len() - running).to_string(), format!("{disabled} disabled")).bar((list.len() - running) as f32 / n, st.warn),
        kit::StatTile::new("Start at boot", auto.to_string(), "Boot, system or automatic").bar(auto as f32 / n, theme::darken_for(pal, theme::INDIGO)),
    ];
    if drivers {
        let third = list.iter().filter(|s| third_party(&s.company)).count();
        tiles.push(kit::StatTile::new("Third party", third.to_string(), "Not signed by Microsoft (company name)").bar(third as f32 / n, theme::darken_for(pal, theme::PURPLE)));
    } else {
        let shared = list.iter().filter(|s| s.kind == "Shared process" && s.running).count();
        tiles.push(kit::StatTile::new("Shared hosts", shared.to_string(), "Running in svchost groups").bar(shared as f32 / n, theme::darken_for(pal, theme::PURPLE)));
    }
    let tiles_h = kit::stat_grid(&p, area, pal, &tiles, 170.0);
    let rest = Rect::from_min_max(pos2(area.left(), area.top() + tiles_h + BLOCK_GAP), area.max);
    if list.is_empty() {
        empty_state(&p, rest, pal, error.unwrap_or("Reading the Service Control Manager\u{2026}"));
        return None;
    }

    // Table on the left (services: panel on the right or below; drivers: bar below).
    let (table_rect, side_rect) = if drivers {
        let bar = Rect::from_min_max(pos2(rest.left(), (rest.bottom() - DRIVER_BAR_H).max(rest.top())), rest.max);
        (Rect::from_min_max(rest.min, pos2(rest.right(), (bar.top() - GAP).max(rest.top()))), bar)
    } else if area.width() >= ASIDE_MIN_PAGE_W {
        let w = (rest.width() * 0.27).clamp(320.0, 440.0);
        (Rect::from_min_max(rest.min, pos2(rest.right() - w - 14.0, rest.bottom())), Rect::from_min_max(pos2(rest.right() - w, rest.top()), rest.max))
    } else {
        let h = (rest.height() * 0.45).clamp(170.0, 280.0).min(rest.height());
        (Rect::from_min_max(rest.min, pos2(rest.right(), rest.bottom() - h - 14.0)), Rect::from_min_max(pos2(rest.left(), rest.bottom() - h), rest.max))
    };

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
    let purple = theme::darken_for(pal, theme::PURPLE);
    let cell = |i: usize, col: usize| -> Cell {
        let s = &list[i];
        match map(col) {
            0 => Cell::new(&s.name).mono().color(pal.text),
            1 => Cell::new(if drivers && !s.description.is_empty() { &s.description } else { &s.display }).color(pal.text_table()),
            2 => {
                let c = match s.state {
                    "Paused" => st.yellow,
                    _ if s.running => st.ok,
                    _ => pal.text_faint,
                };
                Cell::new(s.state).dot(c).color(c)
            }
            3 => Cell::new(s.start).color(if s.start == "Disabled" { st.danger } else { pal.text_dim }),
            4 => Cell::new(s.kind).dim(pal),
            5 => {
                if drivers {
                    Cell::new(&s.company).color(if third_party(&s.company) { purple } else { pal.text_dim })
                } else {
                    Cell::new(if s.pid != 0 { s.pid.to_string() } else { String::new() }).dim(pal)
                }
            }
            6 => Cell::new(&s.version).mono().dim(pal),
            _ => Cell::new(&s.path).mono().color(pal.text_faint),
        }
    };
    let key = |i: usize| list[i].name.clone();
    if let Some(i) = table::show(ui, table_rect, if drivers { "drivers-table" } else { "services-table" }, pal, &cols, &order, state, &key, &cell, icons) {
        let path = PathBuf::from(&list[i].path);
        if path.exists() {
            let _ = kit::reveal(&path);
        }
    }

    let selected = state.selected.as_ref().and_then(|k| list.iter().find(|s| &s.name == k));
    if drivers {
        driver_bar(ui, side_rect, pal, selected);
        None
    } else {
        service_panel(ui, side_rect, fd, selected, info)
    }
}

/// Bar under the Drivers table: the selected driver's file, its maker and two actions.
fn driver_bar(ui: &mut egui::Ui, rect: Rect, pal: &theme::Palette, driver: Option<&Service>) {
    let p = ui.painter().clone();
    widgets::card(&p, rect, pal);
    let Some(s) = driver else {
        p.text(pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, "Select a driver to see its file.", theme::ui(13.0), pal.text_faint);
        return;
    };
    let file = Path::new(&s.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| s.name.clone());
    let mut facts: Vec<String> = Vec::new();
    if !s.company.is_empty() {
        facts.push(s.company.clone());
    }
    if !s.version.is_empty() {
        facts.push(s.version.clone());
    }
    facts.push(format!("{} start", s.start));
    let buttons_w = 330.0;
    let name = widgets::text_fit(&p, pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, &file, theme::mono_medium(13.0), pal.text, (rect.width() - buttons_w - 60.0).max(80.0) * 0.5);
    widgets::text_fit(&p, pos2(name.right() + 16.0, rect.center().y), Align2::LEFT_CENTER, &facts.join(" \u{b7} "), theme::ui(13.0), pal.text_dim, (rect.right() - buttons_w - name.right() - 32.0).max(40.0));
    let exists = !s.path.is_empty() && Path::new(&s.path).exists();
    let r = Rect::from_min_max(pos2(rect.right() - 18.0 - buttons_w, rect.center().y - 17.0), pos2(rect.right() - 18.0, rect.center().y + 17.0));
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(r).layout(Layout::right_to_left(Align::Center)), |ui| {
        if Button::ghost("Copy path").size(ButtonSize::Medium).show(ui, pal).clicked() {
            ui.ctx().copy_text(s.path.clone());
        }
        let open = ui.add_enabled_ui(exists, |ui| Button::new("Open file location").size(ButtonSize::Medium).show(ui, pal).clicked()).inner;
        if open {
            let _ = kit::reveal(Path::new(&s.path));
        }
    });
}

/// Draws a dim `label` and a wrapped `value` (at most `max_rows` lines); returns the row height.
#[allow(clippy::too_many_arguments)]
fn detail_row(p: &egui::Painter, pal: &theme::Palette, pos: egui::Pos2, width: f32, label: &str, value: &str, mono: bool, color: Option<Color32>, max_rows: usize) -> f32 {
    const LABEL_W: f32 = 96.0;
    widgets::text_fit(p, pos2(pos.x, pos.y + 9.0), Align2::LEFT_CENTER, label, theme::ui(13.0), pal.text_dim, LABEL_W - 8.0);
    let (font, default) = if mono { (theme::mono(12.0), pal.text_table()) } else { (theme::ui(13.0), pal.text) };
    let color = color.unwrap_or(default);
    let mut job = egui::text::LayoutJob::single_section(value.to_string(), egui::TextFormat::simple(font, color));
    job.wrap = egui::text::TextWrapping { max_width: (width - LABEL_W).max(40.0), max_rows, break_anywhere: true, overflow_character: Some('\u{2026}') };
    let galley = p.layout_job(job);
    let h = galley.size().y.max(18.0);
    p.galley(pos2(pos.x + LABEL_W, pos.y), galley, color);
    h + 8.0
}

/// The panel next to the Services table: what the selected service does and where it runs.
fn service_panel(ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>, svc: Option<&Service>, info: &mut InfoCache) -> Option<(u32, i64)> {
    let pal = &fd.pal;
    let st = theme::status(pal);
    let p = ui.painter().clone();
    let content = kit::section_card(&p, rect, pal, "What does this service do?", "");
    let Some(s) = svc else {
        empty_state(&p, content, pal, "Select a service in the list.");
        return None;
    };
    let facts = info.get(&s.name, &s.path).clone();
    let mut goto = None;

    // Heading: the display name in Light 20 and the service name in mono beside it.
    let title = if s.display.is_empty() { &s.name } else { &s.display };
    let name_w = if title.eq_ignore_ascii_case(&s.name) { 0.0 } else { widgets::text_width(&p, &s.name, theme::mono(13.0)) + 10.0 };
    let head = widgets::text_fit(&p, content.left_top(), Align2::LEFT_TOP, title, theme::light(20.0), pal.text, (content.width() - name_w).max(60.0));
    if name_w > 0.0 {
        widgets::text_fit(&p, pos2(head.right() + 10.0, head.bottom() - 4.0), Align2::LEFT_BOTTOM, &s.name, theme::mono(13.0), pal.text_faint, (content.right() - head.right() - 10.0).max(20.0));
    }

    // Buttons are pinned to the bottom, the details sit above them.
    let btn_h = ButtonSize::Toolbar.height();
    let actions = Rect::from_min_max(pos2(content.left(), content.bottom() - btn_h), content.max);
    let process_row = fd.rows.iter().find(|r| s.pid != 0 && r.pid == s.pid);
    // Process, resources and company are one line each, the image path may take three.
    let details = 1 + usize::from(process_row.is_some()) + usize::from(!facts.company.is_empty());
    let details_h = details as f32 * 26.0 + if s.path.is_empty() { 0.0 } else { 56.0 };
    let para_top = head.bottom() + 12.0;
    let para_h = (actions.top() - 14.0 - details_h - para_top).max(0.0);
    let text = facts.description.as_deref().unwrap_or("No description provided by the service.");
    let color = if facts.description.is_some() { pal.text_table() } else { pal.text_faint };
    let line_h = 19.5;
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat { font_id: theme::ui(13.0), color, line_height: Some(line_h), ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: content.width(), max_rows: ((para_h / line_h).floor() as usize).max(1), break_anywhere: false, overflow_character: Some('\u{2026}') };
    let galley = p.layout_job(job);
    let para_bottom = para_top + galley.size().y;
    p.galley(pos2(content.left(), para_top), galley, color);

    // Details: process (a link to the Processes page), its resources, the maker and the image.
    let mut y = para_bottom + 14.0;
    let x = content.left();
    let w = content.width();
    {
        let label_y = y + 9.0;
        widgets::text_fit(&p, pos2(x, label_y), Align2::LEFT_CENTER, "Process", theme::ui(13.0), pal.text_dim, 88.0);
        match (s.pid, process_row) {
            (0, _) => {
                p.text(pos2(x + 96.0, label_y), Align2::LEFT_CENTER, "Not running", theme::ui(13.0), pal.text_faint);
            }
            (pid, None) => {
                p.text(pos2(x + 96.0, label_y), Align2::LEFT_CENTER, format!("PID {pid}"), theme::mono(12.0), pal.text_table());
            }
            (_, Some(r)) => {
                let link = format!("{} ({})", r.name, r.pid);
                let hit = Rect::from_min_size(pos2(x + 96.0, y), vec2(widgets::text_width(&p, &link, theme::mono(12.0)).min(w - 96.0), 18.0));
                let resp = ui.interact(hit, ui.id().with(("service-process", r.pid)), Sense::click());
                let c = if resp.hovered() { theme::lighten(st.info, 0.25) } else { st.info };
                widgets::text_fit(&p, pos2(hit.left(), label_y), Align2::LEFT_CENTER, &link, theme::mono(12.0), c, w - 96.0);
                if resp.hovered() {
                    p.line_segment([pos2(hit.left(), hit.bottom() - 1.0), pos2(hit.right(), hit.bottom() - 1.0)], egui::Stroke::new(1.0_f32, c));
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    goto = Some((r.pid, r.create_time));
                }
            }
        }
        y += 26.0;
    }
    if let Some(r) = process_row {
        let res = format!("{:.1}% CPU \u{b7} {} \u{b7} {}", r.cpu_pct, format_bytes(r.private_working_set as f64), format_rate(r.io_bps));
        y += detail_row(&p, pal, pos2(x, y), w, "Resources", &res, true, None, 1);
    }
    if !facts.company.is_empty() {
        let c = third_party(&facts.company).then(|| theme::darken_for(pal, theme::PURPLE));
        y += detail_row(&p, pal, pos2(x, y), w, "Company", &facts.company, false, c, 1);
    }
    if !s.path.is_empty() {
        detail_row(&p, pal, pos2(x, y), w, "Image path", &s.path, true, None, 3);
    }

    // Actions.
    let exists = !s.path.is_empty() && Path::new(&s.path).exists();
    super::kit::row_ui(ui, actions, |ui| {
        let open = ui.add_enabled_ui(exists, |ui| Button::new("Open file location").show(ui, pal).clicked()).inner;
        if open {
            let _ = kit::reveal(Path::new(&s.path));
        }
        if Button::ghost("Copy name").show(ui, pal).clicked() {
            ui.ctx().copy_text(s.name.clone());
        }
    });
    goto
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{History, Snapshot};
    use crate::process::ProcessRow;

    fn service(name: &str, state: &'static str, start: &'static str, kind: &'static str, pid: u32, company: &str) -> Service {
        Service {
            name: name.into(),
            display: format!("{name} display"),
            state,
            running: state == "Running",
            start,
            kind,
            pid,
            path: r"C:\Windows\System32\svchost.exe".into(),
            company: company.into(),
            version: "10.0.1".into(),
            description: "A driver".into(),
        }
    }

    /// Both pages lay out at a wide and a narrow size, with and without a selection.
    #[test]
    fn pages_draw_at_both_widths() {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut icons = super::super::icons::IconCache::new(&ctx);
        let list = vec![
            service("Dnscache", "Running", "Automatic", "Shared process", 2112, ""),
            service("Spooler", "Paused", "Automatic", "Own process", 900, "Vendor"),
            service("BITS", "Stopped", "Disabled", "Shared process", 0, "Microsoft Corporation"),
        ];
        let (snap, hist, smooth) = (Snapshot::default(), History::new(8), super::super::Smooth::default());
        let rows = vec![ProcessRow { pid: 2112, name: "svchost.exe".into(), create_time: 7, cpu_pct: 0.1, ..Default::default() }];
        let fd = FrameData { snap: &snap, hist: &hist, rows: &rows, smooth: &smooth, pal, metric_colors: Default::default(), bloom: 0.0, phase: 0.0, glide: false, time: 0.0, show_self: false, lasting: Default::default() };
        let mut info = InfoCache::default();
        for (width, drivers, selected) in [(1500.0, false, None), (1500.0, false, Some("Dnscache")), (900.0, false, Some("Spooler")), (1500.0, true, None), (900.0, true, Some("Spooler"))] {
            let mut state = TableState { selected: selected.map(String::from), ..Default::default() };
            for _ in 0..2 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let area = Rect::from_min_size(pos2(10.0, 10.0), vec2(width, 700.0));
                        assert_eq!(driver_like_page(ui, area, &fd, &list, &mut state, None, drivers, &mut icons, &mut info), None);
                        assert_eq!(driver_like_page(ui, area, &fd, &[], &mut state, Some("no access"), drivers, &mut icons, &mut info), None);
                    });
                });
            }
        }
    }

    #[test]
    fn only_non_microsoft_companies_are_third_party() {
        assert!(third_party("Vendor") && !third_party("Microsoft Corporation") && !third_party(""));
    }
}
