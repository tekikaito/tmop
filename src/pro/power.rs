//! Power & frequency: active power scheme, processor power policy, battery state.
//!
//! Sources: `PowerGetActiveScheme` and
//! `PowerRead{AC,DC}ValueIndex` for the processor policy, `GetSystemPowerStatus` and
//! `CallNtPowerInformation(SystemBatteryState)` for the battery. Per-core clocks come
//! from the sampler (`CallNtPowerInformation(ProcessorInformation)`).
//!
//! Linux reads `/sys/class/power_supply`, power-profiles-daemon (else the ACPI platform
//! profile) as the schemes and the cpufreq policies (no AC/DC split); macOS reads `pmset`
//! (settings per power source, Low Power Mode as the scheme) and `ioreg` for the charge rate.

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
pub struct PolicyRow {
    pub name: &'static str,
    pub ac: String,
    /// Empty when the OS has one value for both sources (Linux).
    pub dc: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    /// Positive while charging, negative while discharging (mW).
    pub rate_mw: i32,
    pub remaining_mwh: u32,
    pub full_mwh: u32,
    pub seconds_left: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerInfo {
    pub scheme_name: String,
    pub scheme_guid: String,
    /// (guid or profile id, name, active); may be empty.
    pub schemes: Vec<(String, String, bool)>,
    pub policy: Vec<PolicyRow>,
    pub ac_online: Option<bool>,
    pub battery: Option<Battery>,
    /// Where this was read and where to change it (the page's footer).
    pub note: String,
}

/// The page footer before the first read: the Windows source (elsewhere the back end's note).
pub const DEFAULT_NOTE: &str = if cfg!(windows) { "The active plan is read from powercfg; switch plans in Windows settings." } else { "" };

/// What the OS calls a power scheme, singular and plural: "Power plan" on Windows,
/// "Power profile" on Linux, "Energy mode" on macOS.
pub const fn scheme_words() -> (&'static str, &'static str) {
    if cfg!(target_os = "linux") {
        ("Power profile", "profiles")
    } else if cfg!(target_os = "macos") {
        ("Energy mode", "modes")
    } else {
        ("Power plan", "plans")
    }
}

/// Human text for a processor policy value.
pub fn describe(setting: &str, v: u32) -> String {
    match setting {
        "boost" => match v {
            0 => "Disabled",
            1 => "Enabled",
            2 => "Aggressive",
            3 => "Efficient enabled",
            4 => "Efficient aggressive",
            5 => "Aggressive at guaranteed",
            6 => "Efficient aggressive at guaranteed",
            _ => "Unknown",
        }
        .to_string(),
        "cooling" => if v == 0 { "Passive" } else { "Active" }.to_string(),
        "epp" => format!("{v}% {}", if v <= 25 { "(performance)" } else if v >= 75 { "(efficiency)" } else { "(balanced)" }),
        "sleep" => {
            if v == 0 {
                "Never".into()
            } else if v.is_multiple_of(3600) {
                format!("{} h", v / 3600)
            } else {
                format!("{} min", v / 60)
            }
        }
        _ => format!("{v}%"),
    }
}

#[cfg(windows)]
pub fn read() -> Result<PowerInfo, String> {
    use windows::core::GUID;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::System::Power::{
        CallNtPowerInformation, GetSystemPowerStatus, PowerEnumerate, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerReadFriendlyName,
        SystemBatteryState, ACCESS_SCHEME, SYSTEM_BATTERY_STATE, SYSTEM_POWER_STATUS,
    };

    const SUB_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
    const SUB_SLEEP: GUID = GUID::from_u128(0x238c9fa8_0aad_41ed_83f4_97be242c8f20);
    let settings: [(&'static str, &'static str, GUID, GUID); 6] = [
        ("Minimum processor state", "pct", SUB_PROCESSOR, GUID::from_u128(0x893dee8e_2bef_41e0_89c6_b55d0929964c)),
        ("Maximum processor state", "pct", SUB_PROCESSOR, GUID::from_u128(0xbc5038f7_23e0_4960_96da_33abaf5935ec)),
        ("Energy performance preference", "epp", SUB_PROCESSOR, GUID::from_u128(0x36687f9e_e3a5_4dbf_b1dc_15eb381c6863)),
        ("Performance boost mode", "boost", SUB_PROCESSOR, GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7)),
        ("System cooling policy", "cooling", SUB_PROCESSOR, GUID::from_u128(0x94d3a615_a899_4ac5_ae2b_e4d8f634367f)),
        ("Sleep after", "sleep", SUB_SLEEP, GUID::from_u128(0x29f6c1db_86da_48c5_9fdb_f2b67b1f44da)),
    ];

    fn guid_text(g: &GUID) -> String {
        format!("{:?}", g).to_lowercase()
    }

    fn friendly(g: &GUID) -> String {
        let mut size = 0u32;
        // SAFETY: size query, then a read into a buffer of that size.
        unsafe {
            let _ = PowerReadFriendlyName(None, Some(g), None, None, None, &mut size);
            if size == 0 || size > 4096 {
                return String::new();
            }
            let mut buf = vec![0u8; size as usize];
            if PowerReadFriendlyName(None, Some(g), None, None, Some(buf.as_mut_ptr()), &mut size).0 != 0 {
                return String::new();
            }
            let w: Vec<u16> = buf.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            super::winutil::from_wide(&w)
        }
    }

    let mut info = PowerInfo { note: DEFAULT_NOTE.into(), ..Default::default() };
    // SAFETY: PowerGetActiveScheme allocates a GUID that we free with LocalFree.
    let active: Option<GUID> = unsafe {
        let mut p: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut p).0 == 0 && !p.is_null() {
            let g = *p;
            let _ = LocalFree(HLOCAL(p as *mut _));
            Some(g)
        } else {
            None
        }
    };
    if let Some(g) = active {
        info.scheme_guid = guid_text(&g);
        info.scheme_name = friendly(&g);
        for (name, kind, sub, setting) in settings.iter() {
            let mut ac = 0u32;
            let mut dc = 0u32;
            // SAFETY: plain reads into u32 outputs.
            let ok_ac = unsafe { PowerReadACValueIndex(None, Some(&g), Some(sub), Some(setting), &mut ac) }.0 == 0;
            let ok_dc = unsafe { PowerReadDCValueIndex(None, Some(&g), Some(sub), Some(setting), &mut dc) } == 0;
            if ok_ac || ok_dc {
                info.policy.push(PolicyRow {
                    name,
                    ac: if ok_ac { describe(kind, ac) } else { "\u{2014}".into() },
                    dc: if ok_dc { describe(kind, dc) } else { "\u{2014}".into() },
                });
            }
        }
    }
    // All schemes.
    for i in 0..32u32 {
        let mut g = GUID::zeroed();
        let mut size = std::mem::size_of::<GUID>() as u32;
        // SAFETY: the buffer is a GUID of the size passed.
        let st = unsafe { PowerEnumerate(None, None, None, ACCESS_SCHEME, i, Some(&mut g as *mut GUID as *mut u8), &mut size) };
        if st.0 != 0 {
            break;
        }
        info.schemes.push((guid_text(&g), friendly(&g), Some(g) == active));
    }
    // AC line and battery.
    let mut st = SYSTEM_POWER_STATUS::default();
    // SAFETY: writes the struct.
    if unsafe { GetSystemPowerStatus(&mut st) }.is_ok() {
        info.ac_online = match st.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let has_battery = st.BatteryFlag != 128 && st.BatteryFlag != 255;
        if has_battery {
            let mut bs = SYSTEM_BATTERY_STATE::default();
            // SAFETY: output buffer is a SYSTEM_BATTERY_STATE.
            let ok = unsafe {
                CallNtPowerInformation(SystemBatteryState, None, 0, Some(&mut bs as *mut _ as *mut _), std::mem::size_of::<SYSTEM_BATTERY_STATE>() as u32)
            }
            .is_ok();
            info.battery = Some(Battery {
                percent: st.BatteryLifePercent.min(100),
                charging: ok && bs.Charging.as_bool(),
                rate_mw: if ok { bs.Rate as i32 } else { 0 },
                remaining_mwh: if ok { bs.RemainingCapacity } else { 0 },
                full_mwh: if ok { bs.MaxCapacity } else { 0 },
                seconds_left: (st.BatteryLifeTime != u32::MAX).then_some(st.BatteryLifeTime),
            });
        }
    }
    Ok(info)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn read() -> Result<PowerInfo, String> {
    Err(format!("power information is not implemented on {}", crate::platform::os_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions() {
        assert_eq!(describe("pct", 100), "100%");
        assert_eq!(describe("boost", 2), "Aggressive");
        assert_eq!(describe("sleep", 0), "Never");
        assert_eq!(describe("sleep", 1800), "30 min");
        assert!(describe("epp", 0).contains("performance"));
    }

    #[cfg(windows)]
    #[test]
    fn reads_active_scheme() {
        let p = read().unwrap();
        assert!(!p.scheme_guid.is_empty());
        assert!(p.schemes.iter().any(|s| s.2), "active scheme listed");
    }
}
