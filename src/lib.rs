//! TMOP (Task Manager OverPowered): a skinnable Windows system monitor.

pub mod docker;
pub mod engine;
pub mod fonts;
pub mod metrics;
pub mod palette;
pub mod pro;
pub mod process;
pub mod screensaver;
pub mod settings;
pub mod skin;
pub mod taskmgr;
pub mod terminal;
pub mod ui;
pub mod update;

/// Short product name, shown in the UI.
pub const APP_NAME: &str = "TMOP";
/// Full product name.
pub const APP_LONG_NAME: &str = "Task Manager OverPowered";
/// Folder under `%LOCALAPPDATA%` for settings, skins and recordings.
pub const APP_DIR: &str = "TMOP";
/// The folder used before the rename to TMOP; moved to [`APP_DIR`] on first start.
pub const LEGACY_APP_DIR: &str = "tmog-rs";
