//! Installed Apps page.

use super::kit::{self, empty_state, stat_grid, StatTile};
use super::pro_pages::{page_layout, toolbar};
use super::table::{self, cmp_str, Cell, Col};
use super::theme;
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
        let st = theme::status(&pal);
        self.pro.apps.poll();
        let windows = cfg!(windows);
        let source = match self.pro.apps.ok() {
            _ if windows => "the Uninstall registry keys".to_string(),
            Some(a) if !a.is_empty() => crate::pro::apps::sources(a),
            _ => "the package databases".to_string(),
        };
        let subtitle = match self.pro.apps.ok() {
            Some(a) => format!("from {source} \u{b7} {} entries", a.len()),
            None => format!("from {source}"),
        };
        let body = self.page_title(ui, &pal, "Installed Apps", &subtitle);
        let (tools, area) = page_layout(body, 360.0);
        let mut refresh = false;
        toolbar(ui, tools, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            kit::filter_box(ui, &mut self.pro.apps_table.filter, "Filter by name, publisher or version", 240.0, &pal);
        });
        if refresh || (self.pro.apps.get().is_none() && !self.pro.apps.is_loading()) {
            self.pro.apps.start(crate::pro::apps::read);
        }
        let p = ui.painter().clone();
        let apps: &[InstalledApp] = self.pro.apps.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let total: u64 = apps.iter().map(|a| a.size).sum();
        let user = apps.iter().filter(|a| a.scope == "User").count();
        let x86 = apps.iter().filter(|a| a.scope == "Machine (32-bit)").count();
        let publishers: HashSet<&str> = apps.iter().map(|a| a.publisher.as_str()).filter(|p| !p.is_empty()).collect();
        let biggest = apps.iter().max_by_key(|a| a.size);
        let mut largest = StatTile::new("Largest", biggest.map(|b| format_bytes(b.size as f64)).unwrap_or_default(), biggest.map(|b| b.name.clone()).unwrap_or_default());
        if let Some(b) = biggest {
            largest = largest.bar(b.size as f32 / total.max(1) as f32, st.warn);
        }
        let breakdown = if windows { format!("{user} per-user, {x86} 32-bit") } else { source_counts(apps) };
        let tiles = [
            StatTile::new("Apps", apps.len().to_string(), breakdown),
            StatTile::new("Reported size", format_bytes(total as f64), if windows { "Sum of EstimatedSize" } else { "Sum of the reported sizes" }),
            StatTile::new("Publishers", publishers.len().to_string(), "Distinct publishers"),
            largest,
        ];
        let tiles_h = stat_grid(&p, area, &pal, &tiles, 190.0);
        let table_rect = egui::Rect::from_min_max(egui::pos2(area.left(), area.top() + tiles_h + 14.0), area.max);
        if apps.is_empty() {
            empty_state(&p, table_rect, &pal, self.pro.apps.error().unwrap_or(if windows { "Reading the Uninstall registry keys\u{2026}" } else { "Reading installed packages\u{2026}" }));
            return;
        }
        let st_table = &self.pro.apps_table;
        let mut order: Vec<usize> = (0..apps.len()).filter(|i| table::matches(&st_table.filter, &[&apps[*i].name, &apps[*i].publisher, &apps[*i].version, if windows { "" } else { apps[*i].scope }])).collect();
        st_table.sort(apps, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.publisher, &b.publisher),
            2 => a.version.cmp(&b.version),
            3 => a.install_date.cmp(&b.install_date),
            4 => a.size.cmp(&b.size),
            5 => a.scope.cmp(b.scope),
            _ => cmp_str(&a.location, &b.location),
        });
        let cols = [Col::text("Name", 320.0), Col::text("Publisher", 220.0), Col::text("Version", 130.0), Col::text("Installed", 100.0), Col::num("Size", 90.0), Col::text(if windows { "Scope" } else { "Source" }, 130.0), Col::text("Location", 300.0)];
        let key = |i: usize| format!("{}|{}", apps[i].key, apps[i].scope);
        let cell = |i: usize, col: usize| -> Cell {
            let a = &apps[i];
            match col {
                0 => Cell::new(&a.name),
                1 => Cell::new(&a.publisher).dim(&pal),
                2 => Cell::new(&a.version).mono(),
                3 => Cell::new(&a.install_date).mono(),
                4 => Cell::new(if a.size > 0 { format_bytes(a.size as f64) } else { String::new() }),
                5 => Cell::new(a.scope).dim(&pal),
                _ => Cell::new(&a.location).mono().color(pal.text_faint),
            }
        };
        let icons = &mut self.icons;
        if let Some(i) = table::show(ui, table_rect, "apps-table", &pal, &cols, &order, &mut self.pro.apps_table, &key, &cell, icons) {
            let loc = PathBuf::from(&apps[i].location);
            if loc.exists() {
                let _ = kit::reveal(&loc);
            }
        }
    }
}

/// "1890 dpkg, 12 Flatpak, 3 Snap": entries per source, the largest three.
fn source_counts(apps: &[InstalledApp]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for a in apps {
        match counts.iter_mut().find(|c| c.0 == a.scope) {
            Some(c) => c.1 += 1,
            None => counts.push((a.scope, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    counts.iter().take(3).map(|(s, n)| format!("{n} {s}")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_source() {
        let app = |scope| InstalledApp { scope, ..Default::default() };
        assert_eq!(source_counts(&[]), "");
        assert_eq!(source_counts(&[app("Snap"), app("dpkg"), app("dpkg"), app("Flatpak"), app("Flatpak (user)")]), "2 dpkg, 1 Flatpak, 1 Flatpak (user)");
    }
}
