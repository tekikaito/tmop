//! macOS backend of [`crate::platform`]: `~/Library/Application Support`, Finder via `open`.

use super::unix::spawn_reaped;
pub use super::unix::{hide_console, home, hostname, is_elevated, local_time};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `~/Library/Application Support/TMOP`: settings, palettes, recordings, skins.
pub fn data_dir() -> PathBuf {
    match super::unix::home() {
        Some(h) => h.join("Library").join("Application Support").join(crate::APP_DIR),
        None => super::unix::private_temp(crate::APP_DIR),
    }
}

/// Same as [`data_dir`]: macOS keeps preferences files next to app data.
pub fn config_dir() -> PathBuf {
    data_dir()
}

/// `~/Library/Application Support/TMOP/Skins`.
pub fn skin_dirs() -> Vec<PathBuf> {
    vec![data_dir().join("Skins")]
}

/// TMOG is Windows-only: there is nothing to import.
pub fn tmog_dir() -> Option<PathBuf> {
    None
}

/// The pre-rename folder only ever existed on Windows.
pub fn legacy_data_dir() -> Option<PathBuf> {
    None
}

/// Shows a file selected in Finder (`open -R`); a folder is opened.
pub fn reveal(path: &Path) -> Result<(), String> {
    if path.is_file() {
        spawn_reaped(Command::new("open").arg("-R").arg(path))
    } else {
        open_path(path)
    }
}

/// Opens a file or folder with its default application (`open`).
pub fn open_path(path: &Path) -> Result<(), String> {
    spawn_reaped(Command::new("open").arg(path))
}

/// Shows a notification through `osascript` (macOS lists it under Script Editor). Blocks for a
/// moment; call it off the UI thread.
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    let args = super::xdg::osascript_notify_args(title, body);
    super::run_capture("osascript", &args.iter().map(String::as_str).collect::<Vec<_>>(), std::time::Duration::from_secs(10)).map(|_| ())
}
