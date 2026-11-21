//! "Set affinity...": one checkbox per logical processor for the process picked in the context
//! menu. Like "Set priority", it acts on that process only (a group's other processes keep
//! their own mask); on Linux every thread of the process gets the mask.

use super::processes::key_once;
use super::theme::{self, Palette};
use super::widgets::Icon;
use super::{kit, App};
use crate::process::affinity::{self, Selection};
use crate::process::ProcessRow;
use eframe::egui;
use kit::DialogButton;

/// The open dialog.
pub(super) struct AffinityDialog {
    row: ProcessRow,
    sel: Selection,
    /// Why the last try failed, in words.
    error: Option<String>,
}

/// Columns of the checkbox grid for `n` processors.
fn grid_columns(n: usize) -> usize {
    match n {
        0..=4 => n.max(1),
        5..=16 => 4,
        _ => 6,
    }
}

impl App {
    /// Reads the mask of `row` and opens the dialog; a process that cannot even be read is
    /// reported like any other refused action.
    pub(super) fn open_affinity(&mut self, row: ProcessRow) {
        match affinity::read(&row) {
            Ok(sel) => self.affinity = Some(AffinityDialog { row, sel, error: None }),
            Err(e) => self.status = format!("{}: {e}", row.name),
        }
    }

    pub(super) fn affinity_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(mut d) = self.affinity.take() else { return };
        let st = theme::status(pal);
        let mut answer = None;
        let (name, pid) = (d.row.name.clone(), d.row.pid);
        let lead = if cfg!(windows) {
            format!("Choose the logical processors {name} (PID {pid}) may run on. Programs it starts later inherit the choice.")
        } else {
            format!("Choose the logical processors {name} (PID {pid}) may run on. All its threads get the choice; programs it starts later inherit it.")
        };
        kit::dialog(ctx, "affinity", |ui| {
            kit::dialog_header(ui, pal, Some((Icon::Cpu, pal.accent)), "Set affinity", &lead);
            ui.add_space(12.0);
            let mut all = d.sel.all();
            if ui.checkbox(&mut all, theme::button_text("All processors")).clicked() {
                d.sel.toggle_all();
                d.error = None;
            }
            ui.add_space(6.0);
            let n = d.sel.cpus().len();
            let cols = grid_columns(n);
            egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("affinity-grid").num_columns(cols).spacing([18.0, 8.0]).show(ui, |ui| {
                    for i in 0..n {
                        let mut on = d.sel.is_checked(i);
                        let label = format!("CPU {}", d.sel.cpus()[i]);
                        // The last checked box refuses to turn off: the box stays on.
                        if ui.checkbox(&mut on, theme::button_text(&label)).clicked() {
                            d.error = if d.sel.toggle(i) { None } else { Some("At least one processor must stay selected.".into()) };
                        }
                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
            });
            if let Some(e) = &d.error {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(e).font(theme::ui(13.0)).color(st.danger));
            }
            ui.add_space(10.0);
            answer = kit::dialog_buttons(ui, pal, &[DialogButton::Secondary("Cancel"), DialogButton::Primary("OK")]).map(|i| i == 1);
        });
        if key_once(ctx, egui::Key::Escape) {
            answer = Some(false);
        } else if answer.is_none() && key_once(ctx, egui::Key::Enter) {
            answer = Some(true);
        }
        match answer {
            Some(false) => {}
            Some(true) => match affinity::apply(&d.row, &d.sel) {
                Ok(text) => {
                    self.status = text;
                    self.sampler.refresh();
                }
                // Refused (a protected process, or not elevated): said in the dialog, which stays.
                Err(e) => {
                    d.error = Some(format!("{name}: {e}"));
                    self.affinity = Some(d);
                }
            },
            None => self.affinity = Some(d),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_keeps_to_a_few_columns() {
        assert_eq!([grid_columns(0), grid_columns(1), grid_columns(2), grid_columns(4)], [1, 1, 2, 4]);
        assert_eq!([grid_columns(8), grid_columns(16), grid_columns(17), grid_columns(64)], [4, 4, 6, 6]);
    }
}
