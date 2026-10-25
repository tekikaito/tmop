//! Linux: services are the systemd service units (`systemctl list-units` / `list-unit-files`
//! as JSON, details from one `systemctl show`); drivers are the loaded kernel modules
//! (`/proc/modules` plus one batched `modinfo`). The parsers are pure and tested on every host.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::Service;
use std::collections::HashMap;

/// One line of `/proc/modules`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Module {
    pub name: String,
    pub size: u64,
    /// Reference count.
    pub refs: u32,
    /// Modules that use this one.
    pub used_by: Vec<String>,
    /// "Live", "Loading" or "Unloading".
    pub state: String,
    /// Taint letters, e.g. "OE" (out of tree, unsigned) or "P" (proprietary).
    pub taint: String,
}

/// Parses `/proc/modules`: `name size refcount used_by state address [(taint)]`.
pub fn parse_proc_modules(text: &str) -> Vec<Module> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?.to_string();
            let size = it.next()?.parse().ok()?;
            // "-" when the module cannot be unloaded.
            let refs = it.next().and_then(|r| r.parse().ok()).unwrap_or(0);
            let used_by = it.next().unwrap_or("-").split(',').filter(|d| !d.is_empty() && *d != "-").map(String::from).collect();
            let state = it.next().unwrap_or("Live").to_string();
            let _address = it.next();
            let taint = it.next().map(|t| t.trim_matches(|c| c == '(' || c == ')').to_string()).unwrap_or_default();
            Some(Module { name, size, refs, used_by, state, taint })
        })
        .collect()
}

/// The fields of one `modinfo` block (the first value of each key).
pub type ModInfo = HashMap<String, String>;

/// Splits the output of `modinfo a b c` into one map per module. A block starts where
/// `filename` or `name` repeats.
pub fn parse_modinfo(text: &str) -> Vec<ModInfo> {
    let mut out = Vec::new();
    let mut cur = ModInfo::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim();
        if k.is_empty() || k.contains(char::is_whitespace) {
            continue;
        }
        if (k == "filename" || k == "name") && cur.contains_key(k) {
            out.push(std::mem::take(&mut cur));
        }
        cur.entry(k.to_string()).or_insert_with(|| v.trim().to_string());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Module names treat `-` and `_` alike.
fn module_key(name: &str) -> String {
    name.replace('-', "_")
}

/// The module a `modinfo` block describes: its `name`, else its file name up to the first dot.
fn modinfo_key(info: &ModInfo) -> Option<String> {
    let name = info.get("name").cloned().or_else(|| {
        let file = info.get("filename")?;
        let base = file.rsplit('/').next()?;
        Some(base.split('.').next()?.to_string())
    })?;
    Some(module_key(&name)).filter(|n| !n.is_empty())
}

/// The Drivers rows of the loaded modules, with what `modinfo` knows about each.
pub fn modules_to_services(mods: &[Module], infos: &[ModInfo]) -> Vec<Service> {
    let by_name: HashMap<String, &ModInfo> = infos.iter().filter_map(|i| Some((modinfo_key(i)?, i))).collect();
    let mut out: Vec<Service> = mods
        .iter()
        .map(|m| {
            let info = by_name.get(&module_key(&m.name));
            let field = |k: &str| info.and_then(|i| i.get(k)).cloned().unwrap_or_default();
            let path = field("filename");
            let version = Some(field("version")).filter(|v| !v.is_empty()).unwrap_or_else(|| field("vermagic").split_whitespace().next().unwrap_or("").to_string());
            let (kind, third_party) = if m.taint.contains('P') {
                ("Proprietary", true)
            } else if m.taint.contains('O') {
                ("Out of tree", true)
            } else if m.taint.contains('C') {
                ("Staging", false)
            } else {
                ("In tree", false)
            };
            let (state, running) = match m.state.as_str() {
                "Live" => ("Loaded", true),
                "Loading" => ("Loading", false),
                "Unloading" => ("Unloading", false),
                _ => ("Unknown", false),
            };
            Service {
                name: m.name.clone(),
                display: m.name.clone(),
                state,
                running,
                start: if m.refs > 0 || !m.used_by.is_empty() { "In use" } else { "Unused" }.into(),
                kind: kind.into(),
                pid: 0,
                path: if path.starts_with('/') { path } else { String::new() },
                company: field("author"),
                version,
                description: field("description"),
                third_party,
            }
        })
        .collect();
    super::sort(&mut out);
    out
}

/// One row of `systemctl list-units --output=json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unit {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

fn json_rows(json: &str) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| format!("systemctl JSON: {e}"))?;
    let rows = v.as_array().ok_or("systemctl JSON: not a list")?;
    Ok(rows.iter().filter_map(|r| r.as_object().cloned()).collect())
}

fn json_str(row: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Parses `systemctl list-units --type=service --all --output=json` (systemd 246 and later).
pub fn parse_units_json(json: &str) -> Result<Vec<Unit>, String> {
    Ok(json_rows(json)?
        .iter()
        .map(|r| Unit { unit: json_str(r, "unit"), load: json_str(r, "load"), active: json_str(r, "active"), sub: json_str(r, "sub"), description: json_str(r, "description") })
        .filter(|u| !u.unit.is_empty())
        .collect())
}

/// Parses `systemctl list-unit-files --type=service --output=json` into (unit file, state).
pub fn parse_unit_files_json(json: &str) -> Result<Vec<(String, String)>, String> {
    Ok(json_rows(json)?.iter().map(|r| (json_str(r, "unit_file"), json_str(r, "state"))).filter(|(u, _)| !u.is_empty()).collect())
}

/// Splits `systemctl show` output (`Key=value` lines, one blank line between units) into maps.
pub fn parse_show(text: &str) -> Vec<HashMap<String, String>> {
    let mut out = Vec::new();
    let mut cur = HashMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            cur.insert(k.to_string(), v.to_string());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The program of an `ExecStart` property (`{ path=/usr/sbin/cron ; argv[]=... }`).
pub fn exec_path(exec: &str) -> Option<String> {
    let rest = &exec[exec.find("path=")? + 5..];
    let p = rest.split(" ;").next()?.trim();
    (!p.is_empty()).then(|| p.to_string())
}

/// The units the old `--plain` route would list, built from `systemctl show` when JSON is not
/// available (systemd before 246).
pub fn units_from_show(show: &[HashMap<String, String>]) -> Vec<Unit> {
    show.iter()
        .filter_map(|s| {
            let f = |k: &str| s.get(k).cloned().unwrap_or_default();
            Some(Unit { unit: s.get("Id")?.clone(), load: f("LoadState"), active: f("ActiveState"), sub: f("SubState"), description: f("Description") })
        })
        .collect()
}

/// "Running", "Exited", "Failed" ... and whether the unit counts as running. An active one-shot
/// that has exited has no process left, so it does not.
pub fn unit_state(active: &str, sub: &str) -> (&'static str, bool) {
    match (active, sub) {
        ("active", "running") => ("Running", true),
        ("active", "exited") => ("Exited", false),
        ("active", "reload") | ("reloading", _) => ("Reloading", true),
        ("active", _) => ("Active", true),
        ("activating", _) => ("Starting", false),
        ("deactivating", _) => ("Stopping", false),
        ("inactive", _) => ("Stopped", false),
        ("failed", _) => ("Failed", false),
        ("maintenance", _) => ("Maintenance", false),
        _ => ("Unknown", false),
    }
}

/// The start column for a unit-file state.
pub fn start_word(state: &str) -> String {
    match state {
        "enabled" | "enabled-runtime" => "Enabled",
        "disabled" => "Disabled",
        "static" => "Static",
        "masked" | "masked-runtime" => "Masked",
        "indirect" => "Indirect",
        "generated" => "Generated",
        "transient" => "Transient",
        "alias" => "Alias",
        "linked" | "linked-runtime" => "Linked",
        "" => "",
        other => return other.to_string(),
    }
    .into()
}

/// The type column for a service `Type=`.
fn type_word(t: &str) -> String {
    match t {
        "simple" => "Simple",
        "exec" => "Exec",
        "forking" => "Forking",
        "oneshot" => "One-shot",
        "dbus" => "D-Bus",
        "notify" => "Notify",
        "notify-reload" => "Notify (reload)",
        "idle" => "Idle",
        other => return other.to_string(),
    }
    .into()
}

/// "Documentation: man:cron(8), https://..." from `Documentation=` (quoted, space separated).
fn docs_sentence(docs: &str, fragment: &str) -> String {
    let links: Vec<&str> = docs.split_whitespace().map(|d| d.trim_matches('"')).filter(|d| !d.is_empty()).collect();
    let mut parts = Vec::new();
    if !fragment.is_empty() {
        parts.push(format!("Unit file: {fragment}."));
    }
    if !links.is_empty() {
        parts.push(format!("Documentation: {}.", links.join(", ")));
    }
    parts.join(" ")
}

/// The Services rows: the loaded units, plus installed unit files that are not loaded (not
/// templates), with type, main PID, program and documentation from `systemctl show`.
pub fn build_services(units: &[Unit], files: &[(String, String)], show: &[HashMap<String, String>]) -> Vec<Service> {
    let details: HashMap<&str, &HashMap<String, String>> = show.iter().filter_map(|s| Some((s.get("Id")?.as_str(), s))).collect();
    let file_state: HashMap<&str, &str> = files.iter().map(|(u, s)| (u.as_str(), s.as_str())).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut add = |unit: &str, load: &str, active: &str, sub: &str, description: &str| {
        if !seen.insert(unit.to_string()) {
            return;
        }
        let d = details.get(unit);
        let f = |k: &str| d.and_then(|d| d.get(k)).map(String::as_str).unwrap_or("");
        let (state, running) = unit_state(active, sub);
        let start = if load == "masked" { "Masked".to_string() } else { start_word(file_state.get(unit).copied().unwrap_or_else(|| f("UnitFileState"))) };
        let fragment = f("FragmentPath");
        out.push(Service {
            name: unit.strip_suffix(".service").unwrap_or(unit).to_string(),
            display: description.to_string(),
            state,
            running,
            start,
            kind: type_word(f("Type")),
            pid: f("MainPID").parse().unwrap_or(0),
            path: exec_path(f("ExecStart")).unwrap_or_default(),
            description: docs_sentence(f("Documentation"), fragment),
            ..Default::default()
        });
    };
    for u in units {
        // References to units that do not exist, unless something is still running under that name.
        if u.load == "not-found" && matches!(u.active.as_str(), "inactive" | "") {
            continue;
        }
        add(&u.unit, &u.load, &u.active, &u.sub, &u.description);
    }
    for (unit, _) in files {
        if unit.ends_with("@.service") || !unit.ends_with(".service") {
            continue;
        }
        add(unit, "", "inactive", "dead", "");
    }
    super::sort(&mut out);
    out
}

#[cfg(target_os = "linux")]
mod io {
    use super::*;
    use crate::platform::run_capture;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(15);
    const SHOW_PROPS: &str = "Id,Type,MainPID,ExecStart,FragmentPath,Documentation,LoadState,ActiveState,SubState,Description,UnitFileState";

    pub fn services() -> Result<Vec<Service>, String> {
        // sd_booted(): systemd is PID 1 exactly when this directory exists.
        if !std::path::Path::new("/run/systemd/system").is_dir() {
            return Err("This system does not run systemd. TMOP lists services through systemctl.".into());
        }
        let show_all = || run_capture("systemctl", &["show", "--type=service", "--all", "--no-pager", "-p", SHOW_PROPS], TIMEOUT).map(|t| parse_show(&t));
        // `systemctl show` without unit names lists the units only on newer systemd (249, as on
        // Ubuntu 22.04, prints the manager instead), so the loaded units are named explicitly.
        let (units, show) = match run_capture("systemctl", &["list-units", "--type=service", "--all", "--output=json", "--no-pager"], TIMEOUT).and_then(|j| parse_units_json(&j)) {
            Ok(units) if !units.is_empty() => {
                let mut args = vec!["show", "--no-pager", "-p", SHOW_PROPS, "--"];
                args.extend(units.iter().map(|u| u.unit.as_str()));
                let show = run_capture("systemctl", &args, TIMEOUT).map(|t| parse_show(&t)).or_else(|_| show_all())?;
                (units, show)
            }
            _ => {
                let show = show_all()?;
                (units_from_show(&show), show)
            }
        };
        let files = run_capture("systemctl", &["list-unit-files", "--type=service", "--output=json", "--no-pager"], TIMEOUT).and_then(|j| parse_unit_files_json(&j)).unwrap_or_default();
        Ok(build_services(&units, &files, &show))
    }

    pub fn modules() -> Result<Vec<Service>, String> {
        let text = std::fs::read_to_string("/proc/modules").map_err(|e| format!("/proc/modules: {e}"))?;
        let mods = parse_proc_modules(&text);
        if mods.is_empty() {
            // The page would otherwise keep saying it is reading.
            return Err("No loadable kernel modules are loaded: this kernel has its drivers built in.".into());
        }
        let names: Vec<&str> = mods.iter().map(|m| m.name.as_str()).collect();
        let infos = match run_capture("modinfo", &names, TIMEOUT) {
            Ok(out) => parse_modinfo(&out),
            // One module modinfo cannot find fails the batch: ask one by one.
            Err(_) => names.iter().filter_map(|n| run_capture("modinfo", &[n], Duration::from_secs(3)).ok()).flat_map(|o| parse_modinfo(&o)).collect(),
        };
        Ok(modules_to_services(&mods, &infos))
    }
}

#[cfg(target_os = "linux")]
pub use io::{modules, services};

#[cfg(test)]
mod tests {
    use super::*;

    const PROC_MODULES: &str = "\
nvidia_uvm 1867776 2 - Live 0x0000000000000000 (POE)
nvidia 56717312 1234 nvidia_uvm,nvidia_modeset, Live 0xffffffffc1000000 (POE)
snd_hda_intel 57344 3 - Live 0xffffffffc0b4f000
vboxdrv 696320 0 - Loading 0xffffffffc0a00000 (OE)
r8188eu 450560 0 - Live 0xffffffffc0900000 (C)
bad line
";

    const MODINFO: &str = "\
filename:       /lib/modules/6.8.0-45-generic/kernel/sound/pci/hda/snd-hda-intel.ko.zst
description:    Intel HDA driver
license:        GPL
author:         Takashi Iwai
srcversion:     0A1B2C
depends:        snd-hda-codec
intree:         Y
name:           snd_hda_intel
vermagic:       6.8.0-45-generic SMP preempt mod_unload modversions
parm:           index:Index value for Intel HD audio interface. (array of int)
filename:       /lib/modules/6.8.0-45-generic/updates/dkms/nvidia.ko.zst
alias:          char-major-195-*
version:        550.120
supported:      external
license:        NVIDIA
name:           nvidia
filename:       (builtin)
";

    #[test]
    fn parses_proc_modules() {
        let m = parse_proc_modules(PROC_MODULES);
        assert_eq!(m.len(), 5);
        assert_eq!(m[1], Module { name: "nvidia".into(), size: 56717312, refs: 1234, used_by: vec!["nvidia_uvm".into(), "nvidia_modeset".into()], state: "Live".into(), taint: "POE".into() });
        assert_eq!((m[2].refs, m[2].used_by.len(), m[2].taint.as_str()), (3, 0, ""));
        assert_eq!(m[3].state, "Loading");
    }

    #[test]
    fn splits_modinfo_blocks() {
        let infos = parse_modinfo(MODINFO);
        assert_eq!(infos.len(), 3);
        assert_eq!(infos[0].get("parm").map(String::as_str), Some("index:Index value for Intel HD audio interface. (array of int)"));
        assert_eq!(modinfo_key(&infos[0]).as_deref(), Some("snd_hda_intel"));
        assert_eq!(modinfo_key(&infos[1]).as_deref(), Some("nvidia"));
        let mut no_name = infos[0].clone();
        no_name.remove("name");
        assert_eq!(modinfo_key(&no_name).as_deref(), Some("snd_hda_intel"), "from the file name, dashes as underscores");
    }

    /// Third party on Linux means out of tree or proprietary, from the taint flags.
    #[test]
    fn maps_modules() {
        let s = modules_to_services(&parse_proc_modules(PROC_MODULES), &parse_modinfo(MODINFO));
        let get = |n: &str| s.iter().find(|x| x.name == n).unwrap();
        let hda = get("snd_hda_intel");
        assert_eq!((hda.state, hda.running, hda.start.as_str(), hda.kind.as_str(), hda.third_party), ("Loaded", true, "In use", "In tree", false));
        assert_eq!((hda.description.as_str(), hda.company.as_str(), hda.version.as_str()), ("Intel HDA driver", "Takashi Iwai", "6.8.0-45-generic"));
        assert!(hda.path.ends_with("snd-hda-intel.ko.zst"));
        let nv = get("nvidia");
        assert_eq!((nv.kind.as_str(), nv.third_party, nv.version.as_str()), ("Proprietary", true, "550.120"));
        let vbox = get("vboxdrv");
        assert_eq!((vbox.state, vbox.running, vbox.start.as_str(), vbox.kind.as_str(), vbox.third_party), ("Loading", false, "Unused", "Out of tree", true));
        assert_eq!((vbox.path.as_str(), vbox.description.as_str()), ("", ""));
        assert_eq!((get("r8188eu").kind.as_str(), get("r8188eu").third_party), ("Staging", false));
        assert!(get("nvidia_uvm").third_party && get("nvidia_uvm").start == "In use");
        assert_eq!(s.iter().take_while(|x| x.running).count(), 4, "running first");
    }

    const UNITS_JSON: &str = r#"[
{"unit":"cron.service","load":"loaded","active":"active","sub":"running","description":"Regular background program processing daemon"},
{"unit":"apparmor.service","load":"loaded","active":"active","sub":"exited","description":"Load AppArmor profiles"},
{"unit":"auditd.service","load":"not-found","active":"inactive","sub":"dead","description":"auditd.service"},
{"unit":"broken.service","load":"loaded","active":"failed","sub":"failed","description":"Broken thing"},
{"unit":"getty@tty1.service","load":"loaded","active":"activating","sub":"start","description":"Getty on tty1"},
{"unit":"nfs.service","load":"masked","active":"inactive","sub":"dead","description":"nfs.service"}
]"#;

    const FILES_JSON: &str = r#"[
{"unit_file":"cron.service","state":"enabled","preset":"enabled"},
{"unit_file":"apparmor.service","state":"enabled","preset":"enabled"},
{"unit_file":"broken.service","state":"disabled","preset":null},
{"unit_file":"getty@.service","state":"enabled","preset":"enabled"},
{"unit_file":"rsync.service","state":"disabled","preset":"enabled"},
{"unit_file":"systemd-fsck-root.service","state":"static","preset":null}
]"#;

    const SHOW: &str = "\
Type=simple
MainPID=207
ExecStart={ path=/usr/sbin/cron ; argv[]=/usr/sbin/cron -f -P $EXTRA_OPTS ; ignore_errors=no ; start_time=[Sun 2026-10-25 12:02:15 CET] ; stop_time=[n/a] ; pid=207 ; code=(null) ; status=0/0 }
Id=cron.service
Documentation=\"man:cron(8)\" https://example.org/cron
FragmentPath=/usr/lib/systemd/system/cron.service
LoadState=loaded
ActiveState=active
SubState=running
Description=Regular background program processing daemon
UnitFileState=enabled

Type=oneshot
MainPID=0
ExecStart={ path=/lib/apparmor/apparmor.systemd ; argv[]=/lib/apparmor/apparmor.systemd reload ; ignore_errors=no }
Id=apparmor.service
Documentation=
FragmentPath=/usr/lib/systemd/system/apparmor.service
LoadState=loaded
ActiveState=active
SubState=exited
Description=Load AppArmor profiles
UnitFileState=enabled

Type=
MainPID=0
Id=getty@tty1.service
FragmentPath=/usr/lib/systemd/system/getty@.service
LoadState=loaded
ActiveState=activating
SubState=start
Description=Getty on tty1
UnitFileState=enabled
";

    #[test]
    fn parses_systemctl_json() {
        let u = parse_units_json(UNITS_JSON).unwrap();
        assert_eq!(u.len(), 6);
        assert_eq!(u[0], Unit { unit: "cron.service".into(), load: "loaded".into(), active: "active".into(), sub: "running".into(), description: "Regular background program processing daemon".into() });
        let f = parse_unit_files_json(FILES_JSON).unwrap();
        assert_eq!(f[4], ("rsync.service".to_string(), "disabled".to_string()));
        assert!(parse_units_json("Unknown output 'json'.").is_err());
        assert!(parse_units_json("{}").is_err());
    }

    #[test]
    fn parses_show_and_exec_start() {
        let s = parse_show(SHOW);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].get("MainPID").map(String::as_str), Some("207"));
        assert_eq!(exec_path(&s[0]["ExecStart"]).as_deref(), Some("/usr/sbin/cron"));
        assert_eq!(exec_path(""), None);
        assert_eq!(units_from_show(&s)[1], Unit { unit: "apparmor.service".into(), load: "loaded".into(), active: "active".into(), sub: "exited".into(), description: "Load AppArmor profiles".into() });
    }

    #[test]
    fn builds_services() {
        let s = build_services(&parse_units_json(UNITS_JSON).unwrap(), &parse_unit_files_json(FILES_JSON).unwrap(), &parse_show(SHOW));
        let get = |n: &str| s.iter().find(|x| x.name == n).unwrap_or_else(|| panic!("{n} missing"));
        let cron = get("cron");
        assert_eq!((cron.state, cron.running, cron.start.as_str(), cron.kind.as_str(), cron.pid, cron.path.as_str()), ("Running", true, "Enabled", "Simple", 207, "/usr/sbin/cron"));
        assert_eq!(cron.display, "Regular background program processing daemon");
        assert_eq!(cron.description, "Unit file: /usr/lib/systemd/system/cron.service. Documentation: man:cron(8), https://example.org/cron.");
        let aa = get("apparmor");
        assert_eq!((aa.state, aa.running, aa.kind.as_str(), aa.pid), ("Exited", false, "One-shot", 0));
        assert_eq!(aa.description, "Unit file: /usr/lib/systemd/system/apparmor.service.");
        assert_eq!((get("broken").state, get("broken").start.as_str()), ("Failed", "Disabled"));
        // An instance takes its start mode from `systemctl show` (the file is the template).
        assert_eq!((get("getty@tty1").state, get("getty@tty1").start.as_str()), ("Starting", "Enabled"));
        assert_eq!(get("nfs").start, "Masked");
        let rsync = get("rsync");
        assert_eq!((rsync.state, rsync.running, rsync.start.as_str(), rsync.display.as_str()), ("Stopped", false, "Disabled", ""));
        assert_eq!(get("systemd-fsck-root").start, "Static");
        assert!(!s.iter().any(|x| x.name == "auditd" || x.name.ends_with('@')), "not-found units and templates are left out");
        assert_eq!(s.len(), 7);
        assert_eq!(s[0].name, "cron", "running first");
        assert_eq!(s.iter().filter(|x| x.running).count(), 1);
    }

    #[test]
    fn words() {
        assert_eq!(unit_state("active", "running"), ("Running", true));
        assert_eq!(unit_state("reloading", "reload"), ("Reloading", true));
        assert_eq!(unit_state("weird", ""), ("Unknown", false));
        assert_eq!((start_word("enabled-runtime"), start_word("bad"), start_word("")), ("Enabled".to_string(), "bad".to_string(), String::new()));
        assert_eq!((type_word("notify-reload"), type_word("x")), ("Notify (reload)".to_string(), "x".to_string()));
    }

    /// On a systemd host the live list has a running service with a main PID; elsewhere (WSL
    /// without systemd, containers) reading fails with a clear text instead.
    #[cfg(target_os = "linux")]
    #[test]
    fn lists_live_services_and_modules() {
        match services() {
            Ok(s) => {
                assert!(!s.is_empty());
                assert!(s.iter().any(|x| x.running && x.pid != 0), "a running service with a main PID");
            }
            Err(e) => assert!(!std::path::Path::new("/run/systemd/system").is_dir(), "systemd is running but: {e}"),
        }
        // WSL kernels have every driver built in: then reading says so instead of listing nothing.
        let loadable = std::fs::read_to_string("/proc/modules").is_ok_and(|t| !t.trim().is_empty());
        match modules() {
            Ok(m) => assert!(loadable && m.iter().all(|x| !x.name.is_empty() && x.state != "Unknown")),
            Err(e) => assert!(!loadable && e.contains("built in"), "{e}"),
        }
    }
}
