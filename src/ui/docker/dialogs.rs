//! Dialogs of the Docker page (confirmations for container, image and volume actions).
//! The first step has none; [`show`] runs once per frame at the end of the page.

use crate::ui::{App, FrameData};
use eframe::egui;

/// Dialog state (open dialog and its inputs, added by later steps).
#[derive(Debug, Default)]
pub struct DialogState {
    pub wizards: super::wizards::WizardState,
}

/// Draws any open dialog. Called from the Docker page each frame.
pub fn show(app: &mut App, ctx: &egui::Context, fd: &FrameData<'_>) {
    super::wizards::show(app, ctx, fd);
}
