//! Drivers and services. Windows: kernel drivers and Win32 services from the Service Control
//! Manager (`EnumServicesStatusExW` + `QueryServiceConfigW`), plus file version metadata of the
//! driver images. Linux: systemd service units and the loaded kernel modules ([`linux`]).
//! macOS: launchd jobs and the loaded kernel extensions ([`macos`]). Read-only.

mod linux;
mod macos;

use crate::platform::{Os, OS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Kernel and file-system drivers (kernel modules on Linux, kernel extensions on macOS).
    Drivers,
    /// Win32 services (systemd units on Linux, launchd jobs on macOS).
    Services,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Service {
    pub name: String,
    pub display: String,
    /// "Running", "Stopped", ... (Linux also "Exited", "Failed"; modules and kexts "Loaded")
    pub state: &'static str,
    pub running: bool,
    /// Windows: "Boot", "System", "Automatic", "Manual", "Disabled". Linux: "Enabled", "Static",
    /// "Masked" ... macOS: "At load", "On demand", "Disabled". Modules and kexts: "In use", "Unused".
    pub start: String,
    /// Windows: "Kernel", "File system", "Own process", "Shared process". Linux: the unit's
    /// `Type=` or a module's "In tree" / "Out of tree" / "Proprietary". macOS: "Daemon", "Agent".
    pub kind: String,
    pub pid: u32,
    pub path: String,
    pub company: String,
    pub version: String,
    pub description: String,
    /// Not from the OS vendor: a company other than Microsoft (Windows drivers), an out-of-tree
    /// or proprietary module (Linux), a bundle id outside `com.apple.` (macOS).
    pub third_party: bool,
}

/// What the Drivers page lists here, as its title.
pub const DRIVERS_TITLE: &str = match OS {
    Os::Linux => "Kernel modules",
    Os::MacOs => "Kernel extensions",
    _ => "Drivers",
};

/// The Drivers page's name under its rail icon.
pub const DRIVERS_RAIL: &str = match OS {
    Os::Linux => "Modules",
    Os::MacOs => "Kexts",
    _ => "Drivers",
};

/// The plural of what a list holds: "drivers", "modules", "extensions", "services".
pub const fn noun(kind: Kind) -> &'static str {
    match (kind, OS) {
        (Kind::Services, _) => "services",
        (Kind::Drivers, Os::Linux) => "modules",
        (Kind::Drivers, Os::MacOs) => "extensions",
        (Kind::Drivers, _) => "drivers",
    }
}

/// Where a list comes from, for the page subtitle.
pub const fn source(kind: Kind) -> &'static str {
    match (kind, OS) {
        (Kind::Drivers, Os::Linux) => "loaded kernel modules from /proc/modules",
        (Kind::Drivers, Os::MacOs) => "loaded kernel extensions from kmutil",
        (Kind::Drivers, _) => "kernel drivers from the Service Control Manager",
        (Kind::Services, Os::Linux) => "systemd",
        (Kind::Services, Os::MacOs) => "launchd",
        (Kind::Services, _) => "Service Control Manager",
    }
}

/// What the page says while the list loads.
pub const fn reading(kind: Kind) -> &'static str {
    match (kind, OS) {
        (Kind::Drivers, Os::Linux) => "Reading /proc/modules\u{2026}",
        (Kind::Drivers, Os::MacOs) => "Asking kmutil\u{2026}",
        (Kind::Services, Os::Linux) => "Asking systemd\u{2026}",
        (Kind::Services, Os::MacOs) => "Asking launchd\u{2026}",
        _ => "Reading the Service Control Manager\u{2026}",
    }
}

/// True for a start mode that starts the service with the system.
pub fn starts_at_boot(start: &str) -> bool {
    matches!(start, "Boot" | "System" | "Automatic" | "Enabled" | "At load")
}

/// True for a start mode that keeps the service from starting.
pub fn is_disabled(start: &str) -> bool {
    matches!(start, "Disabled" | "Masked")
}

/// Running first, then by name.
fn sort(list: &mut [Service]) {
    list.sort_by(|a, b| b.running.cmp(&a.running).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

/// True for a Windows company name other than Microsoft.
pub fn third_party_company(company: &str) -> bool {
    !company.is_empty() && !company.contains("Microsoft")
}

pub fn state_name(s: u32) -> &'static str {
    match s {
        1 => "Stopped",
        2 => "Starting",
        3 => "Stopping",
        4 => "Running",
        5 => "Continuing",
        6 => "Pausing",
        7 => "Paused",
        _ => "Unknown",
    }
}

pub fn start_name(s: u32) -> &'static str {
    match s {
        0 => "Boot",
        1 => "System",
        2 => "Automatic",
        3 => "Manual",
        4 => "Disabled",
        _ => "Unknown",
    }
}

pub fn kind_name(t: u32) -> &'static str {
    if t & 0x1 != 0 {
        "Kernel"
    } else if t & 0x2 != 0 {
        "File system"
    } else if t & 0x10 != 0 {
        "Own process"
    } else if t & 0x20 != 0 {
        "Shared process"
    } else {
        "Other"
    }
}

/// Turns SCM image paths (`\SystemRoot\...`, `System32\...`, `\??\C:\...`, quoted
/// command lines) into plain file paths.
pub fn normalize_image_path(p: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let mut s = p.trim().to_string();
    if let Some(rest) = s.strip_prefix('"') {
        s = rest.split('"').next().unwrap_or("").to_string();
    } else if let Some(i) = s.to_ascii_lowercase().find(".exe ") {
        s.truncate(i + 4);
    }
    if let Some(rest) = s.strip_prefix(r"\??\") {
        s = rest.to_string();
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with(r"\systemroot\") {
        s = format!("{root}{}", &s[11..]);
    } else if lower.starts_with(r"system32\") || lower.starts_with(r"syswow64\") {
        s = format!(r"{root}\{s}");
    }
    super::expand_env(&s)
}

#[cfg(windows)]
pub fn read(kind: Kind) -> Result<Vec<Service>, String> {
    use super::winutil::wide;
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::{
        CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, ENUM_SERVICE_STATE, ENUM_SERVICE_STATUS_PROCESSW, ENUM_SERVICE_TYPE,
        QUERY_SERVICE_CONFIGW, SC_ENUM_TYPE, SC_HANDLE,
    };

    struct H(SC_HANDLE);
    impl Drop for H {
        fn drop(&mut self) {
            // SAFETY: handle came from OpenSCManagerW/OpenServiceW.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    const SC_MANAGER_CONNECT: u32 = 0x1;
    const SC_MANAGER_ENUMERATE_SERVICE: u32 = 0x4;
    const SERVICE_QUERY_CONFIG: u32 = 0x1;
    let types = match kind {
        Kind::Drivers => 0x0B,
        Kind::Services => 0x30,
    };
    // SAFETY: local SCM, default database.
    let scm = H(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE) }.map_err(|e| format!("OpenSCManager: {e}"))?);
    let mut needed = 0u32;
    let mut count = 0u32;
    let mut resume = 0u32;
    // SAFETY: size query.
    let _ = unsafe { EnumServicesStatusExW(scm.0, SC_ENUM_TYPE(0), ENUM_SERVICE_TYPE(types), ENUM_SERVICE_STATE(3), None, &mut needed, &mut count, Some(&mut resume), PCWSTR::null()) };
    let mut buf = vec![0u64; (needed as usize + 64 * 1024) / 8];
    let bytes: &mut [u8] = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    resume = 0;
    // SAFETY: the buffer is larger than the size the API asked for.
    unsafe { EnumServicesStatusExW(scm.0, SC_ENUM_TYPE(0), ENUM_SERVICE_TYPE(types), ENUM_SERVICE_STATE(3), Some(bytes), &mut needed, &mut count, Some(&mut resume), PCWSTR::null()) }
        .map_err(|e| format!("EnumServicesStatusEx: {e}"))?;
    let items = buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW;
    let mut out = Vec::with_capacity(count as usize);
    let mut cfg_buf = vec![0u64; 8192 / 8];
    for i in 0..count as usize {
        // SAFETY: `count` entries were written at the start of the buffer.
        let it = unsafe { &*items.add(i) };
        let name = unsafe { it.lpServiceName.to_string() }.unwrap_or_default();
        let display = unsafe { it.lpDisplayName.to_string() }.unwrap_or_default();
        let st = &it.ServiceStatusProcess;
        let mut svc = Service {
            name: name.clone(),
            display,
            state: state_name(st.dwCurrentState.0),
            running: st.dwCurrentState.0 == 4,
            start: "Unknown".into(),
            kind: kind_name(st.dwServiceType.0).into(),
            pid: st.dwProcessId,
            ..Default::default()
        };
        let wname = wide(&name);
        // SAFETY: valid service name; config buffer is 8 KiB, re-queried when larger.
        if let Ok(h) = unsafe { OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), SERVICE_QUERY_CONFIG) } {
            let h = H(h);
            let mut need = 0u32;
            let mut ok = unsafe { QueryServiceConfigW(h.0, Some(cfg_buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW), (cfg_buf.len() * 8) as u32, &mut need) }.is_ok();
            if !ok && need as usize > cfg_buf.len() * 8 && need < 1 << 20 {
                cfg_buf = vec![0u64; need as usize / 8 + 1];
                ok = unsafe { QueryServiceConfigW(h.0, Some(cfg_buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW), (cfg_buf.len() * 8) as u32, &mut need) }.is_ok();
            }
            if ok {
                let cfg = unsafe { &*(cfg_buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
                svc.start = start_name(cfg.dwStartType.0).into();
                let raw = unsafe { cfg.lpBinaryPathName.to_string() }.unwrap_or_default();
                svc.path = if raw.is_empty() && kind == Kind::Drivers {
                    format!(r"{}\System32\drivers\{name}.sys", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
                } else {
                    normalize_image_path(&raw)
                };
            }
        }
        if kind == Kind::Drivers && !svc.path.is_empty() {
            if let Some(v) = file_version(&svc.path) {
                svc.company = v.company;
                svc.version = v.version;
                svc.description = v.description;
            }
        }
        svc.third_party = third_party_company(&svc.company);
        out.push(svc);
    }
    sort(&mut out);
    Ok(out)
}

#[cfg(target_os = "linux")]
pub fn read(kind: Kind) -> Result<Vec<Service>, String> {
    match kind {
        Kind::Drivers => linux::modules(),
        Kind::Services => linux::services(),
    }
}

#[cfg(target_os = "macos")]
pub fn read(kind: Kind) -> Result<Vec<Service>, String> {
    match kind {
        Kind::Drivers => macos::kexts(),
        Kind::Services => macos::services(),
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn read(_kind: Kind) -> Result<Vec<Service>, String> {
    Err(format!("services and drivers are not implemented on {}", crate::platform::os_name()))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileVersion {
    pub company: String,
    pub version: String,
    pub description: String,
}

/// `CompanyName`, `FileVersion` and `FileDescription` of a PE file.
#[cfg(windows)]
pub fn file_version(path: &str) -> Option<FileVersion> {
    use super::winutil::wide;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let w = wide(path);
    // SAFETY: size query, then a read into a buffer of that size; VerQueryValueW returns
    // pointers into that buffer, which outlives their use.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(w.as_ptr()), None);
        if size == 0 || size > 1 << 22 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(w.as_ptr()), 0, size, data.as_mut_ptr() as *mut _).ok()?;
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let q = wide(r"\VarFileInfo\Translation");
        let (lang, cp) = if VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 {
            let p = ptr as *const u16;
            (*p, *p.add(1))
        } else {
            (0x0409, 0x04B0)
        };
        let get = |key: &str| -> String {
            let q = wide(&format!(r"\StringFileInfo\{lang:04x}{cp:04x}\{key}"));
            let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut l = 0u32;
            if VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut p, &mut l).as_bool() && l > 0 {
                let s = std::slice::from_raw_parts(p as *const u16, l as usize);
                super::winutil::from_wide(s).trim().to_string()
            } else {
                String::new()
            }
        };
        Some(FileVersion { company: get("CompanyName"), version: get("FileVersion"), description: get("FileDescription") })
    }
}

#[cfg(not(windows))]
pub fn file_version(_path: &str) -> Option<FileVersion> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert_eq!(normalize_image_path(r"\SystemRoot\System32\drivers\x.sys"), format!(r"{root}\System32\drivers\x.sys"));
        assert_eq!(normalize_image_path(r"System32\drivers\y.sys"), format!(r"{root}\System32\drivers\y.sys"));
        assert_eq!(normalize_image_path(r"\??\C:\a\b.sys"), r"C:\a\b.sys");
        assert_eq!(normalize_image_path(r#""C:\Program Files\x\svc.exe" -k run"#), r"C:\Program Files\x\svc.exe");
        assert_eq!(normalize_image_path(r"C:\Windows\system32\svchost.exe -k netsvcs"), r"C:\Windows\system32\svchost.exe");
    }

    /// Third party on Windows means a company other than Microsoft (Linux: taint flags, macOS:
    /// bundle id; see the backend tests).
    #[test]
    fn only_non_microsoft_companies_are_third_party() {
        assert!(third_party_company("Vendor") && !third_party_company("Microsoft Corporation") && !third_party_company(""));
    }

    #[test]
    fn start_modes_of_every_os() {
        for s in ["Boot", "System", "Automatic", "Enabled", "At load"] {
            assert!(starts_at_boot(s) && !is_disabled(s), "{s}");
        }
        for s in ["Disabled", "Masked"] {
            assert!(is_disabled(s) && !starts_at_boot(s), "{s}");
        }
        assert!(!starts_at_boot("Manual") && !starts_at_boot("Static") && !starts_at_boot("On demand") && !starts_at_boot("In use"));
    }

    #[test]
    fn words_match_the_target() {
        if cfg!(windows) {
            assert_eq!((DRIVERS_TITLE, DRIVERS_RAIL, noun(Kind::Drivers), source(Kind::Services)), ("Drivers", "Drivers", "drivers", "Service Control Manager"));
            assert_eq!(reading(Kind::Drivers), reading(Kind::Services));
        } else if cfg!(target_os = "linux") {
            assert_eq!((DRIVERS_TITLE, noun(Kind::Drivers), source(Kind::Services)), ("Kernel modules", "modules", "systemd"));
        }
        assert_eq!(noun(Kind::Services), "services");
    }

    #[cfg(windows)]
    #[test]
    fn lists_drivers_and_services() {
        let d = read(Kind::Drivers).unwrap();
        assert!(d.iter().any(|s| s.running && s.kind == "Kernel"));
        let s = read(Kind::Services).unwrap();
        assert!(s.iter().any(|s| s.running && s.pid != 0));
    }

    #[cfg(windows)]
    #[test]
    fn reads_version_of_a_system_file() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let v = file_version(&format!(r"{root}\System32\kernel32.dll")).expect("version info");
        assert!(v.company.contains("Microsoft"));
    }
}
