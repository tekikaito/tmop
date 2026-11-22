//! "Find what's using a file": a dialog that lists the programs holding a file open (see
//! [`crate::pro::inuse`]), each with a button to select it on the Processes page and one to end it.
//! The search runs on a worker thread; nothing is changed by it.

use super::pro_pages::mono_input;
use super::processes::key_once;
use super::theme::{self, Palette};
use super::widgets::{Button, ButtonKind, ButtonSize, Icon};
use super::{kit, App, Page};
use crate::platform::{self, Os};
use crate::pro::inuse::{self, Holder};
use crate::process::{Action, ProcessRow};
use eframe::egui;
use kit::DialogButton;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// What a finished search found.
struct Found {
    path: PathBuf,
    result: Result<Vec<Holder>, String>,
    /// The (pid, create_time) of the process each holder was matched to when the result arrived
    /// (`None`: no matching process). Filled on the UI thread, see [`resolve`].
    ids: Vec<Option<(u32, i64)>>,
}

/// The dialog, closed or open.
pub struct FileInUseUi {
    open: bool,
    text: String,
    /// Why the path cannot be checked, in words.
    error: Option<String>,
    found: Option<Found>,
    focus: bool,
    job: Option<Receiver<Found>>,
    browse: Option<Receiver<Option<PathBuf>>>,
    /// HWND of the main window, the owner of the file dialog (see `main_hwnd`).
    owner: isize,
}

impl FileInUseUi {
    pub fn new(owner: isize) -> Self {
        Self { open: false, text: String::new(), error: None, found: None, focus: false, job: None, browse: None, owner }
    }

    pub fn open(&mut self) {
        if !self.open {
            *self = Self { open: true, focus: true, ..Self::new(self.owner) };
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.job = None;
        self.browse = None;
    }
}

/// A holder next to its row in the Processes table, if the process still runs.
#[derive(Debug, PartialEq)]
struct HolderRow<'a> {
    holder: &'a Holder,
    process: Option<&'a ProcessRow>,
}

/// A name as the table could show it: lower case, without ".exe".
fn plain_name(s: &str) -> String {
    let s = s.trim().to_lowercase();
    s.strip_suffix(".exe").map_or(s.clone(), str::to_string)
}

/// Whether `row` is the process `h` describes. The PID must match, and then:
/// - when the backend reported a start time (Windows, a FILETIME), it must equal the row's
///   `create_time` (the same kernel timestamp, same unit); the name is not compared there, since
///   Restart Manager reports a friendly application name, not the image name;
/// - otherwise (Linux and macOS, whose backends give the process name but no start time, and
///   whose table units differ per system) the names must be related: equal ignoring case and
///   ".exe", or one the start of the other (`comm` is cut at 15 characters).
fn same_process(h: &Holder, row: &ProcessRow) -> bool {
    if h.pid != row.pid {
        return false;
    }
    if let Some(start) = h.start {
        return u64::try_from(row.create_time).is_ok_and(|t| t == start);
    }
    let (a, b) = (plain_name(&h.name), plain_name(&row.name));
    !a.is_empty() && !b.is_empty() && (a == b || a.starts_with(&b) || b.starts_with(&a))
}

/// The identity (pid, create_time) of the process each holder is right now, once, when the
/// result arrives.
fn resolve(holders: &[Holder], procs: &[ProcessRow]) -> Vec<Option<(u32, i64)>> {
    holders.iter().map(|h| procs.iter().find(|p| same_process(h, p)).map(|p| (p.pid, p.create_time))).collect()
}

/// Pairs each holder with the process of its resolved identity, as long as a process with
/// exactly that (pid, create_time) exists; a holder that has ended (or whose PID was reused)
/// has none and its buttons stay off.
fn holder_rows<'a>(holders: &'a [Holder], ids: &[Option<(u32, i64)>], procs: &'a [ProcessRow]) -> Vec<HolderRow<'a>> {
    holders
        .iter()
        .enumerate()
        .map(|(i, holder)| {
            let process = ids.get(i).copied().flatten().and_then(|(pid, t)| procs.iter().find(|p| p.pid == pid && p.create_time == t));
            HolderRow { holder, process }
        })
        .collect()
}

/// The faint line under the results: without root, Linux and macOS see only the user's own
/// processes. Windows asks the system, which sees every process.
fn scope_note(os: Os, elevated: bool) -> Option<&'static str> {
    (os != Os::Windows && !elevated).then_some("Only your own processes are checked.")
}

/// The sentence for the result of a search.
fn summary(path: &std::path::Path, result: &Result<Vec<Holder>, String>) -> String {
    let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    match result {
        Err(e) => e.clone(),
        Ok(h) if h.is_empty() => "No program has this file open.".into(),
        Ok(h) if h.len() == 1 => format!("1 program has {name} open:"),
        Ok(h) => format!("{} programs have {name} open:", h.len()),
    }
}

impl App {
    /// Opens the dialog (the overflow menu and the Processes toolbar).
    pub(super) fn open_file_in_use(&mut self) {
        self.file_in_use.open();
    }

    pub(super) fn file_in_use_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        if !self.file_in_use.open {
            return;
        }
        let procs = self.snapshot().2;
        let elevated = platform::is_elevated();
        let ui_state = &mut self.file_in_use;
        if let Some(rx) = &ui_state.job {
            match rx.try_recv() {
                Ok(mut f) => {
                    if let Ok(h) = &f.result {
                        f.ids = resolve(h, &procs);
                    }
                    ui_state.found = Some(f);
                    ui_state.job = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    ui_state.error = Some("The check could not run.".into());
                    ui_state.job = None;
                }
            }
        }
        // A file dropped on the window while the dialog is open.
        let mut search = false;
        if ui_state.job.is_none() {
            if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
                ui_state.text = p.to_string_lossy().into_owned();
                search = true;
            }
        }
        if let Some(rx) = &ui_state.browse {
            match rx.try_recv() {
                Ok(path) => {
                    if let Some(p) = path {
                        ui_state.text = p.to_string_lossy().into_owned();
                        search = true;
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
        let mut answer = None;
        let mut browse = false;
        // (index in the list, end the task rather than show it)
        let mut act: Option<(usize, bool)> = None;
        kit::dialog(ctx, "file-in-use", |ui| {
            kit::dialog_header(ui, pal, Some((Icon::Search, pal.accent)), "Find what's using a file", "Pick a file to see which programs have it open, for when a file cannot be moved, renamed or deleted.");
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let browse_w = 96.0;
                let field = ui.add_enabled_ui(!busy, |ui| mono_input(ui, &mut ui_state.text, "Path of a file", (ui.available_width() - browse_w - 8.0).max(120.0), pal)).inner;
                if ui_state.focus {
                    field.request_focus();
                    ui_state.focus = false;
                }
                if field.changed() {
                    ui_state.error = None;
                }
                ui.add_enabled_ui(!busy && ui_state.browse.is_none(), |ui| {
                    browse = Button::new("Browse\u{2026}").kind(ButtonKind::Secondary).size(ButtonSize::Medium).show(ui, pal).clicked();
                });
            });
            if let Some(e) = &ui_state.error {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(e).font(theme::ui(13.0)).color(st.danger));
            }
            if busy {
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Searching\u{2026}").font(theme::ui(13.0)).color(pal.text_faint));
            } else if let Some(f) = &ui_state.found {
                ui.add_space(12.0);
                let color = if f.result.is_err() { st.danger } else { pal.text };
                ui.label(egui::RichText::new(summary(&f.path, &f.result)).font(theme::semibold(13.0)).color(color));
                if let Ok(holders) = &f.result {
                    egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (i, r) in holder_rows(holders, &f.ids, &procs).iter().enumerate() {
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui.label(egui::RichText::new(&r.holder.name).font(theme::semibold(13.0)).color(pal.text));
                                ui.label(egui::RichText::new(format!("PID {}", r.holder.pid)).font(theme::mono(12.0)).color(pal.text_faint));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add_enabled_ui(r.process.is_some(), |ui| {
                                        if Button::new("End task").kind(ButtonKind::Danger).size(ButtonSize::Compact).show(ui, pal).clicked() {
                                            act = Some((i, true));
                                        }
                                        if Button::new("Show in Processes").kind(ButtonKind::Secondary).size(ButtonSize::Compact).show(ui, pal).clicked() {
                                            act = Some((i, false));
                                        }
                                    });
                                    if r.process.is_none() {
                                        ui.label(egui::RichText::new("ended").font(theme::ui(12.0)).color(pal.text_faint));
                                    }
                                });
                            });
                        }
                    });
                    if let Some(note) = scope_note(platform::OS, elevated) {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(note).font(theme::ui(12.0)).color(pal.text_faint));
                    }
                }
            }
            ui.add_space(10.0);
            if !busy {
                answer = kit::dialog_buttons(ui, pal, &[DialogButton::Secondary("Close"), DialogButton::Primary("Search")]).map(|i| i == 1);
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
            Some(true) => search = true,
            None => {}
        }
        if search && ui_state.job.is_none() && ui_state.open {
            match inuse::check_path(&ui_state.text) {
                Err(p) => {
                    ui_state.error = Some(p.message().into());
                    ui_state.found = None;
                    ui_state.focus = true;
                }
                Ok(path) => {
                    ui_state.error = None;
                    ui_state.found = None;
                    let (tx, rx) = mpsc::channel();
                    let repaint = ctx.clone();
                    let spawned = std::thread::Builder::new().name("tmop-file-in-use".into()).spawn(move || {
                        let result = inuse::find(&path);
                        let _ = tx.send(Found { path, result, ids: Vec::new() });
                        repaint.request_repaint();
                    });
                    match spawned {
                        Ok(_) => ui_state.job = Some(rx),
                        Err(_) => ui_state.error = Some("The check could not start.".into()),
                    }
                }
            }
        }
        if ui_state.job.is_some() || ui_state.browse.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some((i, end)) = act {
            // Looked up in the table of this frame by the identity fixed when the result arrived,
            // so a reused PID is never acted on. The Windows backend does not re-check the start
            // time itself, which is why this check matters.
            let row = ui_state.found.as_ref().and_then(|f| f.result.as_ref().ok().and_then(|h| holder_rows(h, &f.ids, &procs).get(i).and_then(|r| r.process.cloned())));
            match row {
                Some(row) if end => self.request(Action::EndTask, row),
                Some(row) => {
                    self.selected = Some((row.pid, row.create_time));
                    self.page = Page::Processes;
                    self.file_in_use.close();
                }
                None => self.status = "That process has already ended.".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(pid: u32, name: &str) -> Holder {
        Holder { pid, name: name.into(), start: None }
    }

    fn named(pid: u32, name: &str, create_time: i64) -> ProcessRow {
        ProcessRow { pid, name: name.into(), create_time, ..Default::default() }
    }

    #[test]
    fn holders_meet_their_running_processes() {
        let holders = [holder(10, "a"), holder(20, "gone")];
        let procs = [named(10, "a", 5), named(30, "c", 5)];
        let ids = resolve(&holders, &procs);
        assert_eq!(ids, [Some((10, 5)), None]);
        let rows = holder_rows(&holders, &ids, &procs);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].process.map(|p| p.pid), Some(10));
        assert!(rows[1].process.is_none(), "a vanished process keeps its row without a target");
        assert!(holder_rows(&[], &[], &procs).is_empty());
    }

    #[test]
    fn a_reused_pid_with_another_name_is_not_the_holder() {
        let holders = [holder(10, "backup-tool")];
        let before = [named(10, "backup-tool", 5)];
        let ids = resolve(&holders, &before);
        assert_eq!(ids, [Some((10, 5))]);
        // The holder exits and an unrelated program gets PID 10: the row turns "ended".
        let after = [named(10, "calc", 99)];
        assert!(holder_rows(&holders, &ids, &after)[0].process.is_none());
        assert_eq!(resolve(&holders, &after), [None]);
    }

    #[test]
    fn a_reused_pid_with_the_same_name_but_another_start_is_not_the_holder() {
        let holders = [holder(10, "tool")];
        let ids = resolve(&holders, &[named(10, "tool", 5)]);
        let after = [named(10, "tool", 99)];
        assert!(holder_rows(&holders, &ids, &after)[0].process.is_none());
        // With a reported start time the start decides, whatever the name.
        let h = Holder { pid: 10, name: "Friendly App Name".into(), start: Some(5) };
        assert!(same_process(&h, &named(10, "app.exe", 5)));
        assert!(!same_process(&h, &named(10, "app.exe", 6)));
        assert!(!same_process(&h, &named(11, "app.exe", 5)));
        assert!(!same_process(&h, &named(10, "app.exe", -5)));
    }

    #[test]
    fn names_are_compared_loosely_but_never_empty() {
        assert!(same_process(&holder(1, "Notepad.EXE"), &named(1, "notepad", 1)));
        assert!(same_process(&holder(1, "a-very-long-proc"), &named(1, "a-very-long-process-name", 1)));
        assert!(!same_process(&holder(1, ""), &named(1, "", 1)));
        assert!(!same_process(&holder(1, "vim"), &named(1, "emacs", 1)));
    }

    #[test]
    fn a_process_that_vanished_after_the_result_has_no_row() {
        let holders = [holder(10, "tool")];
        let ids = resolve(&holders, &[named(10, "tool", 5)]);
        assert!(holder_rows(&holders, &ids, &[])[0].process.is_none());
    }

    #[test]
    fn the_summary_names_the_outcome() {
        let p = std::path::Path::new("some").join("report.docx");
        assert_eq!(summary(&p, &Ok(vec![])), "No program has this file open.");
        assert_eq!(summary(&p, &Ok(vec![holder(1, "a")])), "1 program has report.docx open:");
        assert_eq!(summary(&p, &Ok(vec![holder(1, "a"), holder(2, "b")])), "2 programs have report.docx open:");
        let denied = inuse::rm_error_message(5);
        assert_eq!(summary(&p, &Err(denied.clone())), denied);
    }

    #[test]
    fn the_scope_note_is_for_unprivileged_unix_only() {
        assert!(scope_note(Os::Linux, false).is_some());
        assert!(scope_note(Os::MacOs, false).is_some());
        assert_eq!(scope_note(Os::Linux, true), None);
        assert_eq!(scope_note(Os::Windows, false), None);
    }

    #[test]
    fn opening_resets_the_dialog_but_keeps_the_owner() {
        let mut d = FileInUseUi::new(7);
        d.open();
        d.text = "x".into();
        d.error = Some("e".into());
        d.close();
        d.open();
        assert!(d.open && d.text.is_empty() && d.error.is_none() && d.found.is_none() && d.owner == 7);
    }
}
