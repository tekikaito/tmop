//! Multi-step dialogs of the Docker page: pull/search/run an image, add a host,
//! compose up from a file, export and import volumes.
//!
//! The table toolbars and context menus open a wizard by setting
//! [`WizardState::open`]; [`show`] draws the open wizard each frame. Engine work runs as
//! [`Task`]s on the worker runtime and is collected by [`step`] once per frame.

use super::{DockerState, DockerUi};
use crate::docker::backend::{Command, Engine, NoticeKind, Task};
use crate::docker::context::{default_contexts, Context, Endpoint};
use crate::docker::files;
use crate::docker::model::{format_bytes, Stack};
use crate::docker::run::{self, PullProgress, Restart, RunSpec, SearchHit};
use crate::docker::stack::{self, ComposeRun};
use crate::settings::{DockerHost, Settings};
use crate::ui::theme::{self, Palette};
use crate::ui::widgets;
use crate::ui::{App, FrameData};
use eframe::egui::{self, Align2, RichText};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Hits requested per search.
const SEARCH_LIMIT: u32 = 25;
const NOT_CONNECTED: &str = "Not connected to a Docker engine";
const READ_ONLY: &str = "This host is read-only";

/// Which wizard is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wizard {
    /// Pull an image by name or search the registry through the engine.
    PullImage,
    /// Run a container from the given image (empty = ask for the image).
    RunImage { image: String },
    /// Add a docker context (SSH or TCP host).
    AddHost,
    /// `docker compose up` from a compose file; `stack` preselects a known stack.
    ComposeUp { stack: Option<String> },
    /// Export the named volume to a .tar file.
    ExportVolume { volume: String },
    /// Import a .tar file into the named volume (None = ask for a name).
    ImportVolume { volume: Option<String> },
}

/// State of the wizards: the open wizard and the inputs and running tasks of each.
#[derive(Debug, Default)]
pub struct WizardState {
    pub open: Option<Wizard>,
    /// The wizard drawn in the previous frame; a change resets the form of the new one.
    shown: Option<Wizard>,
    pub pull: PullForm,
    pub run: RunForm,
    pub host: HostForm,
    pub compose: ComposeForm,
    pub volume: VolumeForm,
}

/// A task or process slot. Holds the running value until it is collected.
pub struct Slot<T>(Option<T>);

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Slot({})", if self.0.is_some() { "busy" } else { "idle" })
    }
}

#[derive(Debug, Default)]
pub struct PullForm {
    pub image: String,
    pub term: String,
    /// Image of the running pull, for the toast.
    pub pulling: String,
    pub progress: Arc<Mutex<PullProgress>>,
    pub pull: Slot<Task<Result<(), String>>>,
    pub search: Slot<Task<Result<Vec<SearchHit>, String>>>,
    pub hits: Vec<SearchHit>,
    pub searched: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct RunForm {
    pub spec: RunSpec,
    pub progress: Arc<Mutex<PullProgress>>,
    pub task: Slot<Task<Result<String, String>>>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct HostForm {
    pub name: String,
    /// TCP instead of SSH.
    pub tcp: bool,
    pub host: String,
    pub port: String,
    pub description: String,
}

#[derive(Debug, Default)]
pub struct ComposeForm {
    pub path: String,
    pub project: String,
    pub run: Slot<ComposeRun>,
    /// Output of the last or running `compose up`.
    pub output: String,
    pub result: Option<Result<(), String>>,
}

#[derive(Debug, Default)]
pub struct VolumeForm {
    /// Volume to import into (editable), or the volume being exported.
    pub volume: String,
    pub file: Option<PathBuf>,
    pub export: Slot<Task<Result<u64, String>>>,
    pub export_name: String,
    pub import: Slot<Task<Result<(), String>>>,
    pub import_name: String,
    pub status: Option<String>,
}

/// Toasts and page changes produced while collecting tasks, applied after the state is updated.
#[derive(Default)]
struct Out {
    toasts: Vec<(NoticeKind, String)>,
    refresh: bool,
    select: Option<String>,
}

/// Draws the open wizard, if any. Called from `dialogs::show` each frame.
pub fn show(app: &mut App, ctx: &egui::Context, fd: &FrameData<'_>) {
    step(&mut app.docker);
    let Some(open) = app.docker.ui.dialogs.wizards.open.clone() else { return };
    let pal = fd.pal;
    let busy = busy(&app.docker.ui.dialogs.wizards, &open);
    let (title, width) = match &open {
        Wizard::PullImage => ("Pull image", 640.0),
        Wizard::RunImage { .. } => ("Run image", 620.0),
        Wizard::AddHost => ("Add Docker host", 520.0),
        Wizard::ComposeUp { .. } => ("Compose up", 620.0),
        Wizard::ExportVolume { .. } => ("Export volume", 480.0),
        Wizard::ImportVolume { .. } => ("Import volume", 480.0),
    };
    let mut is_open = true;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut is_open)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(egui::Frame::window(&ctx.style()).fill(pal.panel))
        .show(ctx, |ui| {
            ui.set_width(width);
            match &open {
                Wizard::PullImage => pull_body(app, ui, &pal),
                Wizard::RunImage { .. } => run_body(app, ui, &pal),
                Wizard::AddHost => host_body(app, ui, &pal),
                Wizard::ComposeUp { .. } => compose_body(app, ui, &pal),
                Wizard::ExportVolume { .. } => export_body(app, ui, &pal),
                Wizard::ImportVolume { .. } => import_body(app, ui, &pal),
            }
        });
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if (!is_open || escape) && !busy {
        app.docker.ui.dialogs.wizards.open = None;
    }
}

/// Context names from the Docker CLI and TMOP hosts, with the CLI ones first. TMOP hosts
/// whose name clashes with a CLI context, or whose host string does not parse, are skipped.
pub fn merged_contexts(settings: &Settings) -> Vec<Context> {
    let mut contexts = default_contexts();
    for h in &settings.docker_hosts {
        if contexts.iter().any(|c| c.name == h.name) {
            continue;
        }
        let Ok(endpoint) = Endpoint::parse(&h.host) else { continue };
        let description = if h.description.trim().is_empty() { h.host.clone() } else { h.description.clone() };
        contexts.push(Context { name: h.name.clone(), description, endpoint, current: false });
    }
    contexts
}

/// Whether a wizard has a task or process running; such a wizard cannot be closed.
fn busy(w: &WizardState, open: &Wizard) -> bool {
    match open {
        Wizard::PullImage => w.pull.pull.0.is_some() || w.pull.search.0.is_some(),
        Wizard::RunImage { .. } => w.run.task.0.is_some(),
        Wizard::AddHost => false,
        Wizard::ComposeUp { .. } => w.compose.run.0.is_some(),
        Wizard::ExportVolume { .. } => w.volume.export.0.is_some(),
        Wizard::ImportVolume { .. } => w.volume.import.0.is_some(),
    }
}

/// Once per frame, before drawing: resets the form of a newly opened wizard, then collects
/// finished tasks and the output of a running compose.
fn step(docker: &mut DockerState) {
    let backend = docker.backend.as_ref();
    let ui: &mut DockerUi = &mut docker.ui;
    let mut out = Out::default();
    let w = &mut ui.dialogs.wizards;
    if w.open != w.shown {
        w.shown = w.open.clone();
        open_form(w, &ui.snap.stacks);
    }
    poll_pull(w, &mut out);
    poll_run(w, &mut out);
    poll_compose(w, &mut out);
    poll_volume(w, &mut out);
    if out.refresh {
        if let Some(b) = backend {
            b.send(Command::Refresh);
        }
    }
    if let Some(id) = out.select {
        // The containers table keeps its own selection and copies it back each frame.
        ui.containers.selected = Some(id.clone());
        ui.selected_container = Some(id);
    }
    for (kind, text) in out.toasts {
        ui.toast(kind, text);
    }
}

/// Fills a form from the wizard that was just opened. Forms with a running task keep their state.
fn open_form(w: &mut WizardState, stacks: &[Stack]) {
    match w.open.clone() {
        Some(Wizard::RunImage { image }) => {
            if w.run.task.0.is_none() {
                w.run.spec = RunSpec { image, pull: true, ..Default::default() };
                w.run.error = None;
            }
        }
        Some(Wizard::AddHost) => w.host = HostForm::default(),
        Some(Wizard::ComposeUp { stack }) => {
            if w.compose.run.0.is_none() {
                w.compose.output.clear();
                w.compose.result = None;
                match stack.as_deref().and_then(|n| stacks.iter().find(|s| s.name == n)) {
                    Some(s) => {
                        w.compose.path = stack::local_compose_file(s).map(|p| p.display().to_string()).unwrap_or_default();
                        w.compose.project = s.name.clone();
                    }
                    None => {
                        w.compose.path.clear();
                        w.compose.project.clear();
                    }
                }
            }
        }
        Some(Wizard::ExportVolume { volume }) => {
            w.volume.volume = volume;
            w.volume.status = None;
        }
        Some(Wizard::ImportVolume { volume }) => {
            w.volume.volume = volume.unwrap_or_default();
            w.volume.file = None;
            w.volume.status = None;
        }
        Some(Wizard::PullImage) | None => {}
    }
}

/// Takes a finished task. A task that ended without a value is reported as an error.
fn take_done<A: Send + 'static>(slot: &mut Slot<Task<Result<A, String>>>) -> Option<Result<A, String>> {
    let task = slot.0.as_mut()?;
    if let Some(v) = task.take() {
        slot.0 = None;
        return Some(v);
    }
    if task.is_done() {
        slot.0 = None;
        return Some(Err("The task ended without a result".into()));
    }
    None
}

fn poll_pull(w: &mut WizardState, out: &mut Out) {
    let f = &mut w.pull;
    if let Some(r) = take_done(&mut f.pull) {
        match r {
            Ok(()) => {
                out.toasts.push((NoticeKind::Success, format!("Pulled {}", f.pulling)));
                out.refresh = true;
            }
            Err(e) => {
                out.toasts.push((NoticeKind::Error, format!("Pull failed: {e}")));
                f.error = Some(e);
            }
        }
    }
    if let Some(r) = take_done(&mut f.search) {
        f.searched = true;
        match r {
            Ok(hits) => {
                f.hits = hits;
                f.error = None;
            }
            Err(e) => {
                f.hits.clear();
                f.error = Some(e);
            }
        }
    }
}

fn poll_run(w: &mut WizardState, out: &mut Out) {
    match take_done(&mut w.run.task) {
        Some(Ok(id)) => {
            let typed = w.run.spec.name.trim().to_string();
            let name = if typed.is_empty() { short_id(&id) } else { typed };
            out.toasts.push((NoticeKind::Success, format!("Started {name}")));
            out.refresh = true;
            out.select = Some(id);
            w.run.error = None;
            w.open = None;
        }
        Some(Err(e)) => {
            out.toasts.push((NoticeKind::Error, format!("Run failed: {e}")));
            w.run.error = Some(e);
        }
        None => {}
    }
}

fn poll_compose(w: &mut WizardState, out: &mut Out) {
    let f = &mut w.compose;
    let Some(run) = f.run.0.as_ref() else { return };
    f.output = lock(&run.output).clone();
    let finished = lock(&run.result).clone();
    if let Some(res) = finished {
        match &res {
            Ok(()) => out.toasts.push((NoticeKind::Success, "Compose up finished".into())),
            Err(_) => out.toasts.push((NoticeKind::Error, "Compose up failed".into())),
        }
        f.result = Some(res);
        f.run.0 = None;
        out.refresh = true;
    }
}

fn poll_volume(w: &mut WizardState, out: &mut Out) {
    let f = &mut w.volume;
    if let Some(r) = take_done(&mut f.export) {
        match r {
            Ok(bytes) => {
                let text = format!("Exported {} ({})", f.export_name, format_bytes(bytes));
                out.toasts.push((NoticeKind::Success, text.clone()));
                f.status = Some(text);
            }
            Err(e) => {
                out.toasts.push((NoticeKind::Error, format!("Export failed: {e}")));
                f.status = Some(e);
            }
        }
    }
    if let Some(r) = take_done(&mut f.import) {
        match r {
            Ok(()) => {
                let text = format!("Imported into {}", f.import_name);
                out.toasts.push((NoticeKind::Success, text.clone()));
                out.refresh = true;
                f.status = Some(text);
            }
            Err(e) => {
                out.toasts.push((NoticeKind::Error, format!("Import failed: {e}")));
                f.status = Some(e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn engine(app: &App) -> Option<Engine> {
    app.docker.backend().and_then(|b| b.engine())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}...", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Compose project name from a folder name: lower case, only `[a-z0-9_-]`, starting with a letter or digit.
fn default_project(path: &Path) -> String {
    let dir = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let clean: String = dir.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    clean.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).to_string()
}

fn label(ui: &mut egui::Ui, text: &str, pal: &Palette) {
    ui.label(RichText::new(text).font(theme::ui(13.0)).color(pal.text_dim));
}

fn text(ui: &mut egui::Ui, text: &str, pal: &Palette) {
    ui.label(RichText::new(text).font(theme::ui(13.0)).color(pal.text));
}

fn note(ui: &mut egui::Ui, text: &str, pal: &Palette) {
    ui.label(RichText::new(text).font(theme::ui(12.5)).color(pal.warn));
}

fn single<'a>(text: &'a mut String, hint: &'a str, width: f32) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).hint_text(hint).desired_width(width).font(theme::ui(13.0))
}

fn multi<'a>(text: &'a mut String, hint: &'a str, width: f32) -> egui::TextEdit<'a> {
    egui::TextEdit::multiline(text).hint_text(hint).desired_width(width).desired_rows(2).font(theme::mono(12.0))
}

/// Button that is clicked only when `enabled`. Disabled buttons look the same as enabled ones.
fn enabled_button(ui: &mut egui::Ui, label: &str, pal: &Palette, enabled: bool) -> bool {
    let clicked = ui.add_enabled_ui(enabled, |ui| widgets::button(ui, label, pal, None).clicked()).inner;
    clicked && enabled
}

/// Progress of a pull: bar or spinner, status line and layer count.
fn progress_view(ui: &mut egui::Ui, progress: &Mutex<PullProgress>, pal: &Palette) {
    let p = lock(progress).clone();
    match p.fraction() {
        Some(fraction) => {
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
        }
        None => {
            ui.spinner();
        }
    }
    if !p.status.is_empty() {
        label(ui, &p.status, pal);
    }
    if !p.layers.is_empty() {
        let done = p.layers.values().filter(|(c, t)| c >= t).count();
        label(ui, &format!("Layers {done} of {}", p.layers.len()), pal);
    }
}

// ---------------------------------------------------------------------------
// Pull and search
// ---------------------------------------------------------------------------

enum PullAction {
    Pull(String),
    Search(String),
    Run(String),
}

fn pull_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let engine = engine(app);
    let f = &mut app.docker.ui.dialogs.wizards.pull;
    let pulling = f.pull.0.is_some();
    let searching = f.search.0.is_some();
    let mut action = None;

    ui.horizontal(|ui| {
        label(ui, "Image", pal);
        ui.add(single(&mut f.image, "nginx:latest", 380.0));
        if enabled_button(ui, "Pull", pal, !pulling && !f.image.trim().is_empty()) {
            action = Some(PullAction::Pull(f.image.clone()));
        }
    });
    if pulling {
        progress_view(ui, &f.progress, pal);
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Search").font(theme::semibold(14.0)).color(pal.text));
    ui.horizontal(|ui| {
        label(ui, "Term", pal);
        ui.add(single(&mut f.term, "busybox", 380.0));
        if enabled_button(ui, "Search", pal, !searching && !f.term.trim().is_empty()) {
            action = Some(PullAction::Search(f.term.clone()));
        }
        if searching {
            ui.spinner();
        }
    });
    if let Some(e) = &f.error {
        note(ui, e, pal);
    }
    let hits = f.hits.clone();
    if hits.is_empty() && f.searched && !searching {
        label(ui, "No images found.", pal);
    }
    if !hits.is_empty() {
        egui::ScrollArea::vertical().id_salt("pull-hits").max_height(260.0).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("pull-hit-grid").num_columns(5).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
                for head in ["Name", "Stars", "Official", "Description", ""] {
                    label(ui, head, pal);
                }
                ui.end_row();
                for h in &hits {
                    text(ui, &h.name, pal);
                    text(ui, &h.stars.to_string(), pal);
                    text(ui, if h.official { "yes" } else { "" }, pal);
                    ui.label(RichText::new(clip(&h.description, 50)).font(theme::ui(12.5)).color(pal.text_dim))
                        .on_hover_text(h.description.as_str());
                    ui.horizontal(|ui| {
                        if enabled_button(ui, "Pull", pal, !pulling) {
                            action = Some(PullAction::Pull(h.name.clone()));
                        }
                        if enabled_button(ui, "Run...", pal, true) {
                            action = Some(PullAction::Run(h.name.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    match action {
        Some(PullAction::Pull(image)) => start_pull(&mut app.docker.ui.dialogs.wizards.pull, engine.as_ref(), image),
        Some(PullAction::Search(term)) => start_search(&mut app.docker.ui.dialogs.wizards.pull, engine.as_ref(), term),
        Some(PullAction::Run(image)) => app.docker.ui.dialogs.wizards.open = Some(Wizard::RunImage { image }),
        None => {}
    }
}

fn start_pull(f: &mut PullForm, engine: Option<&Engine>, image: String) {
    f.error = None;
    let image = image.trim().to_string();
    if image.is_empty() {
        f.error = Some("Image is required".into());
        return;
    }
    let Some(engine) = engine else {
        f.error = Some(NOT_CONNECTED.into());
        return;
    };
    let docker = engine.docker.clone();
    let progress = f.progress.clone();
    f.pulling = image.clone();
    f.pull.0 = Some(Task::spawn(&engine.runtime, async move { run::pull(&docker, &image, progress).await }));
}

fn start_search(f: &mut PullForm, engine: Option<&Engine>, term: String) {
    f.error = None;
    let term = term.trim().to_string();
    if term.is_empty() {
        f.error = Some("Search term is empty".into());
        return;
    }
    let Some(engine) = engine else {
        f.error = Some(NOT_CONNECTED.into());
        return;
    };
    let docker = engine.docker.clone();
    f.searched = false;
    f.search.0 = Some(Task::spawn(&engine.runtime, async move { run::search(&docker, &term, SEARCH_LIMIT).await }));
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

fn run_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let engine = engine(app);
    let connected = engine.is_some();
    let read_only = engine.as_ref().map(|e| e.read_only).unwrap_or(false);
    let f = &mut app.docker.ui.dialogs.wizards.run;
    let running = f.task.0.is_some();
    let check = run::build_create(&f.spec).err();
    let mut start = false;
    let mut close = false;

    egui::Grid::new("run-form").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
        label(ui, "Image", pal);
        ui.add(single(&mut f.spec.image, "nginx:latest", 420.0));
        ui.end_row();

        label(ui, "Name", pal);
        ui.add(single(&mut f.spec.name, "optional", 420.0));
        ui.end_row();

        label(ui, "Ports", pal);
        ui.add(multi(&mut f.spec.ports, "8080:80 per line", 420.0));
        ui.end_row();

        label(ui, "Environment", pal);
        ui.add(multi(&mut f.spec.env, "KEY=VALUE per line", 420.0));
        ui.end_row();

        label(ui, "Volumes", pal);
        ui.add(multi(&mut f.spec.volumes, "name-or-path:/container/path[:ro] per line", 420.0));
        ui.end_row();

        label(ui, "Restart policy", pal);
        egui::ComboBox::from_id_salt("run-restart").width(200.0).selected_text(f.spec.restart.label()).show_ui(ui, |ui| {
            for r in Restart::ALL {
                ui.selectable_value(&mut f.spec.restart, r, r.label());
            }
        });
        ui.end_row();

        label(ui, "Command", pal);
        ui.add(single(&mut f.spec.command, "optional, e.g. sleep 60", 420.0));
        ui.end_row();

        label(ui, "Extra options", pal);
        ui.add(single(&mut f.spec.extra, "docker run style flags, e.g. --network host", 420.0));
        ui.end_row();
    });
    ui.horizontal(|ui| {
        widgets::toggle(ui, &mut f.spec.pull, pal);
        text(ui, "Pull image if missing", pal);
    });
    ui.add_space(6.0);
    if let Some(e) = &check {
        note(ui, e, pal);
    }
    if let Some(e) = &f.error {
        note(ui, e, pal);
    }
    if read_only {
        note(ui, READ_ONLY, pal);
    } else if !connected {
        note(ui, NOT_CONNECTED, pal);
    }
    if running {
        progress_view(ui, &f.progress, pal);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if enabled_button(ui, "Run", pal, check.is_none() && !running && connected && !read_only) {
            start = true;
        }
        if !running && widgets::button(ui, "Close", pal, None).clicked() {
            close = true;
        }
    });

    if start {
        start_run(&mut app.docker.ui.dialogs.wizards.run, engine.as_ref());
    }
    if close {
        app.docker.ui.dialogs.wizards.open = None;
    }
}

fn start_run(f: &mut RunForm, engine: Option<&Engine>) {
    f.error = None;
    let Some(engine) = engine else {
        f.error = Some(NOT_CONNECTED.into());
        return;
    };
    *lock(&f.progress) = PullProgress::default();
    let docker = engine.docker.clone();
    let progress = f.progress.clone();
    let spec = f.spec.clone();
    f.task.0 = Some(Task::spawn(&engine.runtime, async move { run::run(&docker, &spec, progress).await }));
}

// ---------------------------------------------------------------------------
// Add host
// ---------------------------------------------------------------------------

/// The Docker host string for the form, or the reason it is invalid.
fn host_string(f: &HostForm, names: &[String]) -> Result<String, String> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err("Name is required".into());
    }
    if names.iter().any(|n| n == name) {
        return Err(format!("A Docker context named {name} exists"));
    }
    let host = f.host.trim();
    if host.is_empty() {
        return Err("Host is required".into());
    }
    if f.tcp && host.contains('@') {
        return Err("TCP hosts take server:port; use SSH for user@server".into());
    }
    let port = f.port.trim();
    let port = if port.is_empty() {
        None
    } else {
        match port.parse::<u16>() {
            Ok(p) if p > 0 => Some(p),
            _ => return Err("Port must be a number from 1 to 65535".into()),
        }
    };
    if port.is_some() && host.contains(':') {
        return Err("Give the port in Host or in Port, not both".into());
    }
    let scheme = if f.tcp { "tcp" } else { "ssh" };
    let mut s = format!("{scheme}://{host}");
    if let Some(p) = port {
        s.push_str(&format!(":{p}"));
    }
    Endpoint::parse(&s).map(|_| s)
}

fn host_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let mut names: Vec<String> = app.docker.contexts.iter().map(|c| c.name.clone()).collect();
    names.extend(app.settings.docker_hosts.iter().map(|h| h.name.clone()));
    let existing = app.settings.docker_hosts.clone();
    let f = &mut app.docker.ui.dialogs.wizards.host;
    let check = host_string(f, &names);
    let mut remove: Option<String> = None;
    let mut save = false;
    let mut close = false;

    egui::Grid::new("host-form").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
        label(ui, "Name", pal);
        ui.add(single(&mut f.name, "work-server", 340.0));
        ui.end_row();

        label(ui, "Type", pal);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut f.tcp, false, "SSH");
            ui.selectable_value(&mut f.tcp, true, "TCP");
        });
        ui.end_row();

        label(ui, "Host", pal);
        let hint = if f.tcp { "server:2375" } else { "user@server" };
        ui.add(single(&mut f.host, hint, 340.0));
        ui.end_row();

        label(ui, "Port", pal);
        ui.add(single(&mut f.port, "optional", 90.0));
        ui.end_row();

        label(ui, "Description", pal);
        ui.add(single(&mut f.description, "optional", 340.0));
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "SSH uses your ssh keys or agent; passwords are never stored. TLS for TCP hosts is not supported in this build.",
        )
        .font(theme::ui(12.0))
        .color(pal.text_faint),
    );
    let input_given = !f.name.trim().is_empty() || !f.host.trim().is_empty();
    if input_given {
        if let Err(e) = &check {
            note(ui, e, pal);
        }
    }
    ui.horizontal(|ui| {
        if enabled_button(ui, "Add host", pal, check.is_ok()) {
            save = true;
        }
        if widgets::button(ui, "Close", pal, None).clicked() {
            close = true;
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new("TMOP hosts").font(theme::semibold(14.0)).color(pal.text));
    if existing.is_empty() {
        label(ui, "No hosts added yet. Hosts added here are kept in TMOP's settings, not in the Docker CLI configuration.", pal);
    }
    for h in &existing {
        ui.horizontal(|ui| {
            text(ui, &h.name, pal);
            ui.label(RichText::new(&h.host).font(theme::mono(12.0)).color(pal.text_dim));
            if widgets::button(ui, "Remove", pal, None).clicked() {
                remove = Some(h.name.clone());
            }
        });
    }

    let host = check.ok();
    if save {
        if let Some(host) = host {
            let (name, description) = (f.name.trim().to_string(), f.description.trim().to_string());
            *f = HostForm::default();
            app.settings.docker_hosts.push(DockerHost { name: name.clone(), host, description });
            apply_hosts(app);
            app.docker.ui.toast(NoticeKind::Success, format!("Added host {name}"));
            app.docker.ui.dialogs.wizards.open = None;
        }
    }
    if let Some(name) = remove {
        remove_host(app, &name);
    }
    if close {
        app.docker.ui.dialogs.wizards.open = None;
    }
}

/// Rebuilds the context list from the CLI and TMOP hosts, and sends it to the worker.
fn apply_hosts(app: &mut App) {
    let contexts = merged_contexts(&app.settings);
    app.settings_dirty = true;
    if let Some(b) = app.docker.backend() {
        b.send(Command::SetContexts(contexts.clone()));
    }
    app.docker.contexts = contexts;
}

/// Removes a TMOP host. When it is the active context, the default context is selected.
fn remove_host(app: &mut App, name: &str) {
    app.settings.docker_hosts.retain(|h| h.name != name);
    app.settings.docker_contexts.remove(name);
    let was_active = app.docker.ui.snap.active == name || app.settings.docker_active_context == name;
    apply_hosts(app);
    if was_active {
        let fallback = super::resolve_name(&app.docker.contexts, "");
        let ro = app.settings.docker_contexts.get(&fallback).map(|c| c.read_only).unwrap_or(false);
        if let Some(b) = app.docker.backend() {
            b.send(Command::SetReadOnly(ro));
            b.send(Command::SwitchContext(fallback.clone()));
        }
        app.settings.docker_active_context = fallback;
    }
    app.docker.ui.toast(NoticeKind::Info, format!("Removed host {name}"));
}

// ---------------------------------------------------------------------------
// Compose up
// ---------------------------------------------------------------------------

fn compose_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let engine = engine(app);
    let connected = engine.is_some();
    let read_only = engine.as_ref().map(|e| e.read_only).unwrap_or(false);
    let context = engine.as_ref().map(|e| e.context.clone()).unwrap_or_default();
    let tmop_host = app.settings.docker_hosts.iter().any(|h| h.name == context);
    let available = stack::compose_available();
    let f = &mut app.docker.ui.dialogs.wizards.compose;
    let running = f.run.0.is_some();
    let file_ok = Path::new(f.path.trim()).is_file();
    let mut browse = false;
    let mut open_editor = false;
    let mut up = false;
    let mut close = false;

    egui::Grid::new("compose-form").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
        label(ui, "Compose file", pal);
        ui.horizontal(|ui| {
            ui.add(single(&mut f.path, "docker-compose.yml", 330.0));
            if enabled_button(ui, "Browse...", pal, !running) {
                browse = true;
            }
        });
        ui.end_row();

        label(ui, "Project", pal);
        ui.add(single(&mut f.project, "folder name", 330.0));
        ui.end_row();

        label(ui, "Host", pal);
        let host = if connected { context.clone() } else { "not connected".to_string() };
        text(ui, &host, pal);
        ui.end_row();
    });
    if !available {
        note(ui, "The docker CLI with the compose plugin was not found.", pal);
    }
    if tmop_host {
        note(ui, "TMOP hosts are not Docker CLI contexts, so compose up cannot target them.", pal);
    }
    if read_only {
        note(ui, READ_ONLY, pal);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if enabled_button(ui, "Open in editor", pal, file_ok) {
            open_editor = true;
        }
        let can_up = available && file_ok && !running && connected && !read_only && !tmop_host;
        if enabled_button(ui, "Up", pal, can_up) {
            up = true;
        }
        if running {
            ui.spinner();
            label(ui, "Running...", pal);
        } else if widgets::button(ui, "Close", pal, None).clicked() {
            close = true;
        }
    });
    if running || !f.output.is_empty() {
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("compose-output")
            .max_height(240.0)
            .min_scrolled_height(120.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.label(RichText::new(&f.output).font(theme::mono(12.0)).color(pal.text));
            });
    }
    match &f.result {
        Some(Ok(())) => text(ui, "Up finished.", pal),
        Some(Err(e)) => note(ui, e, pal),
        None => {}
    }

    if browse {
        if let Some(p) = rfd::FileDialog::new().add_filter("Compose", &["yml", "yaml"]).pick_file() {
            f.path = p.display().to_string();
            if f.project.trim().is_empty() {
                f.project = default_project(&p);
            }
        }
    }
    if open_editor {
        if let Err(e) = stack::open_in_editor(Path::new(f.path.trim())) {
            f.result = Some(Err(e));
        }
    }
    if up {
        start_compose(f, engine.as_ref());
    }
    if close {
        app.docker.ui.dialogs.wizards.open = None;
    }
}

fn start_compose(f: &mut ComposeForm, engine: Option<&Engine>) {
    f.result = None;
    f.output.clear();
    let Some(engine) = engine else {
        f.result = Some(Err(NOT_CONNECTED.into()));
        return;
    };
    let path = PathBuf::from(f.path.trim());
    let project = f.project.trim().to_string();
    let project = (!project.is_empty()).then_some(project);
    match stack::compose_up(&path, project.as_deref(), &engine.context) {
        Ok(run) => f.run.0 = Some(run),
        Err(e) => f.result = Some(Err(e)),
    }
}

// ---------------------------------------------------------------------------
// Export and import volumes
// ---------------------------------------------------------------------------

fn export_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let engine = engine(app);
    let connected = engine.is_some();
    let f = &mut app.docker.ui.dialogs.wizards.volume;
    let running = f.export.0.is_some();
    let mut dest: Option<PathBuf> = None;

    ui.horizontal(|ui| {
        label(ui, "Volume", pal);
        ui.label(RichText::new(&f.volume).font(theme::semibold(13.0)).color(pal.text));
    });
    ui.label(
        RichText::new("The contents of the volume are saved to a .tar archive.").font(theme::ui(12.0)).color(pal.text_faint),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if enabled_button(ui, "Choose file and export...", pal, connected && !running) {
            dest = rfd::FileDialog::new()
                .set_file_name(format!("{}.tar", f.volume))
                .add_filter("Tar archive", &["tar"])
                .save_file();
        }
        if running {
            ui.spinner();
            label(ui, "Exporting...", pal);
        }
    });
    if !connected {
        note(ui, NOT_CONNECTED, pal);
    }
    if let Some(s) = &f.status {
        text(ui, s, pal);
    }
    if let Some(dest) = dest {
        spawn_export(f, engine.as_ref(), dest);
    }
}

fn spawn_export(f: &mut VolumeForm, engine: Option<&Engine>, dest: PathBuf) {
    f.status = None;
    let Some(engine) = engine else {
        f.status = Some(NOT_CONNECTED.into());
        return;
    };
    let docker = engine.docker.clone();
    let volume = f.volume.clone();
    f.export_name = volume.clone();
    f.export.0 = Some(Task::spawn(&engine.runtime, async move { files::export_volume(&docker, &volume, &dest).await }));
}

fn import_body(app: &mut App, ui: &mut egui::Ui, pal: &Palette) {
    let engine = engine(app);
    let connected = engine.is_some();
    let read_only = engine.as_ref().map(|e| e.read_only).unwrap_or(false);
    let f = &mut app.docker.ui.dialogs.wizards.volume;
    let running = f.import.0.is_some();
    let mut browse = false;
    let mut start = false;

    egui::Grid::new("import-form").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
        label(ui, "Archive", pal);
        ui.horizontal(|ui| {
            let shown = f.file.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "no file chosen".into());
            ui.label(RichText::new(clip(&shown, 44)).font(theme::ui(13.0)).color(pal.text));
            if enabled_button(ui, "Choose file...", pal, !running) {
                browse = true;
            }
        });
        ui.end_row();

        label(ui, "Volume", pal);
        ui.add(single(&mut f.volume, "volume name", 300.0));
        ui.end_row();
    });
    ui.add_space(4.0);
    note(ui, "Files in the archive overwrite files with the same name in the volume.", pal);
    if read_only {
        note(ui, READ_ONLY, pal);
    } else if !connected {
        note(ui, NOT_CONNECTED, pal);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let can = connected && !read_only && !running && f.file.is_some() && !f.volume.trim().is_empty();
        if enabled_button(ui, "Import", pal, can) {
            start = true;
        }
        if running {
            ui.spinner();
            label(ui, "Importing...", pal);
        }
    });
    if let Some(s) = &f.status {
        text(ui, s, pal);
    }

    if browse {
        if let Some(p) = rfd::FileDialog::new().add_filter("Tar archive", &["tar"]).pick_file() {
            f.file = Some(p);
        }
    }
    if start {
        spawn_import(f, engine.as_ref());
    }
}

fn spawn_import(f: &mut VolumeForm, engine: Option<&Engine>) {
    f.status = None;
    let Some(engine) = engine else {
        f.status = Some(NOT_CONNECTED.into());
        return;
    };
    let Some(file) = f.file.clone() else { return };
    let docker = engine.docker.clone();
    let volume = f.volume.trim().to_string();
    f.import_name = volume.clone();
    f.import.0 = Some(Task::spawn(&engine.runtime, async move { files::import_volume(&docker, &volume, &file).await }));
}
