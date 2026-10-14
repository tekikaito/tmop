//! Inventory pages: System Info, Services and Startup apps.
//! All read-only, from the registry, the SCM and the Startup folders.

pub use super::drivers::Service;

/// Titled groups of (label, value).
pub type Sections = Vec<(&'static str, Vec<(String, String)>)>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupApp {
    pub name: String,
    pub command: String,
    pub location: String,
    pub enabled: bool,
    pub publisher: String,
}

/// Executable path at the start of a command line.
pub fn command_path(cmd: &str) -> String {
    let c = super::expand_env(cmd.trim());
    if let Some(rest) = c.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    let lower = c.to_ascii_lowercase();
    for ext in [".exe", ".bat", ".cmd", ".com", ".lnk"] {
        if let Some(i) = lower.find(ext) {
            return c[..i + ext.len()].to_string();
        }
    }
    c.split_whitespace().next().unwrap_or("").to_string()
}

/// `StartupApproved` values: an odd first byte means disabled.
pub fn approved_enabled(bytes: &[u8]) -> bool {
    bytes.first().map(|b| b & 1 == 0).unwrap_or(true)
}

#[cfg(windows)]
pub fn system_info() -> Result<Sections, String> {
    use super::winutil::{Key, Value, View};
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::Win32::System::SystemInformation::{GetFirmwareType, GetPhysicallyInstalledSystemMemory, GetTickCount64, FIRMWARE_TYPE};
    let hklm = |p: &str| Key::open(HKEY_LOCAL_MACHINE, p, View::Default);
    let s = |k: &Option<Key>, n: &str| k.as_ref().and_then(|k| k.string(n)).unwrap_or_default();
    let mut out: Sections = Vec::new();

    let cv = hklm(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let build: u32 = s(&cv, "CurrentBuild").parse().unwrap_or(0);
    let mut product = s(&cv, "ProductName");
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let ubr = cv.as_ref().and_then(|k| k.u32("UBR")).map(|u| format!(".{u}")).unwrap_or_default();
    let installed = cv.as_ref().and_then(|k| k.u32("InstallDate")).map(|t| unix_date(t as u64)).unwrap_or_default();
    let uptime = std::time::Duration::from_millis(unsafe { GetTickCount64() });
    let mut fw = FIRMWARE_TYPE::default();
    let firmware = if unsafe { GetFirmwareType(&mut fw) }.is_ok() {
        match fw.0 {
            1 => "Legacy BIOS",
            2 => "UEFI",
            _ => "Unknown",
        }
    } else {
        "Unknown"
    };
    let secure_boot = hklm(r"SYSTEM\CurrentControlSet\Control\SecureBoot\State").and_then(|k| k.u32("UEFISecureBootEnabled"));
    out.push((
        "Operating system",
        vec![
            ("Edition".into(), product),
            ("Version".into(), s(&cv, "DisplayVersion")),
            ("OS build".into(), format!("{build}{ubr}")),
            ("Installed on".into(), installed),
            ("Registered owner".into(), s(&cv, "RegisteredOwner")),
            ("Up time".into(), crate::ui::fmt_hms(uptime)),
            ("Firmware".into(), firmware.into()),
            ("Secure Boot".into(), match secure_boot {
                Some(1) => "On".into(),
                Some(_) => "Off".into(),
                None => "Unsupported".into(),
            }),
        ],
    ));

    let env = |n: &str| std::env::var(n).unwrap_or_default();
    let tz = hklm(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation");
    out.push((
        "Device",
        vec![
            ("Computer name".into(), env("COMPUTERNAME")),
            ("Domain / workgroup".into(), env("USERDOMAIN")),
            ("User".into(), env("USERNAME")),
            ("Time zone".into(), s(&tz, "TimeZoneKeyName")),
            ("System root".into(), env("SystemRoot")),
        ],
    ));

    let bios = hklm(r"HARDWARE\DESCRIPTION\System\BIOS");
    out.push((
        "Hardware",
        vec![
            ("Manufacturer".into(), s(&bios, "SystemManufacturer")),
            ("Model".into(), s(&bios, "SystemProductName")),
            ("Motherboard".into(), format!("{} {}", s(&bios, "BaseBoardManufacturer"), s(&bios, "BaseBoardProduct")).trim().to_string()),
            ("BIOS".into(), format!("{} {}", s(&bios, "BIOSVendor"), s(&bios, "BIOSVersion")).trim().to_string()),
            ("BIOS date".into(), s(&bios, "BIOSReleaseDate")),
        ],
    ));

    let cpu0 = hklm(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
    let logical = hklm(r"HARDWARE\DESCRIPTION\System\CentralProcessor").map(|k| k.subkeys().len()).unwrap_or(0);
    let mut kb = 0u64;
    let installed_ram = if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.is_ok() { crate::process::format_bytes(kb as f64 * 1024.0) } else { String::new() };
    out.push((
        "Processor and memory",
        vec![
            ("Processor".into(), s(&cpu0, "ProcessorNameString").trim().to_string()),
            ("Identifier".into(), s(&cpu0, "Identifier")),
            ("Rated speed".into(), cpu0.as_ref().and_then(|k| k.u32("~MHz")).map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or_default()),
            ("Logical processors".into(), logical.to_string()),
            ("Installed memory".into(), installed_ram),
        ],
    ));

    let mut gpus = Vec::new();
    if let Some(class) = hklm(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}") {
        for sub in class.subkeys().into_iter().filter(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit())) {
            let Some(k) = class.subkey(&sub) else { continue };
            let Some(desc) = k.string("DriverDesc") else { continue };
            let mem = match k.value("HardwareInformation.qwMemorySize") {
                Some(Value::U64(v)) => crate::process::format_bytes(v as f64),
                Some(Value::Bytes(b)) if b.len() >= 8 => crate::process::format_bytes(u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])) as f64),
                _ => match k.value("HardwareInformation.MemorySize") {
                    Some(Value::U32(v)) => crate::process::format_bytes(v as f64),
                    Some(Value::Bytes(b)) if b.len() >= 4 => crate::process::format_bytes(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                    _ => String::new(),
                },
            };
            let mut line = format!("driver {}", k.string("DriverVersion").unwrap_or_default());
            if !mem.is_empty() {
                line = format!("{mem}, {line}");
            }
            gpus.push((desc, line));
        }
    }
    if !gpus.is_empty() {
        out.push(("Display adapters", gpus));
    }
    for (_, rows) in out.iter_mut() {
        rows.retain(|(_, v)| !v.trim().is_empty());
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn system_info() -> Result<Sections, String> {
    Err("system information is only implemented on Windows".into())
}

/// Processor topology and platform facts shown under the CPU graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuDetails {
    pub sockets: usize,
    pub cores: usize,
    pub logical: usize,
    /// Total bytes of all L1 / L2 / L3 caches.
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
    pub virtualization: Option<bool>,
    pub virtual_machine: bool,
    pub fast_startup: Option<bool>,
}

/// Parses a `GetLogicalProcessorInformationEx(RelationAll)` buffer.
pub fn parse_topology(buf: &[u8]) -> CpuDetails {
    let mut d = CpuDetails::default();
    let mut off = 0usize;
    while off + 8 <= buf.len() {
        let rel = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]));
        let size = u32::from_ne_bytes(buf[off + 4..off + 8].try_into().unwrap_or([0; 4])) as usize;
        if size < 8 || off + size > buf.len() {
            break;
        }
        match rel {
            0 => {
                d.cores += 1;
                // PROCESSOR_RELATIONSHIP: Flags, EfficiencyClass, Reserved[20], GroupCount, GroupMask[]
                if off + 8 + 24 + 16 <= buf.len() {
                    let group_count = u16::from_ne_bytes([buf[off + 8 + 22], buf[off + 8 + 23]]) as usize;
                    for g in 0..group_count {
                        let m = off + 8 + 24 + g * 16;
                        if m + 8 <= buf.len() {
                            d.logical += u64::from_ne_bytes(buf[m..m + 8].try_into().unwrap_or([0; 8])).count_ones() as usize;
                        }
                    }
                }
            }
            3 => d.sockets += 1,
            2 if off + 16 <= buf.len() => {
                // CACHE_RELATIONSHIP: Level u8, Associativity u8, LineSize u16, CacheSize u32
                let level = buf[off + 8];
                let size_b = u32::from_ne_bytes(buf[off + 12..off + 16].try_into().unwrap_or([0; 4])) as u64;
                match level {
                    1 => d.l1 += size_b,
                    2 => d.l2 += size_b,
                    3 => d.l3 += size_b,
                    _ => {}
                }
            }
            _ => {}
        }
        off += size;
    }
    d
}

#[cfg(windows)]
pub fn cpu_details() -> CpuDetails {
    use super::winutil::{Key, View};
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationAll};
    use windows::Win32::System::Threading::{IsProcessorFeaturePresent, PROCESSOR_FEATURE_ID};
    let mut len = 0u32;
    // SAFETY: size query, then a read into an 8-byte aligned buffer of that size.
    let mut d = unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationAll, None, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8) + 1];
        if GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr() as *mut _), &mut len).is_ok() {
            let bytes = std::slice::from_raw_parts(buf.as_ptr() as *const u8, len as usize);
            parse_topology(bytes)
        } else {
            CpuDetails::default()
        }
    };
    // PF_VIRT_FIRMWARE_ENABLED (21)
    d.virtualization = Some(unsafe { IsProcessorFeaturePresent(PROCESSOR_FEATURE_ID(21)) }.as_bool());
    let bios = Key::open(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS", View::Default);
    let maker = bios.as_ref().and_then(|k| k.string("SystemManufacturer")).unwrap_or_default().to_lowercase();
    let model = bios.as_ref().and_then(|k| k.string("SystemProductName")).unwrap_or_default().to_lowercase();
    d.virtual_machine = ["vmware", "qemu", "innotek", "virtualbox", "xen", "parallels", "kvm"].iter().any(|v| maker.contains(v) || model.contains(v)) || model.contains("virtual machine");
    d.fast_startup = Key::open(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Power", View::Default).and_then(|k| k.u32("HiberbootEnabled")).map(|v| v == 1);
    d
}

#[cfg(not(windows))]
pub fn cpu_details() -> CpuDetails {
    CpuDetails::default()
}

/// Days since 1970 to `YYYY-MM-DD` (civil-from-days, UTC).
pub fn unix_date(secs: u64) -> String {
    let z = (secs / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn services() -> Result<Vec<Service>, String> {
    super::drivers::read(super::drivers::Kind::Services)
}

#[cfg(windows)]
pub fn startup_apps() -> Result<Vec<StartupApp>, String> {
    use super::winutil::{Key, Value, View};
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
    let approved = |root: HKEY, sub: &str| -> Vec<(String, Value)> { Key::open(root, &format!(r"{APPROVED}\{sub}"), View::Default).map(|k| k.values()).unwrap_or_default() };
    let is_enabled = |list: &[(String, Value)], name: &str| -> bool {
        match list.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some((_, Value::Bytes(b))) => approved_enabled(b),
            _ => true,
        }
    };
    let mut out = Vec::new();
    let sources: [(HKEY, View, &str, &str); 3] = [
        (HKEY_CURRENT_USER, View::Default, "Registry: current user", "Run"),
        (HKEY_LOCAL_MACHINE, View::Wow64_64, "Registry: all users", "Run"),
        (HKEY_LOCAL_MACHINE, View::Wow64_32, "Registry: all users (32-bit)", "Run32"),
    ];
    for (root, view, label, approved_sub) in sources {
        let Some(k) = Key::open(root, RUN, view) else { continue };
        let appr = approved(root, approved_sub);
        for (name, v) in k.values() {
            let Value::Str(cmd) = v else { continue };
            if name.is_empty() {
                continue;
            }
            let path = command_path(&cmd);
            out.push(StartupApp {
                enabled: is_enabled(&appr, &name),
                publisher: super::drivers::file_version(&path).map(|v| v.company).unwrap_or_default(),
                name,
                command: cmd,
                location: label.into(),
            });
        }
    }
    let folders = [
        (std::env::var("APPDATA").ok().map(|a| format!(r"{a}\Microsoft\Windows\Start Menu\Programs\Startup")), "Startup folder: current user", HKEY_CURRENT_USER),
        (std::env::var("ProgramData").ok().map(|a| format!(r"{a}\Microsoft\Windows\Start Menu\Programs\StartUp")), "Startup folder: all users", HKEY_LOCAL_MACHINE),
    ];
    for (dir, label, root) in folders {
        let Some(dir) = dir else { continue };
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let appr = approved(root, "StartupFolder");
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            if file.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            let name = file.rsplit_once('.').map(|(n, _)| n.to_string()).unwrap_or(file.clone());
            out.push(StartupApp { enabled: is_enabled(&appr, &file), publisher: String::new(), name, command: e.path().display().to_string(), location: label.into() });
        }
    }
    out.sort_by(|a, b| b.enabled.cmp(&a.enabled).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

#[cfg(not(windows))]
pub fn startup_apps() -> Result<Vec<StartupApp>, String> {
    Err("startup apps are only implemented on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_paths() {
        assert_eq!(command_path(r#""C:\Program Files\A\a.exe" --min"#), r"C:\Program Files\A\a.exe");
        assert_eq!(command_path(r"C:\Tools\b.exe /background"), r"C:\Tools\b.exe");
        assert_eq!(command_path(r"rundll32.exe shell32.dll,x"), "rundll32.exe");
    }

    #[test]
    fn approved_flags() {
        assert!(approved_enabled(&[2, 0, 0]));
        assert!(approved_enabled(&[6]));
        assert!(!approved_enabled(&[3, 0]));
        assert!(!approved_enabled(&[7]));
    }

    #[test]
    fn dates() {
        assert_eq!(unix_date(0), "1970-01-01");
        assert_eq!(unix_date(1_700_000_000), "2023-11-14");
    }

    #[test]
    fn topology_parsing() {
        let mut buf = Vec::new();
        // one core record (size 48) with group mask 0b11, one L2 cache record (size 48), one package (size 48)
        let mut core = vec![0u8; 48];
        core[4..8].copy_from_slice(&48u32.to_ne_bytes());
        core[8 + 22] = 1;
        core[8 + 24..8 + 32].copy_from_slice(&3u64.to_ne_bytes());
        buf.extend(core);
        let mut cache = vec![0u8; 48];
        cache[0..4].copy_from_slice(&2u32.to_ne_bytes());
        cache[4..8].copy_from_slice(&48u32.to_ne_bytes());
        cache[8] = 2;
        cache[12..16].copy_from_slice(&(1u32 << 20).to_ne_bytes());
        buf.extend(cache);
        let mut pkg = vec![0u8; 48];
        pkg[0..4].copy_from_slice(&3u32.to_ne_bytes());
        pkg[4..8].copy_from_slice(&48u32.to_ne_bytes());
        buf.extend(pkg);
        let d = parse_topology(&buf);
        assert_eq!((d.cores, d.logical, d.sockets, d.l2), (1, 2, 1, 1 << 20));
    }

    #[cfg(windows)]
    #[test]
    fn live_cpu_details() {
        let d = cpu_details();
        assert!(d.cores >= 1 && d.logical >= d.cores && d.sockets >= 1);
    }

    #[cfg(windows)]
    #[test]
    fn system_info_has_os_section() {
        let s = system_info().unwrap();
        assert_eq!(s[0].0, "Operating system");
        assert!(s[0].1.iter().any(|(k, _)| k == "OS build"));
        let _ = startup_apps().unwrap();
    }
}
