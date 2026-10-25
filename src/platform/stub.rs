//! Fallback backend for targets other than Windows, Linux and macOS: XDG folders from the
//! environment and nothing that needs the OS.

use super::xdg;
use std::path::{Path, PathBuf};
use std::process::Command;

fn env(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key)
}

pub fn data_dir() -> PathBuf {
    xdg::data_dir(&env).unwrap_or_else(|| std::env::temp_dir().join(xdg::DIR))
}

pub fn config_dir() -> PathBuf {
    xdg::config_dir(&env).unwrap_or_else(|| std::env::temp_dir().join(xdg::DIR))
}

pub fn skin_dirs() -> Vec<PathBuf> {
    vec![data_dir().join(xdg::SKINS)]
}

pub fn tmog_dir() -> Option<PathBuf> {
    None
}

pub fn legacy_data_dir() -> Option<PathBuf> {
    None
}

/// UTC (no time zone database here).
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (1970, 1, 1, ((s / 3600) % 24) as u16, ((s / 60) % 60) as u16, (s % 60) as u16)
}

pub fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|v| !v.trim().is_empty())
}

pub fn is_elevated() -> bool {
    false
}

pub fn hide_console(_cmd: &mut Command) {}

pub fn reveal(path: &Path) -> Result<(), String> {
    open_path(path)
}

pub fn open_path(path: &Path) -> Result<(), String> {
    Err(format!("cannot open {} on this platform", path.display()))
}
