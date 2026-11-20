//! eframe/egui front end: window chrome (header bar, navigation rail, page title, status bar,
//! see [`chrome`]), the pages, and the skinnable popout window.

pub mod apps_page;
pub mod bench_page;
pub mod chrome;
pub mod connections_page;
pub mod disk_cleanup;
pub mod disk_delete;
pub mod disk_drives;
pub mod diskspace_page;
pub mod docker;
pub mod gpu_activity;
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
pub mod titlebar;
pub mod updates;
pub mod widgets;

use crate::docker::model::EngineStatus;
use crate::metrics::{history_capacity, History, Sampler, Snapshot};
use crate::palette::{self, PaletteCatalog};
use crate::platform::Caps;
use crate::pro::{bench::Bench, recorder::Recorder};
use crate::process::{Column, ProcessRow};
use crate::settings::{self, Settings};
use crate::skin::{self, SkinCatalog};
use eframe::egui::{self, Color32, Key, Sense, ViewportBuilder, ViewportCommand, ViewportId};
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

    /// Whether this page has anything to show with `caps`. Summary, Settings and the pages fed by
    /// the metrics and processes backends are always there.
    pub fn supported(self, caps: &Caps) -> bool {
        match self {
            Page::SystemInfo | Page::StartupApps | Page::Services | Page::Drivers | Page::InstalledApps | Page::Connections | Page::PowerFreq => caps.inventory,
            _ => true,
        }
    }

    /// [`Page::from_id`], with Summary in place of a page `caps` does not support (`--page=`,
    /// `general.defaultPage`, Task Manager hand-offs).
    pub fn from_id_on(id: &str, caps: &Caps) -> Self {
        Some(Page::from_id(id)).filter(|p| p.supported(caps)).unwrap_or(Page::Summary)
    }

    /// The choices for the default start page: the supported pages of [`Page::NAV`], then Settings.
    pub fn start_choices(caps: &Caps) -> Vec<Page> {
        Page::NAV.into_iter().chain([Page::Settings]).filter(|p| p.supported(caps)).collect()
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
            Page::Drivers => crate::pro::drivers::DRIVERS_TITLE,
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
            Page::Drivers => crate::pro::drivers::DRIVERS_RAIL,
            Page::DiskSpace => "Disk",
            Page::Benchmarks => "Bench",
            Page::Settings => "Settings",
        }
    }
    /// The pages with a keyboard shortcut: the key that opens the page when pressed with Ctrl
    /// (Cmd on macOS), and how the shortcut reads in tooltips and subtitles.
    pub const SHORTCUTS: [(Page, Key, &'static str); 11] = [
        (Page::Summary, Key::Num1, "Ctrl+1"),
        (Page::Performance, Key::Num2, "Ctrl+2"),
        (Page::Processes, Key::Num3, "Ctrl+3"),
        (Page::SystemInfo, Key::Num4, "Ctrl+4"),
        (Page::AppHistory, Key::Num5, "Ctrl+5"),
        (Page::StartupApps, Key::Num6, "Ctrl+6"),
        (Page::Users, Key::Num7, "Ctrl+7"),
        (Page::Services, Key::Num8, "Ctrl+8"),
        (Page::Benchmarks, Key::Num9, "Ctrl+9"),
        (Page::DiskSpace, Key::Num0, "Ctrl+0"),
        (Page::Settings, Key::Comma, "Ctrl+,"),
    ];

    /// Keyboard shortcut of the page as text (empty when it has none).
    pub fn shortcut(self) -> &'static str {
        Page::SHORTCUTS.iter().find(|(p, ..)| *p == self).map_or("", |(_, _, text)| text)
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
    /// How long CPU and disk have been critical (see [`health::health`]).
    pub lasting: health::Lasting,
    /// Show each GPU on its own: asked for, and there are at least two.
    pub gpus_separated: bool,
    /// Show each disk on its own: asked for, and there are at least two.
    pub disks_separated: bool,
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

/// [`Page::NAV`] minus the pages `caps` does not support, and minus Docker unless `docker`.
pub fn visible_pages(caps: &Caps, docker: bool) -> Vec<Page> {
    Page::NAV.into_iter().filter(|p| p.supported(caps) && (*p != Page::Docker || docker)).collect()
}

pub struct App {
    /// What this OS and session support, probed once at start-up.
    pub caps: Caps,
    /// The window is drawn by a software OpenGL renderer (llvmpipe in the server package, #46):
    /// every frame costs tens of milliseconds of CPU time, so high frequency visuals stay off.
    pub software_gl: bool,
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
    /// A process action running as administrator (`pkexec`); its status text arrives here.
    elevated_job: Option<std::sync::mpsc::Receiver<String>>,
    /// "HOST {b7} user" shown in the header.
    machine: String,
    /// Ctrl+K was pressed: focus the header search on the next frame.
    focus_search: bool,
    pub status: String,
    applied_top: Option<bool>,
    /// Whether the OS uses dark mode ([`dark_light_system`]): asked once per frame, then shared
    /// by that frame's palette lookups (on Windows each ask is a registry read).
    system_dark: Option<bool>,
    /// TMOP draws the title bar, window buttons and resize edges (see
    /// [`crate::platform::desktop::own_title_bar`]).
    pub own_title_bar: bool,
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
    pub updates: updates::UpdateUi,
    pub docker: docker::DockerState,
    /// Docker series for the Performance page, refreshed when the backend has new stats.
    pub perf_docker: performance::DockerPerfCache,
    /// State of the Summary and Performance pages.
    pub summary: summary::SummaryState,
    /// Follows how long CPU and disk load stay critical.
    pressure: health::Tracker,
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
        let caps = Caps::get();
        let page = match launch.start_page {
            Some(p) if p.supported(&caps) => p,
            Some(_) => Page::Summary,
            None => Page::from_id_on(&settings.general.default_page, &caps),
        };
        let device = launch.start_device.as_deref().and_then(|d| Device::ALL.into_iter().find(|x| x.key() == d)).unwrap_or(Device::Cpu);
        // A normal settings load needs no message.
        let mut status = if status.ends_with("(Loaded)") { String::new() } else { status };
        let failed: Vec<String> = [(catalog.errors.len(), "skin"), (palettes.errors.len(), "palette")].into_iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}(s)")).collect();
        if !failed.is_empty() {
            status = format!("{} failed to load", failed.join(" and "));
        }
        let settings_popout_on = settings.popout.mode != "off";
        cc.egui_ctx.options_mut(|o| {
            // Since 0.34 egui closes the app on Ctrl+Q; TMOP never did (the Docker terminal sends it).
            o.quit_shortcuts.clear();
            // Since 0.36 egui sets the window theme; TMOP leaves the title bar to the system.
            o.sync_window_theme = false;
        });
        let software_gl = gl_renderer(cc).is_some_and(|r| crate::startup::is_software_renderer(&r));
        let docker = docker::DockerState::new(&cc.egui_ctx, &settings);
        let mut app = Self {
            caps,
            software_gl,
            docker,
            perf_docker: performance::DockerPerfCache::default(),
            summary: summary::SummaryState::default(),
            pressure: health::Tracker::default(),
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
            elevated_job: None,
            machine: chrome::machine_line(),
            focus_search: false,
            status,
            applied_top: None,
            system_dark: dark_light_system(cc.egui_ctx.system_theme()),
            own_title_bar: !popout_only && crate::platform::desktop::own_title_bar(),
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
            replace: replace::ReplaceUi::new(&cc.egui_ctx, !popout_only && caps.taskmgr_replace),
            updates: updates::UpdateUi::default(),
            status_seen: (String::new(), Instant::now()),
            ip: (None, None),
            profile: std::env::var_os("TMOP_PROFILE").map(|_| Profile::default()),
        };
        if !popout_only {
            crate::update::remove_leftover();
            app.updates.check_on_start(&app.settings.general);
        }
        app.pro.del.delete_safe = caps.delete_safe;
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
        match launch.engine_test.as_deref().filter(|_| app.caps.engine) {
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

    /// Id of the palette in use: `appearance.theme`, or built-in Dark or Light (following the
    /// system, Light when it is unknown) for `system` and for an id that isn't installed.
    pub fn palette_id(&self) -> String {
        let id = &self.settings.appearance.theme;
        if id != palette::SYSTEM && self.palettes.get(id).is_some() {
            return id.clone();
        }
        if self.system_dark == Some(true) { "dark" } else { "light" }.into()
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

    /// `Page::NAV` without the pages that are hidden right now: unsupported ones, and Docker
    /// when its page is off.
    pub fn visible_pages(&self) -> Vec<Page> {
        visible_pages(&self.caps, self.docker_visible())
    }

    /// Whether the Docker page is listed.
    pub fn docker_visible(&self) -> bool {
        self.docker.visible(&self.settings, self.caps.engine)
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
                l.color = widgets::neutral_graph_color(i);
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
            // A page this OS does not have keeps its key unused rather than landing on Summary.
            go = Page::SHORTCUTS.iter().filter(|(p, ..)| p.supported(&self.caps)).find(|(_, k, _)| ctrl_pressed(i, *k)).map(|(p, ..)| *p);
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
        let level = match self.settings.popout.mode.as_str() {
            _ if !self.caps.window_levels => egui::WindowLevel::Normal,
            "desktop" => egui::WindowLevel::AlwaysOnBottom,
            "normal" => egui::WindowLevel::Normal,
            _ => egui::WindowLevel::AlwaysOnTop,
        };
        let mut builder = ViewportBuilder::default()
            .with_title(format!("TMOP \u{2014} {}", skin.name))
            .with_app_id(crate::APP_ID)
            .with_inner_size(popout::desired_size(&skin))
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_window_level(level);
        if let Some(icon) = crate::icon::window_icon() {
            builder = builder.with_icon(icon);
        }
        let close = ctx.show_viewport_immediate(ViewportId::from_hash_of("tmop-popout"), builder, |ui, _class| {
            let c = self.popout_ui(ui, fd, &skin);
            c || ui.input(|i| i.viewport().close_requested())
        });
        if close {
            self.set_popout(false);
        }
    }

    /// Opens or closes the popout and records it in `popout.mode` (`off`, `desktop`, `normal` or
    /// `topMost`) and `popout.lastMode`.
    pub fn set_popout(&mut self, open: bool) {
        self.popout_open = open;
        let popout = &mut self.settings.popout;
        let new_mode = match (open, popout.mode.as_str()) {
            (false, _) => "off".to_string(),
            (true, "off") => popout.last_mode.clone(),
            (true, m) => m.to_string(),
        };
        if new_mode != "off" {
            popout.last_mode = new_mode.clone();
        }
        if new_mode != popout.mode {
            popout.mode = new_mode;
            self.settings_dirty = true;
        }
    }

    /// Draws the popout with `skin` (see [`App::current_skin`]) into `root` (the viewport's).
    /// Returns true when the user asked to close it.
    fn popout_ui(&mut self, root: &mut egui::Ui, fd: &FrameData<'_>, skin: &skin::Skin) -> bool {
        let ctx = &root.ctx().clone();
        let mut close = false;
        let mut new_skin: Option<String> = None;
        let opacity_setting = self.settings.popout.opacity as f32;
        let fahrenheit = self.settings.fahrenheit();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::TRANSPARENT)).show(root, |ui| {
            let rect = ui.max_rect();
            let hovered = ctx.input(|i| i.pointer.hover_pos().map(|p| rect.contains(p)).unwrap_or(false));
            let op = if self.settings.popout.opaque_on_hover && hovered { 1.0 } else { opacity_setting };
            // Registered before the chrome so the close/chevron/drag areas stay on top of it.
            let menu = ui.interact(rect, ui.scope_id().with("popout-menu"), Sense::click());
            let ev = popout::draw(ui, rect, skin, fd, op, fahrenheit, &mut self.popout_textures, &mut self.icons);
            if ev.drag {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            close |= ev.close;
            let catalog = &self.catalog;
            let skin_menu = |ui: &mut egui::Ui, new_skin: &mut Option<String>, close: &mut bool| {
                ui.label(egui::RichText::new("Skin").strong());
                for k in &catalog.skins {
                    if ui.radio(k.id == skin.id, theme::button_text(&k.name)).clicked() {
                        *new_skin = Some(k.id.clone());
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button(theme::button_text("Close popout")).clicked() {
                    *close = true;
                    ui.close();
                }
            };
            kit::context_menu(&menu, |ui| skin_menu(ui, &mut new_skin, &mut close));
            if let Some(chev) = &ev.chevron {
                let id = ui.scope_id().with("popout-chevron-menu");
                if ev.menu {
                    egui::Popup::toggle_id(ui.ctx(), id);
                }
                kit::popup_below_widget(ui, id, chev, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    ui.set_min_width(140.0);
                    skin_menu(ui, &mut new_skin, &mut close);
                });
                if new_skin.is_some() || close {
                    egui::Popup::close_id(ui.ctx(), id);
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

/// Whether `key` went down with Ctrl (Cmd on macOS) held. The key event's own modifiers decide:
/// the input's `modifiers` are those after the frame's last event, so a quick tap, a macro key or
/// a remote session that presses and releases Ctrl within one frame would be missed by them.
pub(crate) fn ctrl_pressed(i: &egui::InputState, key: Key) -> bool {
    i.events.iter().any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && (modifiers.ctrl || modifiers.command)))
}

/// `HH:MM:SS.cc` stopwatch text.
pub fn fmt_clock(d: Duration) -> String {
    let t = d.as_secs();
    format!("{:02}:{:02}:{:02}.{:02}", (t / 3600) % 100, (t / 60) % 60, t % 60, d.subsec_millis() / 10)
}

/// `HH:MM:SS` of a duration, `D:HH:MM:SS` from one day on (up time, recording length).
pub fn fmt_hms(d: Duration) -> String {
    let t = d.as_secs();
    if t >= 86400 {
        format!("{}:{:02}:{:02}:{:02}", t / 86400, (t / 3600) % 24, (t / 60) % 60, t % 60)
    } else {
        format!("{:02}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    }
}

/// `Some(true)` when the system uses dark mode, `None` when unknown. Windows: the apps' setting
/// in the registry. Elsewhere egui's system theme (`reported`, from winit), else the desktop's
/// setting where one is read ([`crate::platform::desktop::desktop_dark`]).
fn dark_light_system(reported: Option<egui::Theme>) -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
        let _ = reported;
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
        match reported {
            Some(theme) => Some(theme == egui::Theme::Dark),
            None => crate::platform::desktop::desktop_dark(),
        }
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

    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe 0.37 skips `ui` while the window is minimized or hidden (0.29 still ran `update`
        // then): new samples are taken in here, so recording and App history go on meanwhile.
        let (snap, _, rows) = self.snapshot();
        self.take_sample(&snap, &rows, Instant::now());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t_update = Instant::now();
        self.update_inner(ui);
        if let Some(p) = &mut self.profile {
            p.frames += 1;
            p.busy += t_update.elapsed();
            for c in ui.ctx().repaint_causes() {
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

/// OpenGL's `GL_RENDERER` for the window, e.g. `llvmpipe (LLVM 21.1.0, 256 bits)`.
fn gl_renderer(cc: &eframe::CreationContext<'_>) -> Option<String> {
    use eframe::glow::HasContext;
    // SAFETY: eframe made the context current on this thread before calling the app creator.
    cc.gl.as_ref().map(|gl| unsafe { gl.get_parameter_string(eframe::glow::RENDERER) })
}

/// Whether TMOP animates between samples (appearance.highFrequencyVisuals). Not under a
/// software renderer: there each frame costs about 40 ms of CPU time, so 30–60 frames a
/// second kept one core or more busy (#46); without them TMOP redraws a few times a second.
fn high_frequency(setting: bool, software_gl: bool) -> bool {
    setting && !software_gl
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
    /// Feeds a sample the sampler has not delivered before to the recorder, App history and the
    /// temperature log.
    fn take_sample(&mut self, snap: &Snapshot, rows: &[ProcessRow], now: Instant) {
        if snap.seq != self.last_seq {
            self.last_seq = snap.seq;
            self.last_sample_at = now;
            self.recorder.docker = (self.docker.last_status == EngineStatus::Connected).then_some(self.docker.ui.snap.totals);
            self.recorder.on_sample(snap, rows);
            let dt = self.settings.update_speed().interval().map(|d| d.as_secs_f64()).unwrap_or(1.0);
            self.pro.inv.history.update(rows, dt);
            self.perf.note_temperature(snap.thermal.max_c());
        }
    }

    fn update_inner(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        self.system_dark = dark_light_system(ctx.system_theme());
        let pal = self.palette();
        widgets::set_ink(&pal);
        widgets::set_spark_smooth(self.settings.graphs.smooth_curves);
        widgets::set_spark_neutral(!self.settings.graphs.color_keyed);
        // Any change of palette (another palette, or a role edited in the editor) restyles egui,
        // and so does a style that no longer matches it (egui swaps styles with the OS theme).
        let live = ctx.global_style();
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
        if (self.page == Page::Docker && !self.docker_visible()) || !self.page.supported(&self.caps) {
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
        self.take_sample(&snap, &rows, now);
        let interval = self.settings.update_speed().interval();
        let hf = high_frequency(self.settings.appearance.high_frequency_visuals, self.software_gl);
        let phase = match interval {
            Some(iv) if hf => (now.duration_since(self.last_sample_at).as_secs_f32() / iv.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        self.smooth.step(&snap, dt, hf);
        widgets::set_spark_glide(phase, hf && interval.is_some());
        if self.caps.window_levels && self.applied_top != Some(self.settings.window.always_on_top) {
            let level = if self.settings.window.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
            self.applied_top = Some(self.settings.window.always_on_top);
        }
        let smooth = self.smooth.clone();
        let lasting = self.pressure.update(&snap, now);
        let gpus_separated = self.settings.window.gpus_separated && snap.gpu.adapters.len() >= 2;
        let disks_separated = self.settings.window.disks_separated && snap.disk.drives.len() >= 2;
        // Only where it is shown: elsewhere a hybrid laptop's discrete GPU stays free to power down.
        crate::metrics::set_gpu_detail((gpus_separated || self.perf.gpu_activity) && self.page == Page::Performance && self.device == Device::Gpu);
        let fd = FrameData { snap: &snap, hist: &hist, rows: &rows, smooth: &smooth, pal, metric_colors: self.metric_colors(), bloom: self.bloom(), phase, glide: hf && interval.is_some(), time: ctx.input(|i| i.time), show_self: self.settings.general.show_self_in_reports, lasting, gpus_separated, disks_separated };

        if self.popout_only {
            let skin = self.current_skin();
            if self.popout_ui(root, &fd, &skin) {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        } else {
            // Remembered for the next start (window.maximized). A minimized window keeps what
            // it was before.
            let (maximized, minimized) = ctx.input(|i| (i.viewport().maximized, i.viewport().minimized));
            if let (Some(maximized), false) = (maximized, minimized.unwrap_or(false)) {
                if maximized != self.settings.window.maximized {
                    self.settings.window.maximized = maximized;
                    self.settings_dirty = true;
                }
            }
            self.replace_poll(ctx);
            self.updates_poll(ctx);
            if let Some([hi, mid, edge]) = pal.backdrop {
                widgets::fill_radial_backdrop(&ctx.layer_painter(egui::LayerId::background()), ctx.content_rect(), hi, mid, edge);
            }
            self.header_bar(root, &pal, &snap);
            self.replace_banner(root, &pal);
            self.status_bar(root, &pal, &snap);
            self.rail(root, &pal);
            if self.own_title_bar {
                titlebar::frame(ctx, &pal);
            }
            let frame = egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 32, top: 12, bottom: 36 });
            egui::CentralPanel::default().frame(frame).show(root, |ui| match self.page {
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
            self.updates_dialog(ctx, &pal);
            self.disk_delete_dialogs(ctx, &pal);
            self.draw_toasts(ctx);
            if self.popout_open {
                self.popout_viewport(ctx, &fd);
            }
        }

        // Apply cadence changes and persist.
        self.sampler.set_interval(self.settings.update_speed().interval());
        // Sampled at once, so that switching off closes the device even while updates are paused.
        if crate::metrics::pawnio::set_enabled(self.caps.pawnio && self.settings.general.pawnio_cpu_temperature) {
            self.sampler.refresh();
        }
        // What a part of a page had to tell the user (a file manager that would not open).
        if let Some(message) = kit::take_report(ctx) {
            self.status = message;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_rendering_turns_high_frequency_visuals_off() {
        assert!(high_frequency(true, false));
        assert!(!high_frequency(false, false));
        assert!(!high_frequency(true, true));
        assert!(!high_frequency(false, true));
    }

    #[test]
    fn visible_pages_follow_caps_and_the_docker_switch() {
        assert_eq!(visible_pages(&Caps::WINDOWS, true), Page::NAV.to_vec());
        assert_eq!(visible_pages(&Caps::LINUX, true), Page::NAV.to_vec());
        assert!(!visible_pages(&Caps::WINDOWS, false).contains(&Page::Docker));
        // A made-up OS without the inventory backends keeps the monitoring pages, in NAV order.
        let fake = Caps { inventory: false, ..Caps::WINDOWS };
        let shown = visible_pages(&fake, true);
        let expect = [Page::Summary, Page::Performance, Page::Processes, Page::Docker, Page::AppHistory, Page::Users, Page::FlightRecorder, Page::DiskSpace, Page::Benchmarks];
        assert_eq!(shown, expect);
        assert!(Page::NAV.iter().all(|p| shown.contains(p) == p.supported(&fake)));
    }

    /// One table feeds the key handling and the shortcut texts: no page or key is in it twice.
    #[test]
    fn page_shortcuts_are_unique_and_named() {
        let all = Page::SHORTCUTS;
        for (i, (page, key, text)) in all.iter().enumerate() {
            assert_eq!(page.shortcut(), *text);
            assert!(text.starts_with("Ctrl+"), "{text}");
            assert!(all[..i].iter().all(|(p, k, _)| p != page && k != key), "{text} is listed twice");
        }
        assert_eq!(Page::Docker.shortcut(), "", "a page without a shortcut has no text");
    }

    /// A shortcut whose Ctrl is already released when the frame ends still counts; the bare key
    /// and the key's release do not.
    #[test]
    fn ctrl_shortcuts_go_by_the_key_events_modifiers() {
        let key = |key, pressed, modifiers| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
        let seen = |events: Vec<egui::Event>| {
            let ctx = egui::Context::default();
            let mut hit = false;
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| hit = ui.input(|i| ctrl_pressed(i, Key::Num2))).drop_without_applying_deltas();
            hit
        };
        let ctrl = egui::Modifiers::CTRL;
        let none = egui::Modifiers::NONE;
        let ctrl_up = key(Key::ControlLeft, false, none);
        assert!(seen(vec![key(Key::Num2, true, ctrl), key(Key::Num2, false, ctrl), ctrl_up]), "Ctrl released within the frame");
        assert!(seen(vec![key(Key::Num2, true, egui::Modifiers::COMMAND)]));
        assert!(!seen(vec![key(Key::Num2, true, none)]), "the bare key");
        assert!(!seen(vec![key(Key::Num2, false, ctrl)]), "a release is not a press");
        assert!(!seen(vec![key(Key::Num3, true, ctrl)]));
    }

    #[test]
    fn unsupported_start_pages_fall_back_to_summary() {
        let fake = Caps { inventory: false, ..Caps::LINUX };
        assert_eq!(Page::from_id_on("services", &fake), Page::Summary);
        assert_eq!(Page::from_id_on("services", &Caps::LINUX), Page::Services);
        assert_eq!(Page::from_id_on("disk-space", &fake), Page::DiskSpace);
        assert_eq!(Page::from_id_on("settings", &fake), Page::Settings);
        assert_eq!(Page::from_id_on("nonsense", &Caps::WINDOWS), Page::Summary);
        let choices = Page::start_choices(&fake);
        assert!(!choices.contains(&Page::Services) && choices.last() == Some(&Page::Settings));
        assert_eq!(Page::start_choices(&Caps::WINDOWS).len(), Page::NAV.len() + 1);
    }
}
