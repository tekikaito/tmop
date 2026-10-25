//! Pure parsers for the Linux package databases and macOS bundles (tested on every host).

use super::App;
use crate::pro::sysfmt::{self, Plist};

/// `"Name <mail@host>"` to `"Name"`.
pub fn person(s: &str) -> String {
    s.split('<').next().unwrap_or("").trim().trim_matches('"').to_string()
}

/// Installed packages in `/var/lib/dpkg/status` (`Status: ... installed`). `Installed-Size` is KiB.
pub fn parse_dpkg_status(text: &str) -> Vec<App> {
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut a = App { scope: "dpkg", ..Default::default() };
        let mut installed = false;
        let mut arch = "";
        for line in stanza.lines() {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim();
            match k {
                "Package" => a.name = v.to_string(),
                "Status" => installed = v.split_whitespace().nth(2) == Some("installed"),
                "Version" => a.version = v.to_string(),
                "Maintainer" => a.publisher = person(v),
                "Installed-Size" => a.size = v.parse::<u64>().unwrap_or(0) * 1024,
                "Architecture" => arch = v,
                _ => {}
            }
        }
        if installed && !a.name.is_empty() {
            a.key = if arch.is_empty() || arch == "all" { a.name.clone() } else { format!("{}:{arch}", a.name) };
            out.push(a);
        }
    }
    out
}

/// The query format [`parse_rpm`] reads (one tab-separated line per package).
pub const RPM_FORMAT: &str = "%{NAME}\t%{VERSION}-%{RELEASE}\t%{ARCH}\t%{VENDOR}\t%{INSTALLTIME}\t%{SIZE}\n";

/// `rpm -qa --queryformat RPM_FORMAT` output.
pub fn parse_rpm(text: &str) -> Vec<App> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 6 || f[0].is_empty() || f[0] == "gpg-pubkey" {
                return None;
            }
            let none = |s: &str| if s == "(none)" { String::new() } else { s.trim().to_string() };
            Some(App {
                name: f[0].to_string(),
                version: f[1].to_string(),
                publisher: none(f[3]),
                install_date: f[4].trim().parse::<u64>().map(crate::pro::inventory::unix_date).unwrap_or_default(),
                size: f[5].trim().parse().unwrap_or(0),
                scope: "rpm",
                key: format!("{}.{}", f[0], f[2]),
                ..Default::default()
            })
        })
        .collect()
}

/// One `/var/lib/pacman/local/<pkg>/desc` file (`%NAME%` headers, values below).
pub fn parse_pacman_desc(text: &str) -> Option<App> {
    let mut a = App { scope: "pacman", ..Default::default() };
    let mut field = "";
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('%') && line.ends_with('%') {
            field = line;
            continue;
        }
        if line.is_empty() {
            field = "";
            continue;
        }
        match field {
            "%NAME%" => a.name = line.to_string(),
            "%VERSION%" => a.version = line.to_string(),
            "%PACKAGER%" => a.publisher = person(line),
            "%INSTALLDATE%" => a.install_date = line.parse::<u64>().map(crate::pro::inventory::unix_date).unwrap_or_default(),
            "%SIZE%" => a.size = line.parse().unwrap_or(0),
            _ => {}
        }
        if field != "%DESC%" {
            field = "";
        }
    }
    a.key = a.name.clone();
    (!a.name.is_empty()).then_some(a)
}

/// The `flatpak list` columns [`parse_flatpak`] reads.
pub const FLATPAK_COLUMNS: &str = "--columns=name,application,version,origin,installation,size";

/// `flatpak list --app --columns=name,application,version,origin,installation,size` (tab-separated).
pub fn parse_flatpak(text: &str) -> Vec<App> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').map(str::trim).collect();
            if f.len() < 5 || f[1].is_empty() {
                return None;
            }
            Some(App {
                name: if f[0].is_empty() { f[1].to_string() } else { f[0].to_string() },
                publisher: f[3].to_string(),
                version: f[2].to_string(),
                size: f.get(5).and_then(|s| sysfmt::parse_human_size(s)).unwrap_or(0),
                scope: if f[4] == "user" { "Flatpak (user)" } else { "Flatpak" },
                key: f[1].to_string(),
                ..Default::default()
            })
        })
        .collect()
}

/// `snap list`: a header row, then Name, Version, Rev, Tracking, Publisher, Notes.
pub fn parse_snap_list(text: &str) -> Vec<App> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 {
                return None;
            }
            let publisher = f[4].trim_end_matches(['*', '\u{2713}', '\u{2714}']).trim_end_matches("**");
            Some(App {
                name: f[0].to_string(),
                version: f[1].to_string(),
                publisher: if publisher == "-" { String::new() } else { publisher.to_string() },
                scope: "Snap",
                key: format!("{}_{}", f[0], f[2]),
                ..Default::default()
            })
        })
        .collect()
}

/// An `.app` bundle's `Info.plist`: (name, version, bundle id, publisher). `fallback` names a
/// bundle without a name key.
pub fn app_bundle(p: &Plist, fallback: &str) -> App {
    let name = p.text("CFBundleDisplayName").or_else(|| p.text("CFBundleName")).unwrap_or_else(|| fallback.to_string());
    let version = p.text("CFBundleShortVersionString").or_else(|| p.text("CFBundleVersion")).unwrap_or_default();
    App {
        name,
        version,
        publisher: p.text("NSHumanReadableCopyright").map(|c| sysfmt::copyright_holder(&c)).unwrap_or_default(),
        key: p.text("CFBundleIdentifier").unwrap_or_else(|| fallback.to_string()),
        ..Default::default()
    }
}

/// The tap and install time in a Homebrew `INSTALL_RECEIPT.json`.
pub fn brew_receipt(json: &str) -> (String, Option<u64>) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let tap = v.pointer("/source/tap").and_then(|t| t.as_str()).unwrap_or("").to_string();
    (tap, v.get("time").and_then(|t| t.as_u64()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::sysfmt::parse_plist_xml;

    const DPKG_STATUS: &str = "Package: adduser
Status: install ok installed
Priority: important
Installed-Size: 612
Maintainer: Ubuntu Core Developers <ubuntu-devel-discuss@lists.ubuntu.com>
Architecture: all
Version: 3.137ubuntu1
Description: add and remove users and groups
 This package includes the 'adduser' and 'deluser' commands.
 .
 Status: not a real field

Package: libc6
Status: install ok installed
Installed-Size: 13047
Maintainer: Ubuntu Developers <ubuntu-devel-discuss@lists.ubuntu.com>
Architecture: amd64
Version: 2.39-0ubuntu8.4

Package: removed-thing
Status: deinstall ok config-files
Version: 1.0

Package: held
Status: hold ok installed
Architecture: arm64
Version: 9
";

    #[test]
    fn dpkg_status() {
        let apps = parse_dpkg_status(DPKG_STATUS);
        assert_eq!(apps.len(), 3, "{apps:?}");
        assert_eq!(apps[0].name, "adduser");
        assert_eq!(apps[0].publisher, "Ubuntu Core Developers");
        assert_eq!(apps[0].size, 612 * 1024);
        assert_eq!((apps[0].key.as_str(), apps[0].scope), ("adduser", "dpkg"));
        assert_eq!((apps[1].version.as_str(), apps[1].key.as_str()), ("2.39-0ubuntu8.4", "libc6:amd64"));
        assert_eq!(apps[2].name, "held");
    }

    #[test]
    fn rpm_lines() {
        let out = parse_rpm("bash\t5.2.26-3.fc40\tx86_64\tFedora Project\t1700000000\t8301218\ngpg-pubkey\t1-2\t(none)\t(none)\t1\t0\nzlib\t1.3-1\tx86_64\t(none)\tx\t10\n");
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].name.as_str(), out[0].publisher.as_str(), out[0].install_date.as_str(), out[0].size), ("bash", "Fedora Project", "2023-11-14", 8301218));
        assert_eq!((out[1].publisher.as_str(), out[1].install_date.as_str()), ("", ""));
        assert!(RPM_FORMAT.matches('\t').count() == 5);
    }

    #[test]
    fn pacman_desc() {
        let a = parse_pacman_desc("%NAME%\nlinux\n\n%VERSION%\n6.9.7.arch1-1\n\n%DESC%\nThe Linux kernel\nand modules\n\n%PACKAGER%\nJan Alexander Steffens (heftig) <heftig@archlinux.org>\n\n%INSTALLDATE%\n1700000000\n\n%SIZE%\n139206656\n").unwrap();
        assert_eq!((a.name.as_str(), a.version.as_str(), a.publisher.as_str()), ("linux", "6.9.7.arch1-1", "Jan Alexander Steffens (heftig)"));
        assert_eq!((a.install_date.as_str(), a.size, a.scope), ("2023-11-14", 139_206_656, "pacman"));
        assert!(parse_pacman_desc("%VERSION%\n1\n").is_none());
    }

    #[test]
    fn flatpak_and_snap() {
        let f = parse_flatpak("Firefox\torg.mozilla.firefox\t131.0\tflathub\tsystem\t265.1\u{a0}MB\nGIMP\torg.gimp.GIMP\t2.10\tflathub\tuser\t1.2 GB\n");
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].name.as_str(), f[0].key.as_str(), f[0].scope, f[0].size), ("Firefox", "org.mozilla.firefox", "Flatpak", 265_100_000));
        assert_eq!((f[1].scope, f[1].publisher.as_str()), ("Flatpak (user)", "flathub"));
        let s = parse_snap_list("Name      Version        Rev    Tracking         Publisher   Notes\ncore22    20240809       1586   latest/stable    canonical\u{2713}  base\nfirefox   131.0-1        4955   latest/stable/…  mozilla**   -\nlocal     0.1            x1     -                -           -\n");
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].name.as_str(), s[0].publisher.as_str(), s[0].key.as_str()), ("core22", "canonical", "core22_1586"));
        assert_eq!(s[1].publisher, "mozilla");
        assert_eq!(s[2].publisher, "");
    }

    #[test]
    fn bundles_and_brew() {
        let p = parse_plist_xml(r#"<plist><dict><key>CFBundleName</key><string>Firefox</string><key>CFBundleShortVersionString</key><string>131.0</string><key>CFBundleIdentifier</key><string>org.mozilla.firefox</string><key>NSHumanReadableCopyright</key><string>© Firefox and Mozilla Developers; available under the MPL 2 license.</string></dict></plist>"#).unwrap();
        let a = app_bundle(&p, "Firefox.app");
        assert_eq!((a.name.as_str(), a.version.as_str(), a.key.as_str(), a.publisher.as_str()), ("Firefox", "131.0", "org.mozilla.firefox", "Firefox and Mozilla Developers"));
        let bare = app_bundle(&Plist::Dict(vec![]), "Tool");
        assert_eq!((bare.name.as_str(), bare.key.as_str()), ("Tool", "Tool"));
        assert_eq!(brew_receipt(r#"{"time":1700000000,"source":{"tap":"homebrew/core"}}"#), ("homebrew/core".into(), Some(1_700_000_000)));
        assert_eq!(brew_receipt("{}"), (String::new(), None));
    }
}
