//! Linux backend of [`crate::platform`]: XDG folders, the desktop's file manager over D-Bus.

use super::unix::spawn_reaped;
pub use super::unix::{hide_console, hostname, is_elevated, local_time};
use super::xdg;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn env(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key)
}

/// `$XDG_DATA_HOME/tmop` (`~/.local/share/tmop`): recordings, benchmarks, skins, logs.
pub fn data_dir() -> PathBuf {
    xdg::data_dir(&env).unwrap_or_else(|| std::env::temp_dir().join(xdg::DIR))
}

/// `$XDG_CONFIG_HOME/tmop` (`~/.config/tmop`): settings and palettes.
pub fn config_dir() -> PathBuf {
    xdg::config_dir(&env).unwrap_or_else(|| std::env::temp_dir().join(xdg::DIR))
}

/// `<data>/skins`, then `tmop/skins` under each `$XDG_DATA_DIRS` entry.
pub fn skin_dirs() -> Vec<PathBuf> {
    let dirs = xdg::skin_dirs(&env);
    if dirs.is_empty() {
        vec![data_dir().join(xdg::SKINS)]
    } else {
        dirs
    }
}

/// TMOG is Windows-only: there is nothing to import.
pub fn tmog_dir() -> Option<PathBuf> {
    None
}

/// The pre-rename folder only ever existed on Windows.
pub fn legacy_data_dir() -> Option<PathBuf> {
    None
}

/// Shows a file selected in the file manager (FileManager1 `ShowItems` over D-Bus, else
/// `xdg-open` of its folder); a folder is opened.
pub fn reveal(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return open_path(path);
    }
    let parent = path.parent().unwrap_or(path).to_path_buf();
    let mut gdbus = Command::new("gdbus");
    gdbus.args(xdg::show_items_args(&xdg::file_uri(path.as_os_str().as_bytes()))).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match gdbus.spawn() {
        Ok(mut child) => {
            // No file manager owns the name (or gdbus has no session bus): open the folder.
            std::thread::spawn(move || {
                if !child.wait().is_ok_and(|s| s.success()) {
                    let _ = open_path(&parent);
                }
            });
            Ok(())
        }
        Err(_) => open_path(&parent),
    }
}

/// Opens a file or folder with its default application (`xdg-open`).
pub fn open_path(path: &Path) -> Result<(), String> {
    spawn_reaped(Command::new("xdg-open").arg(path))
}
