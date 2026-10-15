//! Terminal tab of the detail area: interactive shells in a container, one sub-tab per
//! session.
//!
//! [`TerminalSessions`] is owned by the detail area and survives container switches and
//! page changes. Sessions end when their container disappears or the host changes.

use super::detail::{Toasts, TOOLBAR_H};
use crate::docker::backend::{Engine, NoticeKind};
use crate::docker::exec::{split_command, ExecOptions, ExecSession};
use crate::docker::model::Container;
use crate::terminal::{sink_for, Emulator, Transport, TransportState};
use crate::ui::kit;
use crate::ui::terminal::TerminalView;
use crate::ui::theme::{self, Palette};
use crate::ui::widgets;
use eframe::egui::{self, pos2, popup::popup_below_widget, popup::PopupCloseBehavior, vec2, Align2, Color32, Rect, Sense, Stroke, TextEdit};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Width of a session's close mark.
const CLOSE_W: f32 = 18.0;
/// Width of the dropdown button that holds the command and user inputs.
const DROP_W: f32 = 26.0;
/// Width of the command and user inputs in the dropdown.
const MENU_W: f32 = 260.0;
/// Initial terminal size in cells.
const COLS: u16 = 80;
const ROWS: u16 = 24;
const SCROLLBACK: usize = 10_000;
/// Opacity of the buttons that cannot be used right now.
const DIMMED: f32 = 0.45;

/// Open terminal sessions, kept across container switches and page changes.
#[derive(Default)]
pub struct TerminalSessions {
    by_container: HashMap<String, ContainerSessions>,
    next_id: u64,
    pending: Option<PendingOpen>,
}

#[derive(Default)]
struct ContainerSessions {
    sessions: Vec<Session>,
    active: usize,
    command: String,
    user: String,
    /// Sessions opened for this container, used in the labels.
    opened: u64,
}

struct Session {
    label: String,
    emulator: Arc<Mutex<Emulator>>,
    transport: ExecSession,
    view: TerminalView,
    /// Kept for the Restart button.
    options: ExecOptions,
}

struct PendingOpen {
    /// Container name or id prefix.
    container: String,
    command: Option<String>,
}

/// What the toolbar asked for this frame.
enum Action {
    None,
    Select(usize),
    Close(usize),
    New(ExecOptions),
    Restart(usize),
}

impl TerminalSessions {
    /// True when the container has at least one session tab open (running or ended).
    pub fn has_sessions(&self, container_id: &str) -> bool {
        self.by_container.get(container_id).is_some_and(|c| !c.sessions.is_empty())
    }

    /// Ids of containers with open session tabs (for the list marker).
    pub fn container_ids(&self) -> HashSet<String> {
        self.by_container.iter().filter(|(_, c)| !c.sessions.is_empty()).map(|(id, _)| id.clone()).collect()
    }

    /// Opens a new session; on success it becomes the active sub-tab and receives focus.
    pub fn open(&mut self, engine: &Engine, ctx: &egui::Context, container_id: &str, options: ExecOptions) -> Result<(), String> {
        let n = self.next_id;
        self.next_id += 1;
        let cs = self.by_container.entry(container_id.to_string()).or_default();
        let label = label_for(&options, cs.opened + 1);
        let session = start(engine, ctx, container_id, options, label, view_salt(container_id, n))?;
        cs.opened += 1;
        cs.sessions.push(session);
        cs.active = cs.sessions.len() - 1;
        Ok(())
    }

    /// Ends and drops every session (host switch, engine gone).
    pub fn close_all(&mut self) {
        self.by_container.clear();
        self.pending = None;
    }

    /// Drops sessions of containers that are no longer listed.
    pub fn retain(&mut self, alive: &HashSet<&str>) {
        self.by_container.retain(|id, _| alive.contains(id.as_str()));
    }

    /// Asks to open a session as soon as the named container (name or id prefix) is listed; used by the start-up flag.
    pub fn request(&mut self, container: String, command: Option<String>) {
        self.pending = Some(PendingOpen { container, command });
    }

    /// Consumes a pending request if its container is listed: returns (container id, options).
    pub fn take_pending(&mut self, containers: &[Container]) -> Option<(String, ExecOptions)> {
        let wanted = self.pending.as_ref()?;
        let id = containers.iter().find(|c| c.name == wanted.container || (!wanted.container.is_empty() && c.id.starts_with(&wanted.container)))?.id.clone();
        let pending = self.pending.take()?;
        let command = pending.command.map(|line| split_command(&line)).filter(|c| !c.is_empty());
        Some((id, ExecOptions { command, user: None, cols: COLS, rows: ROWS }))
    }

    /// Replaces the session at `index` with a new one that starts with the same options.
    fn restart(&mut self, engine: &Engine, ctx: &egui::Context, container_id: &str, index: usize) -> Result<(), String> {
        let n = self.next_id;
        self.next_id += 1;
        let Some(cs) = self.by_container.get_mut(container_id) else { return Ok(()) };
        let Some(old) = cs.sessions.get(index) else { return Ok(()) };
        let session = start(engine, ctx, container_id, old.options.clone(), old.label.clone(), view_salt(container_id, n))?;
        cs.sessions[index] = session;
        cs.active = index;
        Ok(())
    }
}

impl ContainerSessions {
    /// Drops the session at `index` and keeps the active index on a valid tab.
    fn remove(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.sessions.remove(index);
            if index < self.active {
                self.active -= 1;
            }
            self.active = self.active.min(self.sessions.len().saturating_sub(1));
        }
    }
}

fn label_for(options: &ExecOptions, n: u64) -> String {
    let base = options.command.as_ref().and_then(|c| c.first()).map_or("Shell", String::as_str);
    format!("{base} {n}")
}

fn view_salt(container_id: &str, n: u64) -> String {
    format!("docker-term-{container_id}-{n}")
}

fn start(engine: &Engine, ctx: &egui::Context, container_id: &str, options: ExecOptions, label: String, salt: String) -> Result<Session, String> {
    let emulator = Arc::new(Mutex::new(Emulator::new(usize::from(COLS), usize::from(ROWS), SCROLLBACK)));
    let repaint = ctx.clone();
    let sink = sink_for(&emulator, move || repaint.request_repaint());
    let transport = ExecSession::start(engine, container_id, options.clone(), sink)?;
    let mut view = TerminalView::new(salt);
    view.request_focus();
    Ok(Session { label, emulator, transport, view, options })
}

/// Terminal tab: the session toolbar on top, the active session below.
#[allow(clippy::too_many_arguments)]
pub fn terminal_tab(
    state: &mut TerminalSessions,
    engine: &Engine,
    id: &str,
    name: &str,
    running: bool,
    read_only: bool,
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    rect: Rect,
    pal: &Palette,
    toasts: &mut Toasts,
) {
    let split = kit::vsplit(rect, &[TOOLBAR_H, -1.0], 4.0);
    let blocked = if !running {
        Some("Container is not running")
    } else if read_only {
        Some("Context is read-only")
    } else {
        None
    };

    let cs = state.by_container.entry(id.to_string()).or_default();
    let mut action = Action::None;
    kit::row_ui(ui, split[0], |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, s) in cs.sessions.iter().enumerate() {
            if let Some(a) = session_tab(ui, pal, i, s, i == cs.active) {
                action = a;
            }
        }
        ui.add_space(6.0);
        if gated_button(ui, "New session", pal, blocked) {
            action = Action::New(ExecOptions { command: None, user: None, cols: COLS, rows: ROWS });
        }
        let drop = drop_button(ui, pal);
        let menu_id = ui.id().with("docker-term-menu");
        if drop.clicked() {
            ui.memory_mut(|m| m.toggle_popup(menu_id));
        }
        popup_below_widget(ui, menu_id, &drop, PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(MENU_W);
            ui.add(TextEdit::singleline(&mut cs.command).hint_text("Command (empty = bash, then sh)").desired_width(MENU_W).font(theme::ui(13.0)));
            ui.add(TextEdit::singleline(&mut cs.user).hint_text("User (optional)").desired_width(MENU_W).font(theme::ui(13.0)));
            if gated_button(ui, "Start", pal, blocked) {
                let command = Some(split_command(&cs.command)).filter(|c| !c.is_empty());
                let user = Some(cs.user.trim().to_string()).filter(|u| !u.is_empty());
                action = Action::New(ExecOptions { command, user, cols: COLS, rows: ROWS });
                ui.memory_mut(|m| m.close_popup());
            }
        });
        if let Some(s) = cs.sessions.get(cs.active) {
            if matches!(s.transport.state(), TransportState::Ended(_)) {
                if gated_button(ui, "Restart", pal, blocked) {
                    action = Action::Restart(cs.active);
                }
                if widgets::button(ui, "Close", pal, None).clicked() {
                    action = Action::Close(cs.active);
                }
            }
        }
        if let Some(shell) = cs.sessions.get(cs.active).and_then(|s| s.transport.shell()) {
            ui.painter().text(pos2(split[0].right() - 8.0, split[0].center().y), Align2::RIGHT_CENTER, format!("shell: {shell}"), theme::ui(12.0), pal.text_dim);
        }
    });

    match action {
        Action::None => {}
        Action::Select(i) => {
            if let Some(cs) = state.by_container.get_mut(id) {
                cs.active = i;
            }
        }
        Action::Close(i) => {
            if let Some(cs) = state.by_container.get_mut(id) {
                cs.remove(i);
            }
        }
        Action::New(options) => {
            if let Err(e) = state.open(engine, ctx, id, options) {
                toasts.push((NoticeKind::Error, format!("{name}: {e}")));
            }
        }
        Action::Restart(i) => {
            if let Err(e) = state.restart(engine, ctx, id, i) {
                toasts.push((NoticeKind::Error, format!("{name}: {e}")));
            }
        }
    }

    match state.by_container.get_mut(id).filter(|cs| !cs.sessions.is_empty()) {
        Some(cs) => {
            cs.active = cs.active.min(cs.sessions.len() - 1);
            let i = cs.active;
            let s = &mut cs.sessions[i];
            s.view.show(ui, split[1], pal, &s.emulator, &mut s.transport);
        }
        None => kit::empty_state(ui.painter(), split[1], pal, "No terminal session - click New session to open a shell in the container"),
    }
}

/// One session of the toolbar: status dot and label, then a close mark.
fn session_tab(ui: &mut egui::Ui, pal: &Palette, index: usize, s: &Session, selected: bool) -> Option<Action> {
    let font = theme::ui(13.0);
    let label_w = widgets::text_width(ui.painter(), &s.label, font.clone()) + 26.0;
    let (rect, _) = ui.allocate_exact_size(vec2(label_w + CLOSE_W, TOOLBAR_H), Sense::hover());
    let label_rect = Rect::from_min_size(rect.min, vec2(label_w, TOOLBAR_H));
    let close_rect = Rect::from_min_size(pos2(label_rect.right(), rect.top()), vec2(CLOSE_W, TOOLBAR_H));
    let tab = ui.interact(label_rect, ui.id().with(("term-tab", index)), Sense::click());
    let close = ui.interact(close_rect, ui.id().with(("term-close", index)), Sense::click());

    let p = ui.painter();
    if selected {
        p.rect_filled(label_rect, 4.0, pal.selection);
        p.line_segment([pos2(label_rect.left() + 6.0, label_rect.bottom() - 1.0), pos2(label_rect.right() - 6.0, label_rect.bottom() - 1.0)], Stroke::new(2.0_f32, pal.accent));
    } else if tab.hovered() {
        p.rect_filled(label_rect, 4.0, pal.hover);
    }
    p.circle_filled(pos2(label_rect.left() + 10.0, label_rect.center().y), 3.5, status_color(&s.transport.state(), pal));
    let color = if selected { pal.text } else { pal.text_dim };
    p.text(pos2(label_rect.left() + 19.0, label_rect.center().y), Align2::LEFT_CENTER, &s.label, font, color);
    if close.hovered() {
        p.rect_filled(close_rect, 4.0, pal.hover);
    }
    let c = close_rect.center();
    let stroke = Stroke::new(1.2_f32, pal.text_dim);
    p.line_segment([pos2(c.x - 3.5, c.y - 3.5), pos2(c.x + 3.5, c.y + 3.5)], stroke);
    p.line_segment([pos2(c.x + 3.5, c.y - 3.5), pos2(c.x - 3.5, c.y + 3.5)], stroke);

    if close.clicked() {
        Some(Action::Close(index))
    } else if tab.clicked() {
        Some(Action::Select(index))
    } else {
        None
    }
}

fn status_color(state: &TransportState, pal: &Palette) -> Color32 {
    match state {
        TransportState::Starting => pal.text_dim,
        TransportState::Running => pal.ok,
        TransportState::Ended(_) => pal.warn,
    }
}

/// A toolbar button that is dimmed and ignores clicks while `blocked` gives the reason.
/// Returns true when it was clicked and is usable.
fn gated_button(ui: &mut egui::Ui, label: &str, pal: &Palette, blocked: Option<&str>) -> bool {
    let resp = ui
        .scope(|ui| {
            if blocked.is_some() {
                ui.set_opacity(DIMMED);
            }
            widgets::button(ui, label, pal, None)
        })
        .inner;
    let clicked = resp.clicked();
    if let Some(reason) = blocked {
        resp.on_hover_text(reason);
    }
    clicked && blocked.is_none()
}

/// Small button with a down arrow that opens the command and user menu.
fn drop_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(DROP_W, 30.0), Sense::click());
    let p = ui.painter();
    let bg = if resp.is_pointer_button_down_on() {
        pal.selection
    } else if resp.hovered() {
        pal.hover
    } else {
        pal.control
    };
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, pal.control_stroke));
    let c = rect.center();
    p.add(egui::Shape::convex_polygon(vec![pos2(c.x - 4.0, c.y - 2.0), pos2(c.x + 4.0, c.y - 2.0), pos2(c.x, c.y + 2.5)], pal.text, Stroke::NONE));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
