//! Inventory pages: System Info, App history, Startup apps, Users and
//! Services.

use super::kit::{empty_state, header_toolbar, hsplit, kv_list, stat_row, titled_card, vsplit, GAP, MARGIN};
use super::drivers_page::driver_like_page;
use super::table::{self, cmp_f, cmp_str, Cell, Col, TableState};
use super::theme;
use super::widgets::{self, Icon};
use super::{App, FrameData};
use crate::pro::inventory::{Sections, Service, StartupApp};
use crate::pro::Loader;
use crate::process::{format_bytes, format_cpu_time, format_rate, ProcessRow};
use eframe::egui::{self, pos2, vec2, Align2, Rect};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct InventoryState {
    pub sysinfo: Loader<Sections>,
    pub services: Loader<Vec<Service>>,
    pub services_table: TableState,
    pub startup: Loader<Vec<StartupApp>>,
    pub startup_table: TableState,
    pub users_table: TableState,
    pub history: AppHistory,
    pub history_table: TableState,
}

/// Per-application resource use accumulated since TMOP started.
#[derive(Default)]
pub struct AppHistory {
    seen: HashMap<u32, (i64, u64)>,
    pub apps: HashMap<String, HistoryEntry>,
    pub since: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct HistoryEntry {
    pub name: String,
    pub path: String,
    /// CPU time used while observed (100 ns units).
    pub cpu_time: u64,
    pub io_bytes: f64,
    pub peak_memory: u64,
    pub launches: u32,
    pub running: u32,
    pub last_seen: Option<Instant>,
}

impl AppHistory {
    /// Feeds one sample; `dt` is the time since the previous sample.
    pub fn update(&mut self, rows: &[ProcessRow], dt: f64) {
        self.since.get_or_insert_with(Instant::now);
        for e in self.apps.values_mut() {
            e.running = 0;
        }
        let mut next = HashMap::with_capacity(rows.len());
        for r in rows.iter().filter(|r| r.pid != 0 && r.pid != 4) {
            let e = self.apps.entry(r.name.to_lowercase()).or_insert_with(|| HistoryEntry { name: r.name.clone(), ..Default::default() });
            match self.seen.get(&r.pid) {
                Some((ct, prev)) if *ct == r.create_time => e.cpu_time += r.cpu_time.saturating_sub(*prev),
                _ => e.launches += 1,
            }
            if e.path.is_empty() {
                if let Some(p) = &r.path {
                    e.path = p.clone();
                }
            }
            e.io_bytes += r.io_bps * dt;
            e.peak_memory = e.peak_memory.max(r.private_working_set);
            e.running += 1;
            e.last_seen = Some(Instant::now());
            next.insert(r.pid, (r.create_time, r.cpu_time));
        }
        self.seen = next;
    }
}

impl App {
    pub fn system_info_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "System Info", Icon::Info);
        let mut refresh = false;
        header_toolbar(ui, body, 200.0, |ui| refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked());
        let inv = &mut self.pro.inv;
        inv.sysinfo.poll();
        if refresh || (inv.sysinfo.get().is_none() && !inv.sysinfo.is_loading()) {
            inv.sysinfo.start(crate::pro::inventory::system_info);
        }
        let area = body.shrink(MARGIN);
        let p = ui.painter().clone();
        let Some(sections) = inv.sysinfo.ok().cloned() else {
            empty_state(&p, area, &pal, inv.sysinfo.error().unwrap_or("Reading system information\u{2026}"));
            return;
        };
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let s = fd.snap;
        let get = |sec: &str, k: &str| sections.iter().find(|x| x.0 == sec).and_then(|x| x.1.iter().find(|r| r.0 == k)).map(|r| r.1.clone()).unwrap_or_default();
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Windows".into(), get("Operating system", "Version"), format!("{} build {}", get("Operating system", "Edition"), get("Operating system", "OS build")), theme::BLUE, None),
                ("Processor".into(), format!("{} threads", s.cpu.logical_count), if s.cpu.name.is_empty() { get("Processor and memory", "Processor") } else { s.cpu.name.clone() }, fd.metric("cpu"), Some(fd.smooth.cpu / 100.0)),
                ("Memory".into(), format_bytes(s.memory.total as f64), format!("{} installed", get("Processor and memory", "Installed memory")), fd.metric("memory"), Some(fd.smooth.memory / 100.0)),
                ("Up time".into(), super::fmt_hms(s.uptime), get("Device", "Computer name"), theme::ORANGE, None),
            ],
        );
        let n = sections.len().max(1);
        let cols = if rows[1].width() > 1300.0 { 3 } else { 2 };
        let col_rects = hsplit(rows[1], &vec![1.0; cols], GAP);
        let mut col_y: Vec<f32> = col_rects.iter().map(|r| r.top()).collect();
        for (i, (title, items)) in sections.iter().enumerate() {
            let c = if i < cols { i } else { col_y.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).map(|x| x.0).unwrap_or(0) };
            let h = 26.0 + 16.0 + items.len() as f32 * 24.0 + 10.0;
            let r = Rect::from_min_size(pos2(col_rects[c].left(), col_y[c]), vec2(col_rects[c].width(), h.min(col_rects[c].bottom() - col_y[c])));
            if r.height() < 60.0 {
                continue;
            }
            let content = titled_card(&p, r, &pal, fd.bloom, title, "");
            kv_list(&p, content.shrink2(vec2(6.0, 2.0)), &pal, items, 150.0, 24.0);
            col_y[c] += h + GAP;
        }
        let _ = n;
    }

    pub fn app_history_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "App history", Icon::History);
        let mut reset = false;
        header_toolbar(ui, body, 480.0, |ui| {
            reset = widgets::button(ui, "Reset", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.inv.history_table.filter, "Filter by name", 260.0, &pal);
        });
        if reset {
            self.pro.inv.history = AppHistory::default();
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let h = &self.pro.inv.history;
        let list: Vec<HistoryEntry> = h.apps.values().cloned().collect();
        let since = h.since.map(|s| s.elapsed()).unwrap_or_default();
        let total_cpu: u64 = list.iter().map(|e| e.cpu_time).sum();
        let total_io: f64 = list.iter().map(|e| e.io_bytes).sum();
        let top = list.iter().max_by_key(|e| e.cpu_time);
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Observed".into(), super::fmt_hms(since), "Resource use since TMOP started".into(), theme::BLUE, None),
                ("CPU time".into(), format_cpu_time(total_cpu), format!("{} applications", list.len()), theme::GREEN, None),
                ("I/O".into(), format_bytes(total_io), "Read + write, all processes".into(), theme::ORANGE, None),
                ("Busiest".into(), top.map(|t| t.name.clone()).unwrap_or_default(), top.map(|t| format_cpu_time(t.cpu_time)).unwrap_or_default(), theme::RED, top.map(|t| t.cpu_time as f32 / total_cpu.max(1) as f32)),
            ],
        );
        let st = &self.pro.inv.history_table;
        let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&st.filter, &[&list[*i].name, &list[*i].path])).collect();
        st.sort(&list, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => a.cpu_time.cmp(&b.cpu_time),
            2 => cmp_f(a.io_bytes, b.io_bytes),
            3 => a.peak_memory.cmp(&b.peak_memory),
            4 => a.launches.cmp(&b.launches),
            5 => a.running.cmp(&b.running),
            _ => cmp_str(&a.path, &b.path),
        });
        let max_cpu = list.iter().map(|e| e.cpu_time).max().unwrap_or(1).max(1) as f32;
        let max_io = list.iter().map(|e| e.io_bytes).fold(1.0, f64::max) as f32;
        let cols = [Col::text("Name", 260.0), Col::num("CPU time", 110.0), Col::num("I/O", 110.0), Col::num("Peak memory", 120.0), Col::num("Launches", 90.0), Col::num("Running", 90.0), Col::text("Image path", 400.0)];
        let key = |i: usize| list[i].name.to_lowercase();
        let cell = |i: usize, col: usize| -> Cell {
            let e = &list[i];
            match col {
                0 => Cell::new(&e.name).icon(Some(e.path.as_str())),
                1 => Cell::new(format_cpu_time(e.cpu_time)).heat((e.cpu_time as f32 / max_cpu).sqrt()),
                2 => Cell::new(format_bytes(e.io_bytes)).heat((e.io_bytes as f32 / max_io).sqrt()),
                3 => Cell::new(format_bytes(e.peak_memory as f64)),
                4 => Cell::new(e.launches.to_string()).dim(&pal),
                5 => Cell::new(if e.running > 0 { e.running.to_string() } else { "\u{2014}".into() }).dim(&pal),
                _ => Cell::new(&e.path).dim(&pal),
            }
        };
        if list.is_empty() {
            empty_state(&p, rows[1], &pal, "Collecting\u{2026}");
        } else {
            let icons = &mut self.icons;
            table::show(ui, rows[1], "history-table", &pal, &cols, &order, &mut self.pro.inv.history_table, &key, &cell, icons);
        }
    }

    pub fn startup_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Startup apps", Icon::Play);
        let mut refresh = false;
        header_toolbar(ui, body, 480.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.inv.startup_table.filter, "Filter by name or command", 260.0, &pal);
        });
        let inv = &mut self.pro.inv;
        inv.startup.poll();
        if refresh || (inv.startup.get().is_none() && !inv.startup.is_loading()) {
            inv.startup.start(crate::pro::inventory::startup_apps);
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let list: &[StartupApp] = inv.startup.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let enabled = list.iter().filter(|a| a.enabled).count();
        let folder = list.iter().filter(|a| a.location.starts_with("Startup folder")).count();
        let n = list.len().max(1) as f32;
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Enabled".into(), enabled.to_string(), format!("of {} startup entries", list.len()), theme::GREEN, Some(enabled as f32 / n)),
                ("Disabled".into(), (list.len() - enabled).to_string(), "Turned off in Windows settings".into(), theme::ORANGE, Some((list.len() - enabled) as f32 / n)),
                ("Registry".into(), (list.len() - folder).to_string(), "Run keys".into(), theme::BLUE, None),
                ("Startup folders".into(), folder.to_string(), "Shortcuts in Startup".into(), theme::PURPLE, None),
            ],
        );
        if list.is_empty() {
            empty_state(&p, rows[1], &pal, inv.startup.error().unwrap_or("Reading startup entries\u{2026}"));
            return;
        }
        let st = &inv.startup_table;
        let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&st.filter, &[&list[*i].name, &list[*i].command, &list[*i].publisher])).collect();
        st.sort(list, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.publisher, &b.publisher),
            2 => a.enabled.cmp(&b.enabled),
            3 => a.location.cmp(&b.location),
            _ => cmp_str(&a.command, &b.command),
        });
        let cols = [Col::text("Name", 240.0), Col::text("Publisher", 220.0), Col::text("Status", 110.0), Col::text("Location", 220.0), Col::text("Command", 500.0)];
        let key = |i: usize| format!("{}|{}", list[i].location, list[i].name);
        let cell = |i: usize, col: usize| -> Cell {
            let a = &list[i];
            match col {
                0 => Cell::new(&a.name).icon(Some(crate::pro::inventory::command_path(&a.command).as_str())),
                1 => Cell::new(&a.publisher).dim(&pal),
                2 => Cell::new(if a.enabled { "Enabled" } else { "Disabled" }).dot(if a.enabled { theme::GREEN } else { pal.text_faint }),
                3 => Cell::new(&a.location).dim(&pal),
                _ => Cell::new(&a.command).dim(&pal),
            }
        };
        let icons = &mut self.icons;
        table::show(ui, rows[1], "startup-table", &pal, &cols, &order, &mut inv.startup_table, &key, &cell, icons);
    }

    pub fn users_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Users", Icon::User);
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        #[derive(Default, Clone)]
        struct U {
            user: String,
            session: u32,
            procs: usize,
            cpu: f32,
            mem: u64,
            io: f64,
            apps: usize,
        }
        let full = self.settings.general.show_full_account_name;
        let mut by: HashMap<(String, u32), U> = HashMap::new();
        for r in fd.rows.iter().filter(|r| r.pid != 0) {
            let raw = r.user.clone().unwrap_or_else(|| "(unavailable)".into());
            let user = if full { raw } else { raw.rsplit('\\').next().unwrap_or(&raw).to_string() };
            let e = by.entry((user.clone(), r.session_id)).or_insert_with(|| U { user, session: r.session_id, ..Default::default() });
            e.procs += 1;
            e.cpu += r.cpu_pct;
            e.mem += r.private_working_set;
            e.io += r.io_bps;
            e.apps += r.has_window as usize;
        }
        let list: Vec<U> = by.into_values().collect();
        let interactive = list.iter().filter(|u| u.session > 0 && u.apps > 0).count();
        let top = list.iter().max_by(|a, b| a.cpu.total_cmp(&b.cpu));
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Accounts".into(), list.len().to_string(), "Distinct account / session pairs".into(), theme::BLUE, None),
                ("Signed in".into(), interactive.to_string(), "Sessions with app windows".into(), theme::GREEN, None),
                ("Busiest".into(), top.map(|t| format!("{:.1}%", t.cpu)).unwrap_or_default(), top.map(|t| t.user.clone()).unwrap_or_default(), theme::RED, top.map(|t| t.cpu / 100.0)),
                ("Processes".into(), fd.rows.len().to_string(), format!("{} with windows", fd.rows.iter().filter(|r| r.has_window).count()), theme::PURPLE, None),
            ],
        );
        let st = &self.pro.inv.users_table;
        let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&st.filter, &[&list[*i].user])).collect();
        st.sort(&list, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.user, &b.user),
            1 => a.session.cmp(&b.session),
            2 => a.procs.cmp(&b.procs),
            3 => cmp_f(a.cpu as f64, b.cpu as f64),
            4 => a.mem.cmp(&b.mem),
            _ => cmp_f(a.io, b.io),
        });
        let max_mem = list.iter().map(|u| u.mem).max().unwrap_or(1).max(1) as f32;
        let cols = [Col::text("User", 280.0), Col::num("Session", 90.0), Col::num("Processes", 100.0), Col::num("CPU", 90.0), Col::num("Memory", 120.0), Col::num("Disk", 120.0)];
        let key = |i: usize| format!("{}|{}", list[i].user, list[i].session);
        let cell = |i: usize, col: usize| -> Cell {
            let u = &list[i];
            match col {
                0 => Cell::new(&u.user).dot(if u.session > 0 && u.apps > 0 { theme::GREEN } else { pal.text_faint }),
                1 => Cell::new(u.session.to_string()).dim(&pal),
                2 => Cell::new(u.procs.to_string()),
                3 => Cell::new(format!("{:.1}%", u.cpu)).heat(u.cpu / 25.0),
                4 => Cell::new(format_bytes(u.mem as f64)).heat(u.mem as f32 / max_mem),
                _ => Cell::new(format_rate(u.io)),
            }
        };
        let icons = &mut self.icons;
        table::show(ui, rows[1], "users-table", &pal, &cols, &order, &mut self.pro.inv.users_table, &key, &cell, icons);
    }

    pub fn services_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Services", Icon::Wrench);
        let mut refresh = false;
        header_toolbar(ui, body, 480.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.inv.services_table.filter, "Filter by name or path", 260.0, &pal);
        });
        let inv = &mut self.pro.inv;
        inv.services.poll();
        if refresh || inv.services.stale(Duration::from_secs(15)) {
            inv.services.start(crate::pro::inventory::services);
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
        let p = ui.painter().clone();
        let list: &[Service] = inv.services.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        driver_like_page(ui, &p, rows, fd, list, &mut inv.services_table, inv.services.error(), false, &mut self.icons);
        let _ = theme::ui(12.0);
        let _ = Align2::LEFT_TOP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, name: &str, ct: i64, cpu_time: u64) -> ProcessRow {
        ProcessRow { pid, name: name.into(), create_time: ct, cpu_time, io_bps: 100.0, private_working_set: 10, ..Default::default() }
    }

    #[test]
    fn history_accumulates_deltas_and_launches() {
        let mut h = AppHistory::default();
        h.update(&[row(10, "a.exe", 1, 1000)], 1.0);
        h.update(&[row(10, "a.exe", 1, 1500), row(11, "A.exe", 2, 50)], 1.0);
        h.update(&[row(11, "A.exe", 2, 80)], 1.0);
        let e = &h.apps["a.exe"];
        assert_eq!(e.launches, 2);
        assert_eq!(e.cpu_time, 530);
        assert_eq!(e.running, 1);
        assert!((e.io_bytes - 400.0).abs() < 1e-6);
    }
}
