//! Dialogs of the Docker page: the confirmation before a removal or prune, and the stack
//! actions that run as tasks on the worker. [`request`] is the entry point for every action
//! chosen from a toolbar or a context menu; [`show`] runs once per frame at the end of the page.

use super::{engine_wizard, wizards, Snap};
use crate::docker::backend::{Command, ContainerAction, NoticeKind, PruneKind, Task};
use crate::docker::stack::{stack_action, StackAction};
use crate::ui::theme::Palette;
use crate::ui::{widgets, App, FrameData};
use eframe::egui::{self, Align2, Id, Key, RichText};
use std::time::Duration;

/// An action on a Docker object, run from a toolbar, a menu or a confirmation.
#[derive(Clone, Debug)]
pub enum PendingAction {
    Container { id: String, name: String, action: ContainerAction },
    Image { id: String, name: String },
    Volume { name: String },
    Network { id: String, name: String },
    Prune(PruneKind),
    Stack { name: String, action: StackAction },
}

/// A confirmation waiting for the user.
#[derive(Debug)]
pub struct Confirm {
    pub title: String,
    /// Question and context lines, such as "Remove container nginx-1?" and "Host: desktop-linux".
    pub lines: Vec<String>,
    pub action: PendingAction,
    /// Text typed into the confirmation field.
    pub typed: String,
    /// The text that must be typed before the action can run (a volume name).
    pub require: Option<String>,
    pub danger: bool,
    /// Checkboxes such as "Force"; the value is read when the action runs.
    pub options: Vec<(String, bool)>,
}

/// A stack action running on the worker. `ids` are the containers marked busy until it ends.
pub struct StackTask {
    pub stack: String,
    pub ids: Vec<String>,
    pub task: Task<Result<String, String>>,
}

/// Dialog state: the open wizard, the pending confirmation and the running stack tasks.
#[derive(Default)]
pub struct DialogState {
    pub wizards: wizards::WizardState,
    pub confirm: Option<Confirm>,
    pub stack_tasks: Vec<StackTask>,
}

/// Runs an action chosen from a toolbar or menu: asks first when the action needs it.
pub fn request(app: &mut App, action: PendingAction) {
    if app.docker.ui.snap.read_only {
        app.docker.ui.toast(NoticeKind::Error, "This host is read-only");
        return;
    }
    if needs_confirm(&action) {
        let host = app.active_name();
        let confirm = confirm_for(&host, &app.docker.ui.snap, action);
        app.docker.ui.dialogs.confirm = Some(confirm);
    } else {
        execute(app, action, &[]);
    }
}

/// Removals, prunes and stack `Down` ask first; start, stop, restart and pause run at once.
fn needs_confirm(action: &PendingAction) -> bool {
    !matches!(
        action,
        PendingAction::Container { action: ContainerAction::Start | ContainerAction::Stop | ContainerAction::Restart | ContainerAction::Pause | ContainerAction::Unpause | ContainerAction::Kill, .. }
            | PendingAction::Stack { action: StackAction::Start | StackAction::Stop | StackAction::Restart, .. }
    )
}

fn is_running(snap: &Snap, id: &str) -> bool {
    snap.containers.iter().any(|c| c.id == id && c.state.is_running())
}

/// Builds the confirmation for `action` on `host`.
fn confirm_for(host: &str, snap: &Snap, action: PendingAction) -> Confirm {
    let host_line = format!("Host: {host}");
    let (title, lines, options, require) = match &action {
        PendingAction::Container { id, name, action: ContainerAction::Remove { .. } } => {
            let mut lines = vec![format!("Remove container {name}?"), host_line];
            if is_running(snap, id) {
                lines.push("The container is running and will be killed.".into());
            }
            let options = vec![("Also remove anonymous volumes".to_string(), false), ("Force".to_string(), false)];
            ("Remove container".to_string(), lines, options, None)
        }
        PendingAction::Image { name, .. } => {
            let lines = vec![format!("Remove image {name}?"), host_line];
            ("Remove image".to_string(), lines, vec![("Force".to_string(), false)], None)
        }
        PendingAction::Volume { name } => {
            let lines = vec![format!("Remove volume {name}?"), host_line, "Its data is lost.".into()];
            ("Remove volume".to_string(), lines, Vec::new(), Some(name.clone()))
        }
        PendingAction::Network { name, .. } => {
            let lines = vec![format!("Remove network {name}?"), host_line];
            ("Remove network".to_string(), lines, Vec::new(), None)
        }
        PendingAction::Prune(kind) => (prune_title(kind).to_string(), vec![prune_text(kind, host)], Vec::new(), None),
        PendingAction::Stack { name, action: StackAction::Down { .. } } => {
            let lines = vec![format!("Stop and remove the containers and networks of stack {name}?"), host_line];
            ("Down stack".to_string(), lines, vec![("Also remove volumes".to_string(), false)], None)
        }
        _ => ("Confirm".to_string(), vec![host_line], Vec::new(), None),
    };
    Confirm { title, lines, action, typed: String::new(), require, danger: true, options }
}

fn prune_title(kind: &PruneKind) -> &'static str {
    match kind {
        PruneKind::Containers => "Prune containers",
        PruneKind::Images => "Prune dangling images",
        PruneKind::ImagesAll => "Prune unused images",
        PruneKind::Volumes => "Prune volumes",
        PruneKind::Networks => "Prune networks",
        PruneKind::BuildCache => "Prune build cache",
    }
}

fn prune_text(kind: &PruneKind, host: &str) -> String {
    match kind {
        PruneKind::Containers => format!("Removes all stopped containers on {host}."),
        PruneKind::Images => format!("Removes dangling images (untagged and unused) on {host}."),
        PruneKind::ImagesAll => format!("Removes all images not used by a container on {host}."),
        PruneKind::Volumes => format!("Removes all volumes not used by a container on {host}. Their data is lost."),
        PruneKind::Networks => format!("Removes all networks not used by a container on {host}."),
        PruneKind::BuildCache => format!("Removes the build cache on {host}."),
    }
}

/// Button text of the confirmation.
fn verb(action: &PendingAction) -> &'static str {
    match action {
        PendingAction::Container { .. } | PendingAction::Image { .. } | PendingAction::Volume { .. } | PendingAction::Network { .. } => "Remove",
        PendingAction::Prune(_) => "Prune",
        PendingAction::Stack { .. } => "Down",
    }
}

/// Carries out an action. `options` are the confirmation checkboxes, in the order they were built.
fn execute(app: &mut App, action: PendingAction, options: &[(String, bool)]) {
    if app.docker.ui.snap.read_only {
        app.docker.ui.toast(NoticeKind::Error, "This host is read-only");
        return;
    }
    let opt = |i: usize| options.get(i).is_some_and(|o| o.1);
    let command = match action {
        PendingAction::Container { id, name, action } => {
            let action = match action {
                // Containers that are running cannot be removed without force.
                ContainerAction::Remove { .. } => ContainerAction::Remove { force: opt(1) || is_running(&app.docker.ui.snap, &id), volumes: opt(0) },
                other => other,
            };
            Command::Container { id, name, action }
        }
        PendingAction::Image { id, name } => Command::RemoveImage { id, name, force: opt(0) },
        PendingAction::Volume { name } => Command::RemoveVolume { name, force: false },
        PendingAction::Network { id, name } => Command::RemoveNetwork { id, name },
        PendingAction::Prune(kind) => Command::Prune(kind),
        PendingAction::Stack { name, action } => {
            let action = match action {
                StackAction::Down { .. } => StackAction::Down { volumes: opt(0) },
                other => other,
            };
            run_stack(app, name, action);
            return;
        }
    };
    if let Some(b) = &app.docker.backend {
        b.send(command);
    }
}

fn stack_progress(action: StackAction) -> &'static str {
    match action {
        StackAction::Start => "Starting",
        StackAction::Stop => "Stopping",
        StackAction::Restart => "Restarting",
        StackAction::Down { .. } => "Removing",
    }
}

/// Starts a stack action on the worker runtime and marks the stack's containers busy.
fn run_stack(app: &mut App, name: String, action: StackAction) {
    let snap = &app.docker.ui.snap;
    let Some(stack) = snap.stacks.iter().find(|s| s.name == name).cloned() else {
        app.docker.ui.toast(NoticeKind::Error, format!("Stack {name} is gone"));
        return;
    };
    let (containers, networks, volumes) = (snap.containers.clone(), snap.networks.clone(), snap.volumes.clone());
    let Some(backend) = app.docker.backend.as_ref() else { return };
    let Some(engine) = backend.engine() else {
        app.docker.ui.toast(NoticeKind::Error, "Not connected to Docker");
        return;
    };
    let ids = stack.containers.clone();
    for id in &ids {
        backend.set_busy(id, Some(stack_progress(action)));
    }
    let docker = engine.docker.clone();
    let task = Task::spawn(&engine.runtime, async move { stack_action(&docker, &stack, &containers, &networks, &volumes, action).await });
    app.docker.ui.dialogs.stack_tasks.push(StackTask { stack: name, ids, task });
}

/// Collects finished stack tasks: clears their busy marks, toasts the result and refreshes.
fn poll_stack_tasks(app: &mut App) {
    let tasks = std::mem::take(&mut app.docker.ui.dialogs.stack_tasks);
    let mut running = Vec::new();
    let mut done = Vec::new();
    for mut t in tasks {
        match t.task.take() {
            Some(result) => done.push((t, result)),
            None => running.push(t),
        }
    }
    app.docker.ui.dialogs.stack_tasks = running;
    for (t, result) in done {
        if let Some(b) = &app.docker.backend {
            for id in &t.ids {
                b.set_busy(id, None);
            }
            b.send(Command::Refresh);
        }
        match result {
            Ok(text) => app.docker.ui.toast(NoticeKind::Success, text),
            Err(text) => app.docker.ui.toast(NoticeKind::Error, format!("{}: {text}", t.stack)),
        }
    }
}

/// Draws the confirmation window, if one is pending.
fn confirm_window(app: &mut App, ctx: &egui::Context, pal: &Palette) {
    let enter = ctx.input(|i| i.key_pressed(Key::Enter));
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));
    let Some(c) = app.docker.ui.dialogs.confirm.as_mut() else { return };
    let title = c.title.clone();
    let ready = c.require.as_ref().is_none_or(|r| c.typed == *r);
    let verb = verb(&c.action);
    let danger = c.danger;
    let mut answer: Option<bool> = None;
    egui::Window::new(title)
        .id(Id::new("docker-confirm"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(egui::Frame::window(&ctx.style()).fill(pal.panel))
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            for line in &c.lines {
                ui.label(line.as_str());
            }
            if !c.options.is_empty() {
                ui.add_space(6.0);
                for (label, value) in c.options.iter_mut() {
                    ui.checkbox(value, label.as_str());
                }
            }
            if c.require.is_some() {
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut c.typed).hint_text("Type the volume name to confirm").desired_width(f32::INFINITY));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, "Cancel", pal, None).clicked() || escape {
                    answer = Some(false);
                }
                let go = if danger {
                    ui.add_enabled(ready, egui::Button::new(RichText::new(verb).color(pal.warn))).clicked()
                } else {
                    ui.add_enabled_ui(ready, |ui| widgets::button(ui, verb, pal, None)).inner.clicked()
                };
                if go || (enter && ready) {
                    answer = Some(true);
                }
            });
        });
    match answer {
        Some(true) => {
            if let Some(c) = app.docker.ui.dialogs.confirm.take() {
                execute(app, c.action, &c.options);
            }
        }
        Some(false) => app.docker.ui.dialogs.confirm = None,
        None => {}
    }
}

/// Draws any open dialog. Called from the Docker page each frame.
pub fn show(app: &mut App, ctx: &egui::Context, fd: &FrameData<'_>) {
    poll_stack_tasks(app);
    if !app.docker.ui.dialogs.stack_tasks.is_empty() {
        // Task completion does not wake the UI, so poll again shortly.
        ctx.request_repaint_after(Duration::from_millis(200));
    }
    confirm_window(app, ctx, &fd.pal);
    wizards::show(app, ctx, fd);
    engine_wizard::show(app, ctx, fd);
}
