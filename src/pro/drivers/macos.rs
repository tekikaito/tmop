//! macOS: services are the launchd jobs (`launchctl list`, `launchctl print system` and the
//! LaunchDaemons / LaunchAgents property lists); drivers are the loaded kernel extensions
//! (`kmutil showloaded`, else `kextstat`). Without root the state of system daemons may be
//! unknown. The parsers are pure and tested on every host.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::Service;
use crate::pro::plist::Value;
use std::collections::HashMap;

/// A job as `launchctl` lists it: PID while it runs, and the last exit status.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub label: String,
    pub pid: Option<u32>,
    pub status: i64,
}

fn listed(pid: &str, status: &str, label: &str) -> Option<Listed> {
    let pid = match pid {
        "-" => None,
        p => Some(p.parse::<u32>().ok()?).filter(|p| *p != 0),
    };
    let status = if status == "-" { 0 } else { status.parse().ok()? };
    Some(Listed { label: label.to_string(), pid, status })
}

/// Parses `launchctl list`: a `PID Status Label` header, then one tab-separated row per job.
pub fn parse_launchctl_list(text: &str) -> Vec<Listed> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (pid, status, label) = (it.next()?, it.next()?, it.next()?);
            listed(pid, status, label)
        })
        .collect()
}

/// The `services = { pid status label }` block of `launchctl print system` (PID 0: not running).
pub fn parse_print_services(text: &str) -> Vec<Listed> {
    let mut out = Vec::new();
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim();
        if !inside {
            inside = t == "services = {";
            continue;
        }
        if t == "}" {
            break;
        }
        let tok: Vec<&str> = t.split_whitespace().collect();
        if let [pid, status, label] = tok[..] {
            out.extend(listed(pid, status, label));
        }
    }
    out
}

/// A launchd job file.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub label: String,
    pub program: String,
    /// "Daemon" or "Agent".
    pub kind: &'static str,
    pub run_at_load: bool,
    pub keep_alive: bool,
    pub disabled: bool,
}

/// The job a LaunchDaemons / LaunchAgents property list describes.
pub fn job_from_plist(v: &Value, kind: &'static str) -> Option<Job> {
    let label = v.get("Label")?.as_str()?.to_string();
    let program = v.get("Program").and_then(Value::as_str).or_else(|| v.get("ProgramArguments")?.as_array()?.first()?.as_str()).unwrap_or("").to_string();
    let keep_alive = match v.get("KeepAlive") {
        Some(Value::Bool(b)) => *b,
        Some(Value::Dict(d)) => !d.is_empty(),
        _ => false,
    };
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(Job { label, program, kind, run_at_load: flag("RunAtLoad"), keep_alive, disabled: flag("Disabled") })
}

/// Last component of a path or a reverse-DNS name.
fn last(s: &str, sep: char) -> &str {
    s.rsplit(sep).next().unwrap_or(s)
}

/// The Services rows: every job file, plus listed jobs without one. `system_known` says whether
/// the system domain was listed (root, or `launchctl print system` worked); without it a daemon
/// that is not listed is "Unknown" rather than "Not loaded".
pub fn build_launchd(jobs: &[Job], listed: &[Listed], system_known: bool) -> Vec<Service> {
    let by_label: HashMap<&str, &Listed> = listed.iter().map(|l| (l.label.as_str(), l)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let row = |label: &str, job: Option<&Job>| {
        let l = by_label.get(label);
        let (state, running) = match l {
            Some(Listed { pid: Some(_), .. }) => ("Running", true),
            Some(Listed { status, .. }) if *status != 0 => ("Failed", false),
            Some(_) => ("Loaded", false),
            None if system_known || job.is_some_and(|j| j.kind == "Agent") => ("Not loaded", false),
            None => ("Unknown", false),
        };
        let start = match job {
            Some(j) if j.disabled => "Disabled",
            Some(j) if j.run_at_load || j.keep_alive => "At load",
            Some(_) => "On demand",
            None => "",
        };
        let program = job.map(|j| j.program.as_str()).unwrap_or("");
        Service {
            name: label.to_string(),
            display: if program.is_empty() { last(label, '.') } else { last(program, '/') }.to_string(),
            state,
            running,
            start: start.into(),
            kind: job.map(|j| j.kind).unwrap_or("Job").into(),
            pid: l.and_then(|l| l.pid).unwrap_or(0),
            path: program.to_string(),
            third_party: !label.starts_with("com.apple."),
            ..Default::default()
        }
    };
    for j in jobs {
        if seen.insert(j.label.clone()) {
            out.push(row(&j.label, Some(j)));
        }
    }
    for l in listed {
        if seen.insert(l.label.clone()) {
            out.push(row(&l.label, None));
        }
    }
    super::sort(&mut out);
    out
}

/// A loaded kernel extension.
#[derive(Clone, Debug, PartialEq)]
pub struct Kext {
    pub id: String,
    pub version: String,
    pub refs: u32,
    /// Part of the kernel itself (load address 0): the KPI pseudo-extensions.
    pub builtin: bool,
}

/// Parses `kmutil showloaded` or `kextstat`: `Index Refs Address Size Wired Name (Version) ...`.
pub fn parse_kmutil(text: &str) -> Vec<Kext> {
    text.lines()
        .filter_map(|l| {
            let tok: Vec<&str> = l.split_whitespace().collect();
            if tok.len() < 7 || tok[0].parse::<u32>().is_err() {
                return None;
            }
            let version = tok[6].strip_prefix('(').and_then(|v| v.strip_suffix(')')).unwrap_or("");
            Some(Kext { id: tok[5].to_string(), version: version.to_string(), refs: tok[1].parse().unwrap_or(0), builtin: matches!(tok[2], "0" | "0x0") })
        })
        .collect()
}

/// The maker of a bundle id: "Apple" for `com.apple.*`, else its organisation part.
pub fn vendor(id: &str) -> String {
    if id.starts_with("com.apple.") {
        return "Apple".into();
    }
    id.split('.').nth(1).unwrap_or("").to_string()
}

/// The Drivers rows of the loaded extensions; `paths` maps bundle ids to their `.kext`.
pub fn kexts_to_services(kexts: &[Kext], paths: &HashMap<String, String>) -> Vec<Service> {
    let mut out: Vec<Service> = kexts
        .iter()
        .map(|k| Service {
            name: k.id.clone(),
            display: last(&k.id, '.').to_string(),
            state: "Loaded",
            running: true,
            start: if k.refs > 0 { "In use" } else { "Unused" }.into(),
            kind: if k.builtin { "Built in" } else { "Extension" }.into(),
            pid: 0,
            path: paths.get(&k.id).cloned().unwrap_or_default(),
            company: vendor(&k.id),
            version: k.version.clone(),
            description: String::new(),
            third_party: !k.id.starts_with("com.apple."),
        })
        .collect();
    super::sort(&mut out);
    out
}

#[cfg(target_os = "macos")]
mod io {
    use super::*;
    use crate::platform::run_capture;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(20);
    /// Property lists larger than this are not job files.
    const MAX_PLIST: u64 = 1 << 20;

    fn read_plist(path: &Path) -> Option<Value> {
        if std::fs::metadata(path).ok()?.len() > MAX_PLIST {
            return None;
        }
        crate::pro::plist::parse(&std::fs::read(path).ok()?)
    }

    pub fn services() -> Result<Vec<Service>, String> {
        let mut listed = parse_launchctl_list(&run_capture("launchctl", &["list"], TIMEOUT)?);
        let system = run_capture("launchctl", &["print", "system"], TIMEOUT).map(|t| parse_print_services(&t)).unwrap_or_default();
        let system_known = crate::platform::is_elevated() || !system.is_empty();
        for s in system {
            if !listed.iter().any(|l| l.label == s.label) {
                listed.push(s);
            }
        }
        let mut dirs: Vec<(PathBuf, &'static str)> = vec![
            ("/System/Library/LaunchDaemons".into(), "Daemon"),
            ("/Library/LaunchDaemons".into(), "Daemon"),
            ("/System/Library/LaunchAgents".into(), "Agent"),
            ("/Library/LaunchAgents".into(), "Agent"),
        ];
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute()) {
            dirs.push((home.join("Library/LaunchAgents"), "Agent"));
        }
        let mut jobs = Vec::new();
        for (dir, kind) in dirs {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "plist")).collect();
            files.sort();
            jobs.extend(files.iter().filter_map(|p| job_from_plist(&read_plist(p)?, kind)));
        }
        Ok(build_launchd(&jobs, &listed, system_known))
    }

    /// Bundle id -> `.kext` folder, for the extension folders and their PlugIns.
    fn kext_paths() -> HashMap<String, String> {
        let mut out = HashMap::new();
        let mut stack: Vec<PathBuf> = vec!["/System/Library/Extensions".into(), "/Library/Extensions".into()];
        let mut depth_guard = 0;
        while let Some(dir) = stack.pop() {
            depth_guard += 1;
            if depth_guard > 10_000 {
                break;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "kext") {
                    continue;
                }
                if let Some(id) = read_plist(&p.join("Contents/Info.plist")).and_then(|v| v.get("CFBundleIdentifier")?.as_str().map(String::from)) {
                    out.entry(id).or_insert_with(|| p.to_string_lossy().into_owned());
                }
                stack.push(p.join("Contents/PlugIns"));
            }
        }
        out
    }

    pub fn kexts() -> Result<Vec<Service>, String> {
        let text = run_capture("kmutil", &["showloaded"], TIMEOUT).or_else(|e| run_capture("kextstat", &[], TIMEOUT).map_err(|e2| format!("{e}; {e2}")))?;
        let kexts = parse_kmutil(&text);
        if kexts.is_empty() {
            return Err("kmutil listed no kernel extensions".into());
        }
        Ok(kexts_to_services(&kexts, &kext_paths()))
    }
}

#[cfg(target_os = "macos")]
pub use io::{kexts, services};

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "PID\tStatus\tLabel\n-\t0\tcom.apple.SafariHistoryServiceAgent\n512\t0\tcom.apple.Finder\n-\t-9\tcom.vendor.helper\n-\t78\tcom.apple.ReportCrash\n";

    const PRINT: &str = "system = {
\ttype = system
\tservices = {
\t\t       0      -\tcom.apple.wifivelocityd
\t\t     372      -\tcom.apple.mDNSResponder
\t\t       0     78\tcom.apple.ReportCrash.Root
\t}
\tendpoints = {
\t\t       0      -\tnot.a.service
\t}
}";

    #[test]
    fn parses_launchctl() {
        let l = parse_launchctl_list(LIST);
        assert_eq!(l.len(), 4, "the header is skipped");
        assert_eq!(l[1], Listed { label: "com.apple.Finder".into(), pid: Some(512), status: 0 });
        assert_eq!(l[2], Listed { label: "com.vendor.helper".into(), pid: None, status: -9 });
        let p = parse_print_services(PRINT);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], Listed { label: "com.apple.wifivelocityd".into(), pid: None, status: 0 });
        assert_eq!((p[1].pid, p[2].status), (Some(372), 78));
    }

    fn job(label: &str, program: &str, kind: &'static str, run_at_load: bool, disabled: bool) -> Job {
        Job { label: label.into(), program: program.into(), kind, run_at_load, keep_alive: false, disabled }
    }

    #[test]
    fn reads_job_files() {
        let xml = br#"<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>com.vendor.helper</string>
<key>ProgramArguments</key><array><string>/Library/Vendor/helper</string><string>-d</string></array>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict></dict></plist>"#;
        let j = job_from_plist(&crate::pro::plist::parse(xml).unwrap(), "Daemon").unwrap();
        assert_eq!(j, Job { label: "com.vendor.helper".into(), program: "/Library/Vendor/helper".into(), kind: "Daemon", run_at_load: false, keep_alive: true, disabled: false });
        assert_eq!(job_from_plist(&crate::pro::plist::parse(b"<plist><dict></dict></plist>").unwrap(), "Agent"), None);
    }

    #[test]
    fn builds_launchd_services() {
        let jobs = vec![
            job("com.apple.Finder", "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder", "Agent", true, false),
            job("com.vendor.helper", "/Library/Vendor/helper", "Daemon", false, false),
            job("com.apple.ReportCrash", "/System/Library/CoreServices/ReportCrash", "Agent", false, false),
            job("com.apple.mDNSResponder", "/usr/sbin/mDNSResponder", "Daemon", true, false),
            job("com.apple.off", "/usr/libexec/off", "Daemon", true, true),
            job("com.apple.Finder", "/duplicate", "Agent", false, false),
        ];
        let s = build_launchd(&jobs, &parse_launchctl_list(LIST), false);
        let get = |n: &str| s.iter().find(|x| x.name == n).unwrap_or_else(|| panic!("{n} missing"));
        let f = get("com.apple.Finder");
        assert_eq!((f.state, f.running, f.pid, f.start.as_str(), f.kind.as_str(), f.display.as_str(), f.third_party), ("Running", true, 512, "At load", "Agent", "Finder", false));
        let h = get("com.vendor.helper");
        assert_eq!((h.state, h.start.as_str(), h.path.as_str(), h.third_party), ("Failed", "On demand", "/Library/Vendor/helper", true));
        assert_eq!(get("com.apple.ReportCrash").state, "Failed");
        // A daemon the user's listing cannot see.
        assert_eq!((get("com.apple.mDNSResponder").state, get("com.apple.mDNSResponder").start.as_str()), ("Unknown", "At load"));
        assert_eq!(get("com.apple.off").start, "Disabled");
        let only = get("com.apple.SafariHistoryServiceAgent");
        assert_eq!((only.state, only.kind.as_str(), only.display.as_str(), only.start.as_str()), ("Loaded", "Job", "SafariHistoryServiceAgent", ""));
        assert_eq!(s.iter().filter(|x| x.name == "com.apple.Finder").count(), 1);
        assert_eq!(s.len(), 6);
        assert_eq!(s[0].name, "com.apple.Finder", "running first");
        let known = build_launchd(&jobs, &[], true);
        assert_eq!(known.iter().find(|x| x.name == "com.apple.mDNSResponder").unwrap().state, "Not loaded");
    }

    const KMUTIL: &str = "\
No variant specified, falling back to release
Index Refs Address            Size       Wired      Name (Version) UUID <Linked Against>
    1  163 0                  0          0          com.apple.kpi.bsd (21.6.0) 5A9F9DFA-1111-2222-3333-444455556666 <>
   46    2 0xffffff8001234000 0x5000     0x5000     com.apple.iokit.IOSMBusFamily (1.1) 1D2B0000-1111-2222-3333-444455556666 <7 6 5 3>
  187    0 0xffffff7f84a3c000 0x31000    0x31000    org.virtualbox.kext.VBoxDrv (7.0.10) 0E2D0000-1111-2222-3333-444455556666 <8 6 5 3 1>
";

    /// Third party on macOS means a bundle id outside com.apple.
    #[test]
    fn maps_kexts() {
        let k = parse_kmutil(KMUTIL);
        assert_eq!(k.len(), 3);
        assert_eq!(k[0], Kext { id: "com.apple.kpi.bsd".into(), version: "21.6.0".into(), refs: 163, builtin: true });
        let paths = HashMap::from([("org.virtualbox.kext.VBoxDrv".to_string(), "/Library/Extensions/VBoxDrv.kext".to_string())]);
        let s = kexts_to_services(&k, &paths);
        let vbox = s.iter().find(|x| x.name == "org.virtualbox.kext.VBoxDrv").unwrap();
        assert_eq!((vbox.display.as_str(), vbox.start.as_str(), vbox.kind.as_str(), vbox.company.as_str(), vbox.version.as_str(), vbox.third_party), ("VBoxDrv", "Unused", "Extension", "virtualbox", "7.0.10", true));
        assert_eq!(vbox.path, "/Library/Extensions/VBoxDrv.kext");
        let bsd = s.iter().find(|x| x.name == "com.apple.kpi.bsd").unwrap();
        assert_eq!((bsd.kind.as_str(), bsd.company.as_str(), bsd.start.as_str(), bsd.third_party), ("Built in", "Apple", "In use", false));
        assert_eq!(vendor("io"), "");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lists_live_jobs_and_kexts() {
        let s = services().expect("launchctl list");
        assert!(s.iter().any(|x| x.running && x.pid != 0), "a running job");
        assert!(s.iter().any(|x| x.kind == "Daemon"), "LaunchDaemons are read");
        let k = kexts().expect("kmutil showloaded");
        assert!(k.iter().any(|x| x.name.starts_with("com.apple.")));
    }
}
