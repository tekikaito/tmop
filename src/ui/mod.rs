//! eframe/egui front end: window chrome (menu bar, sidebar, page header, status bar),
//! the pages, and the skinnable popout window.

pub mod bench_page;
pub mod icons;
pub mod inventory_pages;
pub mod kit;
pub mod performance;
pub mod popout;
pub mod pro_pages;
pub mod processes;
pub mod settings_page;
pub mod summary;
pub mod table;
pub mod theme;
pub mod widgets;

use crate::metrics::{history_capacity, History, Sampler, Snapshot};
use crate::pro::{bench::Bench, recorder::Recorder};
use crate::process::{self, Action, Column, Priority, ProcessRow};
use crate::settings::{self, Settings};
use crate::skin::{self, SkinCatalog};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Key, Pos2, Rect, Sense, Stroke, Vec2, ViewportBuilder, ViewportCommand, ViewportId};
use std::time::{Duration, Instant};
use theme::Palette;
use widgets::{draw_icon, Graph, GraphLine, Icon};

pub const SIDEBAR_W: f32 = 216.0;
pub const STATUS_H: f32 = 30.0;
pub const HEADER_H: f32 = 52.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Summary,
    Performance,
    Processes,
    SystemInfo,
    AppHistory,
    StartupApps,
    Users,
    Services,
    PowerFreq,
    FlightRecorder,
    Connections,
    InstalledApps,
    Drivers,
    DiskSpace,
    Benchmarks,
    Settings,
}

impl Page {
    pub const NAV: [Page; 15] = [
        Page::Summary,
        Page::Performance,
        Page::Processes,
        Page::SystemInfo,
        Page::AppHistory,
        Page::StartupApps,
        Page::Users,
        Page::Services,
        Page::PowerFreq,
        Page::FlightRecorder,
        Page::Connections,
        Page::InstalledApps,
        Page::Drivers,
        Page::DiskSpace,
        Page::Benchmarks,
    ];

    /// Page ids (`summary`, `performance`, `processes`, `power-and-freq`, ...).
    pub fn from_id(id: &str) -> Self {
        match id {
            "performance" => Page::Performance,
            "processes" | "details" => Page::Processes,
            "system-info" => Page::SystemInfo,
            "app-history" => Page::AppHistory,
            "startup" | "startup-apps" => Page::StartupApps,
            "users" => Page::Users,
            "services" => Page::Services,
            "power-and-freq" | "power" => Page::PowerFreq,
            "flight-recorder" | "recording" => Page::FlightRecorder,
            "connections" => Page::Connections,
            "installed-apps" => Page::InstalledApps,
            "drivers" | "kernel-modules" => Page::Drivers,
            "disk-space" => Page::DiskSpace,
            "benchmarks" => Page::Benchmarks,
            "settings" => Page::Settings,
            _ => Page::Summary,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Page::Summary => "summary",
            Page::Performance => "performance",
            Page::Processes => "processes",
            Page::SystemInfo => "system-info",
            Page::AppHistory => "app-history",
            Page::StartupApps => "startup-apps",
            Page::Users => "users",
            Page::Services => "services",
            Page::PowerFreq => "power-and-freq",
            Page::FlightRecorder => "flight-recorder",
            Page::Connections => "connections",
            Page::InstalledApps => "installed-apps",
            Page::Drivers => "drivers",
            Page::DiskSpace => "disk-space",
            Page::Benchmarks => "benchmarks",
            Page::Settings => "settings",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Page::Summary => "Summary",
            Page::Performance => "Performance",
            Page::Processes => "Processes",
            Page::SystemInfo => "System Info",
            Page::AppHistory => "App history",
            Page::StartupApps => "Startup apps",
            Page::Users => "Users",
            Page::Services => "Services",
            Page::PowerFreq => "Power & Freq",
            Page::FlightRecorder => "Flight Recorder",
            Page::Connections => "Connections",
            Page::InstalledApps => "Installed Apps",
            Page::Drivers => "Drivers",
            Page::DiskSpace => "Disk Space",
            Page::Benchmarks => "Benchmarks",
            Page::Settings => "Settings",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Page::Summary => Icon::Home,
            Page::Performance => Icon::Chart,
            Page::Processes => Icon::List,
            Page::SystemInfo => Icon::Info,
            Page::AppHistory => Icon::History,
            Page::StartupApps => Icon::Play,
            Page::Users => Icon::User,
            Page::Services => Icon::Wrench,
            Page::PowerFreq => Icon::Bolt,
            Page::FlightRecorder => Icon::Record,
            Page::Connections => Icon::Globe,
            Page::InstalledApps => Icon::Apps,
            Page::Drivers => Icon::Driver,
            Page::DiskSpace => Icon::Disk,
            Page::Benchmarks => Icon::Gauge,
            Page::Settings => Icon::Gear,
        }
    }
    fn shortcut(self) -> &'static str {
        match self {
            Page::Summary => "Ctrl+1",
            Page::Performance => "Ctrl+2",
            Page::Processes => "Ctrl+3",
            Page::SystemInfo => "Ctrl+4",
            Page::AppHistory => "Ctrl+5",
            Page::StartupApps => "Ctrl+6",
            Page::Users => "Ctrl+7",
            Page::Services => "Ctrl+8",
            Page::Benchmarks => "Ctrl+9",
            Page::DiskSpace => "Ctrl+0",
            Page::Settings => "Ctrl+,",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Memory,
    Gpu,
    Disk,
    Network,
    Thermal,
}

impl Device {
    pub const ALL: [Device; 6] = [Device::Cpu, Device::Memory, Device::Gpu, Device::Disk, Device::Network, Device::Thermal];
    pub fn key(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Memory => "memory",
            Device::Disk => "disk",
            Device::Network => "network",
            Device::Gpu => "gpu",
            Device::Thermal => "thermal",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Device::Cpu => "CPU",
            Device::Memory => "Memory",
            Device::Disk => "Disk",
            Device::Network => "Network",
            Device::Gpu => "GPU",
            Device::Thermal => "Thermals",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Device::Cpu => Icon::Cpu,
            Device::Memory => Icon::Memory,
            Device::Disk => Icon::Disk,
            Device::Network => Icon::Network,
            Device::Gpu => Icon::Gpu,
            Device::Thermal => Icon::Thermal,
        }
    }
    pub fn color(self) -> Color32 {
        theme::metric_color(self.key())
    }
}

/// Values eased between data updates ("Smooth graph motion and gauge needles").
#[derive(Clone, Debug, Default)]
pub struct Smooth {
    pub cpu: f32,
    pub kernel: f32,
    pub memory: f32,
    pub disk: f32,
    pub gpu: f32,
    pub thermal: f32,
    pub clock: f32,
    pub net: f32,
    pub per_core: Vec<f32>,
    pub per_core_mhz: Vec<f32>,
}

impl Smooth {
    fn step(&mut self, s: &Snapshot, dt: f32, eased: bool) {
        let k = if eased { 1.0 - (-dt * 9.0).exp() } else { 1.0 };
        let ease = |cur: &mut f32, target: f32| *cur += (target - *cur) * k;
        ease(&mut self.cpu, s.cpu.total_pct);
        ease(&mut self.kernel, s.cpu.kernel_pct);
        ease(&mut self.memory, s.memory.used_pct);
        ease(&mut self.disk, s.disk.active_pct);
        ease(&mut self.gpu, s.gpu.utilization_pct);
        ease(&mut self.thermal, s.thermal.max_c().unwrap_or(0.0));
        ease(&mut self.clock, s.cpu.current_mhz.unwrap_or(0) as f32);
        ease(&mut self.net, s.network.utilization_pct);
        if self.per_core.len() != s.cpu.per_core_pct.len() {
            self.per_core = s.cpu.per_core_pct.clone();
        }
        for (c, t) in self.per_core.iter_mut().zip(&s.cpu.per_core_pct) {
            ease(c, *t);
        }
        if self.per_core_mhz.len() != s.cpu.per_core_mhz.len() {
            self.per_core_mhz = s.cpu.per_core_mhz.iter().map(|m| *m as f32).collect();
        }
        for (c, t) in self.per_core_mhz.iter_mut().zip(&s.cpu.per_core_mhz) {
            ease(c, *t as f32);
        }
    }
}

/// Everything a page needs for one frame.
pub struct FrameData<'a> {
    pub snap: &'a Snapshot,
    pub hist: &'a History,
    pub rows: &'a [ProcessRow],
    pub smooth: &'a Smooth,
    pub pal: Palette,
    pub bloom: f32,
    pub phase: f32,
    /// Graphs glide the newest sample in over the update interval (high-frequency visuals on).
    pub glide: bool,
    pub time: f64,
    /// `showSelfInReports`: list TMOP itself in process lists.
    pub show_self: bool,
}

/// Command-line launch options.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub start_page: Option<Page>,
    /// cpu, memory, gpu, disk, network or thermal
    pub start_device: Option<String>,
    pub popout_only: bool,
    /// Start recording at launch: (folder, seconds).
    pub record: Option<(Option<std::path::PathBuf>, Option<u64>)>,
    /// Run these benchmarks at launch (`--bench=all` or comma-separated test ids).
    pub bench: Option<Vec<crate::pro::bench::Test>>,
    /// Start a Disk Space scan of this folder at launch.
    pub disk_scan: Option<std::path::PathBuf>,
}

struct Pending {
    action: Action,
    row: ProcessRow,
    question: String,
}

pub struct App {
    pub settings: Settings,
    pub settings_dirty: bool,
    pub catalog: SkinCatalog,
    pub sampler: Sampler,
    pub page: Page,
    pub device: Device,
    pub popout_open: bool,
    popout_only: bool,
    pub filter: String,
    pub sort: (Column, bool),
    pub selected: Option<(u32, i64)>,
    pending: Option<Pending>,
    pub status: String,
    applied_top: Option<bool>,
    last_seq: u64,
    last_sample_at: Instant,
    last_frame_at: Instant,
    pub smooth: Smooth,
    pub popout_textures: popout::Textures,
    pub recorder: Recorder,
    pub bench: Bench,
    pub bench_ui: bench_page::BenchUi,
    pub pro: pro_pages::ProState,
    pub icons: icons::IconCache,
    pub proc: processes::ProcState,
    status_seen: (String, Instant),
    ip: (Option<String>, Option<Instant>),
    profile: Option<Profile>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings, status: String, launch: Launch) -> Self {
        let popout_only = launch.popout_only;
        theme::install_fonts(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let sampler = Sampler::start(settings.update_speed().interval(), history_capacity(settings.graph_history_multiplier), move || ctx.request_repaint());
        let catalog = skin::scan(&skin::default_skin_dirs());
        let page = launch.start_page.unwrap_or_else(|| Page::from_id(&settings.default_page));
        let device = launch.start_device.as_deref().and_then(|d| Device::ALL.into_iter().find(|x| x.key() == d)).unwrap_or(Device::Cpu);
        // A normal settings load needs no message.
        let mut status = if status.ends_with("(Loaded)") { String::new() } else { status };
        if !catalog.errors.is_empty() {
            status = format!("{} skin(s) failed to load", catalog.errors.len());
        }
        let settings_popout_on = settings.extra.get("popoutMode").and_then(|v| v.as_str()).map(|m| m != "off").unwrap_or(false);
        let app = Self {
            settings,
            settings_dirty: false,
            catalog,
            sampler,
            page,
            device,
            popout_open: popout_only || settings_popout_on,
            popout_only,
            filter: String::new(),
            sort: (Column::Cpu, true),
            selected: None,
            pending: None,
            status,
            applied_top: None,
            last_seq: 0,
            last_sample_at: Instant::now(),
            last_frame_at: Instant::now(),
            smooth: Smooth::default(),
            popout_textures: popout::Textures::default(),
            recorder: Recorder::default(),
            bench: Bench::default(),
            bench_ui: bench_page::BenchUi { table: table::TableState::sorted_by(0, true), ..Default::default() },
            pro: pro_pages::ProState::default(),
            icons: icons::IconCache::new(&cc.egui_ctx),
            proc: processes::ProcState::default(),
            status_seen: (String::new(), Instant::now()),
            ip: (None, None),
            profile: std::env::var_os("TMOP_PROFILE").map(|_| Profile::default()),
        };
        let mut app = app;
        if let Some(dir) = app.settings.extra.get("recordingFolder").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            app.recorder.dir = std::path::PathBuf::from(dir);
        }
        if let Some((dir, secs)) = launch.record {
            if let Some(d) = dir {
                app.recorder.dir = d;
            }
            if let Err(e) = app.recorder.start(secs.map(Duration::from_secs)) {
                app.status = format!("Recording: {e}");
            }
        }
        if let Some(tests) = launch.bench {
            app.bench.start(tests, std::env::temp_dir());
        }
        if let Some(root) = launch.disk_scan {
            app.pro.disk_root = root.display().to_string();
            app.pro.scanner.start(root);
        }
        app.palette().apply(&cc.egui_ctx);
        app
    }

    pub fn palette(&self) -> Palette {
        match self.settings.theme.as_str() {
            "light" => theme::LIGHT,
            "dark" => theme::DARK,
            _ => {
                // "system": follow the OS preference when known.
                if matches!(dark_light_system(), Some(false)) {
                    theme::LIGHT
                } else {
                    theme::DARK
                }
            }
        }
    }

    pub fn bloom(&self) -> f32 {
        if self.settings.visual_bloom {
            (self.settings.visual_bloom_level.min(10) as f32 / 10.0).max(0.1)
        } else {
            0.0
        }
    }

    /// A graph configured from the settings (history compression, smoothing, bloom).
    pub fn graph<'a>(&self, lines: Vec<GraphLine<'a>>, fd: &FrameData<'_>) -> Graph<'a> {
        let mut lines = lines;
        if !self.settings.color_keyed_graphs {
            // Without colour keying every trace uses one neutral blue, later traces dimmer.
            for (i, l) in lines.iter_mut().enumerate() {
                l.color = theme::dim(Color32::from_rgb(0x4F, 0xB4, 0xFF), 1.0 - 0.3 * i.min(2) as f32);
            }
        }
        let mut g = Graph::new(lines);
        g.px_per_update = self.settings.graph_pixels_per_update.clamp(2, 64) as f32;
        g.phase = fd.phase;
        g.glide = fd.glide;
        g.regions = widgets::regions_for(self.settings.graph_history_multiplier, self.settings.compressed_graph_time);
        g.smooth = self.settings.smooth_graph_curves;
        g.bloom = fd.bloom;
        g.grid = Some(fd.pal.grid);
        g.background = Some(fd.pal.graph_bg());
        g
    }

    fn snapshot(&self) -> (std::sync::Arc<Snapshot>, std::sync::Arc<History>, std::sync::Arc<Vec<ProcessRow>>) {
        let g = self.sampler.shared.lock().expect("sampler lock");
        (g.latest.clone(), g.history.clone(), g.processes.clone())
    }

    pub fn save_settings(&mut self) {
        if let Some(p) = &mut self.profile {
            p.saves += 1;
        }
        match self.settings.save(&settings::default_path()) {
            Ok(()) => {}
            Err(e) => self.status = format!("Could not save settings: {e}"),
        }
        self.settings_dirty = false;
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let mut go = None;
        ctx.input(|i| {
            if i.modifiers.ctrl || i.modifiers.command {
                for (k, p) in [
                    (Key::Num1, Page::Summary),
                    (Key::Num2, Page::Performance),
                    (Key::Num3, Page::Processes),
                    (Key::Num4, Page::SystemInfo),
                    (Key::Num5, Page::AppHistory),
                    (Key::Num6, Page::StartupApps),
                    (Key::Num7, Page::Users),
                    (Key::Num8, Page::Services),
                    (Key::Num9, Page::Benchmarks),
                    (Key::Num0, Page::DiskSpace),
                    (Key::Comma, Page::Settings),
                ] {
                    if i.key_pressed(k) {
                        go = Some(p);
                    }
                }
            }
            if i.key_pressed(Key::F5) {
                self.sampler.refresh();
            }
        });
        if let Some(p) = go {
            self.page = p;
        }
    }

    pub fn temp(&self, c: f32) -> String {
        if self.settings.fahrenheit() {
            format!("{:.0} \u{b0}F", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0} \u{b0}C")
        }
    }

    pub fn current_skin(&self) -> skin::Skin {
        match self.catalog.get(&self.settings.popout_skin) {
            Some(s) if s.source != skin::SkinSource::BuiltIn || s.id != "classic" => s.clone(),
            _ => popout::classic_skin(&self.palette()),
        }
    }

    // ------------------------------------------------------------------ chrome

    fn menu_bar(&mut self, ctx: &egui::Context, pal: &Palette) {
        let frame = egui::Frame::none().fill(pal.chrome).inner_margin(egui::Margin::symmetric(6.0, 2.0));
        egui::TopBottomPanel::top("menu").frame(frame).show_separator_line(false).show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Refresh now\tF5").clicked() {
                        self.sampler.refresh();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    for p in Page::NAV.iter().chain([Page::Settings].iter()) {
                        if ui.button(format!("{}\t{}", p.title(), p.shortcut())).clicked() {
                            self.page = *p;
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    let label = if self.popout_open { "Popout: On" } else { "Popout: Off" };
                    if ui.button(label).clicked() {
                        let open = !self.popout_open;
                        self.set_popout(open);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    ui.label(format!("{} ({}): a skinnable system monitor for Windows.", crate::APP_NAME, crate::APP_LONG_NAME));
                    ui.label("No network access, no licensing, no telemetry.");
                });
            });
        });
    }

    fn sidebar(&mut self, ctx: &egui::Context, pal: &Palette) {
        let frame = egui::Frame::none().fill(pal.sidebar);
        egui::SidePanel::left("sidebar").exact_width(SIDEBAR_W).resizable(false).frame(frame).show_separator_line(false).show(ctx, |ui| {
            let full = ui.max_rect();
            let p = ui.painter();
            p.line_segment([pos2(full.right() - 0.5, full.top()), pos2(full.right() - 0.5, full.bottom())], Stroke::new(1.0_f32, pal.sidebar_rule));
            // The nav list gets the space above the bottom controls. The wordmark is shown only when
            // the list fits above it too; if the list doesn't fit at all, it scrolls (mouse wheel).
            let bottom = full.bottom();
            let nav_h = 14.0 + Page::NAV.len() as f32 * 42.0;
            let show_logo = full.top() + nav_h <= bottom - 136.0;
            let nav_rect = Rect::from_min_max(full.min, pos2(full.right(), if show_logo { bottom - 136.0 } else { bottom - 56.0 }));
            let scroll_id = ui.id().with("nav-scroll");
            let max_scroll = (nav_h - nav_rect.height()).max(0.0);
            let mut scroll = ui.data(|d| d.get_temp::<f32>(scroll_id)).unwrap_or(0.0);
            if max_scroll > 0.0 && ui.rect_contains_pointer(nav_rect) {
                scroll -= ui.input(|i| i.smooth_scroll_delta.y);
            }
            scroll = scroll.clamp(0.0, max_scroll);
            ui.data_mut(|d| d.insert_temp(scroll_id, scroll));
            let nav_painter = ui.painter().with_clip_rect(nav_rect);
            // One plain list of all pages.
            let mut y = full.top() + 14.0 - scroll;
            for page in Page::NAV {
                let rect = Rect::from_min_size(pos2(full.left() + 12.0, y), vec2(SIDEBAR_W - 26.0, 36.0));
                y += 42.0;
                let hit = rect.intersect(nav_rect);
                if !hit.is_positive() {
                    continue;
                }
                let resp = ui.interact(hit, ui.id().with(page.id()), Sense::click());
                let selected = self.page == page;
                let p = &nav_painter;
                if selected {
                    p.rect_filled(rect, 4.0, pal.selection);
                } else if resp.hovered() {
                    p.rect_filled(rect, 4.0, pal.hover);
                }
                let fg = if selected { Color32::WHITE } else { pal.text };
                draw_icon(p, Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(16.0, 16.0)), page.icon(), fg);
                p.text(pos2(rect.left() + 50.0, rect.center().y), Align2::LEFT_CENTER, page.title(), theme::ui(13.0), fg);
                if resp.clicked() {
                    self.page = page;
                }
                resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            // Wordmark and bottom controls.
            let p = ui.painter();
            if show_logo {
                let dark = pal.sidebar.r() as u32 + pal.sidebar.g() as u32 + pal.sidebar.b() as u32 <= 3 * 96;
                wordmark(p, Rect::from_min_size(pos2(full.left() + 18.0, bottom - 128.0), vec2(SIDEBAR_W - 36.0, 48.0)), self.bloom(), ui.input(|i| i.time), dark);
            }
            let settings_rect = Rect::from_min_size(pos2(full.left() + 10.0, bottom - 46.0), vec2(96.0, 32.0));
            let resp = ui.interact(settings_rect, ui.id().with("nav-settings"), Sense::click());
            let p = ui.painter();
            if self.page == Page::Settings {
                p.rect_filled(settings_rect, 4.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(settings_rect, 4.0, pal.hover);
            }
            draw_icon(p, Rect::from_center_size(pos2(settings_rect.left() + 20.0, settings_rect.center().y), vec2(15.0, 15.0)), Icon::Gear, pal.text);
            p.text(pos2(settings_rect.left() + 36.0, settings_rect.center().y), Align2::LEFT_CENTER, "Settings", theme::ui(13.0), pal.text);
            if resp.clicked() {
                self.page = Page::Settings;
            }
            p.line_segment([pos2(full.left() + 110.5, bottom - 40.0), pos2(full.left() + 110.5, bottom - 20.0)], Stroke::new(1.0_f32, pal.sidebar_rule));
            let pop_rect = Rect::from_min_size(pos2(full.left() + 116.0, bottom - 46.0), vec2(88.0, 32.0));
            let resp = ui.interact(pop_rect, ui.id().with("nav-popout"), Sense::click());
            let p = ui.painter();
            if self.popout_open {
                p.rect_filled(pop_rect, 4.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(pop_rect, 4.0, pal.hover);
            }
            draw_icon(p, Rect::from_center_size(pos2(pop_rect.left() + 18.0, pop_rect.center().y), vec2(15.0, 15.0)), Icon::Popout, pal.text);
            p.text(pos2(pop_rect.left() + 34.0, pop_rect.center().y), Align2::LEFT_CENTER, "Popout", theme::ui(13.0), pal.text);
            if resp.clicked() {
                let open = !self.popout_open;
                self.set_popout(open);
            }
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context, pal: &Palette, snap: &Snapshot) {
        // Transient messages fade out after a while; the provider line stays.
        if self.status != self.status_seen.0 {
            self.status_seen = (self.status.clone(), Instant::now());
        }
        if self.ip.1.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true) {
            self.ip = (crate::pro::connections::primary_ipv4().map(|a| a.to_string()), Some(Instant::now()));
        }
        let frame = egui::Frame::none().fill(pal.chrome);
        egui::TopBottomPanel::bottom("status").exact_height(STATUS_H).frame(frame).show_separator_line(false).show(ctx, |ui| {
            let r = ui.max_rect();
            let p = ui.painter();
            p.rect_filled(Rect::from_min_max(r.min, pos2(r.left() + SIDEBAR_W, r.bottom())), 0.0, pal.sidebar);
            let healthy = snap.errors.is_empty();
            let dot = if healthy { pal.ok } else { pal.warn };
            let mut x = r.left() + SIDEBAR_W + 16.0;
            p.circle_filled(pos2(x, r.center().y), 3.0, dot);
            x += 10.0;
            let mut parts: Vec<(String, Color32)> = vec![
                (if healthy { "Native providers healthy".to_string() } else { snap.errors[0].clone() }, pal.text_dim),
                (format!("{} processes", snap.process_count), pal.text_dim),
            ];
            if let Some(ip) = &self.ip.0 {
                parts.push((format!("IP {ip}"), pal.accent));
            }
            parts.push((format!("Generation {}", snap.seq), pal.text_dim));
            if self.settings.uptime_visible {
                parts.push((format!("Up {}", fmt_hms(snap.uptime)), pal.text_dim));
            }
            if !self.status_seen.0.is_empty() && self.status_seen.1.elapsed() < Duration::from_secs(10) {
                parts.push((self.status_seen.0.clone(), pal.text));
            }
            let right_reserved = 300.0;
            let clip = p.with_clip_rect(Rect::from_min_max(r.min, pos2(r.right() - right_reserved, r.bottom())));
            for (i, (s, c)) in parts.iter().enumerate() {
                if i > 0 {
                    clip.text(pos2(x, r.center().y), Align2::LEFT_CENTER, "\u{2022}", theme::ui(12.0), pal.text_faint);
                    x += 12.0;
                }
                let rr = clip.text(pos2(x, r.center().y), Align2::LEFT_CENTER, s, theme::ui(12.0), *c);
                x += rr.width() + 10.0;
            }
            // Recorder transport and clock.
            let rec = self.recorder.is_recording();
            let clock = match self.recorder.elapsed() {
                Some(d) => fmt_clock(d),
                None => "00:00:00.00".to_string(),
            };
            let h = 15.0;
            let cw = widgets::seven_segment_width(&clock, h);
            let lcd = Rect::from_min_size(pos2(r.right() - 14.0 - cw - 14.0, r.center().y - 11.0), vec2(cw + 14.0, 22.0));
            p.rect_filled(lcd, 3.0, Color32::from_rgb(0x0A, 0x16, 0x26));
            p.rect_stroke(lcd, 3.0, Stroke::new(1.0_f32, Color32::from_rgb(0x1A, 0x2C, 0x44)));
            let lit = if rec { Color32::from_rgb(0xFF, 0x6A, 0x6A) } else { Color32::from_rgb(0x9C, 0xB8, 0xD8) };
            widgets::seven_segment(p, pos2(lcd.left() + 7.0, lcd.top() + 3.5), &clock, h, lit, Color32::from_rgb(0x16, 0x26, 0x3A), if rec { self.bloom() } else { 0.0 });
            let lap = pos2(lcd.left() - 18.0, r.center().y);
            p.circle_filled(lap, 3.5, Color32::from_rgb(0x36, 0x36, 0x36));
            let stop = Rect::from_center_size(pos2(lap.x - 34.0, r.center().y), vec2(24.0, 20.0));
            let stop_resp = ui.interact(stop, ui.id().with("status-stop"), Sense::click());
            let p = ui.painter();
            p.rect_filled(stop, 3.0, if stop_resp.hovered() && rec { pal.hover } else { Color32::from_rgb(0x26, 0x26, 0x26) });
            p.rect_filled(Rect::from_center_size(stop.center(), vec2(9.0, 9.0)), 1.0, if rec { pal.text } else { Color32::from_rgb(0x3A, 0x3A, 0x3A) });
            p.line_segment([pos2(stop.right() + 9.5, r.top() + 6.0), pos2(stop.right() + 9.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            let ball = pos2(stop.left() - 26.0, r.center().y);
            let rec_resp = ui.interact(Rect::from_center_size(ball, vec2(22.0, 22.0)), ui.id().with("status-rec"), Sense::click());
            let p = ui.painter();
            let base = if rec { Color32::from_rgb(0xE0, 0x20, 0x20) } else { Color32::from_rgb(0x6A, 0x18, 0x18) };
            if rec && self.bloom() > 0.0 {
                p.circle_filled(ball, 13.0, theme::with_alpha(base, 0.22 * self.bloom()));
            }
            p.circle_filled(ball, 8.0, theme::dim(base, 0.75));
            p.circle_filled(ball + vec2(-1.0, -1.0), 6.5, base);
            p.circle_filled(ball + vec2(-2.5, -2.5), 2.5, theme::with_alpha(Color32::WHITE, if rec { 0.45 } else { 0.18 }));
            if rec_resp.hovered() {
                p.circle_stroke(ball, 9.5, Stroke::new(1.0_f32, pal.text_dim));
            }
            p.line_segment([pos2(ball.x - 17.5, r.top() + 6.0), pos2(ball.x - 17.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            p.line_segment([pos2(ball.x + 17.5, r.top() + 6.0), pos2(ball.x + 17.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            if rec_resp.clicked() && !rec {
                if let Err(e) = self.recorder.start(None) {
                    self.status = format!("Recording: {e}");
                }
            }
            if stop_resp.clicked() && rec {
                if let Some(path) = self.recorder.stop() {
                    self.status = format!("Recording saved to {}", path.display());
                }
            }
            rec_resp.on_hover_text(if rec { "Recording" } else { "Start recording to the recordings folder" });
            stop_resp.on_hover_text("Stop recording");
        });
    }
    /// Page title row inside the content area. Returns the rect left for the page body.
    pub fn page_header(&self, ui: &mut egui::Ui, pal: &Palette, title: &str, icon: Icon) -> Rect {
        let full = ui.max_rect();
        let header = Rect::from_min_size(full.min, vec2(full.width(), HEADER_H));
        let p = ui.painter();
        p.rect_filled(header, 0.0, pal.chrome);
        draw_icon(p, Rect::from_center_size(pos2(header.left() + 36.0, header.center().y + 2.0), vec2(16.0, 16.0)), icon, pal.text_dim);
        p.text(pos2(header.left() + 60.0, header.center().y + 1.0), Align2::LEFT_CENTER, title, theme::display(26.0), pal.text);
        Rect::from_min_max(pos2(full.left(), header.bottom()), full.max)
    }

    // ------------------------------------------------------------------ process actions

    pub fn request(&mut self, action: Action, row: ProcessRow) {
        let question = match action {
            Action::EndTask => Some(format!("Force quit {} ({})? Unsaved data will be lost.", row.name, row.pid)),
            Action::EndTree => Some(format!("End the process tree of {} ({})? All descendant processes will also be asked to end.", row.name, row.pid)),
            Action::Efficiency(true) if self.settings.confirm_efficiency_mode => {
                Some(format!("Turn on efficiency mode for {}? It lowers its priority and may make it less responsive.", row.name))
            }
            Action::SetPriority(Priority::Realtime) => Some("Realtime priority can make the system unresponsive. Continue?".into()),
            _ => None,
        };
        match question {
            Some(q) => self.pending = Some(Pending { action, row, question: q }),
            None => self.execute(action, &row),
        }
    }

    fn execute(&mut self, action: Action, row: &ProcessRow) {
        #[cfg(windows)]
        {
            let rows = self.snapshot().2;
            self.status = match process::actions::run(action, row, &rows) {
                Ok(m) => m,
                Err(e) => format!("{}: {e}", row.name),
            };
            self.sampler.refresh();
        }
        #[cfg(not(windows))]
        {
            let _ = (action, row);
            self.status = "Process actions are only implemented on Windows".into();
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(p) = &self.pending else { return };
        let mut answer = None;
        let question = p.question.clone();
        egui::Window::new("Confirm")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).fill(pal.panel))
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(question);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Yes", pal, None).clicked() {
                        answer = Some(true);
                    }
                    if widgets::button(ui, "Cancel", pal, None).clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(yes) = answer {
            let p = self.pending.take().expect("pending");
            if yes {
                self.execute(p.action, &p.row);
            }
        }
    }

    // ------------------------------------------------------------------ popout viewport

    fn popout_viewport(&mut self, ctx: &egui::Context, fd: &FrameData<'_>) {
        let skin = self.current_skin();
        let level = match self.popout_mode().as_str() {
            "desktop" => egui::WindowLevel::AlwaysOnBottom,
            "normal" => egui::WindowLevel::Normal,
            _ => egui::WindowLevel::AlwaysOnTop,
        };
        let builder = ViewportBuilder::default()
            .with_title(format!("TMOP \u{2014} {}", skin.name))
            .with_inner_size(popout::desired_size(&skin))
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_window_level(level);
        let close = ctx.show_viewport_immediate(ViewportId::from_hash_of("tmop-popout"), builder, |ctx, _class| {
            let c = self.popout_ui(ctx, fd);
            c || ctx.input(|i| i.viewport().close_requested())
        });
        if close {
            self.set_popout(false);
        }
    }

    /// `popoutMode`: `off`, `desktop`, `normal` or `topMost`.
    pub fn popout_mode(&self) -> String {
        self.settings.extra.get("popoutMode").and_then(|v| v.as_str()).unwrap_or("off").to_string()
    }

    /// Opens or closes the popout and records it in `popoutMode`.
    pub fn set_popout(&mut self, open: bool) {
        self.popout_open = open;
        let mode = self.popout_mode();
        let new_mode = match (open, mode.as_str()) {
            (false, _) => "off".to_string(),
            (true, "off") => self.settings.extra.get("popoutLastMode").and_then(|v| v.as_str()).unwrap_or("topMost").to_string(),
            (true, m) => m.to_string(),
        };
        if new_mode != "off" {
            self.settings.extra.insert("popoutLastMode".into(), serde_json::Value::String(new_mode.clone()));
        }
        if new_mode != mode {
            self.settings.extra.insert("popoutMode".into(), serde_json::Value::String(new_mode));
            self.settings_dirty = true;
        }
    }

    /// Draws the popout into `ctx`. Returns true when the user asked to close it.
    fn popout_ui(&mut self, ctx: &egui::Context, fd: &FrameData<'_>) -> bool {
        let skin = self.current_skin();
        let mut close = false;
        let mut new_skin: Option<String> = None;
        let opacity_setting = self.settings.popout_opacity as f32;
        let fahrenheit = self.settings.fahrenheit();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::TRANSPARENT)).show(ctx, |ui| {
            let rect = ui.max_rect();
            let hovered = ctx.input(|i| i.pointer.hover_pos().map(|p| rect.contains(p)).unwrap_or(false));
            let op = if self.settings.popout_opaque_on_hover && hovered { 1.0 } else { opacity_setting };
            // Registered before the chrome so the close/chevron/drag areas stay on top of it.
            let menu = ui.interact(rect, ui.id().with("popout-menu"), Sense::click());
            let ev = popout::draw(ui, rect, &skin, fd, op, fahrenheit, &mut self.popout_textures, &mut self.icons);
            if ev.drag {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            close |= ev.close;
            let catalog = &self.catalog;
            let skin_menu = |ui: &mut egui::Ui, new_skin: &mut Option<String>, close: &mut bool| {
                ui.label(egui::RichText::new("Skin").strong());
                for k in &catalog.skins {
                    if ui.radio(k.id == skin.id, &k.name).clicked() {
                        *new_skin = Some(k.id.clone());
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button("Close popout").clicked() {
                    *close = true;
                    ui.close_menu();
                }
            };
            menu.context_menu(|ui| skin_menu(ui, &mut new_skin, &mut close));
            if let Some(chev) = &ev.chevron {
                let id = ui.id().with("popout-chevron-menu");
                if ev.menu {
                    ui.memory_mut(|m| m.toggle_popup(id));
                }
                egui::popup_below_widget(ui, id, chev, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    ui.set_min_width(140.0);
                    skin_menu(ui, &mut new_skin, &mut close);
                });
                if new_skin.is_some() || close {
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            close = true;
        }
        if let Some(id) = new_skin {
            self.settings.popout_skin = id;
            self.settings_dirty = true;
            self.popout_textures = popout::Textures::default();
            let size = popout::desired_size(&self.current_skin());
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
        close
    }
}

/// `HH:MM:SS.cc` stopwatch text.
pub fn fmt_clock(d: Duration) -> String {
    let t = d.as_secs();
    format!("{:02}:{:02}:{:02}.{:02}", (t / 3600) % 100, (t / 60) % 60, t % 60, d.subsec_millis() / 10)
}

/// The TMOP wordmark: a glowing heartbeat trace and the name in neon outline style.
fn wordmark(p: &egui::Painter, r: Rect, bloom: f32, time: f64, dark: bool) {
    let cyan = Color32::from_rgb(0x2E, 0xC8, 0xFF);
    let core = Color32::from_rgb(0xC8, 0xF2, 0xFF);
    let cy = r.center().y;
    let x0 = r.left();
    let beat = [(0.0, 0.0), (0.16, 0.0), (0.2, -0.12), (0.24, 0.1), (0.27, -0.9), (0.31, 0.85), (0.35, -0.35), (0.38, 0.0), (0.44, 0.0)];
    let pts: Vec<Pos2> = beat.iter().map(|(fx, fy)| pos2(x0 + fx * r.width() * 0.7, cy + fy * r.height() * 0.42)).collect();
    // The beat's strokes sit close together and their glows add up, so it gets a lighter bloom.
    widgets::glow_polyline(p, &pts, cyan, 1.8, bloom * 0.6, false, dark);
    if !dark || bloom <= 0.0 {
        p.add(egui::Shape::line(pts.clone(), Stroke::new(0.8_f32, core)));
    }
    // A pulse runs along the trace by arc length (up and down the spikes, not just across),
    // then rests briefly before the next beat.
    let cycle = 2.6;
    let s = ((time % cycle) / cycle) as f32 * 1.35;
    if s <= 1.0 {
        let total: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
        let head = s * total;
        let tail_len = 26.0;
        // Comet tail: points sampled backwards along the path, fading out.
        let tail: Vec<Pos2> = (0..=16).map(|k| point_along(&pts, head - tail_len * (k as f32 / 16.0))).collect();
        for (k, seg) in tail.windows(2).enumerate() {
            let fade = 1.0 - k as f32 / 16.0;
            p.line_segment([seg[0], seg[1]], Stroke::new(1.0 + 2.2 * fade, theme::with_alpha(core, 0.85 * fade * fade)));
        }
        let pos = point_along(&pts, head);
        if bloom > 0.0 {
            for (rad, a) in [(9.0, 0.10), (5.5, 0.22), (3.5, 0.45)] {
                p.circle_filled(pos, rad * (0.6 + 0.4 * bloom), theme::with_alpha(cyan, a * bloom));
            }
        }
        p.circle_filled(pos, 2.2, Color32::WHITE);
    }
    let tx = pts.last().map(|l| l.x + 4.0).unwrap_or(x0);
    let font = theme::display(23.0);
    let pos = pos2(tx, cy + 1.0);
    if bloom > 0.0 {
        for (dx, dy, a) in [(-1.5, 0.0, 0.18), (1.5, 0.0, 0.18), (0.0, -1.5, 0.18), (0.0, 1.5, 0.18), (0.0, 0.0, 0.3)] {
            p.text(pos + vec2(dx, dy), Align2::LEFT_CENTER, "TMOP", font.clone(), theme::with_alpha(cyan, a * bloom));
        }
    }
    p.text(pos, Align2::LEFT_CENTER, "TMOP", font, theme::lighten(cyan, 0.25));
}

/// The point `dist` along a polyline, measured by arc length and clamped to its ends.
fn point_along(pts: &[Pos2], dist: f32) -> Pos2 {
    let mut left = dist.max(0.0);
    for w in pts.windows(2) {
        let len = w[0].distance(w[1]);
        if left <= len && len > 0.0 {
            return w[0] + (w[1] - w[0]) * (left / len);
        }
        left -= len;
    }
    *pts.last().unwrap_or(&Pos2::ZERO)
}

pub fn fmt_hms(d: Duration) -> String {
    let t = d.as_secs();
    if t >= 86400 {
        format!("{}:{:02}:{:02}:{:02}", t / 86400, (t / 3600) % 24, (t / 60) % 60, t % 60)
    } else {
        format!("{:02}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    }
}

/// `Some(true)` when Windows apps use dark mode, `None` when unknown.
fn dark_light_system() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let mut v: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let st = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut v as *mut u32 as *mut _),
                Some(&mut size),
            )
        };
        if st.is_ok() {
            return Some(v == 0);
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.popout_only {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            let c = self.palette().body;
            [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let t_update = Instant::now();
        self.update_inner(ctx);
        if let Some(p) = &mut self.profile {
            p.frames += 1;
            p.busy += t_update.elapsed();
            for c in ctx.repaint_causes() {
                *p.causes.entry(format!("{c:?}")).or_default() += 1;
            }
            if p.since.elapsed() >= Duration::from_secs(2) {
                let secs = p.since.elapsed().as_secs_f64();
                let line = format!(
                    "{:.1} fps, update {:.2} ms avg, saves {}, page {:?}, causes {:?}\n",
                    p.frames as f64 / secs,
                    p.busy.as_secs_f64() * 1000.0 / p.frames.max(1) as f64,
                    p.saves,
                    self.page,
                    p.causes
                );
                let path = std::env::temp_dir().join("tmop-profile.txt");
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
                *p = Profile::default();
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.recorder.stop();
        if self.settings_dirty {
            self.save_settings();
        }
    }
}

/// Frame statistics written to `%TEMP%\tmop-profile.txt` when `TMOP_PROFILE` is set.
pub struct Profile {
    frames: u64,
    busy: Duration,
    saves: u64,
    since: Instant,
    causes: std::collections::BTreeMap<String, u64>,
}

impl Default for Profile {
    fn default() -> Self {
        Self { frames: 0, busy: Duration::ZERO, saves: 0, since: Instant::now(), causes: Default::default() }
    }
}

impl App {
    fn update_inner(&mut self, ctx: &egui::Context) {
        let pal = self.palette();
        widgets::set_led_ground(if pal.dark { None } else { Some(pal.card) });
        if ctx.style().visuals.dark_mode != pal.dark || ctx.style().visuals.panel_fill != pal.body {
            pal.apply(ctx);
        }
        self.shortcuts(ctx);
        self.icons.poll(ctx);
        let (snap, mut hist, rows) = self.snapshot();
        let cap = history_capacity(self.settings.graph_history_multiplier);
        if hist.capacity() != cap {
            if let Ok(mut g) = self.sampler.shared.lock() {
                std::sync::Arc::make_mut(&mut g.history).resize(cap);
            }
            std::sync::Arc::make_mut(&mut hist).resize(cap);
        }
        // Animation clock.
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_at).as_secs_f32().min(0.25);
        self.last_frame_at = now;
        if snap.seq != self.last_seq {
            self.last_seq = snap.seq;
            self.last_sample_at = now;
            self.recorder.on_sample(&snap, &rows);
            let dt = self.settings.update_speed().interval().map(|d| d.as_secs_f64()).unwrap_or(1.0);
            self.pro.inv.history.update(&rows, dt);
        }
        let interval = self.settings.update_speed().interval();
        let hf = self.settings.high_frequency_visuals;
        let phase = match interval {
            Some(iv) if hf => (now.duration_since(self.last_sample_at).as_secs_f32() / iv.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        self.smooth.step(&snap, dt, hf);
        if self.applied_top != Some(self.settings.always_on_top) {
            let level = if self.settings.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
            self.applied_top = Some(self.settings.always_on_top);
        }
        let smooth = self.smooth.clone();
        let fd = FrameData { snap: &snap, hist: &hist, rows: &rows, smooth: &smooth, pal, bloom: self.bloom(), phase, glide: hf && interval.is_some(), time: ctx.input(|i| i.time), show_self: self.settings.show_self_in_reports };

        if self.popout_only {
            if self.popout_ui(ctx, &fd) {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        } else {
            self.menu_bar(ctx, &pal);
            self.status_bar(ctx, &pal, &snap);
            self.sidebar(ctx, &pal);
            let frame = egui::Frame::none().fill(pal.body);
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| match self.page {
                Page::Summary => self.summary_page(ui, &fd),
                Page::Performance => self.performance_page(ui, &fd),
                Page::Processes => self.processes_page(ui, &fd),
                Page::Settings => self.settings_page(ui, &fd),
                Page::Benchmarks => self.benchmarks_page(ui, &fd),
                Page::SystemInfo => self.system_info_page(ui, &fd),
                Page::AppHistory => self.app_history_page(ui, &fd),
                Page::StartupApps => self.startup_page(ui, &fd),
                Page::Users => self.users_page(ui, &fd),
                Page::Services => self.services_page(ui, &fd),
                Page::PowerFreq => self.power_page(ui, &fd),
                Page::FlightRecorder => self.recorder_page(ui, &fd),
                Page::Connections => self.connections_page(ui, &fd),
                Page::InstalledApps => self.apps_page(ui, &fd),
                Page::Drivers => self.drivers_page(ui, &fd),
                Page::DiskSpace => self.diskspace_page(ui, &fd),
            });
            self.confirm_dialog(ctx, &pal);
            if self.popout_open {
                self.popout_viewport(ctx, &fd);
            }
        }

        // Apply cadence changes and persist.
        self.sampler.set_interval(self.settings.update_speed().interval());
        if self.settings_dirty {
            self.save_settings();
        }
        // Frame pacing. egui shortens every delay by its predicted frame time, which would
        // double the rate (or pin it to the monitor's refresh), so add that back to get the
        // configured visualFrameRate.
        let fps = self.settings.visual_frame_rate.clamp(10, 240) as f32;
        let wait = if hf && interval.is_some() { Duration::from_secs_f32(1.0 / fps) } else { Duration::from_millis(500) };
        let predicted = ctx.input(|i| i.predicted_dt).clamp(0.0, 0.1);
        ctx.request_repaint_after(wait + Duration::from_secs_f32(predicted));
    }
}

#[allow(dead_code)]
fn _unused(_: Vec2) {}
