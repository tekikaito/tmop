//! Keeping TMOP in the notification area: hiding and showing the main window (in
//! `App::logic`, which eframe also runs while the window is hidden), the Settings section with
//! the shortcut capture and "Start with Windows". The pieces without a window are in
//! [`crate::tray`].
//!
//! While the window is hidden and no popout is open, TMOP draws nothing and asks for no
//! repaints; sampling slows down to every 5 s unless the flight recorder is writing a
//! recording ([`crate::tray::hidden_interval`]), so the flight recorder's ring and timeline go
//! on; the per-process network counters and GPU detail stop. Container alerts go out as
//! desktop notifications as they come, and the daily update check still runs. The tray icon,
//! the shortcut and a second start wake it.

use super::settings_page::{note, row_hint, toggle_row_hint};
use super::theme::Palette;
use super::widgets::{self, Button, ButtonSize};
use super::{App, Page};
use crate::settings;
use crate::tray::autostart::{self, RunStore};
use crate::tray::{self, Action, Capture, Event, Shortcut, Visibility};
use eframe::egui::{self, ViewportCommand};
use std::time::{Duration, Instant};

pub struct TrayUi {
    /// `None` where there is no tray: another OS, or the popout-only window.
    tray: Option<tray::Tray>,
    pub vis: Visibility,
    /// The main window, for cloaking it during a `--tray` start.
    hwnd: isize,
    cloaked: bool,
    /// Why the icon could not be added.
    icon_error: Option<String>,
    /// The shortcut Windows has given TMOP.
    registered: Option<Shortcut>,
    /// Why the shortcut is not registered, or what to press.
    shortcut_message: Option<String>,
    /// The shortcut field is waiting for keys.
    capturing: bool,
    was_minimized: bool,
    /// The window was just shown and is brought to the front in the next pass.
    focus_pending: bool,
    autostart: autostart::State,
    autostart_read_at: Instant,
    autostart_message: Option<String>,
    /// Another TMOP has the tray (see [`crate::tray::owns_tray`]): this one shows a note.
    other_copy: bool,
    /// A `--replacing` start whose predecessor still holds the shortcut tries again.
    shortcut_retry: Option<(Shortcut, tray::Retry)>,
    /// Sampling was paused for a while the window was hidden (see `Action::Show`).
    unsampled: bool,
}

/// This copy's part in tray mode, decided at start-up.
#[derive(Clone, Copy, Debug, Default)]
pub struct Role {
    /// The primary copy: the tray icon and the shortcut are this TMOP's (when switched on).
    pub owns_tray: bool,
    /// Another TMOP is the primary copy; the Settings section says so.
    pub other_copy: bool,
    /// The start takes the place of an exiting TMOP (`--replacing`).
    pub replacing: bool,
}

/// The real Run key; none off Windows.
fn run_store() -> Option<Box<dyn RunStore>> {
    #[cfg(windows)]
    return Some(Box::new(autostart::Registry));
    #[cfg(not(windows))]
    None
}

fn exe() -> std::path::PathBuf {
    crate::taskmgr::this_exe()
}

fn read_autostart() -> autostart::State {
    run_store().map(|s| autostart::state(s.as_ref(), &exe(), |p| std::fs::canonicalize(p).ok())).unwrap_or(autostart::State::Off)
}

impl TrayUi {
    /// Sets up the icon and the shortcut as `s` asks, when `role.owns_tray`. `start_hidden`:
    /// `--tray`.
    pub fn new(ctx: &egui::Context, hwnd: isize, s: &settings::Tray, role: Role, start_hidden: bool) -> Self {
        let mut ui = TrayUi {
            tray: None,
            vis: Visibility::new(false),
            hwnd,
            cloaked: false,
            icon_error: None,
            registered: None,
            shortcut_message: None,
            capturing: false,
            was_minimized: false,
            focus_pending: false,
            autostart: autostart::State::Off,
            autostart_read_at: Instant::now(),
            autostart_message: None,
            other_copy: role.other_copy,
            shortcut_retry: None,
            unsampled: false,
        };
        if !role.owns_tray {
            return ui;
        }
        let wake = ctx.clone();
        ui.tray = Some(tray::Tray::new(move || wake.request_repaint()));
        ui.autostart = read_autostart();
        if s.keep_running {
            ui.set_icon(true);
            if s.shortcut_enabled {
                let shortcut = Shortcut::parse(&s.shortcut);
                if !ui.register(shortcut) && role.replacing {
                    // The TMOP this one replaces may still hold it for a moment.
                    if let Some(s) = shortcut.filter(Shortcut::is_valid) {
                        ui.shortcut_message = None;
                        ui.shortcut_retry = Some((s, tray::Retry::new(Instant::now(), tray::CLAIM_RETRY)));
                    }
                }
            }
        }
        if start_hidden && ui.keeps_running(s) {
            tray::cloak(hwnd, true);
            ui.cloaked = true;
            ui.vis = Visibility::new(true);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        ui
    }

    /// Tray mode is on and the icon is there to bring the window back.
    pub fn keeps_running(&self, s: &settings::Tray) -> bool {
        s.keep_running && self.tray.as_ref().is_some_and(|t| t.has_icon())
    }

    fn set_icon(&mut self, on: bool) -> bool {
        let Some(t) = &mut self.tray else { return false };
        match t.set_icon(on) {
            Ok(()) => {
                self.icon_error = None;
                true
            }
            Err(e) => {
                self.icon_error = Some(format!("The notification area icon could not be added: {e}"));
                false
            }
        }
    }

    /// Registers `shortcut` (`None`: unregisters). Returns whether it is registered now.
    fn register(&mut self, shortcut: Option<Shortcut>) -> bool {
        let Some(t) = &mut self.tray else { return false };
        self.registered = None;
        let Some(s) = shortcut.filter(Shortcut::is_valid) else {
            let _ = t.set_shortcut(None);
            return false;
        };
        match t.set_shortcut(Some(&s)) {
            Ok(()) => {
                self.registered = Some(s);
                self.shortcut_message = None;
                true
            }
            Err(e) => {
                self.shortcut_message = Some(e);
                false
            }
        }
    }

    /// Stops a capture that is no longer on screen, giving the old shortcut back.
    pub fn end_capture(&mut self, s: &settings::Tray) {
        if self.capturing {
            self.capturing = false;
            self.shortcut_message = None;
            if s.keep_running && s.shortcut_enabled {
                self.register(Shortcut::parse(&s.shortcut));
            }
        }
    }

    /// The shortcut field is waiting for keys (TMOP's own shortcuts wait meanwhile).
    pub fn capturing(&self) -> bool {
        self.capturing
    }

    /// Removes the icon and the shortcut (TMOP is exiting).
    pub fn shut_down(&mut self) {
        self.register(None);
        self.set_icon(false);
        self.tray = None;
    }
}

impl App {
    /// Per pass, shown or hidden: the window's close and minimize, the tray icon and the
    /// shortcut, and launches handed over by later copies.
    pub fn window_logic(&mut self, ctx: &egui::Context) {
        if self.tray_ui.focus_pending && !self.tray_ui.vis.hidden {
            self.tray_ui.focus_pending = false;
            // When Windows refuses, the taskbar button flashes, as it should. There is no
            // fallback to `ViewportCommand::Focus`: winit forces the foreground with a
            // synthetic Alt key press.
            tray::bring_to_front(self.tray_ui.hwnd);
        }
        self.retry_shortcut(ctx);
        let mut events = self.tray_ui.tray.as_ref().map(|t| t.events()).unwrap_or_default();
        match self.replace.take_request().as_deref() {
            None | Some(tray::ALREADY_RUNNING) => {}
            Some(tray::EXIT_REQUEST) => events.push(Event::Exit),
            Some(page) => {
                if !page.is_empty() {
                    self.page = Page::from_id_on(page, &self.caps);
                }
                events.push(Event::SecondStart);
            }
        }
        if self.tray_ui.tray.is_some() {
            let (close, minimized) = ctx.input(|i| (i.viewport().close_requested(), i.viewport().minimized == Some(true)));
            if close {
                events.push(Event::CloseRequested);
            }
            if minimized && !self.tray_ui.was_minimized {
                events.push(Event::Minimized);
            }
            self.tray_ui.was_minimized = minimized;
        }
        let keep = self.tray_ui.keeps_running(&self.settings.tray);
        for e in events {
            let action = if self.tray_ui.tray.is_some() {
                self.tray_ui.vis.on(e, keep)
            } else {
                match e {
                    Event::SecondStart => Action::Show,
                    Event::Exit => Action::Exit,
                    _ => Action::Nothing,
                }
            };
            self.apply_window_action(ctx, action);
        }
        if self.tray_ui.vis.hidden && !self.popout_open {
            let interval = tray::hidden_interval(self.settings.update_speed().interval(), self.recorder.is_recording());
            self.tray_ui.unsampled |= interval.is_none();
            self.set_sample_interval(interval);
            // Container alerts go out as they come, not as a burst of toasts at the next Show.
            self.docker.notify_while_hidden();
        }
    }

    /// A `--replacing` start: registers the shortcut again until the TMOP it replaces has let
    /// go of it, and reports "taken" only after the last try.
    fn retry_shortcut(&mut self, ctx: &egui::Context) {
        let Some((shortcut, mut retry)) = self.tray_ui.shortcut_retry.take() else { return };
        let s = &self.settings.tray;
        // The user changed something in the meantime: that wins.
        if self.tray_ui.capturing || self.tray_ui.registered.is_some() || !s.keep_running || !s.shortcut_enabled || Shortcut::parse(&s.shortcut) != Some(shortcut) {
            return;
        }
        let now = Instant::now();
        match retry.attempt(now) {
            tray::Attempt::Wait => {
                ctx.request_repaint_after(retry.wait(now));
                self.tray_ui.shortcut_retry = Some((shortcut, retry));
            }
            tray::Attempt::Try => {
                if !self.tray_ui.register(Some(shortcut)) {
                    self.tray_ui.shortcut_message = None;
                    ctx.request_repaint_after(retry.wait(now));
                    self.tray_ui.shortcut_retry = Some((shortcut, retry));
                }
            }
            tray::Attempt::Last => {
                self.tray_ui.register(Some(shortcut));
            }
        }
    }

    /// Ends TMOP (File → Exit, an update, a restart as administrator), also in tray mode, where
    /// a plain close only hides the window.
    pub fn quit(&mut self, ctx: &egui::Context) {
        let keep = self.tray_ui.keeps_running(&self.settings.tray);
        let action = self.tray_ui.vis.on(Event::Exit, keep);
        self.apply_window_action(ctx, action);
    }

    fn apply_window_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Nothing | Action::Close => {}
            Action::HideInsteadOfClose => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.hide_window(ctx);
            }
            Action::Hide => self.hide_window(ctx),
            Action::Show => {
                if std::mem::take(&mut self.tray_ui.cloaked) {
                    tray::cloak(self.tray_ui.hwnd, false);
                }
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                // The window is shown at the end of this pass; it comes to the front in the next.
                self.tray_ui.focus_pending = true;
                if std::mem::take(&mut self.tray_ui.unsampled) {
                    // Paused while hidden: the next sample is compared with one from before, and
                    // would list every process start and exit in between at the time of this Show.
                    self.recorder.resync_processes("TMOP was hidden and paused; process starts and exits in that time are not listed");
                }
                self.set_sample_interval(self.settings.update_speed().interval());
                self.sampler.refresh();
                ctx.request_repaint();
            }
            Action::Exit => {
                ctx.send_viewport_cmd(ViewportCommand::Close);
                ctx.request_repaint();
            }
        }
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.tray_ui.end_capture(&self.settings.tray);
        // Nothing shows them now; the next shown frame turns them back on where needed.
        crate::process::netio::set_wanted(false);
        crate::metrics::set_gpu_detail(false);
    }

    /// The Settings section.
    pub fn tray_section(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if self.tray_ui.other_copy {
            let text = if self.settings.tray.keep_running {
                "Another TMOP is running in the notification area; the icon and the shortcut are its. This window is a copy of its own and ends when you close it. Change these settings in that TMOP."
            } else {
                "Another TMOP window is open; these settings are changed there."
            };
            note(ui, pal, text);
            ui.add_space(8.0);
            return;
        }
        if self.tray_ui.autostart_read_at.elapsed() > Duration::from_secs(2) {
            self.tray_ui.autostart = read_autostart();
            self.tray_ui.autostart_read_at = Instant::now();
        }
        let mut on = self.tray_ui.keeps_running(&self.settings.tray);
        let explain = "Closing or minimizing the window hides it in the notification area, next to the clock, instead of ending TMOP. Click the icon to show the window again; right-click it for Exit.";
        if toggle_row_hint(ui, pal, &mut on, "Keep TMOP running in the notification area", explain) {
            self.set_keep_running(on);
        }
        ui.spacing_mut().item_spacing.y = 4.0;
        if let Some(e) = &self.tray_ui.icon_error {
            ui.add_space(4.0);
            note(ui, pal, e);
        }
        if on {
            self.shortcut_row(ui, pal);
        }
        if on || self.tray_ui.autostart.has_entry() {
            self.autostart_row(ui, pal, on);
        }
        ui.add_space(8.0);
    }

    fn set_keep_running(&mut self, on: bool) {
        if on {
            if !self.tray_ui.set_icon(true) {
                return;
            }
            self.settings.tray.keep_running = true;
            if self.settings.tray.shortcut_enabled {
                self.tray_ui.register(Shortcut::parse(&self.settings.tray.shortcut));
            }
        } else {
            self.settings.tray.keep_running = false;
            self.tray_ui.capturing = false;
            self.tray_ui.register(None);
            self.tray_ui.set_icon(false);
            // Starting hidden at sign-in makes no sense without the icon.
            if self.tray_ui.autostart.has_entry() {
                self.set_autostart(false);
            }
        }
        self.settings_dirty = true;
    }

    fn shortcut_row(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        if self.tray_ui.capturing {
            self.capture_keys(&ctx);
        }
        let stored = Shortcut::parse(&self.settings.tray.shortcut);
        let mut enabled = self.tray_ui.registered.is_some();
        let (mut toggled, mut field) = (false, false);
        let explain = "Brings TMOP to the front from any program. Click the field, then hold Ctrl, Alt or the Windows key and press a key.";
        row_hint(ui, pal, "Shortcut to open TMOP", explain, |ui| {
            toggled = ui.add_enabled_ui(stored.is_some() && !self.tray_ui.capturing, |ui| widgets::toggle(ui, &mut enabled, pal).changed()).inner;
            let text = if self.tray_ui.capturing {
                "Press the keys\u{2026}".to_string()
            } else {
                stored.map(|s| s.label()).unwrap_or_else(|| "Choose a shortcut".into())
            };
            let button = if self.tray_ui.capturing { Button::primary(&text) } else { Button::new(&text) };
            field = button.size(ButtonSize::Medium).show(ui, pal).clicked();
        });
        if field {
            if self.tray_ui.capturing {
                self.tray_ui.end_capture(&self.settings.tray);
            } else {
                // The current shortcut is let go, so pressing it again can be captured.
                self.tray_ui.register(None);
                self.tray_ui.capturing = true;
                self.tray_ui.shortcut_message = None;
            }
        }
        if toggled {
            self.settings.tray.shortcut_enabled = enabled && self.tray_ui.register(stored);
            if !enabled {
                self.tray_ui.register(None);
                self.tray_ui.shortcut_message = None;
            }
            self.settings_dirty = true;
        }
        if let Some(m) = &self.tray_ui.shortcut_message {
            note(ui, pal, m);
        }
    }

    /// Reads this frame's key presses into the shortcut field.
    fn capture_keys(&mut self, ctx: &egui::Context) {
        let press = ctx.input(|i| i.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
            _ => None,
        }));
        let Some((key, mods)) = press else { return };
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))));
        match Shortcut::capture(key, mods, tray::win_key_down()) {
            Capture::Done(s) => {
                self.tray_ui.capturing = false;
                self.settings.tray.shortcut = s.to_setting();
                // Choosing a combination means using it, if Windows lets TMOP have it.
                self.settings.tray.shortcut_enabled = self.tray_ui.register(Some(s));
                self.settings_dirty = true;
            }
            Capture::Cancel => self.tray_ui.end_capture(&self.settings.tray),
            Capture::NeedsModifier => self.tray_ui.shortcut_message = Some(format!("{} alone is not a shortcut: hold Ctrl, Alt or the Windows key with it.", key.name())),
            Capture::Unsupported => self.tray_ui.shortcut_message = Some(format!("{} cannot be used. Use a letter, a digit, a function key or a key such as Home or Page Up.", key.name())),
        }
    }

    fn autostart_row(&mut self, ui: &mut egui::Ui, pal: &Palette, keep_running: bool) {
        let state = self.tray_ui.autostart.clone();
        let mut on = state.has_entry();
        let explain = "Starts TMOP hidden in the notification area when you sign in to Windows, for your user only.";
        let changed = ui.add_enabled_ui(keep_running || on, |ui| toggle_row_hint(ui, pal, &mut on, "Start with Windows", explain)).inner;
        if changed {
            self.set_autostart(on);
        }
        if let Some(text) = autostart::describe(&self.tray_ui.autostart) {
            note(ui, pal, &text);
            if matches!(self.tray_ui.autostart, autostart::State::Stale { .. }) && keep_running {
                ui.add_space(4.0);
                if Button::new("Start this copy instead").size(ButtonSize::Medium).show(ui, pal).clicked() {
                    self.set_autostart(true);
                }
            }
        }
        if let Some(m) = &self.tray_ui.autostart_message {
            note(ui, pal, m);
        }
    }

    fn set_autostart(&mut self, on: bool) {
        let Some(mut store) = run_store() else { return };
        let result = autostart::set(store.as_mut(), on, &exe(), |p| std::fs::canonicalize(p).ok());
        self.tray_ui.autostart_message = result.as_ref().err().cloned();
        self.tray_ui.autostart = read_autostart();
        self.tray_ui.autostart_read_at = Instant::now();
        self.status = match result {
            Ok(_) if on => "TMOP starts with Windows, hidden in the notification area".into(),
            Ok(_) => "TMOP no longer starts with Windows".into(),
            Err(e) => format!("Start with Windows not changed: {e}"),
        };
    }
}
