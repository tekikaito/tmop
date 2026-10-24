//! Drivers page, and the shared body of the Drivers and Services pages.

use super::kit::{self, empty_state, header_toolbar, stat_row, vsplit, GAP, MARGIN};
use super::table::{self, cmp_str, Cell, Col, TableState};
use super::theme::{self};
use super::widgets::{self, Icon};
use super::{App, FrameData};
use crate::pro::drivers::Service;
use eframe::egui::{self, Rect};
use std::path::PathBuf;

impl App {
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
