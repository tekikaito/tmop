//! Turning startup entries on and off with the system's own switch. The entry itself (the Run
//! value, the Startup folder file, the launchd plist) is never deleted or rewritten.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | switch | `Explorer\StartupApproved\{Run,Run32,StartupFolder}` value of the entry, HKCU or HKLM | `Hidden=true` in the user's `.desktop` file, or in a copy of the all-users file put into `$XDG_CONFIG_HOME/autostart` | `launchctl disable` / `enable` in `gui/<uid>` (agents) or `system` (daemons) |
//! | not switched | - | `systemd --user` units, an all-users file that turns itself off | - |
//! | needs | administrator for HKLM | - | root for daemons |
//!
//! Everything that decides what to write is a pure function here ([`approved_value`],
//! [`desktop_disable`], [`desktop_enable`], [`xdg_set`] on given folders, [`launchctl_args`]);
//! [`set_enabled`] is the thin shell that reads and writes the real system.

use super::StartupApp;
use std::path::{Path, PathBuf};

/// How TMOP can address an entry to turn it on or off.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Switch {
    /// No switch TMOP knows of.
    #[default]
    None,
    /// Not switched by TMOP, for this reason.
    Fixed(&'static str),
    /// Windows: the value `name` (Run value name, or Startup folder file name with extension) of
    /// `...\Explorer\StartupApproved\<sub>` under HKLM (`machine`) or HKCU.
    Approved { machine: bool, sub: &'static str, name: String },
    /// Linux: the XDG autostart file the row shows; `user` when it is in the user's own folder.
    Desktop { path: PathBuf, user: bool },
    /// macOS: a launchd job; `system` for the system domain (launch daemons).
    Launchd { label: String, system: bool },
}

/// Why the Enable / Disable action is not available on `app`, or None when it is. `privileged`:
/// TMOP runs as administrator (Windows) or root.
pub fn unavailable(app: &StartupApp, privileged: bool) -> Option<&'static str> {
    match &app.switch {
        Switch::None => Some("TMOP cannot switch this kind of entry"),
        Switch::Fixed(why) => Some(why),
        Switch::Approved { machine: true, .. } if !privileged => Some("Needs administrator"),
        Switch::Desktop { user: false, .. } if !app.enabled => Some("The all-users file turns itself off; TMOP does not override it"),
        Switch::Launchd { system: true, .. } if !privileged => Some("Needs root"),
        _ => None,
    }
}

// ---- Windows: StartupApproved values ----

/// Length of the values Windows writes: flags (4 bytes) and a FILETIME (8 bytes).
pub const APPROVED_LEN: usize = 12;

/// The StartupApproved value to write so the entry is `enable`d, from the value there now (None:
/// there is none). None when nothing has to be written: the value already says so, or there is no
/// value and the entry is to be enabled (no value means enabled).
///
/// The first byte's lowest bit is the switch (Task Manager writes 02 / 03, Settings 00 / 01 and
/// 04 / 05). Only that bit is flipped; the other flag bits, bytes 1 to 3 and anything after byte
/// 12 are kept. Bytes 4 to 11 are the FILETIME of the disable, zero when enabled, as Windows writes
/// them. A new value (disabling an entry without one) is Task Manager's `03 00 00 00` + FILETIME.
///
/// Microsoft does not document the value. The layout is the one measured by DLEAPP's
/// `windowsStartupApproved` artifact (Task Manager and Settings toggled on Windows 11, values read
/// after each change) and matches what Electron (`shell/browser/browser_win.cc`) writes and reads.
pub fn approved_value(current: Option<&[u8]>, enable: bool, filetime: u64) -> Option<Vec<u8>> {
    let current = current.filter(|b| !b.is_empty());
    if current.map(super::approved_enabled).unwrap_or(true) == enable {
        return None;
    }
    let mut v = current.map(<[u8]>::to_vec).unwrap_or_else(|| vec![0x02, 0, 0, 0]);
    if v.len() < APPROVED_LEN {
        v.resize(APPROVED_LEN, 0);
    }
    if enable {
        v[0] &= !1;
        v[4..APPROVED_LEN].fill(0);
    } else {
        v[0] |= 1;
        v[4..APPROVED_LEN].copy_from_slice(&filetime.to_le_bytes());
    }
    Some(v)
}

/// A time as a Windows FILETIME (100 ns units since 1601-01-01 UTC).
pub fn filetime(t: std::time::SystemTime) -> u64 {
    const UNIX_TO_1601: u64 = 116_444_736_000_000_000;
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    UNIX_TO_1601 + d.as_secs() * 10_000_000 + d.subsec_nanos() as u64 / 100
}

// ---- Linux: XDG autostart files ----

/// Put above a `Hidden=true` that TMOP added to the user's own file.
pub const DESKTOP_MARK: &str = "# Hidden=true below was added by TMOP to turn this entry off; Enable in TMOP removes both lines";
/// Put above the `Hidden=true` of a copy of an all-users file that TMOP made to turn it off.
pub const DESKTOP_COPY_MARK: &str = "# TMOP copied this all-users entry here to turn it off; Enable in TMOP deletes the copy";

/// A line's text and its line ending ("\n", "\r\n", or "" for a last line without one).
type Line = (Vec<u8>, Vec<u8>);

fn split_lines(b: &[u8]) -> Vec<Line> {
    let mut out = Vec::new();
    let mut rest = b;
    while !rest.is_empty() {
        let (line, tail) = match rest.iter().position(|&c| c == b'\n') {
            Some(i) => (&rest[..=i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        let cut = if line.ends_with(b"\r\n") {
            line.len() - 2
        } else if line.ends_with(b"\n") {
            line.len() - 1
        } else {
            line.len()
        };
        out.push((line[..cut].to_vec(), line[cut..].to_vec()));
        rest = tail;
    }
    out
}

fn join_lines(lines: &[Line]) -> Vec<u8> {
    lines.iter().flat_map(|(t, e)| t.iter().chain(e)).copied().collect()
}

fn trim(b: &[u8]) -> &[u8] {
    let is_ws = |c: &u8| *c == b' ' || *c == b'\t';
    let start = b.iter().position(|c| !is_ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !is_ws(c)).map(|i| i + 1).unwrap_or(start);
    &b[start..end.max(start)]
}

/// The `[Desktop Entry]` group: (header line, end) with `end` the next group's header or the end.
fn desktop_group(lines: &[Line]) -> Option<(usize, usize)> {
    let start = lines.iter().position(|(t, _)| trim(t) == b"[Desktop Entry]")?;
    let end = lines.iter().skip(start + 1).position(|(t, _)| trim(t).starts_with(b"[")).map(|i| start + 1 + i).unwrap_or(lines.len());
    Some((start, end))
}

/// (key, start and end of the value) of a `key=value` line; None for comments and blank lines.
fn key_value(t: &[u8]) -> Option<(&[u8], usize, usize)> {
    let lead = t.len() - t.iter().skip_while(|c| **c == b' ' || **c == b'\t').count();
    if t.get(lead).is_none_or(|c| *c == b'#') {
        return None;
    }
    let eq = t.iter().position(|&c| c == b'=')?;
    let mut vs = eq + 1;
    while t.get(vs).is_some_and(|c| *c == b' ' || *c == b'\t') {
        vs += 1;
    }
    let ve = (vs..t.len()).rev().find(|&i| t[i] != b' ' && t[i] != b'\t').map(|i| i + 1).unwrap_or(vs);
    Some((trim(&t[..eq]), vs, ve))
}

/// The last line of the group with exactly this key (not a localized `Key[xx]`): the one the
/// reader goes by.
fn last_key(lines: &[Line], group: (usize, usize), key: &[u8]) -> Option<usize> {
    (group.0 + 1..group.1).rev().find(|&i| key_value(&lines[i].0).is_some_and(|(k, _, _)| k == key))
}

fn value_of(line: &[u8]) -> &[u8] {
    key_value(line).map(|(_, s, e)| &line[s..e]).unwrap_or_default()
}

/// Replaces the value of a `key=value` line, keeping the spacing around it.
fn set_value(line: &mut Vec<u8>, value: &[u8]) {
    if let Some((_, s, e)) = key_value(line) {
        line.splice(s..e, value.iter().copied());
    }
}

const NO_GROUP: &str = "the file has no [Desktop Entry] group";

/// The file turned off: an existing `Hidden` key is set to `true` in place, else `mark` and
/// `Hidden=true` are added after the group's last line, with the file's line endings. Every other
/// byte stays as it was.
pub fn desktop_disable(text: &[u8], mark: &str) -> Result<Vec<u8>, String> {
    let mut lines = split_lines(text);
    let group = desktop_group(&lines).ok_or(NO_GROUP)?;
    if let Some(i) = last_key(&lines, group, b"Hidden") {
        if value_of(&lines[i].0).eq_ignore_ascii_case(b"true") {
            return Err("the entry is already turned off (Hidden=true)".into());
        }
        set_value(&mut lines[i].0, b"true");
        return Ok(join_lines(&lines));
    }
    let eol = lines.iter().map(|l| l.1.clone()).find(|e| !e.is_empty()).unwrap_or_else(|| b"\n".to_vec());
    let at = (group.0..group.1).rev().find(|&i| !trim(&lines[i].0).is_empty()).unwrap_or(group.0);
    // At the end of a file without a final line break the added lines go after a new one, and the
    // last added line gets none, so that Enable can take back exactly what was added.
    let last_eol = if lines[at].1.is_empty() {
        lines[at].1 = eol.clone();
        Vec::new()
    } else {
        eol.clone()
    };
    lines.insert(at + 1, (mark.as_bytes().to_vec(), eol));
    lines.insert(at + 2, (b"Hidden=true".to_vec(), last_eol));
    Ok(join_lines(&lines))
}

/// The file turned on again: a `Hidden=true` that TMOP added (under one of its marks) is removed
/// with its mark, any other `Hidden=true` becomes `false` in place, and
/// `X-GNOME-Autostart-enabled=false` (GNOME's own switch) becomes `true` in place.
pub fn desktop_enable(text: &[u8]) -> Result<Vec<u8>, String> {
    let mut lines = split_lines(text);
    let group = desktop_group(&lines).ok_or(NO_GROUP)?;
    let mut changed = false;
    if let Some(i) = last_key(&lines, group, b"X-GNOME-Autostart-enabled") {
        if value_of(&lines[i].0).eq_ignore_ascii_case(b"false") {
            set_value(&mut lines[i].0, b"true");
            changed = true;
        }
    }
    if let Some(i) = last_key(&lines, group, b"Hidden") {
        if value_of(&lines[i].0).eq_ignore_ascii_case(b"true") {
            let marked = i > group.0 + 1 && [DESKTOP_MARK, DESKTOP_COPY_MARK].iter().any(|m| trim(&lines[i - 1].0) == m.as_bytes());
            if marked {
                let at_end = lines[i].1.is_empty();
                lines.drain(i - 1..=i);
                if at_end {
                    lines[i - 2].1.clear();
                }
            } else {
                set_value(&mut lines[i].0, b"false");
            }
            changed = true;
        }
    }
    if !changed {
        return Err("the file does not turn the entry off".into());
    }
    Ok(join_lines(&lines))
}

/// Turns the autostart file `path` on or off, with `user_dir` the user's autostart folder and
/// `system_dirs` the all-users ones (most important first). A file in `user_dir` is changed in
/// place; turning off an all-users file puts a copy with `Hidden=true` into `user_dir` (the
/// Desktop Application Autostart Specification's way for a user to turn off a system entry).
/// Turning a copy back on deletes it only when it is exactly what TMOP made from the all-users
/// file as it is now; otherwise it is changed in place like any user file.
pub fn xdg_set(path: &Path, user_dir: &Path, system_dirs: &[PathBuf], enable: bool) -> Result<(), String> {
    let name = path.file_name().ok_or_else(|| format!("{} has no file name", path.display()))?;
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("could not read {}: {e}", p.display()));
    let write = |p: &Path, b: &[u8]| std::fs::write(p, b).map_err(|e| format!("could not write {}: {e}", p.display()));
    if path.parent() == Some(user_dir) {
        let current = read(path)?;
        if !enable {
            return write(path, &desktop_disable(&current, DESKTOP_MARK)?);
        }
        let system = system_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
        if let Some(Ok(copy)) = system.map(|s| read(&s).and_then(|b| desktop_disable(&b, DESKTOP_COPY_MARK))) {
            if copy == current {
                return std::fs::remove_file(path).map_err(|e| format!("could not delete {}: {e}", path.display()));
            }
        }
        return write(path, &desktop_enable(&current)?);
    }
    if enable {
        return Err("the all-users file turns itself off; TMOP does not override it".into());
    }
    let target = user_dir.join(name);
    let copy = desktop_disable(&read(path)?, DESKTOP_COPY_MARK)?;
    std::fs::create_dir_all(user_dir).map_err(|e| format!("could not create {}: {e}", user_dir.display()))?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| format!("could not create {}: {e}", target.display()))?;
    f.write_all(&copy).map_err(|e| format!("could not write {}: {e}", target.display()))
}

// ---- macOS: launchctl ----

/// `launchctl` arguments that turn the job `label` on or off: in the user's GUI login domain
/// (`gui/<uid>`, the domain TMOP reads with `print-disabled`) or, for a daemon, the system domain.
pub fn launchctl_args(enable: bool, label: &str, system: bool, uid: u32) -> Vec<String> {
    let domain = if system { "system".to_string() } else { format!("gui/{uid}") };
    vec![if enable { "enable" } else { "disable" }.to_string(), format!("{domain}/{label}")]
}

// ---- The shell around the system ----

/// Turns `app` on (`enable`) or off with the system's switch. The caller reads the entries again
/// afterwards to show what the system now says.
pub fn set_enabled(app: &StartupApp, enable: bool) -> Result<(), String> {
    if let Some(why) = unavailable(app, crate::platform::is_elevated()) {
        return Err(why.to_string());
    }
    set_enabled_os(app, enable)
}

#[cfg(windows)]
fn set_enabled_os(app: &StartupApp, enable: bool) -> Result<(), String> {
    use crate::pro::winutil::{self, Key, Value, View};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let Switch::Approved { machine, sub, name } = &app.switch else { return Err("TMOP cannot switch this kind of entry".into()) };
    let root = if *machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER };
    let path = format!(r"{}\{sub}", super::STARTUP_APPROVED);
    let current = Key::open(root, &path, View::Default).and_then(|k| k.value(name));
    let bytes = match &current {
        None => None,
        Some(Value::Bytes(b)) => Some(b.as_slice()),
        Some(_) => return Err(format!("the StartupApproved value of {name} is not binary data; TMOP leaves it alone")),
    };
    match approved_value(bytes, enable, filetime(std::time::SystemTime::now())) {
        Some(v) => winutil::set_binary(root, &path, name, &v),
        None => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn set_enabled_os(app: &StartupApp, enable: bool) -> Result<(), String> {
    let Switch::Desktop { path, .. } = &app.switch else { return Err("TMOP cannot switch this kind of entry".into()) };
    let (user, system) = super::linux::autostart_folders();
    let user = user.ok_or("there is no personal autostart folder ($XDG_CONFIG_HOME and $HOME are unset)")?;
    xdg_set(path, &user, &system, enable)
}

#[cfg(target_os = "macos")]
fn set_enabled_os(app: &StartupApp, enable: bool) -> Result<(), String> {
    let Switch::Launchd { label, system } = &app.switch else { return Err("TMOP cannot switch this kind of entry".into()) };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let args = launchctl_args(enable, label, *system, uid);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    crate::platform::run_capture("launchctl", &args, std::time::Duration::from_secs(10)).map(|_| ())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn set_enabled_os(_app: &StartupApp, _enable: bool) -> Result<(), String> {
    Err(format!("startup apps cannot be switched on {}", crate::platform::os_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 0x01DB_3C4D_5E6F_7A8B;

    fn app(switch: Switch, enabled: bool) -> StartupApp {
        StartupApp { name: "x".into(), enabled, switch, ..Default::default() }
    }

    #[test]
    fn availability() {
        let hklm = Switch::Approved { machine: true, sub: "Run", name: "A".into() };
        let hkcu = Switch::Approved { machine: false, sub: "StartupFolder", name: "A.lnk".into() };
        assert_eq!(unavailable(&app(hklm.clone(), true), false), Some("Needs administrator"));
        assert_eq!(unavailable(&app(hklm, true), true), None);
        assert_eq!(unavailable(&app(hkcu, false), false), None);
        assert!(unavailable(&app(Switch::None, true), true).is_some());
        assert_eq!(unavailable(&app(Switch::Fixed("why"), true), true), Some("why"));
        let sys = Switch::Desktop { path: "/etc/xdg/autostart/a.desktop".into(), user: false };
        assert_eq!(unavailable(&app(sys.clone(), true), false), None, "disabling writes only the user's folder");
        assert!(unavailable(&app(sys, false), true).is_some(), "an all-users file that is off itself");
        assert_eq!(unavailable(&app(Switch::Desktop { path: "/h/.config/autostart/a.desktop".into(), user: true }, false), false), None);
        assert_eq!(unavailable(&app(Switch::Launchd { label: "l".into(), system: true }, true), false), Some("Needs root"));
        assert_eq!(unavailable(&app(Switch::Launchd { label: "l".into(), system: false }, true), false), None);
    }

    #[test]
    fn approved_bytes_round_trip_through_the_reader() {
        let enabled = [2u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let off = approved_value(Some(&enabled), false, T).unwrap();
        assert_eq!(off.len(), APPROVED_LEN);
        assert_eq!(&off[..4], &[3, 0, 0, 0]);
        assert_eq!(&off[4..], &T.to_le_bytes());
        assert!(!crate::pro::inventory::approved_enabled(&off));
        let on = approved_value(Some(&off), true, T + 1).unwrap();
        assert_eq!(on, enabled, "disable then enable restores the bytes");
        assert!(crate::pro::inventory::approved_enabled(&on));
    }

    #[test]
    fn approved_flags_of_settings_and_machine_values_are_kept() {
        // Settings writes 00 / 01, and 04 / 05 for machine Run entries: only the low bit moves.
        for base in [0u8, 4, 6] {
            let v = [base, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let off = approved_value(Some(&v), false, T).unwrap();
            assert_eq!(off[0], base | 1);
            assert_eq!(approved_value(Some(&off), true, 0).unwrap(), v);
        }
    }

    #[test]
    fn approved_unknown_bytes_are_kept() {
        let v = [0x02u8, 0xAA, 0xBB, 0xCC, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x22];
        let off = approved_value(Some(&v), false, T).unwrap();
        assert_eq!(&off[1..4], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(&off[12..], &[0x11, 0x22]);
        assert_eq!(approved_value(Some(&off), true, T).unwrap(), v);
        // A short value is padded to the length Windows writes.
        let short = approved_value(Some(&[2u8]), false, T).unwrap();
        assert_eq!(short.len(), APPROVED_LEN);
        assert_eq!(short[0], 3);
    }

    #[test]
    fn approved_writes_nothing_when_nothing_changes() {
        assert_eq!(approved_value(None, true, T), None, "no value already means enabled");
        assert_eq!(approved_value(Some(&[]), true, T), None);
        assert_eq!(approved_value(Some(&[2, 0, 0, 0]), true, T), None);
        assert_eq!(approved_value(Some(&[3, 0, 0, 0]), false, T), None);
        let new = approved_value(None, false, T).unwrap();
        assert_eq!(&new[..4], &[3, 0, 0, 0]);
        assert_eq!(&new[4..], &T.to_le_bytes());
    }

    #[test]
    fn filetimes() {
        assert_eq!(filetime(std::time::UNIX_EPOCH), 116_444_736_000_000_000);
        assert_eq!(filetime(std::time::UNIX_EPOCH + std::time::Duration::from_micros(1_500_001)), 116_444_736_015_000_010);
    }

    fn parse(b: &[u8]) -> crate::pro::sysfmt::DesktopEntry {
        crate::pro::sysfmt::parse_desktop_entry(std::str::from_utf8(b).unwrap()).unwrap()
    }

    fn round_trip(text: &str) -> String {
        let off = desktop_disable(text.as_bytes(), DESKTOP_MARK).unwrap();
        let e = parse(&off);
        assert!(e.hidden, "{}", String::from_utf8_lossy(&off));
        let on = desktop_enable(&off).unwrap();
        assert_eq!(String::from_utf8_lossy(&on), text, "disable then enable restores the file");
        String::from_utf8(off).unwrap()
    }

    #[test]
    fn desktop_adds_hidden_and_keeps_everything_else() {
        let text = "# A comment\n[Desktop Entry]\nType=Application\nName=Foo\nName[de]=Fuh\nExec=foo --bg\n\n[Desktop Action New]\nName=New\n";
        let off = round_trip(text);
        assert_eq!(off, format!("# A comment\n[Desktop Entry]\nType=Application\nName=Foo\nName[de]=Fuh\nExec=foo --bg\n{DESKTOP_MARK}\nHidden=true\n\n[Desktop Action New]\nName=New\n"));
        assert_eq!(parse(off.as_bytes()).name, "Foo");
    }

    #[test]
    fn desktop_crlf_and_no_final_line_break() {
        let off = round_trip("[Desktop Entry]\r\nName=Foo\r\nExec=foo");
        assert_eq!(off, format!("[Desktop Entry]\r\nName=Foo\r\nExec=foo\r\n{DESKTOP_MARK}\r\nHidden=true"));
        let off = round_trip("[Desktop Entry]\nName=Foo\nExec=foo");
        assert!(off.ends_with("\nHidden=true"));
        round_trip("[Desktop Entry]\r\nName=Foo\r\n");
        round_trip("[Desktop Entry]");
        round_trip("[Desktop Entry]\n");
    }

    #[test]
    fn desktop_existing_hidden_is_set_in_place() {
        let text = "[Desktop Entry]\nName=Foo\nHidden = false  \nExec=foo\n";
        let off = round_trip(text);
        assert_eq!(off, "[Desktop Entry]\nName=Foo\nHidden = true  \nExec=foo\n");
        assert_eq!(desktop_disable(off.as_bytes(), DESKTOP_MARK).unwrap_err(), "the entry is already turned off (Hidden=true)");
    }

    #[test]
    fn desktop_localized_and_similar_keys_are_not_the_key() {
        // `Hidden[de]` and `HiddenX` are other keys; the group of another section is not read.
        let text = "[Desktop Entry]\nName=Foo\nHidden[de]=false\nHiddenX=true\n[Other]\nHidden=false\n";
        let off = round_trip(text);
        assert!(off.contains(&format!("HiddenX=true\n{DESKTOP_MARK}\nHidden=true\n[Other]\nHidden=false\n")), "{off}");
    }

    #[test]
    fn desktop_user_hidden_and_gnome_switch_are_turned_on_in_place() {
        let text = "[Desktop Entry]\nName=Foo\nHidden=true\nX-GNOME-Autostart-enabled=false\n";
        let on = desktop_enable(text.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(on.clone()).unwrap(), "[Desktop Entry]\nName=Foo\nHidden=false\nX-GNOME-Autostart-enabled=true\n");
        let e = parse(&on);
        assert!(!e.hidden && e.autostart_enabled);
        assert!(desktop_enable(&on).is_err(), "nothing turns it off any more");
        // The mark only counts right above the Hidden line.
        let text = format!("[Desktop Entry]\n{DESKTOP_MARK}\nName=Foo\nHidden=true\n");
        assert_eq!(String::from_utf8(desktop_enable(text.as_bytes()).unwrap()).unwrap(), format!("[Desktop Entry]\n{DESKTOP_MARK}\nName=Foo\nHidden=false\n"));
    }

    #[test]
    fn desktop_without_group_is_refused() {
        assert_eq!(desktop_disable(b"Name=Foo\n", DESKTOP_MARK).unwrap_err(), NO_GROUP);
        assert_eq!(desktop_enable(b"[Other]\nHidden=true\n").unwrap_err(), NO_GROUP);
        assert_eq!(desktop_disable(b"", DESKTOP_MARK).unwrap_err(), NO_GROUP);
    }

    #[test]
    fn desktop_other_bytes_stay_byte_for_byte() {
        // Latin-1 bytes, a tab, a BOM-less odd encoding: untouched.
        let mut text = b"[Desktop Entry]\nName=Caf\xe9\n\tComment=x\n".to_vec();
        text.extend_from_slice(b"Exec=foo\n");
        let off = desktop_disable(&text, DESKTOP_MARK).unwrap();
        assert!(off.starts_with(b"[Desktop Entry]\nName=Caf\xe9\n\tComment=x\nExec=foo\n"));
        assert_eq!(desktop_enable(&off).unwrap(), text);
    }

    /// A fresh folder under the system temp folder for one test.
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tmop-toggle-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn xdg_system_entry_gets_a_copy_that_enable_deletes() {
        let root = temp("copy");
        let (user, sys) = (root.join("user/autostart"), root.join("etc/autostart"));
        std::fs::create_dir_all(&sys).unwrap();
        let system_text = b"[Desktop Entry]\r\nType=Application\r\nName=Foo\r\nExec=foo".to_vec();
        std::fs::write(sys.join("foo.desktop"), &system_text).unwrap();
        xdg_set(&sys.join("foo.desktop"), &user, std::slice::from_ref(&sys), false).unwrap();
        let copy = std::fs::read(user.join("foo.desktop")).unwrap();
        assert!(parse(&copy).hidden);
        assert!(String::from_utf8_lossy(&copy).contains(DESKTOP_COPY_MARK));
        assert_eq!(std::fs::read(sys.join("foo.desktop")).unwrap(), system_text, "the all-users file is not touched");
        xdg_set(&user.join("foo.desktop"), &user, std::slice::from_ref(&sys), true).unwrap();
        assert!(!user.join("foo.desktop").exists(), "TMOP's own copy is deleted");
        assert_eq!(std::fs::read(sys.join("foo.desktop")).unwrap(), system_text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn xdg_edited_copy_is_kept() {
        let root = temp("edited");
        let (user, sys) = (root.join("user"), root.join("sys"));
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("foo.desktop"), "[Desktop Entry]\nName=Foo\nExec=foo\n").unwrap();
        xdg_set(&sys.join("foo.desktop"), &user, std::slice::from_ref(&sys), false).unwrap();
        // The user changes TMOP's copy: it is theirs now, Enable only takes back the switch.
        let p = user.join("foo.desktop");
        let edited = String::from_utf8(std::fs::read(&p).unwrap()).unwrap().replace("Exec=foo", "Exec=foo --quiet");
        std::fs::write(&p, &edited).unwrap();
        xdg_set(&p, &user, std::slice::from_ref(&sys), true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[Desktop Entry]\nName=Foo\nExec=foo --quiet\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn xdg_user_file_is_changed_in_place_and_never_deleted() {
        let root = temp("user");
        let (user, sys) = (root.join("user"), root.join("sys"));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        // The user's own file, the same as the all-users one: still theirs.
        let text = "[Desktop Entry]\nName=Foo\nExec=foo\n";
        std::fs::write(user.join("foo.desktop"), text).unwrap();
        std::fs::write(sys.join("foo.desktop"), text).unwrap();
        let p = user.join("foo.desktop");
        xdg_set(&p, &user, std::slice::from_ref(&sys), false).unwrap();
        assert!(parse(&std::fs::read(&p).unwrap()).hidden);
        xdg_set(&p, &user, std::slice::from_ref(&sys), true).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        assert!(xdg_set(&p, &user, std::slice::from_ref(&sys), true).is_err(), "already on");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn xdg_refusals() {
        let root = temp("refuse");
        let (user, sys) = (root.join("user"), root.join("sys"));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("off.desktop"), "[Desktop Entry]\nName=Off\nHidden=true\n").unwrap();
        assert!(xdg_set(&sys.join("off.desktop"), &user, std::slice::from_ref(&sys), true).is_err());
        std::fs::write(sys.join("a.desktop"), "[Desktop Entry]\nName=A\n").unwrap();
        std::fs::write(user.join("a.desktop"), "mine").unwrap();
        assert!(xdg_set(&sys.join("a.desktop"), &user, std::slice::from_ref(&sys), false).is_err(), "an existing user file is never overwritten");
        assert_eq!(std::fs::read_to_string(user.join("a.desktop")).unwrap(), "mine");
        assert!(xdg_set(&sys.join("missing.desktop"), &user, std::slice::from_ref(&sys), false).is_err());
        assert!(!user.join("missing.desktop").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launchctl_arguments() {
        assert_eq!(launchctl_args(false, "com.example.agent", false, 501), ["disable", "gui/501/com.example.agent"]);
        assert_eq!(launchctl_args(true, "com.example.agent", false, 501), ["enable", "gui/501/com.example.agent"]);
        assert_eq!(launchctl_args(false, "org.example.daemon", true, 0), ["disable", "system/org.example.daemon"]);
    }
}
