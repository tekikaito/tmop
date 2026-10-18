//! Engine tab of the Docker page: engine facts, recent engine events and the TMOP engine card
//! (set-up, progress, start/stop, updates, removal and the engine's settings).

use super::{engine_wizard, wizards, Snap};
use crate::docker::backend::NoticeKind;
use crate::docker::model::{format_bytes, EngineStatus};
use crate::engine::config::Limits;
use crate::engine::manager::{Phase, Status, WslCheck};
use crate::engine::Overrides;
use crate::ui::kit::{self, GAP};
use crate::ui::theme::{self, Palette};
use crate::ui::widgets::{self, text_fit};
use crate::ui::{App, FrameData};
use eframe::egui::{self, pos2, Align, Align2, Layout, Rect, RichText};
use std::time::{Duration, Instant};

const EVENT_ROW_H: f32 = 18.0;
const MIB: u64 = 1024 * 1024;
/// Height of the recent events card; the TMOP engine card takes the rest of the right column.
const EVENTS_H: f32 = 200.0;
/// Indent of the help text under a switch, so it lines up after the toggle.
const HELP_INDENT: f32 = 44.0;

/// UI state of the TMOP engine card, kept between frames.
#[derive(Debug, Default)]
pub struct EngineTabState {
    /// The drafts below were filled from the engine status.
    pub initialised: bool,
    /// The inline remove confirmation is showing.
    pub confirm_remove: bool,
    /// Draft CPU limit (0 = unlimited).
    pub cpus: u32,
    /// Draft memory limit in GB (0 = unlimited).
    pub memory_gb: u32,
    /// `.wslconfig` memory in GB (0 = WSL default).
    pub wsl_memory_gb: u32,
    /// `.wslconfig` processors (0 = WSL default).
    pub wsl_processors: u32,
}

/// Draws the engine tab in `rect`.
pub fn show(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let bloom = fd.bloom;
    let snap = &app.docker.ui.snap;
    let p = ui.painter().clone();
    let cols = kit::hsplit(rect, &[1.0, 1.0], GAP);

    // Engine facts, with the engine's warnings underneath.
    let left = kit::titled_card(&p, cols[0], &pal, bloom, "Engine", "");
    match &snap.info {
        Some(info) => {
            let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
            let items = vec![
                ("Name".to_string(), or_dash(&info.name)),
                ("Server version".to_string(), or_dash(&info.server_version)),
                ("API version".to_string(), or_dash(&info.api_version)),
                ("OS".to_string(), or_dash(&info.os)),
                ("Architecture".to_string(), or_dash(&info.arch)),
                ("Kernel".to_string(), or_dash(&info.kernel)),
                ("CPUs".to_string(), info.cpus.to_string()),
                ("Memory".to_string(), format_bytes(info.mem_total)),
                ("Storage driver".to_string(), or_dash(&info.driver)),
                ("Docker root".to_string(), or_dash(&info.docker_root)),
                ("Cgroup version".to_string(), or_dash(&info.cgroup_version)),
                (
                    "Containers".to_string(),
                    format!("{} running, {} paused, {} stopped", info.containers_running, info.containers_paused, info.containers_stopped),
                ),
                ("Images".to_string(), info.images.to_string()),
                ("Swarm".to_string(), or_dash(&info.swarm)),
            ];
            let bottom = kit::kv_list(&p, left, &pal, &items, 130.0, 20.0);
            let mut y = bottom + 8.0;
            for w in &info.warnings {
                if y + 16.0 > left.bottom() {
                    break;
                }
                text_fit(&p, pos2(left.left(), y), Align2::LEFT_TOP, w, theme::ui(12.0), pal.warn, left.width());
                y += 16.0;
            }
        }
        None => kit::empty_state(&p, left, &pal, engine_status_text(snap)),
    }

    // Recent events on top, the TMOP engine card below.
    let right = kit::vsplit(cols[1], &[EVENTS_H, -1.0], GAP);
    let events = kit::titled_card(&p, right[0], &pal, bloom, "Recent events", "");
    if snap.events.is_empty() {
        kit::empty_state(&p, events, &pal, "No events yet");
    }
    let time_w = 90.0;
    let kind_w = 90.0;
    let action_w = 110.0;
    let mut y = events.top();
    for e in &snap.events {
        if y + EVENT_ROW_H > events.bottom() {
            break;
        }
        let mid = y + EVENT_ROW_H / 2.0;
        let mut x = events.left();
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &ago(e.at), theme::ui(12.0), pal.text_faint, time_w - 8.0);
        x += time_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.kind, theme::ui(12.0), pal.text_dim, kind_w - 8.0);
        x += kind_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.action, theme::ui(12.0), pal.text, action_w - 8.0);
        x += action_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.actor, theme::ui(12.0), pal.text_dim, events.right() - x);
        y += EVENT_ROW_H;
    }

    let tm = kit::titled_card(&p, right[1], &pal, bloom, "TMOP engine", "");
    engine_card(app, ui, tm, &pal);
}

/// Empty-state text for the engine card.
fn engine_status_text(snap: &Snap) -> &'static str {
    match snap.status {
        EngineStatus::Connected => "No engine information",
        EngineStatus::Unreachable => "Engine not reachable",
        EngineStatus::Connecting | EngineStatus::Unknown => "Connecting...",
    }
}

/// "12 s ago" style age of an event.
fn ago(at: Instant) -> String {
    let s = at.elapsed().as_secs();
    if s < 60 {
        format!("{s} s ago")
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}

/// The TMOP engine card: scrollable, drawn by the engine phase.
fn engine_card(app: &mut App, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let status = app.docker.engine.status();
    {
        // The drafts start from the engine's own limits the first time the tab is drawn.
        let tab = &mut app.docker.ui.engine_tab;
        if !tab.initialised {
            tab.initialised = true;
            tab.cpus = status.limits.cpus;
            tab.memory_gb = status.limits.memory_mb / 1024;
        }
    }
    let mut open_wizard = false;
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::LEFT)), |ui| {
        egui::ScrollArea::vertical().id_salt("engine-card").auto_shrink([false, false]).show(ui, |ui| {
            card_body(app, ui, pal, &status, &mut open_wizard);
        });
    });
    if open_wizard {
        engine_wizard::open(&mut app.docker);
    }
}

/// Card content by phase: unsupported, set-up, progress, or the installed engine's controls.
fn card_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette, st: &Status, open_wizard: &mut bool) {
    if st.phase == Phase::Unsupported {
        ui.label(RichText::new("TMOP's own engine is available on Windows only.").font(theme::ui(12.5)).color(pal.text_dim));
        return;
    }
    if let Some(e) = &st.error {
        ui.label(RichText::new(e).font(theme::ui(12.0)).color(pal.warn));
    }
    match st.phase {
        Phase::Installing | Phase::Updating | Phase::Removing | Phase::Starting | Phase::Stopping => progress_body(app, ui, pal, st),
        Phase::Stopped | Phase::Running => installed_body(app, ui, pal, st),
        _ if st.installed_version.is_some() => installed_body(app, ui, pal, st),
        _ => setup_body(app, ui, pal, st, open_wizard),
    }
}

/// Not installed (or a failed set-up): what it is, the WSL status and the set-up entry points.
fn setup_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette, st: &Status, open_wizard: &mut bool) {
    ui.label(
        RichText::new("Run Docker without Docker Desktop. TMOP downloads a small Linux system (about 140 MB) from GitHub and runs it in WSL 2.")
            .font(theme::ui(12.5))
            .color(pal.text_dim),
    );
    wsl_line(ui, pal, &st.wsl);
    ui.horizontal(|ui| {
        if wizards::enabled_button(ui, "Set up engine...", pal, !st.busy) {
            *open_wizard = true;
        }
        if st.wsl == WslCheck::Missing && widgets::button(ui, "Install WSL...", pal, None).clicked() {
            if let Err(e) = app.docker.engine.install_wsl() {
                app.docker.ui.toast(NoticeKind::Error, e);
            }
        }
    });
    if st.wsl == WslCheck::Missing {
        ui.label(RichText::new("Windows asks for permission; restart your PC when it finishes.").font(theme::ui(12.0)).color(pal.text_faint));
    }
}

/// Progress of a running operation, with Cancel while the set-up downloads.
fn progress_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette, st: &Status) {
    let step = st.progress.as_ref().map(|p| p.step.clone()).unwrap_or_else(|| phase_text(st.phase).to_string());
    ui.label(RichText::new(step).font(theme::ui(12.5)).color(pal.text));
    let bar = match st.progress.as_ref().filter(|p| p.total > 0) {
        Some(p) => {
            let fraction = (p.done as f32 / p.total as f32).clamp(0.0, 1.0);
            egui::ProgressBar::new(fraction).text(format!("{} / {} MB", p.done / MIB, p.total / MIB))
        }
        None => {
            // Unknown total: the bar keeps sweeping until the step reports a size.
            ui.ctx().request_repaint_after(Duration::from_millis(50));
            let t = ui.input(|i| i.time) as f32;
            egui::ProgressBar::new((t * 0.7).fract()).animate(true)
        }
    };
    ui.add(bar.desired_width(ui.available_width().min(420.0)));
    if st.phase == Phase::Installing && widgets::button(ui, "Cancel", pal, None).clicked() {
        app.docker.engine.cancel();
    }
}

/// Status line and the controls of an installed engine.
fn installed_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette, st: &Status) {
    let running = st.phase == Phase::Running;
    let relay = if st.relay_running { "relay running" } else { "relay not running" };
    // The stored version is the release tag, which already starts with `v`.
    let version = st.installed_version.clone().unwrap_or_default();
    let state = if running { "running" } else { "stopped" };
    let line = format!("Engine {version} - {state} - {relay} - context `{}`", st.context_name);
    ui.label(RichText::new(line).font(theme::ui(12.5)).color(pal.text));
    let busy = st.busy;
    ui.horizontal_wrapped(|ui| {
        if !running && wizards::enabled_button(ui, "Start", pal, !busy) {
            app.docker.engine.start();
        }
        if running {
            if wizards::enabled_button(ui, "Stop", pal, !busy) {
                app.docker.engine.stop();
            }
            if wizards::enabled_button(ui, "Restart", pal, !busy) {
                app.docker.engine.restart();
            }
        }
        if let Some(v) = &st.update_available {
            if wizards::enabled_button(ui, &format!("Update to {v}"), pal, !busy) {
                app.docker.engine.update();
            }
        }
        if wizards::enabled_button(ui, "Open data folder", pal, !busy) {
            if let Err(e) = kit::reveal(&st.data_dir) {
                app.docker.ui.toast(NoticeKind::Error, e);
            }
        }
        if wizards::enabled_button(ui, "Remove engine...", pal, !busy) {
            app.docker.ui.engine_tab.confirm_remove = true;
        }
    });

    if app.docker.ui.engine_tab.confirm_remove {
        ui.label(
            RichText::new(format!(
                "This removes the engine, its data folder and all containers, images and volumes stored in it. The `{}` Docker context, the PATH entry and the autostart entry are removed too.",
                st.context_name
            ))
            .font(theme::ui(12.0))
            .color(pal.warn),
        );
        ui.horizontal(|ui| {
            if wizards::enabled_button(ui, "Remove", pal, !busy) {
                app.docker.ui.engine_tab.confirm_remove = false;
                app.docker.engine.remove();
            }
            if widgets::button(ui, "Keep", pal, None).clicked() {
                app.docker.ui.engine_tab.confirm_remove = false;
            }
        });
    }

    ui.add_space(6.0);
    if let Some(on) = switch_row(ui, pal, !busy, "Start engine at login", st.autostart) {
        if let Err(e) = app.docker.engine.set_autostart(on) {
            app.docker.ui.toast(NoticeKind::Error, e);
        }
    }
    if let Some(on) = switch_row(ui, pal, !busy, "Publish ports on this PC only", st.ports_local_only) {
        app.docker.engine.set_ports_local_only(on);
    }
    if !st.ports_local_only {
        help_line(ui, pal, "Off: containers' published ports are reachable from your network. Windows asks once for permission to allow them through the firewall.");
        ui.horizontal(|ui| {
            ui.add_space(HELP_INDENT);
            if wizards::enabled_button(ui, "Allow in firewall...", pal, !busy) {
                if let Err(e) = app.docker.engine.add_firewall_rule() {
                    app.docker.ui.toast(NoticeKind::Error, e);
                }
            }
        });
    }
    if let Some(on) = switch_row(ui, pal, !busy, "Use TMOP as default Docker", st.default_docker) {
        if let Err(e) = app.docker.engine.set_default_docker(on) {
            app.docker.ui.toast(NoticeKind::Error, e);
        }
    }
    let bin_dir = Overrides::from_env().bin_dir();
    help_line(
        ui,
        pal,
        &format!("Installs the docker command into {} and makes `{}` the current context. Can be switched off again.", bin_dir.display(), st.context_name),
    );
    if let Some(on) = switch_row(ui, pal, true, "Check for engine updates on start", app.settings.docker.engine_update_check) {
        app.settings.docker.engine_update_check = on;
        app.settings_dirty = true;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("CPUs").font(theme::ui(13.0)).color(pal.text_dim));
        ui.add_enabled(!busy, egui::DragValue::new(&mut app.docker.ui.engine_tab.cpus).range(0..=256)).on_hover_text("0 = unlimited");
        ui.add_space(8.0);
        ui.label(RichText::new("Memory (GB)").font(theme::ui(13.0)).color(pal.text_dim));
        ui.add_enabled(!busy, egui::DragValue::new(&mut app.docker.ui.engine_tab.memory_gb).range(0..=1024)).on_hover_text("0 = unlimited");
        ui.add_space(8.0);
        if wizards::enabled_button(ui, "Apply", pal, !busy) {
            let tab = &app.docker.ui.engine_tab;
            let limits = Limits { cpus: tab.cpus, memory_mb: tab.memory_gb * 1024 };
            app.docker.engine.set_limits(limits);
        }
    });

    egui::CollapsingHeader::new(RichText::new("Advanced").font(theme::ui(13.0)).color(pal.text_dim))
        .id_salt("engine-advanced")
        .default_open(false)
        .show(ui, |ui| {
            help_line(
                ui,
                pal,
                "Applies to ALL WSL distributions on this PC, not only the TMOP engine. Your existing .wslconfig is backed up first. Takes effect after the WSL VM restarts.",
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new(".wslconfig memory (GB)").font(theme::ui(13.0)).color(pal.text_dim));
                ui.add_enabled(!busy, egui::DragValue::new(&mut app.docker.ui.engine_tab.wsl_memory_gb).range(0..=1024)).on_hover_text("0 = WSL default");
                ui.add_space(8.0);
                ui.label(RichText::new("Processors").font(theme::ui(13.0)).color(pal.text_dim));
                ui.add_enabled(!busy, egui::DragValue::new(&mut app.docker.ui.engine_tab.wsl_processors).range(0..=256)).on_hover_text("0 = WSL default");
            });
            if wizards::enabled_button(ui, "Apply to .wslconfig", pal, !busy) {
                let tab = &app.docker.ui.engine_tab;
                let memory = (tab.wsl_memory_gb > 0).then_some(tab.wsl_memory_gb);
                let processors = (tab.wsl_processors > 0).then_some(tab.wsl_processors);
                match app.docker.engine.apply_wslconfig(memory, processors) {
                    Ok(Some(backup)) => app.docker.ui.toast(NoticeKind::Success, format!("Saved .wslconfig (backup: {})", backup.display())),
                    Ok(None) => app.docker.ui.toast(NoticeKind::Success, "Saved .wslconfig"),
                    Err(e) => app.docker.ui.toast(NoticeKind::Error, e),
                }
            }
        });
}

/// A switch with its label on the right. Returns the new value when the user flipped it.
fn switch_row(ui: &mut egui::Ui, pal: &Palette, enabled: bool, label: &str, on: bool) -> Option<bool> {
    let mut value = on;
    let changed = ui
        .horizontal(|ui| {
            let resp = ui.add_enabled_ui(enabled, |ui| widgets::toggle(ui, &mut value, pal)).inner;
            ui.label(RichText::new(label).font(theme::ui(13.0)).color(pal.text));
            resp.changed()
        })
        .inner;
    (changed && enabled).then_some(value)
}

/// Small dim help text under a switch, indented to line up after the toggle.
fn help_line(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add_space(HELP_INDENT);
        ui.label(RichText::new(text).font(theme::ui(12.0)).color(pal.text_faint));
    });
}

/// WSL status line; a missing or outdated WSL is shown in the warning colour.
pub(super) fn wsl_line(ui: &mut egui::Ui, pal: &Palette, wsl: &WslCheck) {
    let (text, color) = match wsl {
        WslCheck::Unknown => ("Checking WSL...".to_string(), pal.text_faint),
        WslCheck::Ready { version } => (format!("WSL {version} found"), pal.text_dim),
        WslCheck::Missing => ("WSL is not installed".to_string(), pal.warn),
        WslCheck::Outdated(note) => (note.clone(), pal.warn),
    };
    ui.label(RichText::new(text).font(theme::ui(12.5)).color(color));
}

fn phase_text(phase: Phase) -> &'static str {
    match phase {
        Phase::Installing => "Installing...",
        Phase::Updating => "Updating...",
        Phase::Removing => "Removing...",
        Phase::Starting => "Starting...",
        Phase::Stopping => "Stopping...",
        _ => "Working...",
    }
}
