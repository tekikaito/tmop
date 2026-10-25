//! Installed applications. Read-only: nothing is ever uninstalled from here.
//!
//! Windows reads the `Uninstall` registry keys (machine 64-bit, machine 32-bit and per-user);
//! Linux the package databases present (dpkg, rpm, pacman, Flatpak, Snap); macOS the `.app`
//! bundles in `/Applications` and `~/Applications` plus the Homebrew Cellar.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub mod parse;

#[cfg(target_os = "linux")]
pub use linux::read;
#[cfg(target_os = "macos")]
pub use macos::read;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct App {
    pub name: String,
    pub publisher: String,
    pub version: String,
    /// `YYYY-MM-DD` when the key carries an `InstallDate`.
    pub install_date: String,
    /// Bytes (from `EstimatedSize`, which is in KiB).
    pub size: u64,
    pub location: String,
    /// Where the entry comes from: "Machine (64-bit)", "Machine (32-bit)" or "User" on Windows;
    /// "dpkg", "rpm", "pacman", "Flatpak", "Flatpak (user)", "Snap" or "Desktop entry" on Linux;
    /// "Application", "User" or "Homebrew" on macOS.
    pub scope: &'static str,
    pub key: String,
}

/// `20240131` -> `2024-01-31`.
pub fn format_install_date(s: &str) -> String {
    let s = s.trim();
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..])
    } else {
        s.to_string()
    }
}

#[cfg(windows)]
pub fn read() -> Result<Vec<App>, String> {
    use super::winutil::{Key, View};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const PATH: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let sources = [
        (HKEY_LOCAL_MACHINE, View::Wow64_64, "Machine (64-bit)"),
        (HKEY_LOCAL_MACHINE, View::Wow64_32, "Machine (32-bit)"),
        (HKEY_CURRENT_USER, View::Default, "User"),
    ];
    let mut out: Vec<App> = Vec::new();
    for (root, view, scope) in sources {
        let Some(base) = Key::open(root, PATH, view) else { continue };
        for sub in base.subkeys() {
            let Some(k) = base.subkey(&sub) else { continue };
            let Some(name) = k.string("DisplayName") else { continue };
            // Hide updates and components the way Apps & features does.
            if k.u32("SystemComponent") == Some(1) || k.string("ParentKeyName").is_some() || k.string("ReleaseType").map(|r| r.contains("Update")).unwrap_or(false) {
                continue;
            }
            let app = App {
                name: name.trim().to_string(),
                publisher: k.string("Publisher").unwrap_or_default().trim().to_string(),
                version: k.string("DisplayVersion").unwrap_or_default(),
                install_date: k.string("InstallDate").map(|d| format_install_date(&d)).unwrap_or_default(),
                size: k.u32("EstimatedSize").map(|kb| kb as u64 * 1024).unwrap_or(0),
                location: k.string("InstallLocation").unwrap_or_default().trim_matches('"').to_string(),
                scope,
                key: sub,
            };
            if !out.iter().any(|a| a.name == app.name && a.version == app.version) {
                out.push(app);
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn read() -> Result<Vec<App>, String> {
    Err(format!("installed apps are not implemented on {}", crate::platform::os_name()))
}

/// Drops repeats of a (source, key) and sorts by name.
#[cfg_attr(windows, allow(dead_code))]
fn finish(apps: &mut Vec<App>) {
    let mut seen = std::collections::HashSet::new();
    apps.retain(|a| seen.insert((a.scope, a.key.clone())));
    apps.sort_by_key(|a| a.name.to_lowercase());
}

/// The sources in a list, most entries first, as "dpkg, Flatpak and Snap".
pub fn sources(apps: &[App]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for a in apps {
        let base = a.scope.split(" (").next().unwrap_or(a.scope);
        match counts.iter_mut().find(|c| c.0 == base) {
            Some(c) => c.1 += 1,
            None => counts.push((base, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let names: Vec<&str> = counts.iter().map(|c| c.0).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_format() {
        assert_eq!(format_install_date("20240131"), "2024-01-31");
        assert_eq!(format_install_date("1/2/2024"), "1/2/2024");
    }

    #[test]
    fn source_lists() {
        let app = |scope| App { scope, ..Default::default() };
        assert_eq!(sources(&[]), "");
        assert_eq!(sources(&[app("Snap")]), "Snap");
        assert_eq!(sources(&[app("Snap"), app("dpkg"), app("dpkg"), app("dpkg"), app("Flatpak (user)"), app("Flatpak")]), "dpkg, Flatpak and Snap");
        let named = |name: &str, scope| App { name: name.into(), key: "x".into(), scope, ..Default::default() };
        let mut list = vec![named("b", "dpkg"), named("B", "dpkg"), named("a", "Snap")];
        finish(&mut list);
        assert_eq!(list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[cfg(windows)]
    #[test]
    fn lists_some_apps() {
        let apps = read().unwrap();
        assert!(!apps.is_empty(), "every Windows install lists at least one app");
        assert!(apps.iter().all(|a| !a.name.is_empty()));
    }
}
