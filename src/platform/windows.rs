//! Windows backend of [`crate::platform`]: `%LOCALAPPDATA%`, Explorer, the administrator token.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Process creation flag that keeps a console child from opening a window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `%LOCALAPPDATA%`, else the temp folder.
fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// `%LOCALAPPDATA%\TMOP`: settings, palettes, skins, recordings, benchmarks, engine.
pub fn data_dir() -> PathBuf {
    local_app_data().join(crate::APP_DIR)
}

/// Same as [`data_dir`].
pub fn config_dir() -> PathBuf {
    data_dir()
}

/// TMOP's skins, TMOG's per-user skins, then TMOG's installed skins, highest priority first.
pub fn skin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        dirs.push(local.join(crate::APP_DIR).join("Skins"));
        dirs.push(local.join("TMOG").join("Skins"));
    }
    for base in ["ProgramW6432", "ProgramFiles"] {
        if let Some(pf) = std::env::var_os(base).map(PathBuf::from) {
            let d = pf.join("Task Manager TMOG").join("Skins");
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs
}

/// `%LOCALAPPDATA%\TMOG`, where TMOG keeps its settings and skins.
pub fn tmog_dir() -> Option<PathBuf> {
    Some(local_app_data().join("TMOG"))
}

/// `%LOCALAPPDATA%\tmog-rs`, the data folder before the rename to TMOP.
pub fn legacy_data_dir() -> Option<PathBuf> {
    Some(local_app_data().join(crate::LEGACY_APP_DIR))
}

/// Local wall-clock time as (year, month, day, hour, minute, second).
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    // SAFETY: GetLocalTime only writes the returned struct.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// `%COMPUTERNAME%` (else `%HOSTNAME%`).
pub fn hostname() -> Option<String> {
    ["COMPUTERNAME", "HOSTNAME"].iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()))
}

/// True when the process runs elevated (administrator token).
pub fn is_elevated() -> bool {
    // SAFETY: no arguments.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// Keeps console windows of child processes from flashing.
pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Opens a folder (or selects a file) in Explorer.
pub fn reveal(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new("explorer.exe");
    if path.is_file() {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens a file with its default application (or a folder in Explorer).
pub fn open_path(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new("explorer.exe");
    cmd.arg(path);
    hide_console(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The folders are where TMOP 0.5 put them.
    #[test]
    fn paths_unchanged() {
        let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else { return };
        assert_eq!(data_dir(), local.join("TMOP"));
        assert_eq!(config_dir(), local.join("TMOP"));
        assert_eq!(tmog_dir(), Some(local.join("TMOG")));
        assert_eq!(legacy_data_dir(), Some(local.join("tmog-rs")));
        assert_eq!(skin_dirs()[..2], [local.join("TMOP").join("Skins"), local.join("TMOG").join("Skins")]);
    }
}
