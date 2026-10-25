//! Process actions (end task, end tree, efficiency mode, priority) and their confirmation dialog.

use super::processes::{app_label, key_once};
use super::theme::{self, Palette};
use super::widgets::Icon;
use super::{kit, App};
use crate::process::{self, format_bytes, Action, Priority, ProcessRow};
use eframe::egui::{self, Color32};
use kit::DialogButton;

/// What a confirmation is about.
pub(super) enum Confirm {
    /// The process and everything below it (the root included).
    EndTree { members: Vec<ProcessRow> },
    EndTask,
    Efficiency,
    Realtime,
    /// The OS refused the action (the question holds why); offer it again as administrator.
    Elevate,
}

/// An action waiting for the user's answer to `question`.
pub(super) struct Pending {
    pub(super) action: Action,
    pub(super) row: ProcessRow,
    pub(super) question: String,
    pub(super) kind: Confirm,
}

/// How the user answered the dialog.
enum Answer {
    Cancel,
    /// Run the pending action.
    Confirm,
    /// End only the process itself instead of its tree.
    OnlyRoot,
}

/// "Question? rest" into the title and the paragraph below it.
fn split_question(q: &str) -> (&str, &str) {
    match q.split_once("? ") {
        Some((a, b)) => (&q[..a.len() + 1], b),
        None => (q, ""),
    }
}

impl App {
    pub fn request(&mut self, action: Action, row: ProcessRow) {
        let question = match action {
            Action::EndTask => Some((format!("Force quit {} ({})? Unsaved data will be lost.", row.name, row.pid), Confirm::EndTask)),
            Action::EndTree => {
                let rows = self.snapshot().2;
                let pids = process::tree_of(&rows, row.pid);
                let members: Vec<ProcessRow> = pids.iter().filter_map(|p| rows.iter().find(|r| r.pid == *p)).cloned().collect();
                let members = if members.is_empty() { vec![row.clone()] } else { members };
                Some((format!("End the process tree of {} ({})? All descendant processes will also be asked to end.", row.name, row.pid), Confirm::EndTree { members }))
            }
            Action::Efficiency(true) if self.settings.general.confirm_efficiency_mode => {
                Some((format!("Turn on {} for {}? It lowers its priority and may make it less responsive.", process::efficiency_word().to_lowercase(), row.name), Confirm::Efficiency))
            }
            Action::SetPriority(Priority::Realtime) => Some(("Realtime priority can make the system unresponsive. Continue?".into(), Confirm::Realtime)),
            _ => None,
        };
        match question {
            Some((question, kind)) => self.pending = Some(Pending { action, row, question, kind }),
            None => self.execute(action, &row),
        }
    }

    pub(super) fn execute(&mut self, action: Action, row: &ProcessRow) {
        let rows = self.snapshot().2;
        self.status = match process::actions::run(action, row, &rows) {
            Ok(m) => m,
            Err(e) if process::elevated::offered(action, &e) => {
                let question = format!("{}: {e}", row.name);
                self.pending = Some(Pending { action, row: row.clone(), question: question.clone(), kind: Confirm::Elevate });
                question
            }
            Err(e) => format!("{}: {e}", row.name),
        };
        self.sampler.refresh();
    }

    /// Runs `action` as administrator on a worker thread (the system asks for the password); the
    /// outcome lands in the status line through [`App::poll_elevated`].
    fn execute_elevated(&mut self, ctx: &egui::Context, action: Action, row: ProcessRow) {
        let rows = self.snapshot().2;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("tmop-elevated".into()).spawn(move || {
            let text = match process::elevated::run(action, &row, &rows) {
                Ok(m) => m,
                Err(e) => format!("{}: {e}", row.name),
            };
            let _ = tx.send(text);
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => {
                self.elevated_job = Some(rx);
                self.status = "Waiting for administrator authorization\u{2026}".into();
            }
            Err(e) => self.status = format!("could not start the elevated action: {e}"),
        }
    }

    /// Picks up the result of [`App::execute_elevated`].
    fn poll_elevated(&mut self) {
        let Some(rx) = &self.elevated_job else { return };
        match rx.try_recv() {
            Ok(text) => {
                self.status = text;
                self.elevated_job = None;
                self.sampler.refresh();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.elevated_job = None,
        }
    }

    pub(super) fn confirm_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        self.poll_elevated();
        let Some(p) = &self.pending else { return };
        let st = theme::status(pal);
        let logical = self.snapshot().0.cpu.logical_count;
        let mut answer = None;
        let row = &p.row;
        let app = app_label(row);
        let cpu_text = |pct: f32| format!("{:.1}% CPU", self.shown_cpu(pct, logical));
        kit::dialog(ctx, "confirm", |ui| {
            ui.spacing_mut().item_spacing.y = 18.0;
            match &p.kind {
                Confirm::EndTree { members } => {
                    let n = members.len();
                    let mem: u64 = members.iter().map(|m| m.private_working_set).sum();
                    let cpu: f32 = members.iter().map(|m| m.cpu_pct).sum();
                    let windows = members.iter().filter(|m| m.has_window).count();
                    let system = members.iter().filter(|m| process::is_system_account(m.user.as_deref())).count();
                    kit::dialog_header(
                        ui,
                        pal,
                        Some((Icon::Octagon, st.danger)),
                        &format!("End {app} and all **{n} processes**?"),
                        &format!("Every {app} window closes at once. Unsaved data in those processes is lost."),
                    );
                    let frees = format!("{} \u{b7} {}", format_bytes(mem as f64), cpu_text(cpu));
                    let open = format!("{windows} window{}", if windows == 1 { "" } else { "s" });
                    let sys = if system == 0 { "None affected".to_string() } else { format!("{system} process{}", if system == 1 { "" } else { "es" }) };
                    kit::fact_box(ui, pal, &[("Frees about", &frees, Some(st.ok)), ("Open windows", &open, None), ("System processes", &sys, (system > 0).then_some(st.warn))]);
                    let only = format!("End only {} ({})", row.name, row.pid);
                    let all = format!("End all {n} processes");
                    answer = match kit::dialog_buttons(ui, pal, &[DialogButton::Ghost("Cancel"), DialogButton::Secondary(&only), DialogButton::Danger(&all)]) {
                        Some(0) => Some(Answer::Cancel),
                        Some(1) => Some(Answer::OnlyRoot),
                        Some(_) => Some(Answer::Confirm),
                        None => None,
                    };
                }
                Confirm::EndTask => {
                    kit::dialog_header(
                        ui,
                        pal,
                        Some((Icon::Octagon, st.danger)),
                        &format!("End **{}**?", row.name),
                        &format!("{} (PID {}) is closed at once. Unsaved data in it is lost.", row.name, row.pid),
                    );
                    let frees = format!("{} \u{b7} {}", format_bytes(row.private_working_set as f64), cpu_text(row.cpu_pct));
                    kit::fact_box(ui, pal, &[("Frees about", &frees, Some(st.ok))]);
                    let end = format!("End {}", row.name);
                    answer = match kit::dialog_buttons(ui, pal, &[DialogButton::Ghost("Cancel"), DialogButton::Danger(&end)]) {
                        Some(0) => Some(Answer::Cancel),
                        Some(_) => Some(Answer::Confirm),
                        None => None,
                    };
                }
                Confirm::Elevate => {
                    kit::dialog_header(
                        ui,
                        pal,
                        Some((Icon::Shield, st.warn)),
                        &format!("**{}** needs administrator rights", row.name),
                        &format!("{}. Retry as administrator asks for an administrator password and runs only this one action.", p.question),
                    );
                    answer = match kit::dialog_buttons(ui, pal, &[DialogButton::Secondary("Cancel"), DialogButton::Primary("Retry as administrator")]) {
                        Some(0) => Some(Answer::Cancel),
                        Some(_) => Some(Answer::Confirm),
                        None => None,
                    };
                }
                Confirm::Efficiency | Confirm::Realtime => {
                    let (title, body) = split_question(&p.question);
                    let warn: Color32 = st.warn;
                    kit::dialog_header(ui, pal, Some((Icon::Alert, warn)), title, body);
                    answer = match kit::dialog_buttons(ui, pal, &[DialogButton::Secondary("Cancel"), DialogButton::Primary("Continue")]) {
                        Some(0) => Some(Answer::Cancel),
                        Some(_) => Some(Answer::Confirm),
                        None => None,
                    };
                }
            }
        });
        if answer.is_none() {
            if key_once(ctx, egui::Key::Escape) {
                answer = Some(Answer::Cancel);
            } else if key_once(ctx, egui::Key::Enter) {
                answer = Some(Answer::Confirm);
            }
        }
        if let Some(a) = answer {
            let p = self.pending.take().expect("pending");
            match a {
                Answer::Cancel => {}
                Answer::Confirm if matches!(p.kind, Confirm::Elevate) => self.execute_elevated(ctx, p.action, p.row),
                Answer::Confirm => self.execute(p.action, &p.row),
                Answer::OnlyRoot => self.execute(Action::EndTask, &p.row),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_split_into_title_and_body() {
        assert_eq!(split_question("Turn on efficiency mode for x? It lowers its priority."), ("Turn on efficiency mode for x?", "It lowers its priority."));
        assert_eq!(split_question("Realtime priority can make the system unresponsive. Continue?"), ("Realtime priority can make the system unresponsive. Continue?", ""));
    }
}
