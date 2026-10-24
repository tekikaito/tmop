//! Opening TMOP in place of Windows Task Manager: the Settings section, the warning banner for a
//! registration that points at a deleted copy, the confirmation dialog, and Task Manager
//! launches handed over by later copies. The registry work is in [`crate::taskmgr`].

use super::theme::{self, Palette};
use super::widgets;
use super::{App, Page};
use crate::pro::Loader;
use crate::taskmgr::{self, State};
use eframe::egui::{self, Align2, RichText, ViewportCommand};
use std::time::{Duration, Instant};

pub struct ReplaceUi {
    /// Last state read from the registry.
    pub state: State,
    read_at: Instant,
    /// An elevated change in progress; its result is the status message.
    job: Loader<String>,
    /// Text of the "turn on" confirmation while it is open.
    confirm: Option<String>,
    banner_dismissed: bool,
    /// Task Manager launches handed over by later copies.
    inbox: Option<taskmgr::Inbox>,
}

impl ReplaceUi {
    /// Reads the state; with `listen`, also takes over later Task Manager launches.
    pub fn new(ctx: &egui::Context, listen: bool) -> Self {
        let inbox = if listen {
            let ctx = ctx.clone();
            taskmgr::listen(move || ctx.request_repaint())
        } else {
            None
        };
        Self { state: taskmgr::state(), read_at: Instant::now(), job: Loader::default(), confirm: None, banner_dismissed: false, inbox }
    }

    fn refresh(&mut self) {
        self.state = taskmgr::state();
        self.read_at = Instant::now();
    }

    fn start(&mut self, on: bool) {
        self.job.start(move || {
            taskmgr::set(on)
                .map(|()| if on { "TMOP now opens in place of Windows Task Manager".to_string() } else { "Windows Task Manager is restored".to_string() })
                .map_err(|e| format!("Task Manager setting not changed: {e}"))
        });
    }
}

impl App {
    /// Per-frame work: finished changes and handed-over Task Manager launches.
    pub fn replace_poll(&mut self, ctx: &egui::Context) {
        if self.replace.job.poll() {
            self.status = match self.replace.job.get() {
                Some(Ok(m)) | Some(Err(m)) => m.clone(),
                None => String::new(),
            };
            self.replace.refresh();
        }
        if self.replace.job.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        let request = self.replace.inbox.as_ref().and_then(|i| i.take());
        if let Some(page) = request {
            if !page.is_empty() {
                self.page = Page::from_id(&page);
            }
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    /// "Open Windows Task Manager" for the overflow menu, while a TMOP copy replaces it. True
    /// when it was clicked.
    pub fn replace_menu_item(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.replace.state.is_tmop() {
            return false;
        }
        let busy = self.replace.job.is_loading();
        let clicked = ui.add_enabled_ui(!busy, |ui| super::kit::menu_item(ui, "Open Windows Task Manager", None, false)).inner;
        if clicked {
            // Off the UI thread: Task Manager may need an elevated start, which waits for UAC.
            self.replace.job.start(|| taskmgr::open_real().map(|()| String::new()));
        }
        ui.separator();
        clicked
    }

    /// Banner shown when Task Manager launches point at a TMOP.exe that no longer exists.
    pub fn replace_banner(&mut self, ctx: &egui::Context, pal: &Palette) {
        let State::OtherTmop { path, exists: false } = &self.replace.state else { return };
        if self.replace.banner_dismissed {
            return;
        }
        let text = format!("Ctrl+Shift+Esc is set to open TMOP at {}, which no longer exists, so no task manager opens.", path.display());
        let frame = egui::Frame::none().fill(theme::with_alpha(pal.warn, 0.18)).inner_margin(egui::Margin::symmetric(12.0, 6.0));
        let busy = self.replace.job.is_loading();
        let (mut repoint, mut restore, mut dismiss) = (false, false, false);
        egui::TopBottomPanel::top("taskmgr-banner").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).font(theme::ui(12.5)).color(pal.text));
                ui.add_enabled_ui(!busy, |ui| {
                    repoint = widgets::button(ui, "Use this copy", pal, None).clicked();
                    restore = widgets::button(ui, "Restore Windows Task Manager", pal, None).clicked();
                });
                dismiss = widgets::button(ui, "Dismiss", pal, None).clicked();
            });
        });
        if repoint || restore {
            self.replace.start(repoint);
        }
        if dismiss {
            self.replace.banner_dismissed = true;
        }
    }

    /// The Settings section.
    pub fn replace_section(&mut self, ui: &mut egui::Ui, pal: &Palette, toggle_row: impl FnOnce(&mut egui::Ui, &mut bool) -> bool, hint: impl Fn(&mut egui::Ui, &str)) {
        if self.replace.read_at.elapsed() > Duration::from_secs(2) && !self.replace.job.is_loading() {
            self.replace.refresh();
        }
        let busy = self.replace.job.is_loading();
        let mut on = self.replace.state == State::Ours;
        let changed = ui.add_enabled_ui(!busy, |ui| toggle_row(ui, &mut on)).inner;
        if changed {
            if on {
                self.replace.confirm = Some(self.confirm_text());
            } else {
                self.replace.start(false);
            }
        }
        if busy {
            hint(ui, "Waiting for administrator permission\u{2026}");
        }
        match &self.replace.state {
            State::Default | State::Ours => {}
            State::OtherTmop { path, exists: true } => hint(ui, &format!("Task Manager currently opens another copy of TMOP: {}", path.display())),
            State::OtherTmop { path, exists: false } => hint(ui, &format!("Task Manager is set to open {}, which no longer exists.", path.display())),
            State::Foreign(v) => hint(ui, &format!("Another program replaces Task Manager: {v}")),
        }
        if matches!(self.replace.state, State::OtherTmop { .. }) {
            let mut restore = false;
            ui.add_enabled_ui(!busy, |ui| restore = widgets::button(ui, "Restore Windows Task Manager", pal, None).clicked());
            if restore {
                self.replace.start(false);
            }
        }
        hint(ui, "Ctrl+Shift+Esc, the taskbar menu and taskmgr.exe open TMOP for every user on this PC. To undo it without TMOP, see the README or run TMOP.exe --restore-taskmgr.");
    }

    fn confirm_text(&self) -> String {
        let exe = taskmgr::this_exe().display().to_string();
        let mut t = String::from(
            "Ctrl+Shift+Esc, the taskbar menu and taskmgr.exe will open TMOP instead of Windows Task Manager, for every user on this PC. Windows will ask for administrator permission.",
        );
        t.push_str(&format!("\n\nThis copy is registered: {exe}\nBefore moving or deleting it, turn this off again (or run TMOP.exe --restore-taskmgr)."));
        if let Some(w) = taskmgr::location_warning() {
            t.push_str(&format!("\n\n{w}"));
        }
        match &self.replace.state {
            State::Foreign(v) => t.push_str(&format!("\n\nThis replaces the program that currently opens instead of Task Manager: {v}")),
            State::OtherTmop { path, .. } => t.push_str(&format!("\n\nThis replaces the other TMOP copy at {}.", path.display())),
            _ => {}
        }
        t
    }

    pub fn replace_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(text) = self.replace.confirm.clone() else { return };
        let mut answer = None;
        egui::Window::new("Replace Task Manager")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).fill(pal.panel))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Continue", pal, None).clicked() {
                        answer = Some(true);
                    }
                    if widgets::button(ui, "Cancel", pal, None).clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(yes) = answer {
            self.replace.confirm = None;
            if yes {
                self.replace.start(true);
            }
        }
    }
}
