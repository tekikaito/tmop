//! "Run new task": a small dialog that starts a program through [`crate::launch`]. Nothing runs
//! until the user presses OK (or Enter).

use super::pro_pages::mono_input;
use super::processes::key_once;
use super::theme::{self, Palette};
use super::widgets::{Button, ButtonKind, ButtonSize, Icon};
use super::{kit, App};
use crate::launch::{self, Outcome};
use crate::platform::{self, Os};
use eframe::egui;
use kit::DialogButton;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// The dialog, closed or open.
pub struct RunTaskUi {
    open: bool,
    text: String,
    admin: bool,
    /// Why the last try failed, in words.
    error: Option<String>,
    /// TMOP runs elevated: a task inherits it unless the OS is asked for something else.
    elevated: bool,
    focus: bool,
    /// The program is being started (an administrator prompt may be open).
    job: Option<Receiver<Result<Outcome, String>>>,
    browse: Option<Receiver<Option<std::path::PathBuf>>>,
    /// HWND of the main window, the owner of the file dialog (see `main_hwnd`).
    owner: isize,
}

impl RunTaskUi {
    pub fn new(owner: isize) -> Self {
        Self { open: false, text: String::new(), admin: false, error: None, elevated: false, focus: false, job: None, browse: None, owner }
    }

    pub fn open(&mut self) {
        if !self.open {
            *self = Self { open: true, focus: true, elevated: platform::is_elevated(), ..Self::new(self.owner) };
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.job = None;
        self.browse = None;
    }
}

/// `path` as one word of a command line: quoted when it holds a space (inside double quotes a
/// backslash and a quote are escaped on Unix; on Windows the path goes in as it is).
fn quote_path(path: &str, os: Os) -> String {
    if !path.chars().any(char::is_whitespace) {
        return path.to_string();
    }
    match os {
        Os::Windows => format!("\"{path}\""),
        _ => format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\"")),
    }
}

/// The line to show under the field, or `None`: a task inherits the elevation of TMOP, so say
/// so when TMOP is elevated and the checkbox is not about to ask for it anyway.
fn elevation_note(elevated: bool, admin_checked: bool) -> Option<String> {
    (elevated && !admin_checked).then(|| format!("TMOP runs as {}, so the new task will too.", platform::elevation_word()))
}

impl App {
    /// Opens the dialog (the button of the Processes page).
    pub(super) fn open_run_task(&mut self) {
        self.run_task.open();
    }

    pub(super) fn run_task_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        if !self.run_task.open {
            return;
        }
        let caps = self.caps;
        let ui_state = &mut self.run_task;
        if let Some(rx) = &ui_state.job {
            match rx.try_recv() {
                Ok(Ok(Outcome::Started(_))) => {
                    let what = ui_state.text.trim().to_string();
                    ui_state.close();
                    self.status = format!("Started {what}");
                    return;
                }
                // The permission prompt was declined: not an error, the dialog stays as it was.
                Ok(Ok(Outcome::Cancelled)) => ui_state.job = None,
                Ok(Err(e)) => {
                    ui_state.error = Some(e);
                    ui_state.job = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    ui_state.error = Some("The program could not be started.".into());
                    ui_state.job = None;
                }
            }
        }
        if let Some(rx) = &ui_state.browse {
            match rx.try_recv() {
                Ok(path) => {
                    if let Some(p) = path {
                        ui_state.text = quote_path(&p.to_string_lossy(), platform::OS);
                        ui_state.error = None;
                    }
                    ui_state.browse = None;
                    ui_state.focus = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => ui_state.browse = None,
            }
        }
        let st = theme::status(pal);
        let busy = ui_state.job.is_some();
        let lead = match platform::OS {
            Os::Windows => "Type the name of a program, folder, document or Internet address, and Windows opens it for you.",
            _ => "Type a program and its arguments. It starts without a shell: quotes group words, but nothing is expanded.",
        };
        let mut answer = None;
        let mut browse = false;
        kit::dialog(ctx, "run-task", |ui| {
            kit::dialog_header(ui, pal, Some((Icon::Play, pal.accent)), "Run new task", lead);
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let browse_w = 96.0;
                let field = ui.add_enabled_ui(!busy, |ui| mono_input(ui, &mut ui_state.text, "Program or file", (ui.available_width() - browse_w - 8.0).max(120.0), pal)).inner;
                if ui_state.focus {
                    field.request_focus();
                    ui_state.focus = false;
                }
                if field.changed() {
                    ui_state.error = None;
                }
                // Reused from the palette import and export: the file picker of the platform.
                ui.add_enabled_ui(!busy && ui_state.browse.is_none(), |ui| {
                    browse = Button::new("Browse\u{2026}").kind(ButtonKind::Secondary).size(ButtonSize::Medium).show(ui, pal).clicked();
                });
            });
            if caps.run_task_admin {
                ui.add_space(10.0);
                ui.add_enabled(!busy, egui::Checkbox::new(&mut ui_state.admin, theme::button_text("Run as administrator")));
            }
            if let Some(note) = elevation_note(ui_state.elevated, caps.run_task_admin && ui_state.admin) {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(note).font(theme::ui(12.0)).color(st.warn));
            }
            if let Some(e) = &ui_state.error {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(e).font(theme::ui(13.0)).color(st.danger));
            }
            ui.add_space(10.0);
            if busy {
                ui.label(egui::RichText::new("Starting\u{2026}").font(theme::ui(13.0)).color(pal.text_faint));
            } else {
                answer = kit::dialog_buttons(ui, pal, &[DialogButton::Secondary("Cancel"), DialogButton::Primary("OK")]).map(|i| i == 1);
            }
        });
        if !busy {
            if key_once(ctx, egui::Key::Escape) {
                answer = Some(false);
            } else if answer.is_none() && key_once(ctx, egui::Key::Enter) {
                answer = Some(true);
            }
        }
        if browse {
            let d = kit::FileDialog { save: false, title: "Browse".into(), filter: ("All files".into(), "*".into()), file_name: String::new(), owner: ui_state.owner };
            ui_state.browse = Some(kit::file_dialog(d));
        }
        match answer {
            Some(false) => ui_state.close(),
            Some(true) => {
                let line = ui_state.text.trim().to_string();
                if line.is_empty() {
                    ui_state.error = Some("Type the program, file or address to open.".into());
                    ui_state.focus = true;
                } else {
                    let admin = caps.run_task_admin && ui_state.admin;
                    let (tx, rx) = mpsc::channel();
                    let repaint = ctx.clone();
                    let spawned = std::thread::Builder::new().name("tmop-run-task".into()).spawn(move || {
                        let _ = tx.send(launch::run(&line, admin));
                        repaint.request_repaint();
                    });
                    match spawned {
                        Ok(_) => {
                            ui_state.error = None;
                            ui_state.job = Some(rx);
                        }
                        Err(_) => ui_state.error = Some("The program could not be started.".into()),
                    }
                }
            }
            None => {}
        }
        if ui_state.job.is_some() || ui_state.browse.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_are_quoted_for_the_platform() {
        assert_eq!(quote_path(r"C:\Windows\notepad.exe", Os::Windows), r"C:\Windows\notepad.exe");
        assert_eq!(quote_path(r"C:\Program Files\a.exe", Os::Windows), r#""C:\Program Files\a.exe""#);
        assert_eq!(quote_path("/opt/my app/run", Os::Linux), "\"/opt/my app/run\"");
        assert_eq!(quote_path("/opt/a \"b\"/run", Os::MacOs), "\"/opt/a \\\"b\\\"/run\"");
    }

    #[test]
    fn the_elevation_note_shows_only_when_elevation_would_be_inherited() {
        assert_eq!(elevation_note(false, false), None);
        assert_eq!(elevation_note(true, true), None);
        assert!(elevation_note(true, false).unwrap().contains("will too"));
    }

    #[test]
    fn opening_resets_the_dialog_but_keeps_the_owner() {
        let mut d = RunTaskUi::new(7);
        d.open();
        d.text = "x".into();
        d.error = Some("e".into());
        d.close();
        d.open();
        assert!(d.open && d.text.is_empty() && d.error.is_none() && d.owner == 7);
    }
}
