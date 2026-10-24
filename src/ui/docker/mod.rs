//! Docker page: host picker, engine tabs, the detail area and in-app toasts for engine
//! notices.
//!
//! [`DockerState`] owns the background [`Backend`]. The page draws from [`Snap`], a copy
//! of the engine lists refreshed only when the backend reports a change, so the backend
//! lock is held for copying and never while drawing.
//!
//! Extension points for later steps:
//! * [`DockerUi::toast`] shows a message; [`DockerState::backend`] gives the worker handle
//!   (`engine()` for a live connection, `send` for commands);
//! * `detail::detail_area` and `dialogs::show` are the entry points of the detail area
//!   and the dialogs.

pub mod detail;
pub mod dialogs;
pub mod engine_tab;
pub mod engine_wizard;
pub mod tables;
pub mod terminal_tab;
pub mod wizards;

use super::kit;
use super::table::TableState;
use super::theme::{self, Palette};
use super::widgets::{self, Button, Icon};
use super::{App, FrameData, STATUS_H};
use crate::docker::backend::{Backend, Command, EngineEvent, NoticeKind, Shared};
use crate::docker::context::{docker_dir, Context};
use crate::docker::model::{Container, EngineInfo, EngineStatus, Image, Network, Stack, StatsSample, Totals, Volume};
use crate::docker::notify;
use crate::engine::manager::{self, EngineManager, Phase};
use crate::engine::Overrides;
use crate::settings::Settings;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Id, Layout, Order, Rect, RichText, Sense};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Toasts shown at once; older ones are dropped first.
const MAX_TOASTS: usize = 5;
/// Engine events kept in the snapshot for the engine tab.
const EVENTS_SHOWN: usize = 30;

/// Which table the page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Containers,
    Stacks,
    Images,
    Volumes,
    Networks,
    Engine,
}

/// A short message shown in the bottom-right corner.
#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: NoticeKind,
    pub text: String,
    pub at: Instant,
}

/// Copy of the engine state used by the page. Refreshed by [`DockerState::refresh_snap`].
#[derive(Clone, Debug, Default)]
pub struct Snap {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
    pub stacks: Vec<Stack>,
    /// Latest sample per tracked (running) container, keyed by full id.
    pub stats_last: BTreeMap<String, StatsSample>,
    pub totals: Totals,
    pub status: EngineStatus,
    pub error: Option<String>,
    pub info: Option<EngineInfo>,
    /// Context in use; empty until the worker has picked one.
    pub active: String,
    /// Object id or name to the label of the action in progress.
    pub busy: BTreeMap<String, String>,
    pub read_only: bool,
    /// The newest [`EVENTS_SHOWN`] events, newest first.
    pub events: Vec<EngineEvent>,
    /// Time of the newest engine event, used to detect new events.
    pub last_event: Option<Instant>,
    /// When the container list was last read since the engine connected; `None` until then.
    pub last_list_at: Option<Instant>,
}

/// UI state of the Docker page.
#[derive(Default)]
pub struct DockerUi {
    pub tab: Tab,
    pub filter: String,
    pub containers: TableState,
    pub stacks: TableState,
    pub images: TableState,
    pub volumes: TableState,
    pub networks: TableState,
    /// Container id of the selected row.
    pub selected_container: Option<String>,
    /// Stack name of the selected row.
    pub selected_stack: Option<String>,
    /// Image id of the selected row.
    pub selected_image: Option<String>,
    /// Volume name of the selected row.
    pub selected_volume: Option<String>,
    /// Network id of the selected row.
    pub selected_network: Option<String>,
    pub toasts: Vec<Toast>,
    pub detail: detail::DetailState,
    pub dialogs: dialogs::DialogState,
    pub engine_tab: engine_tab::EngineTabState,
    pub engine_wizard: engine_wizard::EngineWizard,
    pub last_list_seq: u64,
    pub last_stats_seq: u64,
    pub snap: Snap,
}

impl DockerUi {
    /// Queues a toast for the bottom-right corner.
    pub fn toast(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.toasts.push(Toast { kind, text: text.into(), at: Instant::now() });
        let excess = self.toasts.len().saturating_sub(MAX_TOASTS);
        self.toasts.drain(..excess);
    }
}

/// Docker state owned by the app.
pub struct DockerState {
    pub backend: Option<Backend>,
    pub contexts: Vec<Context>,
    pub ui: DockerUi,
    /// The Docker CLI configuration folder exists.
    pub installed: bool,
    /// Engine status as of the last [`DockerState::poll`].
    pub last_status: EngineStatus,
    /// Desktop notifications for alerts, from the active context's setting.
    pub notify_enabled: bool,
    /// TMOP's own Docker engine: state machine and worker.
    pub engine: EngineManager,
    /// Set by the setup wizard; the engine's context becomes the active one once it runs.
    pub switch_to_engine_when_ready: bool,
    page_open: Option<bool>,
}

impl DockerState {
    /// Reads the Docker contexts and starts the background worker on the active one.
    pub fn new(ctx: &egui::Context, settings: &Settings) -> Self {
        let contexts = wizards::merged_contexts(settings);
        let installed = docker_dir().map(|d| d.exists()).unwrap_or(false);
        let active = settings.docker.active_context.clone();
        let resolved = resolve_name(&contexts, &active);
        let read_only = settings.docker.contexts.get(&resolved).map(|c| c.read_only).unwrap_or(false);
        let notify_enabled = settings.docker.contexts.get(&resolved).map(|c| c.notifications).unwrap_or(true);
        let repaint = ctx.clone();
        let backend = Backend::start(contexts.clone(), active, read_only, move || repaint.request_repaint());
        let engine_repaint = ctx.clone();
        let engine = EngineManager::new(
            Overrides::from_env().data_dir(&settings.docker.engine_data_dir),
            settings.docker.engine_update_check,
            Arc::new(move || engine_repaint.request_repaint()),
        );
        Self {
            backend: Some(backend),
            contexts,
            ui: DockerUi::default(),
            installed,
            last_status: EngineStatus::Unknown,
            notify_enabled,
            engine,
            switch_to_engine_when_ready: false,
            page_open: None,
        }
    }

    /// The worker handle, when one is running.
    pub fn backend(&self) -> Option<&Backend> {
        self.backend.as_ref()
    }

    /// Takes the notification setting of the context now in `settings.docker.active_context`.
    pub fn note_context_switch(&mut self, settings: &Settings) {
        let resolved = resolve_name(&self.contexts, &settings.docker.active_context);
        self.notify_enabled = settings.docker.contexts.get(&resolved).map(|c| c.notifications).unwrap_or(true);
    }

    /// Whether the Docker page is listed in the navigation.
    pub fn visible(&self, settings: &Settings) -> bool {
        // The engine is offered wherever it can run, so it can be set up from here.
        let engine_supported = cfg!(windows);
        !settings.docker.hide_page && (self.installed || self.last_status == EngineStatus::Connected || engine_supported)
    }

    /// Once per frame: caches the engine status, moves notices into toasts, shows alerts as
    /// toasts and desktop notifications, drops expired toasts, and tells the worker when the
    /// page is open.
    pub fn poll(&mut self, page_open: bool) {
        let Some(b) = &self.backend else { return };
        let alerts: Vec<_> = {
            let mut s = lock(&b.shared);
            self.last_status = s.status;
            while let Some(n) = s.notices.pop_front() {
                self.ui.toasts.push(Toast { kind: n.kind, text: n.text, at: Instant::now() });
            }
            s.alerts.drain(..).collect()
        };
        for alert in alerts {
            let (title, body) = notify::alert_text(&alert);
            self.ui.toast(NoticeKind::Info, format!("{title}: {body}"));
            if self.notify_enabled {
                // The toast call blocks for a moment, so it runs off the UI thread.
                std::thread::spawn(move || {
                    let _ = notify::send(&title, &body);
                });
            }
        }
        let excess = self.ui.toasts.len().saturating_sub(MAX_TOASTS);
        self.ui.toasts.drain(..excess);
        let now = Instant::now();
        self.ui.toasts.retain(|t| now.duration_since(t.at) < toast_ttl(t.kind));
        if !page_open {
            // No terminal session is shown off the page.
            self.ui.detail.terminals.set_visible(None);
        }
        if self.page_open != Some(page_open) {
            if self.page_open == Some(true) && !page_open {
                // Leaving the page stops the log follow and releases any volume helper.
                let engine = b.engine();
                self.ui.detail.close(engine.as_ref());
            }
            self.page_open = Some(page_open);
            b.send(Command::SetPageOpen(page_open));
        }
    }

    /// Copies the engine lists into [`Snap`] when the worker published a change.
    pub fn refresh_snap(&mut self) {
        let Some(b) = &self.backend else { return };
        let s = lock(&b.shared);
        let ui = &mut self.ui;
        let stale = s.list_seq != ui.last_list_seq
            || s.stats_seq != ui.last_stats_seq
            || s.status != ui.snap.status
            || s.error != ui.snap.error
            || s.active != ui.snap.active
            || s.busy != ui.snap.busy
            || s.events.back().map(|e| e.at) != ui.snap.last_event;
        if !stale {
            return;
        }
        ui.last_list_seq = s.list_seq;
        ui.last_stats_seq = s.stats_seq;
        ui.snap = snap_of(&s);
    }
}

fn snap_of(s: &Shared) -> Snap {
    Snap {
        containers: s.containers.clone(),
        images: s.images.clone(),
        volumes: s.volumes.clone(),
        networks: s.networks.clone(),
        stacks: s.stacks.clone(),
        stats_last: s.stats.iter().map(|(k, v)| (k.clone(), v.last)).collect(),
        totals: s.totals,
        status: s.status,
        error: s.error.clone(),
        info: s.info.clone(),
        active: s.active.clone(),
        busy: s.busy.clone(),
        read_only: s.read_only,
        events: s.events.iter().rev().take(EVENTS_SHOWN).cloned().collect(),
        last_event: s.events.back().map(|e| e.at),
        last_list_at: s.last_list_at,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn toast_ttl(kind: NoticeKind) -> Duration {
    match kind {
        NoticeKind::Error => Duration::from_secs(10),
        _ => Duration::from_secs(6),
    }
}

/// The context name to use: `name` when set, else the one the Docker CLI marks as current.
pub fn resolve_name(contexts: &[Context], name: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    contexts.iter().find(|c| c.current).map(|c| c.name.clone()).unwrap_or_else(|| "default".into())
}

/// Tab named by `--docker-tab=<id>`: `engine`, `containers`, `stacks`, `images`, `volumes` or `networks`.
pub fn tab_from_id(id: &str) -> Option<Tab> {
    match id {
        "containers" => Some(Tab::Containers),
        "stacks" => Some(Tab::Stacks),
        "images" => Some(Tab::Images),
        "volumes" => Some(Tab::Volumes),
        "networks" => Some(Tab::Networks),
        "engine" => Some(Tab::Engine),
        _ => None,
    }
}

/// Unix time in seconds, for age labels.
pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn context_label(c: &Context) -> String {
    format!("{} - {}", c.name, c.host_label())
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}...", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

impl App {
    /// Once per frame, before [`DockerState::poll`]: engine toasts, context list refresh, and the
    /// switch to the engine's context once a setup started from the wizard is running.
    pub fn engine_poll(&mut self) {
        for event in self.docker.engine.poll() {
            match event {
                manager::Event::Toast { kind, text } => self.docker.ui.toast(kind, text),
                manager::Event::ContextsChanged => {
                    let contexts = wizards::merged_contexts(&self.settings);
                    if let Some(b) = &self.docker.backend {
                        b.send(Command::SetContexts(contexts.clone()));
                    }
                    // The engine was removed while its context was in use: fall back like an unset choice.
                    let engine_ctx = self.docker.engine.status().context_name;
                    if self.settings.docker.active_context == engine_ctx && !contexts.iter().any(|c| c.name == engine_ctx) {
                        let fallback = resolve_name(&contexts, "");
                        let ro = self.settings.docker.contexts.get(&fallback).map(|c| c.read_only).unwrap_or(false);
                        if let Some(b) = &self.docker.backend {
                            let engine = b.engine();
                            self.docker.ui.detail.close(engine.as_ref());
                            self.docker.ui.detail.close_terminals();
                            b.send(Command::SetReadOnly(ro));
                            b.send(Command::SwitchContext(fallback.clone()));
                        }
                        self.settings.docker.active_context = fallback;
                        self.settings_dirty = true;
                        self.docker.note_context_switch(&self.settings);
                    }
                    self.docker.contexts = contexts;
                }
            }
        }
        let status = self.docker.engine.status();
        match status.phase {
            // `busy` is set as soon as the setup is queued; a stale phase before that must not clear the flag.
            Phase::Failed | Phase::NotInstalled if !status.busy => self.docker.switch_to_engine_when_ready = false,
            // The context is listed only after the engine has been started, so wait for it.
            Phase::Running if self.docker.switch_to_engine_when_ready && self.docker.contexts.iter().any(|c| c.name == status.context_name) => {
                self.docker.switch_to_engine_when_ready = false;
                let name = status.context_name;
                let ro = self.settings.docker.contexts.get(&name).map(|c| c.read_only).unwrap_or(false);
                if let Some(b) = &self.docker.backend {
                    // Release the helper and log session on the old host before the switch.
                    let engine = b.engine();
                    self.docker.ui.detail.close(engine.as_ref());
                    self.docker.ui.detail.close_terminals();
                    b.send(Command::SetReadOnly(ro));
                    b.send(Command::SwitchContext(name.clone()));
                }
                self.settings.docker.active_context = name;
                self.settings_dirty = true;
                self.docker.note_context_switch(&self.settings);
            }
            _ => {}
        }
    }

    /// The Docker page: title block and toolbar, the tabs, and the selected tab's table with the
    /// detail aside beside it (below it on narrow windows).
    pub fn docker_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        self.docker.refresh_snap();
        let active = self.active_name();
        let full = ui.max_rect();
        let (subtitle, dot) = self.engine_line(&active, &pal);

        // Title block: the cube, the title and under it the state of the engine with a status dot.
        let p = ui.painter().clone();
        widgets::draw_icon(&p, Rect::from_min_size(pos2(full.left() + 12.0, full.top() + 4.0), vec2(56.0, 56.0)), Icon::Container, pal.text_code());
        let title_area = Rect::from_min_max(pos2(full.left() + 72.0, full.top()), full.max);
        // The two spaces leave room for the dot.
        let body = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(title_area), |ui| self.page_title(ui, &pal, "Docker", &format!("  {subtitle}"))).inner;
        let dot_at = pos2(title_area.left() + 17.0, full.top() + 52.0);
        p.circle_filled(dot_at, 9.0, theme::with_alpha(dot, 0.10));
        p.circle_filled(dot_at, 6.5, theme::with_alpha(dot, 0.20));
        p.circle_filled(dot_at, 4.0, dot);

        let area = Rect::from_min_max(pos2(full.left() + 12.0, body.top()), pos2(full.right() - 16.0, body.bottom()));
        // The toolbar shares the title row when there is room, else it gets a row of its own.
        let beside = area.width() >= 1560.0;
        let (bar, tabs_top) = if beside {
            (Rect::from_min_max(pos2(area.left() + 700.0, full.top() + 14.0), pos2(area.right(), full.top() + 50.0)), body.top())
        } else {
            (Rect::from_min_size(pos2(area.left(), body.top() - 4.0), vec2(area.width(), 36.0)), body.top() + 40.0)
        };
        self.docker_toolbar(ui, bar, &pal, &active);

        let tabs = Rect::from_min_size(pos2(area.left(), tabs_top), vec2(area.width(), 36.0));
        self.tab_strip(ui, tabs, &pal, &active);
        let content = Rect::from_min_max(pos2(area.left(), tabs.bottom() + 14.0), area.max);
        if self.docker.ui.tab == Tab::Engine {
            // The engine tab has no detail aside, so its content takes the whole width.
            self.docker.ui.detail.terminals.set_visible(None);
            engine_tab::show(self, ui, content, fd);
        } else if area.width() >= 1100.0 {
            let cols = kit::hsplit(content, &[3.0, 1.4], 14.0);
            tables::show(self, ui, cols[0], fd);
            detail::detail_area(self, ui, cols[1], fd);
        } else {
            let detail_h = (content.height() * 0.5).clamp(240.0, 340.0);
            let rows = kit::vsplit(content, &[-1.0, detail_h], 14.0);
            tables::show(self, ui, rows[0], fd);
            detail::detail_area(self, ui, rows[1], fd);
        }
        dialogs::show(self, ui.ctx(), fd);
    }

    /// Context in use: the worker's choice, else the configured one, else the Docker CLI's current context.
    fn active_name(&self) -> String {
        let active = &self.docker.ui.snap.active;
        if active.is_empty() {
            resolve_name(&self.docker.contexts, &self.settings.docker.active_context)
        } else {
            active.clone()
        }
    }

    /// The state of the engine for the title block: the line under the title and the colour of its dot.
    fn engine_line(&self, active: &str, pal: &Palette) -> (String, Color32) {
        let snap = &self.docker.ui.snap;
        let st = theme::status(pal);
        match snap.status {
            EngineStatus::Connected => {
                let host = self.docker.contexts.iter().find(|c| c.name == active).map(|c| c.host_label()).unwrap_or_else(|| active.to_string());
                let text = match snap.info.as_ref() {
                    Some(i) => format!("Connected \u{b7} Docker {} ({}/{}) \u{b7} host {host}", i.server_version, i.os_type, i.arch),
                    None => format!("Connected \u{b7} host {host}"),
                };
                (text, st.ok)
            }
            EngineStatus::Unreachable => match &snap.error {
                Some(e) => (format!("Engine not reachable \u{b7} {}", short(e, 80)), st.warn),
                None => ("Engine not reachable".to_string(), st.warn),
            },
            EngineStatus::Connecting | EngineStatus::Unknown => ("Connecting\u{2026}".to_string(), pal.text_faint),
        }
    }

    /// Host picker, filter, Refresh, Run container and Add host.
    fn docker_toolbar(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Palette, active: &str) {
        let mut switch_to: Option<String> = None;
        let mut refresh = false;
        let mut add_host = false;
        let mut run = false;
        let contexts = &self.docker.contexts;
        let filter = &mut self.docker.ui.filter;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // Right to left: the first widget added sits at the right edge.
            if Button::ghost("Add host\u{2026}").show(ui, pal).clicked() {
                add_host = true;
            }
            if Button::primary("Run container\u{2026}").show(ui, pal).clicked() {
                run = true;
            }
            if widgets::button(ui, "Refresh", pal, Some(Icon::Refresh)).clicked() {
                refresh = true;
            }
            kit::filter_box(ui, filter, "Filter", 150.0, pal);
            let shown = contexts.iter().find(|c| c.name == active).map(context_label).unwrap_or_else(|| active.to_string());
            egui::ComboBox::from_id_salt("docker-host").width(220.0).selected_text(shown).show_ui(ui, |ui| {
                for c in contexts {
                    let is_active = c.name == active;
                    if ui.selectable_label(is_active, context_label(c)).clicked() && !is_active {
                        switch_to = Some(c.name.clone());
                    }
                }
            });
            ui.label(RichText::new("Host").font(theme::ui(12.0)).color(pal.text_faint));
        });

        if let Some(name) = switch_to {
            // The worker reads the read-only flag when it connects, so it is sent first.
            let ro = self.settings.docker.contexts.get(&name).map(|c| c.read_only).unwrap_or(false);
            if let Some(b) = &self.docker.backend {
                // Release the helper and log session on the old host before the switch.
                let engine = b.engine();
                self.docker.ui.detail.close(engine.as_ref());
                self.docker.ui.detail.close_terminals();
                b.send(Command::SetReadOnly(ro));
                b.send(Command::SwitchContext(name.clone()));
            }
            self.settings.docker.active_context = name;
            self.settings_dirty = true;
            self.docker.note_context_switch(&self.settings);
        }
        if refresh {
            if let Some(b) = &self.docker.backend {
                b.send(Command::Refresh);
            }
        }
        if add_host {
            self.docker.ui.dialogs.wizards.open = Some(wizards::Wizard::AddHost);
        }
        if run {
            self.docker.ui.dialogs.wizards.open = Some(wizards::Wizard::RunImage { image: String::new() });
        }
    }

    /// Read-only and Notifications switches of the active context, and the count of notifications
    /// on screen.
    fn switches(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Palette, active: &str) {
        let entry = self.settings.docker.contexts.get(active).cloned().unwrap_or_default();
        let (mut ro, mut notif) = (entry.read_only, entry.notifications);
        let mut ro_changed = false;
        let mut notif_changed = false;
        let shown = self.docker.ui.toasts.len();
        kit::row_ui(ui, rect, |ui| {
            // Right to left: the first widget added sits at the right edge.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (chip, _) = ui.allocate_exact_size(vec2(widgets::text_width(ui.painter(), &shown.to_string(), theme::mono(12.0)) + 14.0, 20.0), Sense::hover());
                ui.painter().rect_filled(chip, 3.0, pal.raise(0.08));
                ui.painter().text(chip.center(), Align2::CENTER_CENTER, shown, theme::mono(12.0), pal.text_dim);
                ui.add_space(4.0);
                let mut n = notif;
                if widgets::toggle(ui, &mut n, pal).changed() {
                    notif = n;
                    notif_changed = true;
                }
                ui.label(RichText::new("Notifications").font(theme::ui(12.0)).color(pal.text_faint));
                ui.add_space(10.0);
                let mut r = ro;
                if widgets::toggle(ui, &mut r, pal).changed() {
                    ro = r;
                    ro_changed = true;
                }
                ui.label(RichText::new("Read-only").font(theme::ui(12.0)).color(pal.text_faint));
            });
        });
        if ro_changed || notif_changed {
            let e = self.settings.docker.contexts.entry(active.to_string()).or_default();
            if ro_changed {
                e.read_only = ro;
            }
            if notif_changed {
                e.notifications = notif;
            }
            self.settings_dirty = true;
            if notif_changed {
                self.docker.notify_enabled = notif;
            }
            if ro_changed {
                if ro {
                    // Shells cannot take input on a read-only host, so their sessions end.
                    self.docker.ui.detail.close_terminals();
                }
                if let Some(b) = &self.docker.backend {
                    b.send(Command::SetReadOnly(ro));
                }
            }
        }
    }

    /// Tabs with counts, and the switches on the right.
    fn tab_strip(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Palette, active: &str) {
        let snap = &self.docker.ui.snap;
        let tabs: [(&str, Option<usize>); 6] = [
            ("Containers", Some(snap.containers.len())),
            ("Stacks", Some(snap.stacks.len())),
            ("Images", Some(snap.images.len())),
            ("Volumes", Some(snap.volumes.len())),
            ("Networks", Some(snap.networks.len())),
            ("Engine", None),
        ];
        const ORDER: [Tab; 6] = [Tab::Containers, Tab::Stacks, Tab::Images, Tab::Volumes, Tab::Networks, Tab::Engine];
        let current = self.docker.ui.tab;
        let selected = ORDER.iter().position(|t| *t == current).unwrap_or(0);
        if let Some(i) = widgets::tab_bar(ui, rect, pal, &tabs, selected, None) {
            self.docker.ui.tab = ORDER[i];
        }
        let switches_rect = Rect::from_min_max(pos2(rect.right() - 320.0, rect.top()), pos2(rect.right(), rect.top() + 34.0));
        self.switches(ui, switches_rect, pal, active);
    }

    /// Bottom-right stack of toasts. Click a toast to dismiss it.
    pub fn draw_toasts(&mut self, ctx: &egui::Context) {
        if self.docker.ui.toasts.is_empty() {
            return;
        }
        let pal = self.palette();
        let toasts = self.docker.ui.toasts.clone();
        let mut dismiss: Option<usize> = None;
        egui::Area::new(Id::new("docker-toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -(STATUS_H + 12.0)))
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for (i, t) in toasts.iter().enumerate() {
                        let st = theme::status(&pal);
                        let bar = match t.kind {
                            NoticeKind::Error => st.danger,
                            NoticeKind::Success => st.ok,
                            NoticeKind::Info => pal.accent,
                        };
                        // The glass of the dialogs, with a bar in the colour of the kind at the left.
                        let frame = egui::Frame::window(&ctx.style()).rounding(8.0).inner_margin(egui::Margin { left: 18.0, right: 14.0, top: 10.0, bottom: 10.0 });
                        let r = frame
                            .show(ui, |ui| {
                                ui.set_width(296.0);
                                ui.label(RichText::new(&t.text).font(theme::ui(13.0)).color(pal.text));
                            })
                            .response
                            .rect;
                        ui.painter().rect_filled(Rect::from_min_max(r.left_top() + vec2(0.0, 6.0), pos2(r.left() + 4.0, r.bottom() - 6.0)), 2.0, bar);
                        let click = ui.interact(r, ui.id().with(("docker-toast", i)), Sense::click());
                        if click.clicked() {
                            dismiss = Some(i);
                        }
                        click.on_hover_cursor(egui::CursorIcon::PointingHand);
                        ui.add_space(6.0);
                    }
                });
            });
        if let Some(i) = dismiss {
            if i < self.docker.ui.toasts.len() {
                self.docker.ui.toasts.remove(i);
            }
        }
    }
}
