//! Process actions (end task, end tree, efficiency mode, priority) and their confirmation dialog.

use super::theme::Palette;
use super::{widgets, App};
#[cfg(windows)]
use crate::process;
use crate::process::{Action, Priority, ProcessRow};
use eframe::egui::{self, Align2};

/// An action waiting for the user's answer to `question`.
pub(super) struct Pending {
    pub(super) action: Action,
    pub(super) row: ProcessRow,
    pub(super) question: String,
}

impl App {
    pub fn request(&mut self, action: Action, row: ProcessRow) {
        let question = match action {
            Action::EndTask => Some(format!("Force quit {} ({})? Unsaved data will be lost.", row.name, row.pid)),
            Action::EndTree => Some(format!("End the process tree of {} ({})? All descendant processes will also be asked to end.", row.name, row.pid)),
            Action::Efficiency(true) if self.settings.general.confirm_efficiency_mode => {
                Some(format!("Turn on efficiency mode for {}? It lowers its priority and may make it less responsive.", row.name))
            }
            Action::SetPriority(Priority::Realtime) => Some("Realtime priority can make the system unresponsive. Continue?".into()),
            _ => None,
        };
        match question {
            Some(q) => self.pending = Some(Pending { action, row, question: q }),
            None => self.execute(action, &row),
        }
    }

    pub(super) fn execute(&mut self, action: Action, row: &ProcessRow) {
        #[cfg(windows)]
        {
            let rows = self.snapshot().2;
            self.status = match process::actions::run(action, row, &rows) {
                Ok(m) => m,
                Err(e) => format!("{}: {e}", row.name),
            };
            self.sampler.refresh();
        }
        #[cfg(not(windows))]
        {
            let _ = (action, row);
            self.status = "Process actions are only implemented on Windows".into();
        }
    }

    pub(super) fn confirm_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(p) = &self.pending else { return };
        let mut answer = None;
        let question = p.question.clone();
        egui::Window::new("Confirm")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(question);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Yes", pal, None).clicked() {
                        answer = Some(true);
                    }
                    if widgets::button(ui, "Cancel", pal, None).clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(yes) = answer {
            let p = self.pending.take().expect("pending");
            if yes {
                self.execute(p.action, &p.row);
            }
        }
    }
}
