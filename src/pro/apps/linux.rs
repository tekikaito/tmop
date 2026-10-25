//! Linux installed apps: dpkg, rpm, pacman, Flatpak and Snap, by presence; the desktop's
//! application entries when no package database is found.

use super::parse;
use super::App;
use crate::platform::run_capture;
use crate::pro::inventory::unix_date;
use crate::pro::sysfmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// `YYYY-MM-DD` of a file's modification time.
fn mdate(path: &Path) -> String {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| unix_date(d.as_secs())).unwrap_or_default()
}

fn has_entries(dir: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_some())
}

pub fn read() -> Result<Vec<App>, String> {
    let t = Duration::from_secs(30);
    let mut out: Vec<App> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if let Ok(status) = std::fs::read_to_string("/var/lib/dpkg/status") {
        let info = Path::new("/var/lib/dpkg/info");
        for mut a in parse::parse_dpkg_status(&status) {
            // `<pkg>.list`, or `<pkg>:<arch>.list` for Multi-Arch: same packages.
            let list = [format!("{}.list", a.key), format!("{}.list", a.name)].into_iter().map(|f| info.join(f)).find(|p| p.exists());
            a.install_date = list.map(|p| mdate(&p)).unwrap_or_default();
            out.push(a);
        }
    }

    if ["/var/lib/rpm", "/usr/lib/sysimage/rpm"].iter().any(|d| has_entries(d)) {
        match run_capture("rpm", &["-qa", "--queryformat", parse::RPM_FORMAT], t) {
            Ok(text) => out.extend(parse::parse_rpm(&text)),
            Err(e) => errors.push(e),
        }
    }

    if let Ok(rd) = std::fs::read_dir("/var/lib/pacman/local") {
        for e in rd.flatten() {
            if let Some(a) = std::fs::read_to_string(e.path().join("desc")).ok().and_then(|d| parse::parse_pacman_desc(&d)) {
                out.push(a);
            }
        }
    }

    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    let user_flatpak = home.as_ref().map(|h| h.join(".local/share/flatpak"));
    if Path::new("/var/lib/flatpak").is_dir() || user_flatpak.as_ref().is_some_and(|p| p.is_dir()) {
        match run_capture("flatpak", &["list", "--app", parse::FLATPAK_COLUMNS], t) {
            Ok(text) => {
                for mut a in parse::parse_flatpak(&text) {
                    let root = if a.scope == "Flatpak" { Some(PathBuf::from("/var/lib/flatpak")) } else { user_flatpak.clone() };
                    if let Some(dir) = root.map(|r| r.join("app").join(&a.key)).filter(|d| d.is_dir()) {
                        a.install_date = mdate(&dir);
                        a.location = dir.display().to_string();
                    }
                    out.push(a);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    if Path::new("/var/lib/snapd/snaps").is_dir() {
        match run_capture("snap", &["list"], t) {
            Ok(text) => {
                for mut a in parse::parse_snap_list(&text) {
                    let file = Path::new("/var/lib/snapd/snaps").join(format!("{}.snap", a.key));
                    if let Ok(m) = std::fs::metadata(&file) {
                        a.size = m.len();
                        a.install_date = mdate(&file);
                    }
                    let dir = Path::new("/snap").join(&a.name);
                    if dir.is_dir() {
                        a.location = dir.display().to_string();
                    }
                    out.push(a);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    if out.is_empty() {
        out = desktop_apps(home.as_deref());
    }
    if out.is_empty() {
        return Err(match errors.first() {
            Some(e) => format!("no installed packages found ({e})"),
            None => "no package database found (dpkg, rpm, pacman, Flatpak or Snap)".into(),
        });
    }
    super::finish(&mut out);
    Ok(out)
}

/// Visible application entries under `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`.
fn desktop_apps(home: Option<&Path>) -> Vec<App> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(d) => dirs.push(d),
        None => dirs.extend(home.map(|h| h.join(".local/share"))),
    }
    let system = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| d.starts_with('/')).map(PathBuf::from));
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir.join("applications")) else { continue };
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            if !file.ends_with(".desktop") || !seen.insert(file.clone()) {
                continue;
            }
            let Some(d) = std::fs::read_to_string(e.path()).ok().and_then(|t| sysfmt::parse_desktop_entry(&t)) else { continue };
            if d.hidden || d.no_display || d.name.is_empty() || (!d.kind.is_empty() && d.kind != "Application") {
                continue;
            }
            out.push(App { name: d.name, install_date: mdate(&e.path()), scope: "Desktop entry", key: file, ..Default::default() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_some_apps() {
        let apps = super::read().unwrap();
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|a| !a.name.is_empty() && !a.scope.is_empty()));
    }
}
