//! macOS inventory: `sysctl` for the OS, processor and memory, `system_profiler` (on the loader
//! thread) for the model and GPUs, and launchd job plists for startup entries.

use super::parse;
use super::{keys, CpuDetails, Sections, StartupApp};
use crate::platform::run_capture;
use crate::pro::sysfmt;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A string sysctl (`kern.osproductversion`, `machdep.cpu.brand_string`, ...).
fn sysctl_str(name: &str) -> Option<String> {
    let c = CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: a size query with a null buffer, then a read into a buffer of that size.
    unsafe {
        if libc::sysctlbyname(c.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(c.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        buf.truncate(len);
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).trim().to_string()).filter(|s| !s.is_empty())
    }
}

/// An integer sysctl of 4 or 8 bytes.
fn sysctl_u64(name: &str) -> Option<u64> {
    let c = CString::new(name).ok()?;
    let mut buf = [0u8; 8];
    let mut len: libc::size_t = buf.len();
    // SAFETY: the buffer is valid for `len` bytes; the kernel writes at most that many.
    let rc = unsafe { libc::sysctlbyname(c.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    match (rc, len) {
        (0, 4) => Some(u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64),
        (0, 8) => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// Seconds since boot from `kern.boottime`.
fn uptime() -> Option<Duration> {
    let c = CString::new("kern.boottime").ok()?;
    // SAFETY: an all-zero timeval is valid; sysctl writes at most `len` bytes into it.
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len: libc::size_t = std::mem::size_of::<libc::timeval>();
    let rc = unsafe { libc::sysctlbyname(c.as_ptr(), (&mut tv as *mut libc::timeval).cast(), &mut len, std::ptr::null_mut(), 0) };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    (rc == 0 && tv.tv_sec > 0).then(|| Duration::from_secs(now.saturating_sub(tv.tv_sec as u64)))
}

pub fn system_info() -> Result<Sections, String> {
    let version = sysctl_str("kern.osproductversion").unwrap_or_default();
    let name = parse::macos_name(&version);
    let mut out: Sections = Vec::new();
    out.push((
        keys::OS,
        vec![
            (keys::EDITION.into(), format!("macOS {name}").trim().to_string()),
            (keys::VERSION.into(), version),
            (keys::BUILD.into(), sysctl_str("kern.osversion").unwrap_or_default()),
            ("Kernel".into(), sysctl_str("kern.osrelease").map(|r| format!("Darwin {r}")).unwrap_or_default()),
            ("Architecture".into(), std::env::consts::ARCH.into()),
            ("Up time".into(), uptime().map(crate::ui::fmt_hms).unwrap_or_default()),
        ],
    ));

    let zone = std::fs::read_link("/etc/localtime").map(|t| parse::zone_from_link(&t.to_string_lossy())).unwrap_or_default();
    out.push((
        keys::DEVICE,
        vec![
            (keys::COMPUTER.into(), crate::platform::hostname().unwrap_or_default()),
            ("User".into(), std::env::var("USER").unwrap_or_default()),
            ("Time zone".into(), zone),
        ],
    ));

    // system_profiler takes a second or so; this runs on the loader thread.
    let t = Duration::from_secs(15);
    let hw = run_capture("system_profiler", &["SPHardwareDataType", "-json"], t).map(|j| parse::profiler_hardware(&j)).unwrap_or_default();
    let hw_get = |k: &str| hw.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let model = Some(hw_get("Model")).filter(|m| !m.is_empty()).or_else(|| sysctl_str("hw.model")).unwrap_or_default();
    out.push(("Hardware", vec![("Manufacturer".into(), "Apple".into()), ("Model".into(), model), ("Firmware version".into(), hw_get("Firmware version"))]));

    let brand = sysctl_str("machdep.cpu.brand_string").or_else(|| Some(hw_get("Chip")).filter(|c| !c.is_empty())).unwrap_or_default();
    let mem = sysctl_u64("hw.memsize").map(|b| crate::process::format_bytes(b as f64)).unwrap_or_default();
    out.push((
        keys::CPU_MEM,
        vec![
            (keys::PROCESSOR.into(), brand),
            ("Rated speed".into(), sysctl_u64("hw.cpufrequency").filter(|h| *h > 0).map(|h| format!("{:.2} GHz", h as f64 / 1e9)).unwrap_or_default()),
            ("Physical cores".into(), sysctl_u64("hw.physicalcpu").map(|c| c.to_string()).unwrap_or_default()),
            ("Logical processors".into(), sysctl_u64("hw.logicalcpu").map(|c| c.to_string()).unwrap_or_default()),
            (keys::INSTALLED_MEM.into(), mem),
        ],
    ));

    let gpus = run_capture("system_profiler", &["SPDisplaysDataType", "-json"], t).map(|j| parse::profiler_displays(&j)).unwrap_or_default();
    if !gpus.is_empty() {
        out.push((keys::GPUS, gpus));
    }
    for (_, rows) in out.iter_mut() {
        rows.retain(|(_, v)| !v.trim().is_empty());
    }
    out.retain(|(_, rows)| !rows.is_empty());
    Ok(out)
}

pub fn cpu_details() -> CpuDetails {
    let logical = sysctl_u64("hw.logicalcpu").unwrap_or(0) as usize;
    let (l1, l2, l3) = parse::mac_caches(&|k| sysctl_u64(k));
    CpuDetails {
        sockets: sysctl_u64("hw.packages").unwrap_or(1) as usize,
        cores: sysctl_u64("hw.physicalcpu").map(|c| c as usize).unwrap_or(logical),
        logical,
        l1,
        l2,
        l3,
        virtualization: sysctl_u64("kern.hv_support").map(|v| v == 1),
        virtual_machine: sysctl_u64("kern.hv_vmm_present") == Some(1),
        fast_startup: None,
    }
}

pub fn startup_apps() -> Result<Vec<StartupApp>, String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let t = Duration::from_secs(5);
    let overrides = |domain: &str| run_capture("launchctl", &["print-disabled", domain], t).map(|s| parse::parse_launchctl_disabled(&s)).unwrap_or_default();
    let user_off = overrides(&format!("gui/{uid}"));
    let system_off = overrides("system");
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    // (folder, location, true for the system domain)
    let mut dirs: Vec<(PathBuf, &str, bool)> = Vec::new();
    if let Some(h) = home {
        dirs.push((h.join("Library/LaunchAgents"), "Launch agent: current user", false));
    }
    dirs.push((PathBuf::from("/Library/LaunchAgents"), "Launch agent: all users", false));
    dirs.push((PathBuf::from("/Library/LaunchDaemons"), "Launch daemon: system", true));
    let mut out = Vec::new();
    for (dir, label, system) in dirs {
        let off = if system { &system_off } else { &user_off };
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "plist") {
                continue;
            }
            let Some(plist) = sysfmt::read_plist(&path) else { continue };
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut job = parse::launchd_job(&plist, &stem);
            if let Some((_, disabled)) = off.iter().find(|(l, _)| *l == job.name) {
                job.enabled = !disabled;
            }
            job.location = label.into();
            job.publisher = bundle_vendor(&job.command);
            out.push(job);
        }
    }
    out.sort_by(|a, b| b.enabled.cmp(&a.enabled).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// The copyright holder of the `.app` bundle a job's program lives in, if any.
fn bundle_vendor(command: &str) -> String {
    let program = super::command_path(command);
    let Some(app) = Path::new(&program).ancestors().find(|a| a.extension().is_some_and(|x| x == "app")) else { return String::new() };
    sysfmt::read_plist(&app.join("Contents/Info.plist")).and_then(|p| p.text("NSHumanReadableCopyright")).map(|c| sysfmt::copyright_holder(&c)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_sections_are_not_empty() {
        let s = system_info().unwrap();
        assert_eq!(s[0].0, keys::OS);
        assert!(s[0].1.iter().any(|(k, v)| k == keys::VERSION && !v.is_empty()), "{s:?}");
        let d = cpu_details();
        assert!(d.logical >= 1 && d.cores >= 1 && d.sockets >= 1, "{d:?}");
        let _ = startup_apps().unwrap();
    }
}
