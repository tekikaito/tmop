//! Multi-step dialogs of the Docker page: pull/search/run an image, add a host,
//! compose up from a file, export and import volumes.
//!
//! The table toolbars and context menus open a wizard by setting
//! [`WizardState::open`]; [`show`] draws the open wizard each frame.

use crate::ui::{App, FrameData};
use eframe::egui;

/// Which wizard is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wizard {
    /// Pull an image by name or search the registry through the engine.
    PullImage,
    /// Run a container from the given image (empty = ask for the image).
    RunImage { image: String },
    /// Add a docker context (SSH or TCP host).
    AddHost,
    /// `docker compose up` from a compose file; `stack` preselects a known stack.
    ComposeUp { stack: Option<String> },
    /// Export the named volume to a .tar file.
    ExportVolume { volume: String },
    /// Import a .tar file into the named volume (None = ask for a name).
    ImportVolume { volume: Option<String> },
}

/// State of the wizards (open wizard and its inputs; fields are added by the wizard step).
#[derive(Debug, Default)]
pub struct WizardState {
    pub open: Option<Wizard>,
}

/// Draws the open wizard, if any. Called from `dialogs::show` each frame.
pub fn show(_app: &mut App, _ctx: &egui::Context, _fd: &FrameData<'_>) {}
