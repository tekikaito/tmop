//! Inventory pages: System Info, App history, Startup apps, Users and
//! Services.

use super::drivers_page::{driver_like_page, page_area, InfoCache};
use super::hardware::{self, Hardware};
use super::kit::{self, category_color, empty_state, filter_box, header_toolbar, section_card, StatTile, STAT_GAP, STAT_TILE_H};
use super::table::{self, cmp_f, cmp_str, Cell, Col, TableState};
use super::theme;
use super::widgets::{self, Button};
use super::{App, FrameData, Page};
use crate::metrics::Series;
use crate::pro::inventory::{Sections, StartupApp};
use crate::pro::Loader;
use crate::process::{format_bytes, format_cpu_time, format_rate, ProcessRow};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, Sense};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Gap between the blocks of a page (tiles, cards, table).
const BLOCK_GAP: f32 = 16.0;
/// Gap between the section cards of System Info.
const CARD_GAP: f32 = 14.0;
/// Narrowest System Info card; the page has as many columns as fit.
const CARD_MIN_W: f32 = 300.0;

pub struct InventoryState {
    pub sysinfo: Loader<Sections>,
    /// Local time (hour, minute, second) of the last successful System Info read.
    pub sysinfo_read_at: Option<(u16, u16, u16)>,
    pub services: Loader<Vec<crate::pro::inventory::Service>>,
    pub services_table: TableState,
    /// Description and company of the services looked at in the side panel.
    pub svc_info: InfoCache,
    pub startup: Loader<Vec<StartupApp>>,
    pub startup_table: TableState,
    pub users_table: TableState,
    pub history: AppHistory,
    pub history_table: TableState,
}

impl Default for InventoryState {
    fn default() -> Self {
        Self {
            sysinfo: Loader::default(),
            sysinfo_read_at: None,
            services: Loader::default(),
            services_table: TableState::default(),
            svc_info: InfoCache::default(),
            startup: Loader::default(),
            startup_table: TableState::default(),
            // Busiest first, as in the design.
            users_table: TableState::sorted_by(3, true),
            history: AppHistory::default(),
            history_table: TableState::sorted_by(1, true),
        }
    }
}

/// Per-application resource use accumulated since TMOP started.
#[derive(Default)]
pub struct AppHistory {
    seen: HashMap<u32, (i64, u64)>,
    pub apps: HashMap<String, HistoryEntry>,
    pub since: Option<Instant>,
    /// Recent CPU use (percent of the machine) summed per (account, session) for the Users page.
    /// It is not part of the app history: [`AppHistory::reset`] keeps it.
    pub user_cpu: HashMap<(String, u32), Series>,
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

/// How an account is named in the process list.
fn user_label(r: &ProcessRow) -> String {
    r.user.clone().unwrap_or_else(|| "(unavailable)".into())
}

/// The account without its domain.
fn short_account(user: &str) -> &str {
    user.rsplit('\\').next().unwrap_or(user)
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
        self.update_users(rows, dt);
    }

    /// Pushes this sample's CPU use of every (account, session); the series cover about a minute.
    fn update_users(&mut self, rows: &[ProcessRow], dt: f64) {
        let cap = (60.0 / dt.max(0.05)).round().clamp(10.0, 600.0) as usize;
        let mut sums: HashMap<(String, u32), f32> = HashMap::new();
        for r in rows.iter().filter(|r| r.pid != 0) {
            *sums.entry((user_label(r), r.session_id)).or_default() += r.cpu_pct;
        }
        self.user_cpu.retain(|k, _| sums.contains_key(k));
        for (k, v) in sums {
            let s = self.user_cpu.entry(k).or_insert_with(|| Series::new(cap));
            if s.capacity != cap {
                s.resize(cap);
            }
            s.push(v);
        }
    }

    /// Forgets the applications seen so far (the Reset button).
    pub fn reset(&mut self) {
        let user_cpu = std::mem::take(&mut self.user_cpu);
        *self = Self { user_cpu, ..Self::default() };
    }

    /// CPU use over time of the accounts in `session` that `matches` (the accounts one Users row
    /// stands for), oldest first. Series of several accounts are added up, aligned at the newest.
    pub fn user_trend(&self, session: u32, matches: impl Fn(&str) -> bool) -> Vec<f32> {
        let series: Vec<&Series> = self.user_cpu.iter().filter(|((u, s), _)| *s == session && matches(u)).map(|(_, v)| v).collect();
        let len = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
        let mut out = vec![0.0; len];
        for s in series {
            let skip = len - s.values.len();
            for (i, v) in s.values.iter().enumerate() {
                out[skip + i] += v;
            }
        }
        out
    }
}

/// Title block subtitle: `parts` without the empty ones, joined by dots.
fn subtitle(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" \u{b7} ")
}

/// Days since 1970-01-01 of a calendar date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "Thu 09:41": the weekday and time the machine booted, from the local time now (year, month,
/// day, hour, minute, second) and the up time.
fn boot_label(now: (u16, u16, u16, u16, u16, u16), uptime: Duration) -> String {
    let (y, mo, d, h, mi, s) = now;
    let secs = days_from_civil(y as i64, mo as i64, d as i64) * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64 - uptime.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // 1970-01-01 was a Thursday.
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!("{} {:02}:{:02}", DAYS[(days + 4).rem_euclid(7) as usize], rem / 3600, rem % 3600 / 60)
}

/// All sections as text: "Section", then "  key: value" per line, a blank line between sections.
fn report_text(sections: &Sections) -> String {
    sections
        .iter()
        .map(|(title, items)| {
            let mut s = format!("{title}\n");
            for (k, v) in items {
                s.push_str(&format!("  {k}: {v}\n"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Values that are versions, builds, names or sizes read better in the mono font.
fn is_mono(section: &str, key: &str) -> bool {
    if section == "Display adapters" {
        return true;
    }
    let k = key.to_lowercase();
    ["build", "version", "identifier", "speed", "memory", "logical", "computer name", "root", "date", "bios"].iter().any(|w| k.contains(w))
}

/// Rows of stat tiles that `stat_grid` draws for `n` tiles in `width` (see [`kit::stat_grid`]).
fn tiles_height(width: f32, n: usize, min_w: f32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let cols = (((width + STAT_GAP) / (min_w + STAT_GAP)).floor() as usize).clamp(1, n);
    let rows = n.div_ceil(cols);
    rows as f32 * STAT_TILE_H + (rows - 1) as f32 * STAT_GAP
}

/// Where each System Info card goes: `(column, top, height)` per section and the page height.
fn masonry(heights: &[f32], cols: usize) -> (Vec<(usize, f32)>, f32) {
    let mut col_y = vec![0.0_f32; cols];
    let mut at = Vec::with_capacity(heights.len());
    for (i, h) in heights.iter().enumerate() {
        let c = if i < cols { i } else { col_y.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).map(|x| x.0).unwrap_or(0) };
        at.push((c, col_y[c]));
        col_y[c] += h + CARD_GAP;
    }
    let total = col_y.iter().cloned().fold(0.0, f32::max);
    (at, if total > 0.0 { total - CARD_GAP } else { 0.0 })
}

impl App {
    pub fn system_info_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let inv = &mut self.pro.inv;
        if inv.sysinfo.poll() && inv.sysinfo.ok().is_some() {
            let (_, _, _, h, m, s) = crate::platform::local_time();
            inv.sysinfo_read_at = Some((h, m, s));
        }
        let sections = inv.sysinfo.ok().cloned();
        let get = |sec: &str, k: &str| -> String {
            sections.as_ref().and_then(|s| s.iter().find(|x| x.0 == sec)).and_then(|x| x.1.iter().find(|r| r.0 == k)).map(|r| r.1.clone()).unwrap_or_default()
        };
        let computer = Some(get("Device", "Computer name")).filter(|c| !c.is_empty()).or_else(crate::platform::hostname).unwrap_or_default();
        let read = inv.sysinfo_read_at.map(|(h, m, s)| format!("read {h:02}:{m:02}:{s:02}")).unwrap_or_default();
        let body = self.page_title(ui, &pal, "System Info", &subtitle(&[computer.clone(), read, Page::SystemInfo.shortcut().into()]));
        let mut refresh = false;
        let mut copy = false;
        header_toolbar(ui, body, 240.0, |ui| {
            copy = Button::ghost("Copy report").show(ui, &pal).clicked();
            refresh = Button::new("Refresh").show(ui, &pal).clicked();
        });
        let inv = &mut self.pro.inv;
        if refresh || (inv.sysinfo.get().is_none() && !inv.sysinfo.is_loading()) {
            inv.sysinfo.start(crate::pro::inventory::system_info);
        }
        let area = page_area(body);
        let p = ui.painter().clone();
        let Some(sections) = sections.as_ref() else {
            empty_state(&p, area, &pal, inv.sysinfo.error().unwrap_or("Reading system information\u{2026}"));
            return;
        };
        if copy {
            ui.ctx().copy_text(report_text(sections));
        }
        let s = fd.snap;
        let st = theme::status(&pal);
        let edition = get("Operating system", "Edition");
        let version = get("Operating system", "Version");
        let windows = subtitle_free(&[edition.strip_prefix("Windows ").unwrap_or(&edition).to_string(), version.clone()]);
        let installed = get("Processor and memory", "Installed memory");
        let tiles = [
            StatTile::new("Windows", windows, format!("build {}", get("Operating system", "OS build"))),
            StatTile::new("Processor", format!("{} threads", s.cpu.logical_count), if s.cpu.name.is_empty() { get("Processor and memory", "Processor") } else { s.cpu.name.clone() }).bar(fd.smooth.cpu / 100.0, pal.accent),
            StatTile::new("Memory", format_bytes(s.memory.total as f64), if installed.is_empty() { String::new() } else { format!("{installed} installed") }).bar(fd.smooth.memory / 100.0, theme::darken_for(&pal, theme::INDIGO)),
            StatTile::new("Up time", super::fmt_hms(s.uptime), subtitle(&[computer, format!("last boot {}", boot_label(crate::platform::local_time(), s.uptime))])),
        ];

        // Cards flow into as many columns as fit, each below the shortest so far.
        let w = area.width();
        let cols = (((w + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize).clamp(1, 3);
        let card_w = (w - CARD_GAP * (cols - 1) as f32) / cols as f32;
        // Heading row + rows of 22 px; the Device card also has to clear its 96 x 64 drawing.
        let heights: Vec<f32> = sections.iter().map(|(title, items)| (40.0 + items.len() as f32 * 22.0 + 18.0).max(if *title == "Device" { 96.0 } else { 0.0 })).collect();
        let (at, cards_h) = masonry(&heights, cols);
        let total = tiles_height(w, tiles.len(), 190.0) + BLOCK_GAP + cards_h;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(area).id_salt("sysinfo-scroll"), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(w, total), Sense::hover());
                let p = ui.painter().clone();
                let used = kit::stat_grid(&p, rect, &pal, &tiles, 190.0);
                let top = rect.top() + used + BLOCK_GAP;
                for (((title, items), (c, y)), h) in sections.iter().zip(&at).zip(&heights) {
                    let r = Rect::from_min_size(pos2(rect.left() + *c as f32 * (card_w + CARD_GAP), top + y), vec2(card_w, *h));
                    let content = section_card(&p, r, &pal, title, "");
                    let rows: Vec<(String, String, bool, Option<Color32>)> = items
                        .iter()
                        .map(|(k, v)| {
                            let good = v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("enabled");
                            (k.clone(), v.clone(), is_mono(title, k), good.then_some(st.ok))
                        })
                        .collect();
                    if *title == "Device" {
                        hardware::draw(&p, Rect::from_min_size(pos2(r.right() - 22.0 - 96.0, r.top() + 14.0), vec2(96.0, 64.0)), Hardware::Mainboard, pal.text_rail());
                        // The first rows sit beside the drawing.
                        let near = rows.len().min(2);
                        let y = kit::kv_grid(&p, Rect::from_min_max(content.min, pos2(content.right() - 108.0, content.bottom())), &pal, &rows[..near], 130.0, 22.0);
                        kit::kv_grid(&p, Rect::from_min_max(pos2(content.left(), y), content.max), &pal, &rows[near..], 130.0, 22.0);
                    } else {
                        kit::kv_grid(&p, content, &pal, &rows, 130.0, 22.0);
                    }
                }
            });
        });
    }

    pub fn app_history_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_title(ui, &pal, "App history", &subtitle(&["resource use since TMOP started".into(), Page::AppHistory.shortcut().into()]));
        let mut reset = false;
        header_toolbar(ui, body, 290.0, |ui| {
            reset = Button::new("Reset").show(ui, &pal).clicked();
            filter_box(ui, &mut self.pro.inv.history_table.filter, "Filter by name", 200.0, &pal);
        });
        if reset {
            self.pro.inv.history.reset();
        }
        let area = page_area(body);
        let p = ui.painter().clone();
        let h = &self.pro.inv.history;
        let list: Vec<HistoryEntry> = h.apps.values().cloned().collect();
        let since = h.since.map(|s| s.elapsed()).unwrap_or_default();
        let total_cpu: u64 = list.iter().map(|e| e.cpu_time).sum();
        let total_io: f64 = list.iter().map(|e| e.io_bytes).sum();
        let top = list.iter().max_by_key(|e| e.cpu_time);
        let tiles = [
            StatTile::new("Observed", super::fmt_hms(since), "since TMOP started"),
            StatTile::new("CPU time", format_cpu_time(total_cpu), format!("{} applications", list.len())),
            StatTile::new("Busiest", top.map(|t| t.name.clone()).unwrap_or_default(), top.map(|t| format!("{} CPU", format_cpu_time(t.cpu_time))).unwrap_or_default())
                .bar(top.map(|t| t.cpu_time as f32 / total_cpu.max(1) as f32).unwrap_or(0.0), theme::status(&pal).danger),
            StatTile::new("I/O", format_bytes(total_io), "read + written by apps"),
        ];
        let used = kit::stat_grid(&p, area, &pal, &tiles, 190.0);

        // Share of CPU time: the five busiest apps and the rest.
        let share_h = 18.0 + 22.0 + 12.0 + 14.0 + 18.0 + 18.0;
        let share = Rect::from_min_size(pos2(area.left(), area.top() + used + BLOCK_GAP), vec2(area.width(), share_h));
        let content = section_card(&p, share, &pal, "Share of CPU time", &format!("{} total", format_cpu_time(total_cpu)));
        let mut ranked: Vec<&HistoryEntry> = list.iter().filter(|e| e.cpu_time > 0).collect();
        ranked.sort_by(|a, b| b.cpu_time.cmp(&a.cpu_time).then_with(|| cmp_str(&a.name, &b.name)));
        let bar = Rect::from_min_size(content.min, vec2(content.width(), 12.0));
        if ranked.is_empty() {
            widgets::bar(&p, bar, 0.0, pal.accent);
        } else {
            let mut parts: Vec<(f32, Color32)> = ranked.iter().take(5).enumerate().map(|(i, e)| (e.cpu_time as f32, category_color(i))).collect();
            let top5: u64 = ranked.iter().take(5).map(|e| e.cpu_time).sum();
            parts.push((total_cpu.saturating_sub(top5) as f32, pal.raise(0.18)));
            widgets::share_bar(&p, bar, &parts, 2.0);
            let pct = |t: u64| t as f64 * 100.0 / total_cpu.max(1) as f64;
            let mut labels: Vec<(String, Color32)> = ranked.iter().take(5).enumerate().map(|(i, e)| (format!("{} {:.0}%", clip_name(&e.name), pct(e.cpu_time)), category_color(i))).collect();
            if total_cpu > top5 {
                labels.push((format!("others {:.0}%", pct(total_cpu - top5)), pal.raise(0.18)));
            }
            let items: Vec<(&str, Color32)> = labels.iter().map(|(l, c)| (l.as_str(), *c)).collect();
            widgets::legend(&p.with_clip_rect(content.expand2(vec2(4.0, 0.0))), pos2(content.left(), bar.bottom() + 18.0), &items, theme::ui(12.0));
        }

        let table_rect = Rect::from_min_max(pos2(area.left(), share.bottom() + BLOCK_GAP), area.max);
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
        let cols = [Col::text("Name", 260.0), Col::num("CPU time", 110.0), Col::num("I/O", 110.0), Col::num("Peak memory", 120.0), Col::num("Launches", 90.0), Col::num("Running", 90.0), Col::text("Image path", 400.0)];
        let key = |i: usize| list[i].name.to_lowercase();
        let cell = |i: usize, col: usize| -> Cell {
            let e = &list[i];
            match col {
                0 => Cell::new(&e.name).icon(Some(e.path.as_str())),
                1 => Cell::new(format_cpu_time(e.cpu_time)),
                2 => Cell::new(format_bytes(e.io_bytes)),
                3 => Cell::new(format_bytes(e.peak_memory as f64)),
                4 => Cell::new(e.launches.to_string()),
                5 if e.running == 0 => Cell::new("0").color(pal.text_faint),
                5 => Cell::new(e.running.to_string()),
                _ => Cell::new(&e.path).mono().color(pal.text_faint),
            }
        };
        if list.is_empty() {
            empty_state(&p, table_rect, &pal, "Collecting\u{2026}");
        } else {
            let icons = &mut self.icons;
            table::show(ui, table_rect, "history-table", &pal, &cols, &order, &mut self.pro.inv.history_table, &key, &cell, icons);
        }
    }

    pub fn startup_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.pro.inv.startup.poll();
        let count = self.pro.inv.startup.ok().map(|l| format!("{} entries", l.len())).unwrap_or_default();
        let boot = format!("last boot {}", boot_label(crate::platform::local_time(), fd.snap.uptime));
        let body = self.page_title(ui, &pal, "Startup apps", &subtitle(&[count, boot, Page::StartupApps.shortcut().into()]));
        let mut refresh = false;
        header_toolbar(ui, body, 320.0, |ui| {
            refresh = Button::new("Refresh").show(ui, &pal).clicked();
            filter_box(ui, &mut self.pro.inv.startup_table.filter, "Filter by name or command", 220.0, &pal);
        });
        let inv = &mut self.pro.inv;
        if refresh || (inv.startup.get().is_none() && !inv.startup.is_loading()) {
            inv.startup.start(crate::pro::inventory::startup_apps);
        }
        let area = page_area(body);
        let p = ui.painter().clone();
        let st = theme::status(&pal);
        let list: &[StartupApp] = inv.startup.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let enabled = list.iter().filter(|a| a.enabled).count();
        let folder = list.iter().filter(|a| a.location.starts_with("Startup folder")).count();
        let n = list.len().max(1) as f32;
        let tiles = [
            StatTile::new("Enabled", enabled.to_string(), format!("of {} startup entries", list.len())).bar(enabled as f32 / n, st.ok),
            StatTile::new("Disabled", (list.len() - enabled).to_string(), "Turned off in Windows settings").bar((list.len() - enabled) as f32 / n, st.warn),
            StatTile::new("Registry", (list.len() - folder).to_string(), "Run keys"),
            StatTile::new("Startup folders", folder.to_string(), "Shortcuts in Startup"),
        ];
        let used = kit::stat_grid(&p, area, &pal, &tiles, 190.0);
        let table_rect = Rect::from_min_max(pos2(area.left(), area.top() + used + BLOCK_GAP), area.max);
        if list.is_empty() {
            empty_state(&p, table_rect, &pal, inv.startup.error().unwrap_or("Reading startup entries\u{2026}"));
            return;
        }
        let st_table = &inv.startup_table;
        let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&st_table.filter, &[&list[*i].name, &list[*i].command, &list[*i].publisher])).collect();
        st_table.sort(list, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.publisher, &b.publisher),
            2 => a.enabled.cmp(&b.enabled),
            3 => a.location.cmp(&b.location),
            _ => cmp_str(&a.command, &b.command),
        });
        let cols = [Col::text("Name", 240.0), Col::text("Publisher", 220.0), Col::text("Status", 110.0), Col::text("Location", 220.0), Col::text("Command", 500.0)];
        let windir = std::env::var("windir").unwrap_or_default().to_lowercase();
        let key = |i: usize| format!("{}|{}", list[i].location, list[i].name);
        let cell = |i: usize, col: usize| -> Cell {
            let a = &list[i];
            match col {
                0 => {
                    let path = crate::pro::inventory::command_path(&a.command);
                    let cell = Cell::new(&a.name).icon(Some(path.as_str()));
                    if !windir.is_empty() && path.to_lowercase().starts_with(&windir) {
                        cell.badge("WINDOWS")
                    } else {
                        cell
                    }
                }
                1 => Cell::new(&a.publisher).dim(&pal),
                2 => {
                    let c = if a.enabled { st.ok } else { pal.text_faint };
                    Cell::new(if a.enabled { "Enabled" } else { "Disabled" }).dot(c).color(c)
                }
                3 => Cell::new(&a.location).dim(&pal),
                _ => Cell::new(&a.command).mono().color(pal.text_dim),
            }
        };
        let icons = &mut self.icons;
        table::show(ui, table_rect, "startup-table", &pal, &cols, &order, &mut inv.startup_table, &key, &cell, icons);
    }

    pub fn users_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let full = self.settings.general.show_full_account_name;
        let list = user_rows(fd.rows, full);
        let body = self.page_title(ui, &pal, "Users", &subtitle(&[format!("{} sessions", list.len()), Page::Users.shortcut().into()]));
        let mut refresh = false;
        header_toolbar(ui, body, 100.0, |ui| refresh = Button::new("Refresh").show(ui, &pal).clicked());
        if refresh {
            self.sampler.refresh();
        }
        let area = page_area(body);
        let p = ui.painter().clone();
        let interactive = list.iter().filter(|u| u.is_active()).count();
        let top = list.iter().max_by(|a, b| a.cpu.total_cmp(&b.cpu));
        let tiles = [
            StatTile::new("Accounts", list.len().to_string(), "Distinct account / session pairs"),
            StatTile::new("Signed in", interactive.to_string(), "Sessions with app windows"),
            StatTile::new("Busiest", top.map(|t| format!("{:.1}%", t.cpu)).unwrap_or_default(), top.map(|t| short_account(&t.user).to_string()).unwrap_or_default()).bar(top.map(|t| t.cpu / 100.0).unwrap_or(0.0), st.danger),
            StatTile::new("Processes", fd.rows.len().to_string(), format!("{} with windows", fd.rows.iter().filter(|r| r.has_window).count())),
        ];
        let used = kit::stat_grid(&p, area, &pal, &tiles, 190.0);

        let stt = &self.pro.inv.users_table;
        let mut order: Vec<usize> = (0..list.len()).filter(|i| table::matches(&stt.filter, &[&list[*i].user])).collect();
        let trends: Vec<Vec<f32>> = list.iter().map(|u| self.pro.inv.history.user_trend(u.session, |raw| if full { raw == u.user } else { short_account(raw) == u.user })).collect();
        stt.sort(&list, &mut order, |a, b, col| match col {
            0 => cmp_str(&a.user, &b.user),
            1 => a.session.cmp(&b.session),
            2 => a.procs.cmp(&b.procs),
            3 | 6 => cmp_f(a.cpu as f64, b.cpu as f64),
            4 => a.mem.cmp(&b.mem),
            5 => cmp_f(a.io, b.io),
            _ => a.is_active().cmp(&b.is_active()),
        });

        // The session card below the table: the selected row, else the first one.
        let key = |i: usize| format!("{}|{}", list[i].user, list[i].session);
        let shown = stt.selected.as_ref().and_then(|k| list.iter().position(|u| &key_of(u) == k)).or_else(|| order.first().copied());
        let apps = shown.map(|i| top_apps(fd.rows, full, &list[i].user, list[i].session)).unwrap_or_default();
        let card_cols = (((area.width() - 44.0 + 24.0) / (220.0 + 24.0)).floor() as usize).clamp(1, 3);
        let card_h = 18.0 + 22.0 + apps.len().max(1).div_ceil(card_cols) as f32 * 28.0 + 8.0;
        let table_bottom = if shown.is_some() { area.bottom() - card_h - BLOCK_GAP } else { area.bottom() };
        let table_rect = Rect::from_min_max(pos2(area.left(), area.top() + used + BLOCK_GAP), pos2(area.right(), table_bottom.max(area.top() + used + BLOCK_GAP + 120.0)));

        let cols = [Col::text("User", 280.0), Col::num("Session", 80.0), Col::num("Processes", 90.0), Col::num("CPU", 80.0), Col::num("Memory", 100.0), Col::num("Disk", 100.0), Col::spark("CPU \u{b7} 60s", 120.0), Col::text("Status", 180.0)];
        let spark_color = pal.accent;
        let cell = |i: usize, col: usize| -> Cell {
            let u = &list[i];
            match col {
                0 => {
                    let name = short_account(&u.user);
                    let cell = Cell::new(name);
                    let cell = if crate::process::is_system_account(Some(name)) { cell.badge(crate::process::os_badge()) } else { cell };
                    if full && name != u.user {
                        cell.sub(&u.user)
                    } else {
                        cell
                    }
                }
                1 => Cell::new(u.session.to_string()).color(pal.text_dim),
                2 => Cell::new(u.procs.to_string()),
                3 => Cell::new(format!("{:.1}%", u.cpu)),
                4 => Cell::new(format_bytes(u.mem as f64)),
                5 => Cell::new(format_rate(u.io)),
                6 => Cell::new("").spark(trends[i].clone(), spark_color, true),
                _ if u.is_active() => Cell::new("Active \u{b7} console").dot(st.ok).color(st.ok),
                _ if u.session == 0 && self.caps.proc.session => Cell::new("Services \u{b7} no window").color(pal.text_dim),
                _ => Cell::new("Background \u{b7} no window").color(pal.text_dim),
            }
        };
        let icons = &mut self.icons;
        table::show(ui, table_rect, "users-table", &pal, &cols, &order, &mut self.pro.inv.users_table, &key, &cell, icons);

        if let Some(i) = shown {
            let u = &list[i];
            let card = Rect::from_min_max(pos2(area.left(), area.bottom() - card_h), area.max);
            let content = section_card(&p, card, &pal, &format!("Session {} \u{b7} {} \u{b7} top apps", u.session, short_account(&u.user)), "");
            if apps.is_empty() {
                empty_state(&p, content, &pal, "No processes in this session.");
            } else {
                let col_w = (content.width() - 24.0 * (card_cols - 1) as f32) / card_cols as f32;
                for (n, a) in apps.iter().enumerate() {
                    let x = content.left() + (n % card_cols) as f32 * (col_w + 24.0);
                    let y = content.top() + (n / card_cols) as f32 * 28.0 + 9.0;
                    let right = format!("{:.1}% \u{b7} {}", a.cpu, format_bytes(a.mem as f64));
                    let right_w = widgets::text_width(&p, &right, theme::mono(12.0));
                    p.text(pos2(x + col_w, y), Align2::RIGHT_CENTER, &right, theme::mono(12.0), pal.text_table());
                    let name = widgets::text_fit(&p, pos2(x, y), Align2::LEFT_CENTER, &a.name, theme::ui(13.0), pal.text, (col_w - right_w - 28.0).max(40.0));
                    if a.count > 1 {
                        p.text(pos2(name.right() + 6.0, y), Align2::LEFT_CENTER, a.count.to_string(), theme::ui(13.0), pal.text_faint);
                    }
                }
            }
        }
    }

    pub fn services_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.pro.inv.services.poll();
        let count = self.pro.inv.services.ok().map(|l| format!("{} services", l.len())).unwrap_or_default();
        let body = self.page_title(ui, &pal, "Services", &subtitle(&[count, service_manager().into(),Page::Services.shortcut().into()]));
        let mut refresh = false;
        header_toolbar(ui, body, 320.0, |ui| {
            refresh = Button::new("Refresh").show(ui, &pal).clicked();
            filter_box(ui, &mut self.pro.inv.services_table.filter, "Filter by name or path", 200.0, &pal);
        });
        let inv = &mut self.pro.inv;
        if refresh || inv.services.stale(Duration::from_secs(15)) {
            inv.services.start(crate::pro::inventory::services);
        }
        let list: &[crate::pro::inventory::Service] = inv.services.ok().map(|v| v.as_slice()).unwrap_or(&[]);
        let open = driver_like_page(ui, page_area(body), fd, list, &mut inv.services_table, inv.services.error(), false, &mut self.icons, &mut inv.svc_info);
        if let Some(sel) = open {
            self.selected = Some(sel);
            self.page = Page::Processes;
        }
    }
}

/// An account name with `…` when it is too long for a legend.
fn clip_name(name: &str) -> String {
    if name.chars().count() > 22 {
        format!("{}\u{2026}", name.chars().take(21).collect::<String>())
    } else {
        name.to_string()
    }
}

/// Who runs the services: the source named on the Services page.
const fn service_manager() -> &'static str {
    use crate::platform::{Os, OS};
    match OS {
        Os::Linux => "systemd",
        Os::MacOs => "launchd",
        _ => "Service Control Manager",
    }
}

/// One row of the Users table: an account in one session.
#[derive(Default, Clone)]
struct UserRow {
    user: String,
    session: u32,
    procs: usize,
    cpu: f32,
    mem: u64,
    io: f64,
    apps: usize,
}

impl UserRow {
    /// Signed in with app windows (in an interactive session, where the OS has sessions).
    fn is_active(&self) -> bool {
        (self.session > 0 || !crate::process::caps().session) && self.apps > 0
    }
}

fn key_of(u: &UserRow) -> String {
    format!("{}|{}", u.user, u.session)
}

/// The account of a process as the Users page names it.
fn shown_account(r: &ProcessRow, full: bool) -> String {
    let raw = user_label(r);
    if full {
        raw
    } else {
        short_account(&raw).to_string()
    }
}

/// Totals per (account, session).
fn user_rows(rows: &[ProcessRow], full: bool) -> Vec<UserRow> {
    let mut by: HashMap<(String, u32), UserRow> = HashMap::new();
    for r in rows.iter().filter(|r| r.pid != 0) {
        let user = shown_account(r, full);
        let e = by.entry((user.clone(), r.session_id)).or_insert_with(|| UserRow { user, session: r.session_id, ..Default::default() });
        e.procs += 1;
        e.cpu += r.cpu_pct;
        e.mem += r.private_working_set;
        e.io += r.io_bps;
        e.apps += r.has_window as usize;
    }
    by.into_values().collect()
}

/// One application of a session in the session card.
#[derive(Clone, Debug, PartialEq)]
struct SessionApp {
    name: String,
    count: usize,
    cpu: f32,
    mem: u64,
}

/// The six busiest applications (processes grouped by image name) of one account's session.
fn top_apps(rows: &[ProcessRow], full: bool, user: &str, session: u32) -> Vec<SessionApp> {
    let mut groups: HashMap<String, SessionApp> = HashMap::new();
    for r in rows.iter().filter(|r| r.pid != 0 && r.session_id == session && shown_account(r, full) == user) {
        let e = groups.entry(r.name.to_lowercase()).or_insert_with(|| SessionApp { name: r.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| r.name.clone()), count: 0, cpu: 0.0, mem: 0 });
        e.count += 1;
        e.cpu += r.cpu_pct;
        e.mem += r.private_working_set;
    }
    let mut apps: Vec<SessionApp> = groups.into_values().collect();
    apps.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)).then_with(|| cmp_str(&a.name, &b.name)));
    apps.truncate(6);
    apps
}

/// Parts joined by a space, without the empty ones.
fn subtitle_free(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" ")
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

    fn user_row(pid: u32, name: &str, user: &str, session: u32, cpu: f32, mem: u64) -> ProcessRow {
        ProcessRow { pid, name: name.into(), user: Some(user.into()), session_id: session, cpu_pct: cpu, private_working_set: mem, ..Default::default() }
    }

    /// The Users page's trend adds up the CPU use of an account per session and survives a reset.
    #[test]
    fn user_trends_follow_sessions() {
        let mut h = AppHistory::default();
        h.update(&[user_row(10, "a.exe", r"PC\alex", 1, 2.0, 1), user_row(11, "b.exe", r"PC\alex", 1, 3.0, 1), user_row(12, "s.exe", r"NT AUTHORITY\SYSTEM", 0, 1.0, 1)], 1.0);
        h.update(&[user_row(10, "a.exe", r"PC\alex", 1, 4.0, 1), user_row(12, "s.exe", r"NT AUTHORITY\SYSTEM", 0, 1.0, 1)], 1.0);
        assert_eq!(h.user_trend(1, |u| short_account(u) == "alex"), vec![5.0, 4.0]);
        assert_eq!(h.user_trend(0, |u| u == r"NT AUTHORITY\SYSTEM"), vec![1.0, 1.0]);
        assert!(h.user_trend(1, |u| u == "alex").is_empty(), "full names must match exactly");
        h.reset();
        assert!(h.apps.is_empty() && h.since.is_none());
        assert_eq!(h.user_cpu.len(), 2, "resetting the app history keeps the user trends");
        // A session that is gone loses its series.
        h.update(&[user_row(12, "s.exe", r"NT AUTHORITY\SYSTEM", 0, 1.0, 1)], 1.0);
        assert_eq!(h.user_cpu.len(), 1);
    }

    #[test]
    fn session_card_lists_the_busiest_apps() {
        let mut rows = vec![user_row(1, "chrome.exe", r"PC\alex", 1, 5.0, 100), user_row(2, "chrome.exe", r"PC\alex", 1, 6.0, 100), user_row(3, "idle.exe", r"PC\alex", 1, 0.0, 500), user_row(4, "x.exe", r"PC\bob", 1, 50.0, 1)];
        rows[0].description = Some("Google Chrome".into());
        let apps = top_apps(&rows, false, "alex", 1);
        assert_eq!(apps, vec![SessionApp { name: "Google Chrome".into(), count: 2, cpu: 11.0, mem: 200 }, SessionApp { name: "idle.exe".into(), count: 1, cpu: 0.0, mem: 500 }]);
        assert!(top_apps(&rows, true, "alex", 1).is_empty(), "full names are matched as they are");
        let all = user_rows(&rows, false);
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().find(|u| u.user == "bob").map(|u| u.procs), Some(1));
        let many: Vec<ProcessRow> = (0..9).map(|i| user_row(i + 1, &format!("p{i}.exe"), "a", 1, i as f32, 1)).collect();
        assert_eq!(top_apps(&many, false, "a", 1).len(), 6);
    }

    #[test]
    fn boot_time_is_a_weekday_and_clock() {
        // 2024-01-01 was a Monday.
        assert_eq!(boot_label((2024, 1, 1, 10, 0, 0), Duration::from_secs(11 * 3600)), "Sun 23:00");
        assert_eq!(boot_label((2024, 3, 1, 9, 41, 59), Duration::from_secs(0)), "Fri 09:41");
        assert_eq!(boot_label((1970, 1, 1, 0, 0, 0), Duration::from_secs(0)), "Thu 00:00");
        assert_eq!(days_from_civil(2024, 1, 1), 19_723);
    }

    #[test]
    fn report_and_layout_helpers() {
        let sections: Sections = vec![("Device", vec![("Computer name".into(), "PC".into())]), ("Hardware", vec![("Model".into(), "X".into()), ("BIOS".into(), "1".into())])];
        assert_eq!(report_text(&sections), "Device\n  Computer name: PC\n\nHardware\n  Model: X\n  BIOS: 1\n");
        assert!(is_mono("Operating system", "OS build") && !is_mono("Hardware", "Manufacturer"));
        assert!(is_mono("Display adapters", "NVIDIA GeForce"));
        let (at, total) = masonry(&[100.0, 50.0, 60.0, 30.0], 2);
        assert_eq!(at, vec![(0, 0.0), (1, 0.0), (1, 64.0), (0, 114.0)]);
        assert_eq!(total, 144.0);
        assert_eq!(masonry(&[], 3).1, 0.0);
        assert_eq!(tiles_height(1000.0, 4, 190.0), STAT_TILE_H);
        assert_eq!(tiles_height(300.0, 4, 190.0), 4.0 * STAT_TILE_H + 3.0 * STAT_GAP);
        assert_eq!(subtitle(&["a".into(), String::new(), "b".into()]), "a \u{b7} b");
        assert!(crate::process::is_system_account(Some(if cfg!(windows) { "system" } else { "root" })) && !crate::process::is_system_account(Some("alex")));
    }
}
