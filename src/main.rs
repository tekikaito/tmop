#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! TMOP entry point.
//!
//! Usage:
//!   TMOP                  main window (page from `defaultPage`)
//!   TMOP --page=<id>      start on summary|performance|processes|docker|settings
//!   TMOP --docker-terminal=<container>[;<command>]
//!                         open a terminal session in a container on the Docker page
//!   TMOP --docker-tab=<tab>    open the Docker page on engine|containers|stacks|images|volumes|networks (test-only)
//!   TMOP --engine-test=<op>    setup|wizard|start|stop|restart|remove|update the TMOP engine at start-up (test-only)
//!   TMOP --disk-root=<path>    start on Disk Space and scan <path> (alias --disk-scan=)
//!   TMOP --popout         only the skinned popout window
//!   TMOP --dump           print two samples to stdout and exit (no UI)
//!   TMOP --restore-taskmgr  stop opening TMOP in place of Windows Task Manager
//!   TMOP /s | /p <hwnd> | /c   screensaver conventions (build with `--features screensaver`)
//!   TMOP --engine-relay        run the Docker engine named-pipe relay (single instance, Windows only)
//!   TMOP --engine-relay-stop   ask the running relay to stop, then exit (Windows only)
//!   TMOP --volume-caches=query:<drive>[:<ids>]   helper: print the Windows Disk Cleanup handlers of <drive> (C or C:; only <ids> when given) as JSON lines (Windows only)
//!   TMOP --volume-caches=purge:<drive>:<id1,id2,...>   helper: purge exactly those handler ids (Clean up starts it after the confirm dialog)
//!
//! The `--engine-relay` modes honour the `TMOP_ENGINE_*` test overrides (`tmop::engine::Overrides`).

use tmop::metrics::Collector;
use tmop::settings;
use tmop::ui::{App, Launch, Page};

fn dump() {
    let mut c = Collector::new();
    let _ = c.collect();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let (s, procs) = c.collect();
    println!("CPU      {:5.1}%  kernel {:4.1}%  cores {}  clock {:?} MHz", s.cpu.total_pct, s.cpu.kernel_pct, s.cpu.per_core_pct.len(), s.cpu.current_mhz);
    println!(
        "Clock    base {:?} MHz  MaxMhz {:?}  MhzLimit {:?}  %ProcessorPerformance {:?}  ProcessorFrequency {:?}  live {}",
        s.cpu.base_mhz, s.cpu.max_mhz, s.cpu.mhz_limit, s.cpu.perf_pct, s.cpu.pdh_freq_mhz, s.cpu.clock_live
    );
    println!("Memory   {:5.1}%  {} / {} bytes", s.memory.used_pct, s.memory.total - s.memory.available, s.memory.total);
    println!("Disk     {:5.1}%  read {:.0} B/s  write {:.0} B/s", s.disk.active_pct, s.disk.read_bps, s.disk.write_bps);
    println!("Network  rx {:.0} B/s  tx {:.0} B/s  link {:.0} b/s", s.network.rx_bps, s.network.tx_bps, s.network.link_bps);
    println!("GPU      {:5.1}%  available {}  engines {:?}", s.gpu.utilization_pct, s.gpu.available, s.gpu.engines);
    println!("Thermal  {:?}  cpu {:?}  [{}]", s.thermal.zones, s.thermal.cpu, s.thermal.sources);
    println!("Procs    {} processes, {} threads, {} handles", s.process_count, s.thread_count, s.handle_count);
    let mut top: Vec<_> = procs.iter().collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.private_working_set));
    for p in top.iter().take(5) {
        println!("  {:>6} {:<28} {:>10} KB  user {:?}", p.pid, p.name, p.private_working_set / 1024, p.user);
    }
    for e in &s.errors {
        println!("error: {e}");
    }
}

#[cfg(feature = "screensaver")]
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
            ScrMode::Configure => eframe::run_simple_native(
                "TMOP screensaver",
                eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 120.0]), ..Default::default() },
                |ctx, _| {
                    egui::CentralPanel::default().show(ctx, |ui| {
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
                        let sampler = Sampler::start(Some(std::time::Duration::from_millis(500)), history_capacity(30), move || ctx.request_repaint());
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
        fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
            let h = self.sampler.shared.lock().map(|g| std::sync::Arc::clone(&g.history)).ok();
            egui::CentralPanel::default().frame(egui::Frame::none().fill(egui::Color32::BLACK)).show(ctx, |ui| {
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

    #[cfg(feature = "screensaver")]
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
        dump();
        return Ok(());
    }
    let value = |names: &[&str]| args.iter().find_map(|a| names.iter().find_map(|n| a.strip_prefix(n)).map(|v| v.to_string()));
    let popout_only = args.iter().any(|a| a == "--popout");
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
    };
    let size = value(&["--window-size="]).and_then(|s| {
        let (w, h) = s.split_once(['x', 'X', ','])?;
        Some([w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?])
    });

    settings::migrate_legacy_dir();
    let (mut settings, status) = settings::load_or_import();
    if let Some(skin) = value(&["--popout-skin="]) {
        settings.popout.skin = skin;
    }
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(format!("{} \u{2013} {}", tmop::APP_NAME, tmop::APP_LONG_NAME))
        .with_inner_size(size.unwrap_or([1320.0, 860.0]))
        .with_min_inner_size([900.0, 600.0])
        .with_maximized(args.iter().any(|a| a == "--maximized"));
    if let Some(icon) = tmop::icon::window_icon() {
        viewport = viewport.with_icon(icon);
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
}
