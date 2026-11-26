//! eframe/egui front end: window chrome (header bar, navigation rail, page title, status bar,
//! see [`chrome`]), the pages, and the skinnable popout window.

pub mod affinity_dialog;
pub mod apps_page;
pub mod bench_page;
pub mod chrome;
pub mod connections_page;
pub mod csv;
pub mod disk_cleanup;
pub mod disk_delete;
pub mod disk_drives;
pub mod diskspace_page;
pub mod docker;
pub mod gpu_activity;
pub mod drivers_page;
pub mod elide;
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
pub mod file_in_use;
pub mod run_task;
pub mod settings_page;
pub mod summary;
pub mod table;
pub mod terminal;
pub mod theme;
pub mod titlebar;
pub mod tray;
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
    /// Eases the values towards `s` over `dt`. Returns whether a value that pages without a
    /// gliding graph show still moves (#194): CPU, RAM, disk and GPU, read out in the header of
    /// every page and on the System information tiles. One within [`SETTLED`] of its target
    /// takes it, so the frames can stop. The other values (kernel, temperature, clock, network,
    /// per core) are shown only on Summary, Performance, Power and frequency and the popout,
    /// whose graphs glide and keep the frames going anyway; they ease the same way.
    fn step(&mut self, s: &Snapshot, dt: f32, eased: bool) -> bool {
        let k = if eased { 1.0 - (-dt * 9.0).exp() } else { 1.0 };
        let mut moving = false;
        let mut settle = |cur: &mut f32, target: f32| {
            *cur += (target - *cur) * k;
            // Not a number on either side counts as settled, so it cannot keep the frames going.
            if (target - *cur).abs().partial_cmp(&SETTLED) == Some(std::cmp::Ordering::Greater) {
                moving = true;
            } else {
                *cur = target;
            }
        };
        settle(&mut self.cpu, s.cpu.total_pct);
        settle(&mut self.memory, s.memory.used_pct);
        settle(&mut self.disk, s.disk.active_pct);
        settle(&mut self.gpu, s.gpu.utilization_pct);
        let ease = |cur: &mut f32, target: f32| *cur += (target - *cur) * k;
        ease(&mut self.kernel, s.cpu.kernel_pct);
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
        moving
    }
}

/// How close (in percent points) an eased readout must come to its target to take it: half of
/// the smallest step a whole-percent readout shows, under a pixel on a 200 px bar.
const SETTLED: f32 = 0.25;


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
    /// Settings section to scroll to (`graphs`, `general`, ...), see [`settings_page::section_index`].
    pub settings_section: Option<String>,
    /// `--screenshot=`: render frames to PNG files and exit, see [`crate::shot`].
    pub shot: Option<crate::shot::Plan>,
    /// Start hidden in the notification area (`--tray`), when tray mode is on.
    pub tray: bool,
    /// This start takes the place of a TMOP that is exiting (`--replacing`, or an elevated
    /// start that asked one that is not elevated to exit): the pipe and the shortcut are
    /// claimed with retries while that one lets go of them.
    pub replacing: bool,
}

/// [`Page::NAV`] minus the pages `caps` does not support, and minus Docker unless `docker`.
pub fn visible_pages(caps: &Caps, docker: bool) -> Vec<Page> {
    Page::NAV.into_iter().filter(|p| p.supported(caps) && (*p != Page::Docker || docker)).collect()
}

/// How long after start-up the window may take to show as maximized (seconds).
const MAXIMIZE_GRACE: f64 = 3.0;

/// Decides what to remember of the window's maximized state (`window.maximized`). A window
/// asked to start maximized can report `maximized: false` for its first frames, before the
/// request is applied; recording that would clear the setting at once. Until the window has
/// been seen maximized (or [`MAXIMIZE_GRACE`] has passed, the request was refused) a `false`
/// is not recorded.
#[derive(Debug)]
pub struct MaximizedMemory {
    /// Asked to open maximized and not yet seen so, since this time.
    waiting: bool,
    started: Option<f64>,
}

impl MaximizedMemory {
    /// `requested`: the window was asked to open maximized.
    pub fn new(requested: bool) -> Self {
        Self { waiting: requested, started: None }
    }

    /// One frame's report at `time` (seconds, any origin): the value to remember, if any.
    pub fn observe(&mut self, reported: Option<bool>, time: f64) -> Option<bool> {
        let reported = reported?;
        let started = *self.started.get_or_insert(time);
        if reported {
            self.waiting = false;
        } else if self.waiting {
            if time - started < MAXIMIZE_GRACE {
                return None;
            }
            self.waiting = false;
        }
        Some(reported)
    }
}

pub struct App {
    /// What this OS and session support, probed once at start-up.
    pub caps: Caps,
    /// The window is drawn by a software OpenGL renderer (llvmpipe in the server package, #46):
    /// every frame costs tens of milliseconds of CPU time, so high frequency visuals stay off.
    pub software_gl: bool,
    pub settings: Settings,
    pub settings_dirty: bool,
    /// What of the window's maximized state is remembered, see [`MaximizedMemory`].
    maximized_memory: MaximizedMemory,
    pub catalog: SkinCatalog,
    pub palettes: PaletteCatalog,
    pub palette_ui: settings_page::PaletteUi,
    pub sampler: Sampler,
    /// What the alerts on the sampler thread read; kept current from the settings every frame.
    alert_cfg: std::sync::Arc<std::sync::Mutex<crate::alerts::Config>>,
    pub page: Page,
    pub device: Device,
    pub popout_open: bool,
    popout_only: bool,
    /// Whether the popout's corners can be transparent here (#166), decided once at start-up.
    popout_corners: crate::platform::transparency::Corners,
    pub filter: String,
    pub sort: (Column, bool),
    pub selected: Option<(u32, i64)>,
    pending: Option<process_actions::Pending>,
    /// The "Set affinity" dialog.
    affinity: Option<affinity_dialog::AffinityDialog>,
    /// The "Run new task" dialog.
    run_task: run_task::RunTaskUi,
    file_in_use: file_in_use::FileInUseUi,
    /// A process action running as administrator (`pkexec`); its status text arrives here.
    elevated_job: Option<std::sync::mpsc::Receiver<String>>,
    /// A memory dump being written (process name, result), see `start_dump`.
    dump_job: Option<(String, std::sync::mpsc::Receiver<Result<std::path::PathBuf, String>>)>,
    /// A finished dump (process name, file) the dialog shows.
    dump_done: Option<(String, std::path::PathBuf)>,
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
    /// The interval the sampler was last given (slower while hidden in the tray; see
    /// [`App::set_sample_interval`]); App history counts each sample as that long.
    sample_interval: Option<Duration>,
    /// The power source, for "Turn on automatically on battery" (#194).
    battery: crate::lowpower::BatteryWatch,
    last_frame_at: Instant,
    /// The last frame asked for the next one at the frame rate (something moved, #194).
    paced: bool,
    pub smooth: Smooth,
    pub popout_textures: popout::Textures,
    pub recorder: Recorder,
    pub bench: Bench,
    pub bench_ui: bench_page::BenchUi,
    pub pro: pro_pages::ProState,
    pub icons: icons::IconCache,
    pub proc: processes::ProcState,
    pub replace: replace::ReplaceUi,
    /// The notification area icon, the shortcut and the window hidden in the tray.
    pub tray_ui: tray::TrayUi,
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
    /// Settings section the Settings page scrolls to on its next frame.
    pending_section: Option<usize>,
    /// A `--screenshot=` run.
    shot: Option<crate::shot::Run>,
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
        let settings_maximized = settings.window.maximized;
        let display_font = install_display_font(&cc.egui_ctx, &settings.appearance.display_font);
        let ctx = cc.egui_ctx.clone();
        csv::set_owner(&ctx, main_hwnd(cc));
        let alert_cfg = std::sync::Arc::new(std::sync::Mutex::new(crate::alerts::Config::from_settings(&settings.alerts, settings.fahrenheit())));
        let alerts_hook = crate::alerts::hook(alert_cfg.clone());
        let start_interval = crate::lowpower::interval(settings.update_speed().interval(), settings.general.low_power, false);
        let battery_ctx = cc.egui_ctx.clone();
        let battery = crate::lowpower::BatteryWatch::start(crate::pro::power::source, move || battery_ctx.request_repaint());
        let sampler = Sampler::start(start_interval, history_capacity(settings.graphs.history_multiplier), move |snap, rows| {
            alerts_hook(snap, rows);
            ctx.request_repaint();
        });
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
        let (popout_corners, corner_facts) = crate::platform::transparency::current(popout_only, software_gl);
        if launch.shot.is_some() && popout_only {
            eprintln!("TMOP: popout corners {popout_corners:?} ({corner_facts:?})");
        }
        let docker = docker::DockerState::new(&cc.egui_ctx, &settings);
        // The pipe first: only the copy that has it gets the tray icon and the shortcut.
        let claim_for = if launch.replacing { crate::tray::CLAIM_RETRY } else { Duration::ZERO };
        let replace = replace::ReplaceUi::new(&cc.egui_ctx, !popout_only && caps.taskmgr_replace, claim_for);
        // A screenshot run has no icon or shortcut (its close must end it), and its Settings
        // section shows the switches as they are, not the note for a second copy.
        let shot = launch.shot.is_some();
        let owns_tray = crate::tray::owns_tray(caps.tray, popout_only, replace.listening()) && !shot;
        let role = tray::Role { owns_tray, other_copy: caps.tray && !popout_only && !owns_tray && !shot, replacing: launch.replacing };
        let tray_ui = tray::TrayUi::new(&cc.egui_ctx, main_hwnd(cc), &settings.tray, role, launch.tray && !shot);
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
            maximized_memory: MaximizedMemory::new(settings_maximized),
            catalog,
            palettes,
            palette_ui: settings_page::PaletteUi::new(main_hwnd(cc)),
            sampler,
            alert_cfg,
            page,
            device,
            popout_open: popout_only || settings_popout_on,
            popout_only,
            popout_corners,
            filter: String::new(),
            sort: (Column::Cpu, true),
            selected: None,
            pending: None,
            affinity: None,
            run_task: run_task::RunTaskUi::new(main_hwnd(cc)),
            file_in_use: file_in_use::FileInUseUi::new(main_hwnd(cc)),
            elevated_job: None,
            dump_job: None,
            dump_done: None,
            machine: if launch.shot.is_some() { crate::shot::MACHINE.into() } else { chrome::machine_line() },
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
            sample_interval: None,
            battery,
            last_frame_at: Instant::now(),
            paced: true,
            smooth: Smooth::default(),
            popout_textures: popout::Textures::default(),
            recorder: Recorder::default(),
            bench: Bench::default(),
            bench_ui: bench_page::BenchUi { table: table::TableState::sorted_by(0, true), ..Default::default() },
            pro: pro_pages::ProState::default(),
            icons: icons::IconCache::new(&cc.egui_ctx),
            proc: processes::ProcState::default(),
            replace,
            tray_ui,
            updates: updates::UpdateUi::default(),
            status_seen: (String::new(), Instant::now()),
            ip: (None, None),
            profile: Profile::from_env(),
            pending_section: launch.settings_section.as_deref().and_then(|name| settings_page::section_index(name, &caps)),
            shot: launch.shot.map(|plan| {
                let run = crate::shot::Run::new(plan);
                if popout_only {
                    run.popout(popout_corners)
                } else {
                    run
                }
            }),
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
        let set = if self.settings.appearance.bloom { (self.settings.appearance.bloom_level.min(10) as f32 / 10.0).max(0.1) } else { 0.0 };
        crate::lowpower::bloom(set, self.low_power().is_some())
    }

    /// Whether low-power mode is on, and why (#194).
    pub fn low_power(&self) -> Option<crate::lowpower::Why> {
        let g = &self.settings.general;
        crate::lowpower::active(g.low_power, g.low_power_on_battery, self.battery.on_battery())
    }

    /// Whether Settings offers "Turn on automatically on battery": a battery was read, or the
    /// option is on already (so it can always be switched off again).
    pub fn battery_known(&self) -> bool {
        self.battery.has_battery() || self.settings.general.low_power_on_battery
    }

    /// The sample interval in use while the window is shown: the configured one, slower in
    /// low-power mode unless a recording runs. Pages that name the rate or a history span use it.
    pub fn interval(&self) -> Option<Duration> {
        crate::lowpower::interval(self.settings.update_speed().interval(), self.low_power().is_some(), self.recorder.is_recording())
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
        // The keys belong to the shortcut field of Settings while it listens.
        if self.tray_ui.capturing() {
            return;
        }
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
            format!("{:.0} \u{b0}F", to_fahrenheit(c))
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
            let ev = popout::draw(ui, rect, skin, fd, op, fahrenheit, self.popout_corners, &mut self.popout_textures, &mut self.icons);
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

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe 0.37 skips `ui` while the window is minimized or hidden (0.29 still ran `update`
        // then): new samples are taken in here, so recording and App history go on meanwhile.
        let (snap, _, rows) = self.snapshot();
        self.take_sample(&snap, &rows, Instant::now());
        if !self.popout_only {
            self.window_logic(ctx);
            // Here rather than in `ui`, which does not run while the window is hidden in the
            // notification area: a TMOP that stays there for days still checks once a day.
            self.updates.check_while_running(&self.settings.general);
            // Wakes a TMOP that is hidden and paused (nothing else would) for the next look.
            ctx.request_repaint_after(updates::LOOK_EVERY);
            self.updates_poll(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Hidden in the notification area: eframe still calls this for a window that is hidden
        // but not minimized, so draw nothing and ask for no repaint (see `ui::tray`).
        if self.tray_ui.vis.hidden && !self.popout_open {
            return;
        }
        let t_update = Instant::now();
        self.update_inner(ui);
        if let Some(run) = &mut self.shot {
            run.step(ui.ctx());
        }
        if let Some(p) = &mut self.profile {
            p.frames += 1;
            p.busy += t_update.elapsed();
            p.frame_cpu += frame.info().cpu_usage.unwrap_or(0.0) as f64;
            p.paced += u64::from(self.paced);
            for c in ui.ctx().repaint_causes() {
                *p.causes.entry(format!("{c:?}")).or_default() += 1;
            }
            // A measurement-only window state (TMOP_PROFILE_WINDOW), once, after the first seconds.
            if let Some(cmd) = p.window.take_due(p.started.elapsed()) {
                ui.ctx().send_viewport_cmd(cmd);
            }
            if p.since.elapsed() >= Duration::from_secs(2) {
                let page = if self.popout_only { "popout" } else { self.page.id() };
                let line = p.line(page);
                let _ = std::fs::OpenOptions::new().create(true).append(true).open(&p.path).and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
                p.reset();
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.tray_ui.shut_down();
        self.recorder.stop();
        // Hangs up open container terminals; bounded so that exit waits at most a second.
        self.docker.ui.detail.terminals.close_for_exit(std::time::Duration::from_secs(1));
        crate::process::netio::set_wanted(false);
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

/// Frame and sampling statistics, one line every 2 s, while `TMOP_PROFILE` is set (#194). Its
/// value is the file to append to when it names one (contains a path separator or a dot), else
/// `%TEMP%\tmop-profile.txt`. `scripts/measure.ps1` reads these lines. Fields (`key=value`):
/// `t` seconds since start, `page`, `fps`, `paced` the share of frames that asked for the next at
/// the frame rate (something moved), `ui_ms` the app's own update per frame, `frame_ms`
/// eframe's CPU time per frame (update plus tessellation and painting), `ui_cpu` and
/// `sampler_cpu` the UI and sampler threads' CPU time in % of one core (Windows only), then the
/// sampler's parts as `<part>=<runs>x<avg ms>` and the repaint causes.
///
/// `TMOP_PROFILE_WINDOW=minimized|hidden` minimizes or hides the window 3 s after the start, to
/// measure those states without touching the window. `hidden` hides the bare window; it is not
/// tray mode's hiding (`ui::tray`), which also stops drawing.
pub struct Profile {
    frames: u64,
    busy: Duration,
    frame_cpu: f64,
    /// Frames that asked for the next one at the frame rate.
    paced: u64,
    saves: u64,
    since: Instant,
    causes: std::collections::BTreeMap<String, u64>,
    started: Instant,
    path: std::path::PathBuf,
    ui_cpu: Option<Duration>,
    sampler_cpu: Duration,
    window: ProfileWindow,
}

impl Profile {
    fn from_env() -> Option<Self> {
        let value = std::env::var_os("TMOP_PROFILE")?;
        let named = value.to_string_lossy().contains(['/', '\\', '.']);
        let path = if named { std::path::PathBuf::from(value) } else { std::env::temp_dir().join("tmop-profile.txt") };
        let window = ProfileWindow::parse(&std::env::var("TMOP_PROFILE_WINDOW").unwrap_or_default());
        let now = Instant::now();
        Some(Self {
            frames: 0,
            busy: Duration::ZERO,
            frame_cpu: 0.0,
            paced: 0,
            saves: 0,
            since: now,
            causes: Default::default(),
            started: now,
            path,
            ui_cpu: crate::metrics::cost::thread_cpu(),
            sampler_cpu: crate::metrics::cost::sampler_cpu(),
            window,
        })
    }

    /// The line for the period since the last reset.
    fn line(&self, page: &str) -> String {
        let secs = self.since.elapsed().as_secs_f64().max(1e-3);
        let per_frame = |d: f64| d * 1000.0 / self.frames.max(1) as f64;
        let share = |d: Duration| d.as_secs_f64() / secs * 100.0;
        let ui_cpu = match (self.ui_cpu, crate::metrics::cost::thread_cpu()) {
            (Some(a), Some(b)) => format!("{:.1}", share(b.saturating_sub(a))),
            _ => "n/a".into(),
        };
        let sampler = crate::metrics::cost::sampler_cpu();
        let sampler_cpu = if sampler.is_zero() { "n/a".into() } else { format!("{:.1}", share(sampler.saturating_sub(self.sampler_cpu))) };
        let parts: Vec<String> = crate::metrics::cost::take()
            .iter()
            .zip(crate::metrics::cost::Part::ALL)
            .map(|((runs, total), part)| format!("{}={}x{:.2}", part.name(), runs, total.as_secs_f64() * 1000.0 / (*runs).max(1) as f64))
            .collect();
        // A cause is `<file>:<line>`; the last two path parts are enough (and name no home folder).
        let short = |k: &str| {
            let parts: Vec<&str> = k.split(['/', '\\']).collect();
            parts[parts.len().saturating_sub(2)..].join("/").replace(' ', "")
        };
        let causes: Vec<String> = self.causes.iter().map(|(k, n)| format!("{n}:{}", short(k))).collect();
        format!(
            "t={:.1} page={page} window={} fps={:.1} paced={:.0}% ui_ms={:.2} frame_ms={:.2} ui_cpu={ui_cpu} sampler_cpu={sampler_cpu} {} saves={} causes={}\n",
            self.started.elapsed().as_secs_f64(),
            self.window.state(),
            self.frames as f64 / secs,
            self.paced as f64 * 100.0 / self.frames.max(1) as f64,
            per_frame(self.busy.as_secs_f64()),
            per_frame(self.frame_cpu),
            parts.join(" "),
            self.saves,
            causes.join(",")
        )
    }

    /// Starts the next period.
    fn reset(&mut self) {
        self.frames = 0;
        self.busy = Duration::ZERO;
        self.frame_cpu = 0.0;
        self.paced = 0;
        self.saves = 0;
        self.since = Instant::now();
        self.causes.clear();
        self.ui_cpu = crate::metrics::cost::thread_cpu();
        self.sampler_cpu = crate::metrics::cost::sampler_cpu();
    }
}

/// The window state `TMOP_PROFILE_WINDOW` asks for, and whether it was applied.
#[derive(Debug, PartialEq)]
enum ProfileWindow {
    Normal,
    Minimized { done: bool },
    Hidden { done: bool },
}

impl ProfileWindow {
    fn parse(v: &str) -> Self {
        match v.trim() {
            "minimized" => ProfileWindow::Minimized { done: false },
            "hidden" => ProfileWindow::Hidden { done: false },
            _ => ProfileWindow::Normal,
        }
    }

    fn state(&self) -> &'static str {
        match self {
            ProfileWindow::Normal => "normal",
            ProfileWindow::Minimized { done: true } => "minimized",
            ProfileWindow::Hidden { done: true } => "hidden",
            _ => "pending",
        }
    }

    /// The command to send now, once, `running` after the start.
    fn take_due(&mut self, running: Duration) -> Option<ViewportCommand> {
        if running < Duration::from_secs(3) {
            return None;
        }
        match self {
            ProfileWindow::Minimized { done } if !*done => {
                *done = true;
                Some(ViewportCommand::Minimized(true))
            }
            ProfileWindow::Hidden { done } if !*done => {
                *done = true;
                Some(ViewportCommand::Visible(false))
            }
            _ => None,
        }
    }
}

impl App {
    /// Sets the sampler's interval (`None`: paused) and remembers it.
    pub fn set_sample_interval(&mut self, interval: Option<Duration>) {
        self.sample_interval = interval;
        self.sampler.set_interval(interval);
    }

    /// Feeds a sample the sampler has not delivered before to the recorder, App history and the
    /// temperature log.
    fn take_sample(&mut self, snap: &Snapshot, rows: &[ProcessRow], now: Instant) {
        if snap.seq != self.last_seq {
            self.last_seq = snap.seq;
            self.last_sample_at = now;
            self.recorder.docker = (self.docker.last_status == EngineStatus::Connected).then_some(self.docker.ui.snap.totals);
            self.recorder.on_sample(snap, rows);
            let dt = self.sample_interval.or_else(|| self.interval()).map(|d| d.as_secs_f64()).unwrap_or(1.0);
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
        // The tables read their column orders from the context (see `table::sync_orders`).
        table::sync_orders(ctx, &mut self.settings.window.table_column_order);
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
        // After a pause in the frames (nothing moved) an ease starts with one frame's step, not
        // with the whole pause, so it glides in as it would at the frame rate.
        let frame_dt = 1.0 / self.settings.appearance.frame_rate.clamp(10, 240) as f32;
        let dt = if self.paced { now.duration_since(self.last_frame_at).as_secs_f32().min(0.25) } else { frame_dt };
        self.last_frame_at = now;
        // Cleared here: what this frame draws says whether the next one comes at the frame rate.
        let _ = widgets::take_motion();
        self.take_sample(&snap, &rows, now);
        let interval = self.interval();
        // Low-power mode (#194): no glide, no glow or fill on graphs; the settings stay as they are.
        let low = self.low_power().is_some();
        self.battery.set_wanted(self.settings.general.low_power_on_battery);
        widgets::set_plain(!crate::lowpower::fills(low));
        let hf = crate::lowpower::glide(high_frequency(self.settings.appearance.high_frequency_visuals, self.software_gl), low);
        let phase = match interval {
            Some(iv) if hf => (now.duration_since(self.last_sample_at).as_secs_f32() / iv.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let smooth_moving = self.smooth.step(&snap, dt, hf);
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
        } else if self.tray_ui.vis.hidden {
            // Hidden in the notification area: only the popout is drawn.
            if self.popout_open {
                self.popout_viewport(ctx, &fd);
            }
        } else {
            if self.page != Page::Settings {
                self.tray_ui.end_capture(&self.settings.tray);
            }
            // Remembered for the next start (window.maximized). A minimized window keeps what
            // it was before.
            let (maximized, minimized, time) = ctx.input(|i| (i.viewport().maximized, i.viewport().minimized, i.time));
            if !minimized.unwrap_or(false) {
                if let Some(maximized) = self.maximized_memory.observe(maximized, time) {
                    if maximized != self.settings.window.maximized {
                        self.settings.window.maximized = maximized;
                        self.settings_dirty = true;
                    }
                }
            }
            self.replace_poll(ctx);
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
            self.dump_dialog(ctx, &pal);
            self.startup_error_dialog(ctx, &pal);
            self.service_dialogs(ctx, &pal);
            self.affinity_dialog(ctx, &pal);
            self.run_task_dialog(ctx, &pal);
            self.file_in_use_dialog(ctx, &pal);
            self.replace_dialog(ctx, &pal);
            self.updates_dialog(ctx, &pal);
            self.disk_delete_dialogs(ctx, &pal);
            self.draw_toasts(ctx);
            if self.popout_open {
                self.popout_viewport(ctx, &fd);
            }
        }

        // Apply cadence changes and persist.
        self.set_sample_interval(self.interval());
        {
            let cfg = crate::alerts::Config::from_settings(&self.settings.alerts, self.settings.fahrenheit());
            let mut shared = self.alert_cfg.lock().unwrap_or_else(|e| e.into_inner());
            if *shared != cfg {
                *shared = cfg;
            }
        }
        // Sampled at once, so that switching off closes the device even while updates are paused.
        if crate::metrics::pawnio::set_enabled(self.caps.pawnio && self.settings.general.pawnio_cpu_temperature) {
            self.sampler.refresh();
        }
        // The network counters run while the Processes table shows their column (a popout-only
        // TMOP has no table).
        crate::process::netio::set_wanted(!self.popout_only && !self.tray_ui.vis.hidden && self.caps.proc.network && self.column_visible(Column::Network));
        // What a part of a page had to tell the user (a file manager that would not open).
        if let Some(message) = kit::take_report(ctx) {
            self.status = message;
        }
        csv::poll(ctx);
        self.poll_palette_dialog();
        self.save_palette_edits(false);
        if table::sync_orders(ctx, &mut self.settings.window.table_column_order) {
            self.settings_dirty = true;
        }
        if self.settings_dirty {
            self.save_settings();
        }
        // Frame pacing. egui shortens every delay by its predicted frame time, which would
        // double the rate (or pin it to the monitor's refresh), so add that back to get the
        // configured appearance.frameRate.
        // At that rate only while something moves on its own (#194): a gliding graph, an eased
        // readout still on its way, a pulse or a running clock (`widgets::note_motion`). Else the
        // next frame comes with the next sample (the sampler asks for it), with input, with an
        // animation of egui's own, or with this heartbeat at the latest (status messages that
        // time out, clocks while sampling is paused).
        if self.recorder.is_recording() {
            widgets::note_motion();
        }
        let fps = self.settings.appearance.frame_rate.clamp(10, 240) as f32;
        let drawn_motion = widgets::take_motion();
        self.paced = hf && (drawn_motion || smooth_moving);
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let wait = if self.paced { Duration::from_secs_f32(1.0 / fps) } else { crate::lowpower::heartbeat(low, focused) };
        let predicted = ctx.input(|i| i.predicted_dt).clamp(0.0, 0.1);
        ctx.request_repaint_after(wait + Duration::from_secs_f32(predicted));
    }
}

/// °C as °F.
pub(crate) fn to_fahrenheit(c: f32) -> f32 {
    c * 9.0 / 5.0 + 32.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window asked to open maximized may report `false` first: that must not clear the setting.
    #[test]
    fn maximized_is_not_forgotten_before_the_window_is_seen_maximized() {
        let mut m = MaximizedMemory::new(true);
        assert_eq!(m.observe(None, 0.0), None);
        assert_eq!(m.observe(Some(false), 0.1), None);
        assert_eq!(m.observe(Some(false), 1.0), None);
        assert_eq!(m.observe(Some(true), 1.2), Some(true));
        // Seen maximized: from here on the user's un-maximize is recorded at once.
        assert_eq!(m.observe(Some(false), 1.3), Some(false));
        // The request was never applied (refused): after the grace period false counts.
        let mut m = MaximizedMemory::new(true);
        assert_eq!(m.observe(Some(false), 10.0), None);
        assert_eq!(m.observe(Some(false), 12.9), None);
        assert_eq!(m.observe(Some(false), 13.0), Some(false));
        // Not asked to maximize: reports are taken as they come.
        let mut m = MaximizedMemory::new(false);
        assert_eq!(m.observe(Some(false), 0.0), Some(false));
        assert_eq!(m.observe(Some(true), 0.1), Some(true));
    }

    /// Eased readouts move towards a new sample over several frames, then take the exact value
    /// and report that nothing moves any more, so the frames can stop (#194).
    #[test]
    fn eased_values_settle_on_the_sample_and_then_stop_moving() {
        let mut s = Snapshot::default();
        s.cpu.total_pct = 40.0;
        s.memory.used_pct = 55.5;
        s.cpu.current_mhz = Some(4200);
        let mut sm = Smooth::default();
        let dt = 1.0 / 60.0;
        assert!(sm.step(&s, dt, true), "a jump of 40 points eases");
        assert!(sm.cpu > 0.0 && sm.cpu < 40.0, "{}", sm.cpu);
        let mut frames = 1;
        while sm.step(&s, dt, true) {
            frames += 1;
            assert!(frames < 120, "never settles");
        }
        assert!(frames > 10, "eases over several frames, not {frames}");
        assert_eq!((sm.cpu, sm.memory), (40.0, 55.5), "the readouts end on the sample exactly");
        assert!(sm.clock > 4100.0 && sm.clock < 4200.0, "the clock eases on without holding frames: {}", sm.clock);
        assert!(!sm.step(&s, dt, true), "a settled value stays still");
        // Off: values follow at once and never ask for frames.
        let mut off = Smooth::default();
        assert!(!off.step(&s, dt, false));
        assert_eq!(off.cpu, 40.0);
        // A reading that is not a number cannot keep the frames going.
        s.gpu.utilization_pct = f32::NAN;
        assert!(!sm.step(&s, dt, true));
        assert!(!sm.step(&s, dt, true), "nor later");
    }

    /// TMOP_PROFILE_WINDOW sends its one command after 3 s, and only once.
    #[test]
    fn profile_window_state_is_sent_once_after_the_start() {
        let mut w = ProfileWindow::parse("minimized");
        assert_eq!(w.take_due(Duration::from_secs(1)), None);
        assert_eq!(w.state(), "pending");
        assert_eq!(w.take_due(Duration::from_secs(3)), Some(ViewportCommand::Minimized(true)));
        assert_eq!(w.take_due(Duration::from_secs(9)), None);
        assert_eq!(w.state(), "minimized");
        assert_eq!(ProfileWindow::parse("hidden").take_due(Duration::from_secs(5)), Some(ViewportCommand::Visible(false)));
        let mut n = ProfileWindow::parse("");
        assert_eq!((n.take_due(Duration::from_secs(5)), n.state()), (None, "normal"));
    }

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
