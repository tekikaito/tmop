//! "Set up TMOP engine" wizard: what gets installed, the WSL check, the data folder and the
//! set-up. Opened from the Engine tab; [`show`] draws it each frame from `dialogs::show`.

use super::{engine_tab, wizards, DockerState};
use crate::docker::backend::NoticeKind;
use crate::engine::manager::WslCheck;
use crate::ui::theme;
use crate::ui::widgets;
use crate::ui::{App, FrameData};
use eframe::egui::{self, Align2, RichText};
use std::path::PathBuf;

/// State of the wizard, kept in [`DockerUi`](super::DockerUi).
#[derive(Debug, Default)]
pub struct EngineWizard {
    pub open: bool,
    /// Data folder to set up in; prefilled from the engine status when opened.
    pub data_dir: String,
    /// The WSL check was requested for this opening.
    pub checked: bool,
}

/// Opens the wizard. The WSL check runs on the next frame, once per opening.
pub fn open(docker: &mut DockerState) {
    let status = docker.engine.status();
    let w = &mut docker.ui.engine_wizard;
    w.open = true;
    w.checked = false;
    w.data_dir = status.data_dir.display().to_string();
}

/// Draws the wizard while it is open. Escape and the close button hide it.
pub fn show(app: &mut App, ctx: &egui::Context, fd: &FrameData<'_>) {
    if !app.docker.ui.engine_wizard.open {
        return;
    }
    if !app.docker.ui.engine_wizard.checked {
        app.docker.ui.engine_wizard.checked = true;
        app.docker.engine.check_wsl();
    }
    let pal = fd.pal;
    let status = app.docker.engine.status();
    let mut is_open = true;
    let mut close = false;
    let mut setup: Option<PathBuf> = None;
    let mut failure: Option<String> = None;
    egui::Window::new("Set up TMOP engine")
        .collapsible(false)
        .resizable(false)
        .open(&mut is_open)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(egui::Frame::window(&ctx.style()).fill(pal.panel))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            let w = &mut app.docker.ui.engine_wizard;
            ui.label(
                RichText::new("Installs a small Linux system with Docker, run by WSL 2. About 140 MB is downloaded from github.com/tekikaito/tmop.")
                    .font(theme::ui(13.0))
                    .color(pal.text),
            );
            ui.label(
                RichText::new("Containers, images and volumes are stored in the data folder below. Choose another drive if C: is small.")
                    .font(theme::ui(12.5))
                    .color(pal.text_dim),
            );
            ui.add_space(6.0);
            engine_tab::wsl_line(ui, &pal, &status.wsl);
            if status.wsl == WslCheck::Missing {
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Install WSL...", &pal, None).clicked() {
                        failure = app.docker.engine.install_wsl().err();
                    }
                    ui.label(RichText::new("Windows asks for permission; restart your PC when it finishes.").font(theme::ui(12.0)).color(pal.text_faint));
                });
            }

            ui.add_space(8.0);
            wizards::label(ui, "Data folder", &pal);
            ui.horizontal(|ui| {
                ui.add(wizards::single(&mut w.data_dir, "", 400.0));
                if widgets::button(ui, "Browse...", &pal, None).clicked() {
                    let mut dialog = rfd::FileDialog::new();
                    if !w.data_dir.trim().is_empty() {
                        dialog = dialog.set_directory(w.data_dir.trim());
                    }
                    if let Some(dir) = dialog.pick_folder() {
                        w.data_dir = dir.display().to_string();
                    }
                }
            });

            ui.add_space(10.0);
            let wsl_ready = matches!(status.wsl, WslCheck::Ready { .. });
            let ready = wsl_ready && !status.busy && !w.data_dir.trim().is_empty();
            ui.horizontal(|ui| {
                if wizards::enabled_button(ui, "Set up", &pal, ready) {
                    setup = Some(PathBuf::from(w.data_dir.trim()));
                }
                if widgets::button(ui, "Cancel", &pal, None).clicked() {
                    close = true;
                }
            });
        });
    if let Some(e) = failure {
        app.docker.ui.toast(NoticeKind::Error, e);
    }
    if let Some(dir) = setup {
        // The engine's context becomes the active one once the engine runs (see `App::engine_poll`).
        app.docker.engine.setup(dir.clone());
        app.settings.engine_data_dir = dir.display().to_string();
        app.settings_dirty = true;
        app.docker.switch_to_engine_when_ready = true;
        close = true;
    }
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if !is_open || escape || close {
        app.docker.ui.engine_wizard.open = false;
    }
}
