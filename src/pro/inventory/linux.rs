//! Linux inventory: os-release, `uname`, `/proc`, DMI and DRM sysfs, CPU topology from sysfs,
//! XDG autostart entries and enabled `systemd --user` services.

use super::parse::{self, CacheInfo, SysCpu};
use super::{keys, CpuDetails, Sections, StartupApp};
use crate::pro::sysfmt::{self, env_value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// `uname()`: (kernel release, machine).
fn uname() -> (String, String) {
    // SAFETY: an all-zero utsname is valid; uname fills NUL-terminated fields.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return (String::new(), std::env::consts::ARCH.to_string());
    }
    let field = |f: &[libc::c_char]| {
        let bytes: Vec<u8> = f.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    (field(&u.release), field(&u.machine))
}

const DMI: &str = "/sys/class/dmi/id";

/// A DMI string, empty for the OEM placeholders.
fn dmi(name: &str) -> String {
    let v = read(Path::new(DMI).join(name));
    let lower = v.to_lowercase();
    if ["to be filled by o.e.m.", "default string", "not specified", "system product name", "system manufacturer", "none"].contains(&lower.as_str()) {
        return String::new();
    }
    v
}

fn joined(a: String, b: String) -> String {
    format!("{a} {b}").trim().to_string()
}

pub fn system_info() -> Result<Sections, String> {
    let mut os_text = read("/etc/os-release");
    if os_text.is_empty() {
        os_text = read("/usr/lib/os-release");
    }
    let os = sysfmt::parse_env_file(&os_text);
    let (release, machine) = uname();
    let first = |keys: &[&str]| keys.iter().map(|k| env_value(&os, k)).find(|v| !v.is_empty()).unwrap_or_default();
    let uptime = read("/proc/uptime").split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).map(|s| Duration::from_secs(s as u64));
    let installed = std::fs::metadata("/").and_then(|m| m.created()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| super::unix_date(d.as_secs())).unwrap_or_default();
    let efi = Path::new("/sys/firmware/efi").is_dir();
    let firmware = if efi {
        "UEFI"
    } else if Path::new(DMI).is_dir() {
        "Legacy BIOS"
    } else {
        ""
    };
    // EFI variable: 4 attribute bytes, then the value.
    let secure_boot = std::fs::read("/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c").ok().and_then(|b| b.get(4).copied());
    let mut out: Sections = Vec::new();
    out.push((
        keys::OS,
        vec![
            (keys::EDITION.into(), Some(first(&["NAME", "PRETTY_NAME"])).filter(|e| !e.is_empty()).unwrap_or_else(|| "Linux".into())),
            (keys::VERSION.into(), first(&["VERSION", "VERSION_ID", "BUILD_ID"])),
            (keys::BUILD.into(), release),
            ("Architecture".into(), machine.clone()),
            ("Installed on".into(), installed),
            ("Up time".into(), uptime.map(crate::ui::fmt_hms).unwrap_or_default()),
            ("Firmware".into(), firmware.into()),
            ("Secure Boot".into(), match (efi, secure_boot) {
                (true, Some(1)) => "On".into(),
                (true, Some(_)) => "Off".into(),
                (true, None) => "Unsupported".into(),
                (false, _) => String::new(),
            }),
        ],
    ));

    let env = |n: &str| std::env::var(n).unwrap_or_default();
    let session = match env("XDG_SESSION_TYPE").as_str() {
        "wayland" => " (Wayland)",
        "x11" => " (X11)",
        _ => "",
    };
    let desktop = env("XDG_CURRENT_DESKTOP").replace(':', ", ");
    let zone = std::fs::read_link("/etc/localtime").map(|t| parse::zone_from_link(&t.to_string_lossy())).unwrap_or_default();
    let user = Some(env("USER")).filter(|u| !u.is_empty()).unwrap_or_else(|| env("LOGNAME"));
    out.push((
        keys::DEVICE,
        vec![
            (keys::COMPUTER.into(), crate::platform::hostname().unwrap_or_default()),
            ("User".into(), user),
            ("Time zone".into(), if zone.is_empty() { read("/etc/timezone") } else { zone }),
            ("Desktop".into(), if desktop.is_empty() { String::new() } else { format!("{desktop}{session}") }),
        ],
    ));

    let model = joined(dmi("product_name"), dmi("product_version"));
    out.push((
        "Hardware",
        vec![
            ("Manufacturer".into(), dmi("sys_vendor")),
            ("Model".into(), model),
            ("Motherboard".into(), joined(dmi("board_vendor"), dmi("board_name"))),
            ("BIOS".into(), joined(dmi("bios_vendor"), dmi("bios_version"))),
            ("BIOS date".into(), dmi("bios_date")),
        ],
    ));

    let cpu = parse::parse_cpuinfo(&read("/proc/cpuinfo"));
    let logical = online_cpus().len().max(cpu.processors);
    let base_khz: Option<u64> = ["base_frequency", "cpuinfo_max_freq"].iter().find_map(|f| read(format!("/sys/devices/system/cpu/cpu0/cpufreq/{f}")).parse().ok());
    let meminfo = read("/proc/meminfo");
    let mem = installed_memory().or_else(|| parse::meminfo_bytes(&meminfo, "MemTotal"));
    let base_label = if Path::new("/sys/devices/system/cpu/cpu0/cpufreq/base_frequency").exists() { "Rated speed" } else { "Maximum speed" };
    out.push((
        keys::CPU_MEM,
        vec![
            (keys::PROCESSOR.into(), if cpu.model.is_empty() { machine } else { cpu.model.clone() }),
            ("Identifier".into(), cpu.identifier()),
            (base_label.into(), base_khz.map(|k| format!("{:.2} GHz", k as f64 / 1e6)).unwrap_or_default()),
            ("Logical processors".into(), if logical > 0 { logical.to_string() } else { String::new() }),
            (keys::INSTALLED_MEM.into(), mem.map(|b| crate::process::format_bytes(b as f64)).unwrap_or_default()),
        ],
    ));

    let gpus = display_adapters();
    if !gpus.is_empty() {
        out.push((keys::GPUS, gpus));
    }
    for (_, rows) in out.iter_mut() {
        rows.retain(|(_, v)| !v.trim().is_empty());
    }
    out.retain(|(_, rows)| !rows.is_empty());
    Ok(out)
}

/// Bytes of online memory blocks (closer to the installed RAM than `MemTotal`).
fn installed_memory() -> Option<u64> {
    let base = Path::new("/sys/devices/system/memory");
    let block = u64::from_str_radix(&read(base.join("block_size_bytes")), 16).ok()?;
    let online = std::fs::read_dir(base).ok()?.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("memory") && read(e.path().join("online")) == "1").count() as u64;
    (online > 0).then_some(online * block)
}

/// Online logical processors (`/sys/devices/system/cpu/cpuN` with no `online` file or `1`).
fn online_cpus() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir("/sys/devices/system/cpu") else { return Vec::new() };
    let mut cpus: Vec<(u32, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n: u32 = e.file_name().to_str()?.strip_prefix("cpu")?.parse().ok()?;
            let online = read(e.path().join("online"));
            (online.is_empty() || online == "1").then(|| (n, e.path()))
        })
        .collect();
    cpus.sort();
    cpus.into_iter().map(|c| c.1).collect()
}

/// (name, "VRAM, driver x") per DRM card, named from `pci.ids` when it is installed.
fn display_adapters() -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut cards: Vec<PathBuf> = rd.flatten().filter(|e| e.file_name().to_str().and_then(|n| n.strip_prefix("card")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))).map(|e| e.path()).collect();
    cards.sort();
    let ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids"].iter().map(read).find(|t| !t.is_empty()).unwrap_or_default();
    let mut out = Vec::new();
    for card in cards {
        let dev = card.join("device");
        let driver = std::fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
        if driver.is_empty() || ["vgem", "vkms"].contains(&driver.as_str()) {
            continue;
        }
        let (vendor, device) = (parse::hex_id(&read(dev.join("vendor"))), parse::hex_id(&read(dev.join("device"))));
        let name = match (vendor, device) {
            (Some(v), Some(d)) => match parse::pci_name(&ids, v, d) {
                (Some(vn), Some(dn)) => format!("{vn} {dn}"),
                (Some(vn), None) => format!("{vn} device {d:04x}"),
                _ => format!("PCI {v:04x}:{d:04x}"),
            },
            _ => driver.clone(),
        };
        let vram = read(dev.join("mem_info_vram_total")).parse::<u64>().ok().filter(|b| *b > 0).map(|b| crate::process::format_bytes(b as f64));
        let line = match vram {
            Some(v) => format!("{v}, driver {driver}"),
            None => format!("driver {driver}"),
        };
        if !out.iter().any(|(n, l): &(String, String)| *n == name && *l == line) {
            out.push((name, line));
        }
    }
    out
}

pub fn cpu_details() -> CpuDetails {
    let cpus: Vec<SysCpu> = online_cpus()
        .iter()
        .map(|p| {
            let topo = p.join("topology");
            let siblings = Some(read(topo.join("thread_siblings_list"))).filter(|s| !s.is_empty()).unwrap_or_else(|| read(topo.join("core_cpus_list")));
            let mut caches = Vec::new();
            if let Ok(rd) = std::fs::read_dir(p.join("cache")) {
                for e in rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("index")) {
                    let c = e.path();
                    caches.push(CacheInfo { level: read(c.join("level")).parse().unwrap_or(0), kind: read(c.join("type")), size: parse::parse_cache_size(&read(c.join("size"))), shared: read(c.join("shared_cpu_list")) });
                }
            }
            SysCpu { package: read(topo.join("physical_package_id")), siblings, caches }
        })
        .collect();
    let mut d = parse::summarize_cpus(&cpus);
    let info = parse::parse_cpuinfo(&read("/proc/cpuinfo"));
    if d.logical == 0 {
        d.logical = info.processors;
    }
    if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
        d.virtualization = Some(info.flags.contains("vmx") || info.flags.contains("svm"));
    }
    d.virtual_machine = info.flags.contains("hypervisor") || parse::looks_virtual(&dmi("sys_vendor"), &dmi("product_name"));
    d
}

/// `$XDG_CONFIG_HOME/autostart` (user) and each `$XDG_CONFIG_DIRS/autostart` (all users).
fn autostart_dirs() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    let user = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| home.map(|h| h.join(".config")));
    if let Some(u) = user {
        dirs.push((u.join("autostart"), "Autostart: current user"));
    }
    let system = std::env::var("XDG_CONFIG_DIRS").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    for d in system.split(':').filter(|d| d.starts_with('/')) {
        dirs.push((Path::new(d).join("autostart"), "Autostart: all users"));
    }
    dirs
}

pub fn startup_apps() -> Result<Vec<StartupApp>, String> {
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    // The first directory holding a file name wins (the user's copy overrides the system one).
    let mut by_file: HashMap<String, StartupApp> = HashMap::new();
    for (dir, label) in autostart_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            if !file.ends_with(".desktop") || by_file.contains_key(&file) {
                continue;
            }
            let Some(entry) = std::fs::read_to_string(e.path()).ok().and_then(|t| sysfmt::parse_desktop_entry(&t)) else { continue };
            if !sysfmt::shows_in(&entry, &current) {
                // Not for this desktop: still recorded so a system copy cannot reappear.
                by_file.insert(file, StartupApp::default());
                continue;
            }
            let name = if entry.name.is_empty() { file.trim_end_matches(".desktop").to_string() } else { entry.name.clone() };
            let app = StartupApp { name, command: sysfmt::exec_command(&entry.exec), location: label.into(), enabled: !entry.hidden && entry.autostart_enabled, publisher: String::new() };
            by_file.insert(file, app);
        }
    }
    let mut out: Vec<StartupApp> = by_file.into_values().filter(|a| !a.name.is_empty()).collect();
    out.extend(systemd_user_units());
    out.sort_by(|a, b| b.enabled.cmp(&a.enabled).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// Enabled `systemd --user` services; nothing when there is no user manager.
fn systemd_user_units() -> Vec<StartupApp> {
    let t = Duration::from_secs(3);
    let Ok(list) = crate::platform::run_capture("systemctl", &["--user", "list-unit-files", "--type=service", "--state=enabled", "--no-legend", "--plain", "--no-pager"], t) else { return Vec::new() };
    let units = parse::unit_names(&list);
    if units.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["--user", "show", "-p", "Id,Description,ExecStart", "--no-pager"];
    args.extend(units.iter().map(String::as_str));
    let shown = crate::platform::run_capture("systemctl", &args, t).map(|s| parse::parse_systemd_show(&s)).unwrap_or_default();
    units
        .iter()
        .map(|u| {
            let (desc, cmd) = shown.iter().find(|s| &s.0 == u).map(|s| (s.1.clone(), s.2.clone())).unwrap_or_default();
            StartupApp { name: if desc.is_empty() { u.clone() } else { desc }, command: if cmd.is_empty() { u.clone() } else { cmd }, location: "systemd: current user".into(), enabled: true, publisher: String::new() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sections_are_not_empty() {
        let s = system_info().unwrap();
        assert_eq!(s[0].0, keys::OS);
        assert!(s[0].1.iter().any(|(k, v)| k == keys::BUILD && !v.is_empty()), "{s:?}");
        assert!(s.iter().any(|(t, rows)| *t == keys::CPU_MEM && rows.iter().any(|(k, _)| k == keys::INSTALLED_MEM)), "{s:?}");
        let d = cpu_details();
        assert!(d.logical >= 1 && d.cores >= 1 && d.logical >= d.cores && d.sockets >= 1, "{d:?}");
        let _ = startup_apps().unwrap();
    }
}
