//! eframe/egui front end: window chrome (header bar, navigation rail, page title, status bar,
//! see [`chrome`]), the pages, and the skinnable popout window.

pub mod apps_page;
pub mod bench_page;
pub mod chrome;
pub mod connections_page;
pub mod disk_cleanup;
pub mod disk_delete;
pub mod diskspace_page;
pub mod docker;
pub mod drivers_page;
pub mod hardware;
pub mod health;
pub mod icons;
pub mod inventory_pages;
pub mod kit;
pub mod performance;
pub mod popout;
pub mod power_page;
pub mod pro_pages;
pub mod process_actions;
pub mod processes;
pub mod recorder_page;
pub mod replace;
pub mod settings_page;
pub mod summary;
pub mod table;
pub mod terminal;
pub mod theme;
pub mod widgets;

use crate::docker::model::EngineStatus;
use crate::metrics::{history_capacity, History, Sampler, Snapshot};
use crate::palette::{self, PaletteCatalog};
use crate::pro::{bench::Bench, recorder::Recorder};
use crate::process::{Column, ProcessRow};
use crate::settings::{self, Settings};
use crate::skin::{self, SkinCatalog};
use eframe::egui::{self, Color32, Key, Sense, Vec2, ViewportBuilder, ViewportCommand, ViewportId};
use std::time::{Duration, Instant};
use theme::{MetricColors, Palette};
use widgets::{Graph, GraphLine, Icon};

/// Width of the navigation rail.
pub const RAIL_W: f32 = 84.0;
pub const STATUS_H: f32 = 30.0;
/// Height of the window's header bar (logo, search, readouts).
pub const TOP_BAR_H: f32 = 64.0;
/// Height of a page's title block (see [`App::page_title`]).
pub const HEADER_H: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Summary,
    Performance,
    Processes,
    Docker,
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
    pub const NAV: [Page; 16] = [
        Page::Summary,
        Page::Performance,
        Page::Processes,
        Page::Docker,
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
            "docker" => Page::Docker,
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
            Page::Docker => "docker",
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
            Page::Docker => "Docker",
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
            Page::Docker => Icon::Container,
            Page::SystemInfo => Icon::Info,
            Page::AppHistory => Icon::History,
            Page::StartupApps => Icon::Power,
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
    /// Short name under the rail icon.
    pub fn rail_label(self) -> &'static str {
        match self {
            Page::Summary => "Summary",
            Page::Performance => "Perform.",
            Page::Processes => "Processes",
            Page::Docker => "Docker",
            Page::SystemInfo => "System",
            Page::AppHistory => "History",
            Page::StartupApps => "Startup",
            Page::Users => "Users",
            Page::Services => "Services",
            Page::PowerFreq => "Power",
            Page::FlightRecorder => "Recorder",
            Page::Connections => "Connect.",
            Page::InstalledApps => "Apps",
            Page::Drivers => "Drivers",
            Page::DiskSpace => "Disk",
            Page::Benchmarks => "Bench",
            Page::Settings => "Settings",
        }
    }
    /// Keyboard shortcut of the page (empty when it has none).
    pub fn shortcut(self) -> &'static str {
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
    Docker,
}

impl Device {
    pub const ALL: [Device; 7] = [Device::Cpu, Device::Memory, Device::Gpu, Device::Disk, Device::Network, Device::Thermal, Device::Docker];
    pub fn key(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Memory => "memory",
            Device::Disk => "disk",
            Device::Network => "network",
            Device::Gpu => "gpu",
            Device::Thermal => "thermal",
            Device::Docker => "docker",
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
            Device::Docker => "Docker",
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
            Device::Docker => Icon::Container,
        }
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
    /// Metric colours with the user's overrides (see [`FrameData::metric`]).
    pub metric_colors: MetricColors,
    pub bloom: f32,
    pub phase: f32,
    /// Graphs glide the newest sample in over the update interval (high-frequency visuals on).
    pub glide: bool,
    pub time: f64,
    /// `showSelfInReports`: list TMOP itself in process lists.
    pub show_self: bool,
}

impl FrameData<'_> {
    /// The colour of metric `key` (`cpu`, `memory`, ...; see [`MetricColors`]). A built-in colour
    /// is darkened on light palettes; a `graphs.metricColors` override is used as it is.
    pub fn metric(&self, key: &str) -> Color32 {
        match self.metric_colors.custom(key) {
            Some(c) => c,
            None => theme::darken_for(&self.pal, theme::default_metric_color(key)),
        }
    }
}

/// Command-line launch options.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub start_page: Option<Page>,
    /// cpu, memory, gpu, disk, network, thermal or docker
    pub start_device: Option<String>,
    pub popout_only: bool,
    /// Start recording at launch: (folder, seconds).
    pub record: Option<(Option<std::path::PathBuf>, Option<u64>)>,
    /// Run these benchmarks at launch (`--bench=all` or comma-separated test ids).
    pub bench: Option<Vec<crate::pro::bench::Test>>,
    /// Start a Disk Space scan of this folder at launch.
    pub disk_scan: Option<std::path::PathBuf>,
    /// Container (name or id prefix) to open a terminal session in at start-up, with an optional command (used for runtime checks).
    pub docker_terminal: Option<(String, Option<String>)>,
    /// Docker page tab at start-up: engine, containers, stacks, images, volumes or networks.
    pub docker_tab: Option<String>,
    /// Test-only engine operation at start-up: setup, wizard, start, stop, restart, remove or update.
    pub engine_test: Option<String>,
}


pub struct App {
    pub settings: Settings,
    pub settings_dirty: bool,
    pub catalog: SkinCatalog,
    pub palettes: PaletteCatalog,
    pub palette_ui: settings_page::PaletteUi,
    pub sampler: Sampler,
    pub page: Page,
    pub device: Device,
    pub popout_open: bool,
    popout_only: bool,
    pub filter: String,
    pub sort: (Column, bool),
    pub selected: Option<(u32, i64)>,
    pending: Option<process_actions::Pending>,
    /// "HOST {b7} user" shown in the header.
    machine: String,
    /// Ctrl+K was pressed: focus the header search on the next frame.
    focus_search: bool,
    pub status: String,
    applied_top: Option<bool>,
    /// The palette last given to egui's style.
    applied_pal: Option<Palette>,
    /// The `appearance.displayFont` last installed, and whether it fell back to Michroma.
    display_font: (String, bool),
    pub font_list: settings_page::FontList,
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
    pub replace: replace::ReplaceUi,
    pub docker: docker::DockerState,
    /// Docker series for the Performance page, refreshed when the backend has new stats.
    pub perf_docker: performance::DockerPerfCache,
    /// State of the Summary and Performance pages.
    pub summary: summary::SummaryState,
    pub perf: performance::PerfState,
    status_seen: (String, Instant),
    ip: (Option<String>, Option<Instant>),
    profile: Option<Profile>,
}

/// The main window's HWND (0 if unknown): the owner of the palette file dialogs, so they open
/// above it even when it is always on top.
fn main_hwnd(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings, status: String, launch: Launch) -> Self {
        let popout_only = launch.popout_only;
        let display_font = install_display_font(&cc.egui_ctx, &settings.appearance.display_font);
        let ctx = cc.egui_ctx.clone();
        let sampler = Sampler::start(settings.update_speed().interval(), history_capacity(settings.graphs.history_multiplier), move || ctx.request_repaint());
        let catalog = skin::scan(&skin::default_skin_dirs());
        let palettes = palette::scan(&palette::default_dir());
        let page = launch.start_page.unwrap_or_else(|| Page::from_id(&settings.general.default_page));
        let device = launch.start_device.as_deref().and_then(|d| Device::ALL.into_iter().find(|x| x.key() == d)).unwrap_or(Device::Cpu);
        // A normal settings load needs no message.
        let mut status = if status.ends_with("(Loaded)") { String::new() } else { status };
        let failed: Vec<String> = [(catalog.errors.len(), "skin"), (palettes.errors.len(), "palette")].into_iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}(s)")).collect();
        if !failed.is_empty() {
            status = format!("{} failed to load", failed.join(" and "));
        }
        let settings_popout_on = settings.popout.mode != "off";
        let docker = docker::DockerState::new(&cc.egui_ctx, &settings);
        let app = Self {
            docker,
            perf_docker: performance::DockerPerfCache::default(),
            summary: summary::SummaryState::default(),
            perf: performance::PerfState::default(),
            settings,
            settings_dirty: false,
            catalog,
            palettes,
            palette_ui: settings_page::PaletteUi::new(main_hwnd(cc)),
            sampler,
            page,
            device,
            popout_open: popout_only || settings_popout_on,
            popout_only,
            filter: String::new(),
            sort: (Column::Cpu, true),
            selected: None,
            pending: None,
            machine: chrome::machine_line(),
            focus_search: false,
            status,
            applied_top: None,
            applied_pal: None,
            display_font,
            font_list: settings_page::FontList::default(),
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
            replace: replace::ReplaceUi::new(&cc.egui_ctx, !popout_only),
            status_seen: (String::new(), Instant::now()),
            ip: (None, None),
            profile: std::env::var_os("TMOP_PROFILE").map(|_| Profile::default()),
        };
        let mut app = app;
        if !app.settings.general.recording_folder.is_empty() {
            app.recorder.dir = std::path::PathBuf::from(&app.settings.general.recording_folder);
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
            let root = crate::pro::diskdelete::canonical_root(root);
            app.pro.disk_root = root.display().to_string();
            app.pro.scanner.start(root);
        }
        let pal = app.palette();
        pal.apply(&cc.egui_ctx);
        app.applied_pal = Some(pal);
        if let Some((container, command)) = launch.docker_terminal {
            app.docker.ui.detail.terminals.request(container, command);
        }
        if let Some(tab) = launch.docker_tab.as_deref().and_then(docker::tab_from_id) {
            app.docker.ui.tab = tab;
        }
        match launch.engine_test.as_deref() {
            Some("setup") => {
                // Same as the wizard: the engine's context becomes the active one once it runs.
                app.docker.engine.setup(crate::engine::Overrides::from_env().data_dir(&app.settings.docker.engine_data_dir));
                app.docker.switch_to_engine_when_ready = true;
            }
            Some("wizard") => docker::engine_wizard::open(&mut app.docker),
            Some("start") => app.docker.engine.start(),
            Some("stop") => app.docker.engine.stop(),
            Some("restart") => app.docker.engine.restart(),
            Some("remove") => app.docker.engine.remove(),
            Some("update") => app.docker.engine.update(),
            _ => {}
        }
        app
    }

    /// Id of the palette in use: `appearance.theme`, or built-in Dark or Light (following
    /// Windows) for `system` and for an id that isn't installed.
    pub fn palette_id(&self) -> String {
        let id = &self.settings.appearance.theme;
        if id != palette::SYSTEM && self.palettes.get(id).is_some() {
            return id.clone();
        }
        if matches!(dark_light_system(), Some(false)) { "light" } else { "dark" }.into()
    }

    /// The palette in use, with `appearance.accent` applied.
    pub fn palette(&self) -> Palette {
        let mut pal = self.palettes.get(&self.palette_id()).map(|p| p.palette).unwrap_or(theme::DARK);
        if let Some(accent) = skin::Color::parse(&self.settings.appearance.accent).map(theme::from_skin) {
            pal.accent = accent;
            // Active entries are the accent washed over the page.
            pal.selection = theme::mix(pal.body, accent, if pal.dark { 0.12 } else { 0.18 });
        }
        pal
    }

    /// Metric colours from `graphs.metricColors` (the built-in ones without colour keying).
    pub fn metric_colors(&self) -> MetricColors {
        MetricColors::from_settings(&self.settings.graphs)
    }

    /// `Page::NAV` without the pages that are hidden right now (Docker when its page is off).
    pub fn visible_pages(&self) -> Vec<Page> {
        Page::NAV.into_iter().filter(|p| *p != Page::Docker || self.docker.visible(&self.settings)).collect()
    }

    pub fn bloom(&self) -> f32 {
        if self.settings.appearance.bloom {
            (self.settings.appearance.bloom_level.min(10) as f32 / 10.0).max(0.1)
        } else {
            0.0
        }
    }

    /// A graph configured from the settings (history compression, smoothing, bloom).
    pub fn graph<'a>(&self, lines: Vec<GraphLine<'a>>, fd: &FrameData<'_>) -> Graph<'a> {
        let mut lines = lines;
        if !self.settings.graphs.color_keyed {
            // Without colour keying every trace uses one neutral blue, later traces dimmer.
            for (i, l) in lines.iter_mut().enumerate() {
                l.color = theme::dim(Color32::from_rgb(0x4F, 0xB4, 0xFF), 1.0 - 0.3 * i.min(2) as f32);
            }
        }
        let mut g = Graph::new(lines);
        g.px_per_update = self.settings.graphs.pixels_per_update.clamp(2, 64) as f32;
        g.phase = fd.phase;
        g.glide = fd.glide;
        g.regions = widgets::regions_for(self.settings.graphs.history_multiplier, self.settings.graphs.compressed_time);
        g.smooth = self.settings.graphs.smooth_curves;
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
        if Self::search_shortcut(ctx) {
            self.focus_search = true;
        }
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
        match self.catalog.get(&self.settings.popout.skin) {
            Some(s) if s.source != skin::SkinSource::BuiltIn || s.id != "classic" => s.clone(),
            _ => popout::classic_skin(&self.palette(), &self.metric_colors()),
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

    /// `popout.mode`: `off`, `desktop`, `normal` or `topMost`.
    pub fn popout_mode(&self) -> String {
        self.settings.popout.mode.clone()
    }

    /// Opens or closes the popout and records it in `popout.mode` (and `popout.lastMode`).
    pub fn set_popout(&mut self, open: bool) {
        self.popout_open = open;
        let mode = self.popout_mode();
        let new_mode = match (open, mode.as_str()) {
            (false, _) => "off".to_string(),
            (true, "off") => self.settings.popout.last_mode.clone(),
            (true, m) => m.to_string(),
        };
        if new_mode != "off" {
            self.settings.popout.last_mode = new_mode.clone();
        }
        if new_mode != mode {
            self.settings.popout.mode = new_mode;
            self.settings_dirty = true;
        }
    }

    /// Draws the popout into `ctx`. Returns true when the user asked to close it.
    fn popout_ui(&mut self, ctx: &egui::Context, fd: &FrameData<'_>) -> bool {
        let skin = self.current_skin();
        let mut close = false;
        let mut new_skin: Option<String> = None;
        let opacity_setting = self.settings.popout.opacity as f32;
        let fahrenheit = self.settings.fahrenheit();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::TRANSPARENT)).show(ctx, |ui| {
            let rect = ui.max_rect();
            let hovered = ctx.input(|i| i.pointer.hover_pos().map(|p| rect.contains(p)).unwrap_or(false));
            let op = if self.settings.popout.opaque_on_hover && hovered { 1.0 } else { opacity_setting };
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
            self.settings.popout.skin = id;
            self.settings_dirty = true;
            self.popout_textures = popout::Textures::default();
            let size = popout::desired_size(&self.current_skin());
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
        close
    }
}

/// Installs the egui fonts with display font `name` (`appearance.displayFont`): Michroma, or an
/// installed family read here. Returns `name` and whether it fell back to Michroma.
fn install_display_font(ctx: &egui::Context, name: &str) -> (String, bool) {
    let (file, missing) = crate::fonts::resolve(name, crate::fonts::load);
    theme::install_fonts(ctx, file);
    (name.to_string(), missing)
}

/// `HH:MM:SS.cc` stopwatch text.
pub fn fmt_clock(d: Duration) -> String {
    let t = d.as_secs();
    format!("{:02}:{:02}:{:02}.{:02}", (t / 3600) % 100, (t / 60) % 60, t % 60, d.subsec_millis() / 10)
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
            let pal = self.palette();
            let c = pal.backdrop.map(|b| b[2]).unwrap_or(pal.body);
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
        self.save_palette_edits(true);
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
        widgets::set_ink(&pal);
        widgets::set_spark_smooth(self.settings.graphs.smooth_curves);
        // Any change of palette (another palette, or a role edited in the editor) restyles egui,
        // and so does a style that no longer matches it (egui swaps styles with the OS theme).
        let live = ctx.style();
        if self.applied_pal != Some(pal) || live.visuals.panel_fill != pal.body || live.visuals.dark_mode != pal.dark {
            pal.apply(ctx);
            self.applied_pal = Some(pal);
        }
        if self.settings.appearance.display_font != self.display_font.0 {
            self.display_font = install_display_font(ctx, &self.settings.appearance.display_font);
        }
        self.shortcuts(ctx);
        self.icons.poll(ctx);
        self.engine_poll();
        self.docker.poll(self.page == Page::Docker);
        self.docker.refresh_snap();
        self.refresh_docker_perf();
        if self.page == Page::Docker && !self.docker.visible(&self.settings) {
            self.page = Page::Summary;
        }
        let (snap, mut hist, rows) = self.snapshot();
        let cap = history_capacity(self.settings.graphs.history_multiplier);
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
            self.recorder.docker = (self.docker.last_status == EngineStatus::Connected).then_some(self.docker.ui.snap.totals);
            self.recorder.on_sample(&snap, &rows);
            let dt = self.settings.update_speed().interval().map(|d| d.as_secs_f64()).unwrap_or(1.0);
            self.pro.inv.history.update(&rows, dt);
        }
        let interval = self.settings.update_speed().interval();
        let hf = self.settings.appearance.high_frequency_visuals;
        let phase = match interval {
            Some(iv) if hf => (now.duration_since(self.last_sample_at).as_secs_f32() / iv.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        self.smooth.step(&snap, dt, hf);
        if self.applied_top != Some(self.settings.window.always_on_top) {
            let level = if self.settings.window.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
            self.applied_top = Some(self.settings.window.always_on_top);
        }
        let smooth = self.smooth.clone();
        let fd = FrameData { snap: &snap, hist: &hist, rows: &rows, smooth: &smooth, pal, metric_colors: self.metric_colors(), bloom: self.bloom(), phase, glide: hf && interval.is_some(), time: ctx.input(|i| i.time), show_self: self.settings.general.show_self_in_reports };

        if self.popout_only {
            if self.popout_ui(ctx, &fd) {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        } else {
            self.replace_poll(ctx);
            if let Some([hi, mid, edge]) = pal.backdrop {
                widgets::fill_radial_backdrop(&ctx.layer_painter(egui::LayerId::background()), ctx.screen_rect(), hi, mid, edge);
            }
            self.header_bar(ctx, &pal, &snap);
            self.replace_banner(ctx, &pal);
            self.status_bar(ctx, &pal, &snap);
            self.rail(ctx, &pal);
            let frame = egui::Frame::none().inner_margin(egui::Margin { left: 8.0, right: 32.0, top: 12.0, bottom: 36.0 });
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
                Page::Docker => self.docker_page(ui, &fd),
            });
            self.confirm_dialog(ctx, &pal);
            self.replace_dialog(ctx, &pal);
            self.disk_delete_dialogs(ctx, &pal);
            self.draw_toasts(ctx);
            if self.popout_open {
                self.popout_viewport(ctx, &fd);
            }
        }

        // Apply cadence changes and persist.
        self.sampler.set_interval(self.settings.update_speed().interval());
        self.poll_palette_dialog();
        self.save_palette_edits(false);
        if self.settings_dirty {
            self.save_settings();
        }
        // Frame pacing. egui shortens every delay by its predicted frame time, which would
        // double the rate (or pin it to the monitor's refresh), so add that back to get the
        // configured appearance.frameRate.
        let fps = self.settings.appearance.frame_rate.clamp(10, 240) as f32;
        let wait = if hf && interval.is_some() { Duration::from_secs_f32(1.0 / fps) } else { Duration::from_millis(500) };
        let predicted = ctx.input(|i| i.predicted_dt).clamp(0.0, 0.1);
        ctx.request_repaint_after(wait + Duration::from_secs_f32(predicted));
    }
}

#[allow(dead_code)]
fn _unused(_: Vec2) {}
