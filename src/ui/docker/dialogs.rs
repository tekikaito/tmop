//! Dialogs of the Docker page (confirmations for container, image and volume actions).
//! The first step has none; [`show`] runs once per frame at the end of the page.

use crate::ui::{App, FrameData};
use eframe::egui;

/// Dialog state (open dialog and its inputs, added by later steps).
#[derive(Debug, Default)]
pub struct DialogState {}

/// Draws any open dialog. Called from the Docker page each frame.
pub fn show(_app: &mut App, _ctx: &egui::Context, _fd: &FrameData<'_>) {}
