//! TMOP (Task Manager OverPowered): a skinnable system monitor for Windows, Linux and macOS.

pub mod docker;
pub mod engine;
pub mod fonts;
pub mod icon;
pub mod metrics;
pub mod palette;
pub mod platform;
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
/// Folder for settings, skins and recordings: under `%LOCALAPPDATA%` on Windows and
/// `~/Library/Application Support` on macOS (Linux uses the lower-case XDG `tmop`, see [`platform`]).
pub const APP_DIR: &str = "TMOP";
/// Application id given to every viewport: the Wayland `app_id`, matched by `tmop.desktop`.
pub const APP_ID: &str = "tmop";
/// The folder used before the rename to TMOP; moved to [`APP_DIR`] on first start.
pub const LEGACY_APP_DIR: &str = "tmog-rs";
