#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! TMOP entry point.
//!
//! Usage:
//!   TMOP                  main window (page from `defaultPage`)
//!   TMOP --version | -V   print the version and exit
//!   TMOP --help | -h      print the user-facing options and exit (text in src/cli.rs)
//!   TMOP --page=<id>     start on summary|performance|processes|docker|settings
//!   TMOP --docker-terminal=<container>[;<command>]
//!                         open a terminal session in a container on the Docker page
//!   TMOP --docker-tab=<tab>    open the Docker page on engine (local)|containers|stacks|images|volumes|networks (test-only)
//!   TMOP --engine-test=<op>    setup|wizard|start|stop|restart|remove|update the TMOP engine at start-up (test-only)
//!   TMOP --disk-root=<path>    start on Disk Space and scan <path> (alias --disk-scan=)
//!   TMOP --screenshot=<file.png> [--page=<id>] [--device=<id>] [--window-size=WxH] [--theme=dark|light]
//!        [--settings-section=<name>] [--wait=<s>] [--frames=<n> --every=<ms>] [--shot-home=<dir>]
//!                         render the window's own frames to PNG files and exit (see src/shot.rs)
//!   TMOP --popout         only the skinned popout window
//!   TMOP --dump           print two samples to stdout and exit (no UI)
//!   TMOP --restore-taskmgr  stop opening TMOP in place of Windows Task Manager
//!   TMOP /s | /p <hwnd> | /c   screensaver conventions (Windows, built with `--features screensaver`; a no-op elsewhere)
//!   TMOP --engine-relay        run the Docker engine named-pipe relay (single instance, Windows only)
//!   TMOP --engine-relay-stop   ask the running relay to stop, then exit (Windows only)
//!   TMOP --volume-caches=query:<drive>[:<ids>]   helper: print the Windows Disk Cleanup handlers of <drive> (C or C:; only <ids> when given) as JSON lines (Windows only)
//!   TMOP --volume-caches=purge:<drive>:<id1,id2,...>   helper: purge exactly those handler ids (Clean up starts it after the confirm dialog)
//!
//! The `--engine-relay` modes honour the `TMOP_ENGINE_*` test overrides (`tmop::engine::Overrides`).

use std::io::{self, BufWriter, Write};
use tmop::metrics::Collector;
use tmop::settings;
use tmop::ui::{App, Launch, Page};

/// Exit code for a command that prints to stdout. A reader that closes the pipe early
/// (`| head -1`, `| grep -q`) is a normal end, not an error; `println!` would panic on it (#78).
fn output_exit_code(result: io::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            eprintln!("TMOP: cannot write the output: {e}");
            1
        }
    }
}

/// Runs a print-and-exit command through a locked, buffered stdout and returns its exit code.
fn run_output(write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> i32 {
    let mut out = BufWriter::new(io::stdout().lock());
    let result = write(&mut out).and_then(|()| out.flush());
    output_exit_code(result)
}

/// A value that may be absent: the bare value, or `n/a`.
fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "n/a".to_string(), |v| v.to_string())
}

/// `(name, number)` pairs as `name 1.5<unit>, name 2.0<unit>`; `n/a` for none.
fn pairs(v: &[(String, f32)], unit: &str) -> String {
    if v.is_empty() {
        return "n/a".to_string();
    }
    v.iter().map(|(k, n)| format!("{k} {n:.1}{unit}")).collect::<Vec<_>>().join(", ")
}

/// One GPU adapter as labelled fields; absent readings are `n/a`.
fn adapter_text(a: &tmop::metrics::GpuAdapter) -> String {
    let temps = if a.temps.is_empty() { "n/a".to_string() } else { a.temps.iter().map(|t| format!("{} {:.1} C ({})", t.kind.label(), t.c, t.source)).collect::<Vec<_>>().join(", ") };
    format!(
        "id {}  name {}  utilization {:.1}%  engines {}  dedicated {} / {} bytes  shared {} / {} bytes  temps {temps}  power {} W  clock {} MHz",
        a.id,
        a.name,
        a.utilization_pct,
        pairs(&a.engines, "%"),
        opt(a.dedicated_used),
        opt(a.dedicated_total),
        opt(a.shared_used),
        opt(a.shared_total),
        opt(a.power_w.map(|w| format!("{w:.1}"))),
        opt(a.clock_mhz),
    )
}

/// Writes the two-sample report to `out`; the caller decides what a failed write means.
fn dump(out: &mut dyn Write) -> io::Result<()> {
    tmop::metrics::set_gpu_detail(true);
    let mut c = Collector::new();
    let _ = c.collect();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let (s, procs) = c.collect();
    writeln!(out, "CPU      {:5.1}%  kernel {:4.1}%  cores {}  clock {} MHz", s.cpu.total_pct, s.cpu.kernel_pct, s.cpu.per_core_pct.len(), opt(s.cpu.current_mhz))?;
    if cfg!(windows) {
        writeln!(
            out,
            "Clock    base {} MHz  MaxMhz {}  MhzLimit {}  %ProcessorPerformance {}  ProcessorFrequency {}  live {}",
            opt(s.cpu.base_mhz),
            opt(s.cpu.max_mhz),
            opt(s.cpu.mhz_limit),
            opt(s.cpu.perf_pct),
            opt(s.cpu.pdh_freq_mhz),
            s.cpu.clock_live
        )?;
    } else {
        let diag: String = s.cpu.diag.iter().map(|(k, v)| format!("  {k} {v}")).collect();
        writeln!(out, "Clock    base {} MHz  max {}  boost {}  live {}{diag}", opt(s.cpu.base_mhz), opt(s.cpu.max_mhz), opt(s.cpu.boost_mhz), s.cpu.clock_live)?;
    }
    writeln!(out, "Memory   {:5.1}%  {} / {} bytes", s.memory.used_pct, s.memory.total.saturating_sub(s.memory.available), s.memory.total)?;
    if !cfg!(windows) {
        writeln!(out, "Swap     {} / {} bytes  commit {} / {}", opt(s.memory.swap_used), opt(s.memory.swap_total), opt(s.memory.commit_used), opt(s.memory.commit_limit))?;
    }
    writeln!(out, "Disk     {:5.1}%  read {:.0} B/s  write {:.0} B/s", s.disk.active_pct, s.disk.read_bps, s.disk.write_bps)?;
    for d in &s.disk.drives {
        let space = d.space.map(|(used, total)| format!("  {used} / {total} bytes")).unwrap_or_default();
        let firmware = if d.firmware.is_empty() { String::new() } else { format!("  firmware {}", d.firmware) };
        writeln!(out, "  {}  [{}]{firmware}  {:5.1}%  read {:.0} B/s  write {:.0} B/s{space}", d.title, d.model, d.active_pct, d.read_bps, d.write_bps)?;
    }
    writeln!(out, "Network  rx {:.0} B/s  tx {:.0} B/s  link {:.0} b/s", s.network.rx_bps, s.network.tx_bps, s.network.link_bps)?;
    writeln!(out, "GPU      {:5.1}%  available {}  engines {}", s.gpu.utilization_pct, s.gpu.available, pairs(&s.gpu.engines, "%"))?;
    for (i, a) in s.gpu.adapters.iter().enumerate() {
        writeln!(out, "GPU {i}    {}", adapter_text(a))?;
    }
    writeln!(out, "GPU      {} processes", s.gpu.processes.len())?;
    if let Some(note) = &s.gpu.note {
        writeln!(out, "GPU      {note}")?;
    }
    let cpu_temp = s.thermal.cpu.as_ref().map(|(label, c)| format!("{label} {c:.1} C"));
    writeln!(out, "Thermal  {}  cpu {}  gpu {}  [{}]", pairs(&s.thermal.zones, " C"), opt(cpu_temp), pairs(&s.thermal.gpu, " C"), s.thermal.sources)?;
    let handles = s.handle_count.map(|h| format!(", {h} {}", tmop::metrics::SOURCES.handles.to_lowercase())).unwrap_or_default();
    writeln!(out, "Procs    {} processes, {} threads{handles}", s.process_count, s.thread_count)?;
    let mut top: Vec<_> = procs.iter().collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.private_working_set));
    for p in top.iter().take(5) {
        writeln!(out, "  {:>6} {:<28} {:>10} KB  user {}", p.pid, p.name, p.private_working_set / 1024, opt(p.user.as_deref()))?;
    }
    for e in &s.errors {
        writeln!(out, "error: {e}")?;
    }
    Ok(())
}

#[cfg(all(windows, feature = "screensaver"))]
mod scr {
    use eframe::egui;
    use tmop::metrics::{history_capacity, Sampler};
    use tmop::screensaver::ScrMode;
    use tmop::settings::{self, Settings};
    use tmop::ui::theme::{self, MetricColors};
    use tmop::ui::widgets::{draw_graph, Graph, GraphLine};

    pub fn run(mode: ScrMode) -> eframe::Result<()> {
        match mode {
            // Rendering into the Control Panel preview HWND is not supported by eframe;
            // exit quietly so Windows shows its default preview (design choice).
            ScrMode::Preview(_) => Ok(()),
            ScrMode::Configure => eframe::run_ui_native(
                "TMOP screensaver",
                eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 120.0]), ..Default::default() },
                |ui, _| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.label("TMOP screensaver: CPU, GPU and memory traces.");
                        ui.label("Windows controls idle time and sign-in on resume.");
                    });
                },
            ),
            ScrMode::Run { .. } => {
                let opts = eframe::NativeOptions {
                    viewport: egui::ViewportBuilder::default().with_fullscreen(true).with_decorations(false).with_always_on_top(),
                    ..Default::default()
                };
                eframe::run_native(
                    "TMOP screensaver",
                    opts,
                    Box::new(|cc| {
                        theme::install_fonts(&cc.egui_ctx, None);
                        let ctx = cc.egui_ctx.clone();
                        let sampler = Sampler::start(Some(std::time::Duration::from_millis(500)), history_capacity(30), move |_, _| ctx.request_repaint());
                        Ok(Box::new(Saver { sampler, start: std::time::Instant::now(), origin: None, colors: metric_colors() }))
                    }),
                )
            }
        }
    }

    /// Metric colours from the settings file, read without migrating or writing it.
    fn metric_colors() -> MetricColors {
        let s = std::fs::read_to_string(settings::default_path()).ok().and_then(|t| Settings::from_json(&t).ok()).map(|l| l.settings).unwrap_or_default();
        MetricColors::from_settings(&s.graphs)
    }

    struct Saver {
        sampler: Sampler,
        start: std::time::Instant,
        origin: Option<egui::Pos2>,
        colors: MetricColors,
    }

    impl eframe::App for Saver {
        fn ui(&mut self, root: &mut egui::Ui, _f: &mut eframe::Frame) {
            let ctx = &root.ctx().clone();
            let h = self.sampler.shared.lock().map(|g| std::sync::Arc::clone(&g.history)).ok();
            egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::BLACK)).show(root, |ui| {
                let r = ui.max_rect().shrink(40.0);
                if let Some(h) = h {
                    // Glowing traces on black.
                    let rows = [("cpu", "CPU", &h.cpu), ("gpu", "GPU", &h.gpu), ("memory", "MEMORY", &h.memory)];
                    let rh = r.height() / 3.0;
                    for (i, (k, label, s)) in rows.iter().enumerate() {
                        let rr = egui::Rect::from_min_size(r.min + egui::vec2(0.0, i as f32 * rh + 40.0), egui::vec2(r.width(), rh - 60.0));
                        let c = self.colors.get(k);
                        let mut g = Graph::new(vec![GraphLine { series: s, color: c, range: (0.0, 100.0) }]);
                        g.px_per_update = (rr.width() / s.capacity.max(2) as f32).max(2.0);
                        g.bloom = 1.0;
                        g.line_width = 3.0;
                        g.fill = 0.10;
                        g.markers = false;
                        draw_graph(ui.painter(), rr, &g);
                        ui.painter().text(rr.left_top() - egui::vec2(0.0, 36.0), egui::Align2::LEFT_TOP, format!("{label}  {:.0}%", s.last()), theme::display(26.0), c);
                    }
                }
            });
            // Any input after a short grace period ends the screensaver.
            let quit = ctx.input(|i| {
                let moved = match (i.pointer.hover_pos(), self.origin) {
                    (Some(p), Some(o)) => p.distance(o) > 12.0,
                    _ => false,
                };
                !i.events.iter().all(|e| matches!(e, egui::Event::PointerMoved(_))) || moved
            });
            if self.origin.is_none() {
                self.origin = ctx.input(|i| i.pointer.hover_pos());
            }
            if quit && self.start.elapsed().as_millis() > 1500 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

#[cfg(windows)]
fn engine_relay() -> i32 {
    tmop::engine::relay::run()
}

#[cfg(not(windows))]
fn engine_relay() -> i32 {
    eprintln!("available on Windows only");
    2
}

#[cfg(windows)]
fn engine_relay_stop() -> i32 {
    let pipe = tmop::engine::Overrides::from_env().pipe;
    if tmop::engine::relay::signal_stop(&pipe) {
        0
    } else {
        eprintln!("no engine relay is running on pipe {pipe}");
        1
    }
}

#[cfg(not(windows))]
fn engine_relay_stop() -> i32 {
    eprintln!("available on Windows only");
    2
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Before anything touches the display, the settings or the single-instance hand-off (#106).
    match tmop::cli::parse(&args) {
        tmop::cli::Cli::Run => {}
        tmop::cli::Cli::Version => {
            tmop::cli::attach_console();
            std::process::exit(run_output(|out| out.write_all(tmop::cli::version_text().as_bytes())));
        }
        tmop::cli::Cli::Help => {
            tmop::cli::attach_console();
            std::process::exit(run_output(|out| out.write_all(tmop::cli::help_text().as_bytes())));
        }
        tmop::cli::Cli::Unknown(arg) => {
            tmop::cli::attach_console();
            let _ = io::stderr().write_all(tmop::cli::unknown_text(&arg).as_bytes());
            std::process::exit(2);
        }
    }

    // The AppImage's AppRun puts its bundled libraries first on LD_LIBRARY_PATH. The loader has
    // read it already, so the programs TMOP starts (xdg-open, gdbus, ...) get the caller's back.
    #[cfg(target_os = "linux")]
    if let Some(host) = std::env::var_os("TMOP_HOST_LD_LIBRARY_PATH") {
        if host.is_empty() {
            std::env::remove_var("LD_LIBRARY_PATH");
        } else {
            std::env::set_var("LD_LIBRARY_PATH", host);
        }
        std::env::remove_var("TMOP_HOST_LD_LIBRARY_PATH");
    }

    // Task Manager replacement: the elevated helper and restore switches run without a window.
    if let Some(code) = tmop::taskmgr::run_switches(&args) {
        std::process::exit(code);
    }
    // Windows Disk Cleanup helper: runs the COM handlers in this separate process.
    if let Some(code) = tmop::pro::volcache::run_switches(&args) {
        std::process::exit(code);
    }
    // Started in place of Task Manager: a running TMOP window takes it over if there is one.
    let taskmgr = tmop::taskmgr::taskmgr_launch(&args);
    if let Some(l) = &taskmgr {
        if tmop::taskmgr::hand_off(l.page.unwrap_or("")) {
            return Ok(());
        }
    }

    #[cfg(all(windows, feature = "screensaver"))]
    {
        let looks_like_scr = args.first().map(|a| a.starts_with('/') || a == "-s" || a == "-p" || a == "-c").unwrap_or(false)
            || std::env::current_exe().map(|p| p.extension().map(|e| e.eq_ignore_ascii_case("scr")).unwrap_or(false)).unwrap_or(false);
        if looks_like_scr {
            if let Some(mode) = tmop::screensaver::parse_args(args.clone()) {
                return scr::run(mode);
            }
        }
    }

    if args.iter().any(|a| a == "--engine-relay") {
        std::process::exit(engine_relay());
    }
    if args.iter().any(|a| a == "--engine-relay-stop") {
        std::process::exit(engine_relay_stop());
    }
    if args.iter().any(|a| a == "--dump") {
        tmop::cli::attach_console();
        std::process::exit(run_output(dump));
    }
    // From here on TMOP opens a window. Without an X11 or Wayland session on Linux that ends in
    // an abort inside the graphics libraries; say so in one line instead (#106).
    #[cfg(target_os = "linux")]
    if tmop::cli::display_missing(|n| std::env::var_os(n).is_some_and(|v| !v.is_empty())) {
        let _ = io::stderr().write_all(tmop::cli::NO_DISPLAY_TEXT.as_bytes());
        std::process::exit(1);
    }
    // From here on TMOP opens a window: make failures visible (#46).
    tmop::startup::install_panic_hook();
    tmop::startup::software_opengl();
    let value = |names: &[&str]| args.iter().find_map(|a| names.iter().find_map(|n| a.strip_prefix(n)).map(|v| v.to_string()));
    let popout_only = args.iter().any(|a| a == "--popout");
    let shot = match tmop::shot::Plan::parse(&args) {
        Ok(plan) => plan,
        Err(e) => {
            tmop::startup::report(&e);
            std::process::exit(2);
        }
    };
    // Before anything reads the settings: a screenshot run never touches the user's.
    if let Some(plan) = &shot {
        tmop::shot::private_home(plan);
    }
    let launch = Launch {
        start_page: value(&["--page=", "--start-page="]).map(|p| Page::from_id(&p)).or_else(|| taskmgr.as_ref().and_then(|l| l.page).map(Page::from_id)),
        start_device: value(&["--device=", "--start-device="]),
        popout_only,
        record: if args.iter().any(|a| a == "--record" || a.starts_with("--record=")) {
            Some((value(&["--record="]).map(std::path::PathBuf::from), value(&["--record-seconds="]).and_then(|s| s.parse().ok())))
        } else {
            None
        },
        bench: value(&["--bench="]).map(|v| {
            if v == "all" {
                tmop::pro::bench::Test::ALL.to_vec()
            } else {
                v.split(',').filter_map(tmop::pro::bench::Test::from_id).collect()
            }
        }),
        disk_scan: value(&["--disk-scan=", "--disk-root="]).map(std::path::PathBuf::from),
        docker_terminal: value(&["--docker-terminal="]).and_then(|v| {
            let (container, command) = v.split_once(';').unwrap_or((v.as_str(), ""));
            let container = container.trim();
            let command = command.trim();
            (!container.is_empty()).then(|| (container.to_string(), (!command.is_empty()).then(|| command.to_string())))
        }),
        docker_tab: value(&["--docker-tab="]),
        engine_test: value(&["--engine-test="]),
        settings_section: value(&["--settings-section="]),
        shot,
    };
    let size = value(&["--window-size="]).and_then(|s| {
        let (w, h) = s.split_once(['x', 'X', ','])?;
        Some([w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?])
    });

    tmop::platform::raise_open_files_limit();
    settings::migrate_legacy_dir();
    let (mut settings, status) = settings::load_or_import();
    if let Some(skin) = value(&["--popout-skin="]) {
        settings.popout.skin = skin;
    }
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(format!("{} \u{2013} {}", tmop::APP_NAME, tmop::APP_LONG_NAME))
        .with_app_id(tmop::APP_ID)
        .with_inner_size(size.unwrap_or([1320.0, 860.0]))
        .with_min_inner_size([900.0, 600.0])
        // The main window opens as it was closed, unless a size is asked for; the pop-out alone
        // has its own fixed size.
        .with_maximized(args.iter().any(|a| a == "--maximized") || (!popout_only && size.is_none() && settings.window.maximized));
    if let Some(plan) = &launch.shot {
        settings.appearance.theme = plan.theme.clone();
        settings.popout.mode = "off".into();
        // The window renders its own frames; it need not take the focus from what the user is doing.
        viewport = viewport.with_active(false);
    }
    if let Some(icon) = tmop::icon::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    // A Wayland compositor without server-side decorations (GNOME): TMOP's header becomes the
    // title bar instead of winit's bare fallback frame.
    if !popout_only && tmop::platform::desktop::own_title_bar() {
        viewport = viewport.with_decorations(false);
    }
    if popout_only {
        let skins = tmop::skin::scan(&tmop::skin::default_skin_dirs());
        let skin = skins.get(&settings.popout.skin).cloned().unwrap_or_else(tmop::skin::Skin::classic);
        viewport = viewport
            .with_title(format!("TMOP \u{2014} {}", skin.name))
            .with_inner_size(tmop::ui::popout::desired_size(&skin))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top();
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "TMOP",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, settings, status, launch)))),
    )
    .inspect_err(|e| tmop::startup::report(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_prints_the_bare_value_or_na() {
        assert_eq!(opt(Some(4700u32)), "4700");
        assert_eq!(opt(Some(0u64)), "0");
        assert_eq!(opt(Some("x")), "x");
        assert_eq!(opt(None::<u32>), "n/a");
    }

    #[test]
    fn pairs_are_a_plain_list() {
        assert_eq!(pairs(&[], "%"), "n/a");
        assert_eq!(pairs(&[("3D".into(), 2.53), ("Copy".into(), 0.0)], "%"), "3D 2.5%, Copy 0.0%");
    }

    #[test]
    fn adapter_text_has_no_rust_syntax() {
        let a = tmop::metrics::GpuAdapter { id: "0x1".into(), name: "Card".into(), dedicated_used: Some(5), ..Default::default() };
        let t = adapter_text(&a);
        assert!(t.contains("name Card") && t.contains("dedicated 5 / n/a bytes") && t.contains("clock n/a MHz"), "{t}");
        assert!(!t.contains("Some(") && !t.contains("None") && !t.contains(char::from(34)) && !t.contains(char::from(91)), "{t}");
    }
}
