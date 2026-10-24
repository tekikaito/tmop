//! Installed Apps page.

use super::kit::{self, empty_state, header_toolbar, stat_row, vsplit, GAP, MARGIN};
use super::table::{self, cmp_str, Cell, Col};
use super::theme::{self};
use super::widgets::{self, Icon};
use super::{App, FrameData};
use crate::pro::apps::App as InstalledApp;
use crate::process::format_bytes;
use eframe::egui::{self};
use std::collections::HashSet;
use std::path::PathBuf;

impl App {
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
}
