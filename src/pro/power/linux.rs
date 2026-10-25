//! Linux power: `/sys/class/power_supply`, power-profiles-daemon (else the ACPI platform
//! profile) and the cpufreq policies.

use super::parse;
use super::PowerInfo;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn attr(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string())
}

fn subdirs(dir: &str, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(prefix)).map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

pub fn read() -> Result<PowerInfo, String> {
    let mut info = PowerInfo::default();
    let supplies: Vec<parse::Supply> = subdirs("/sys/class/power_supply", "").iter().map(|d| parse::supply(&|k| attr(d, k))).collect();
    (info.ac_online, info.battery) = parse::combine(&supplies);

    let profile_src = match crate::platform::run_capture("powerprofilesctl", &["list"], Duration::from_secs(5)).map(|t| parse::parse_profiles(&t)) {
        Ok(list) if !list.is_empty() => {
            info.schemes = list.into_iter().map(|(id, active)| (id.clone(), parse::profile_title(&id), active)).collect();
            "power-profiles-daemon"
        }
        _ => {
            let acpi = Path::new("/sys/firmware/acpi");
            let current = attr(acpi, "platform_profile").unwrap_or_default();
            let choices = attr(acpi, "platform_profile_choices").unwrap_or_default();
            info.schemes = choices.split_whitespace().map(|c| (c.to_string(), parse::profile_title(c), c == current)).collect();
            if info.schemes.is_empty() {
                ""
            } else {
                "the ACPI platform profile"
            }
        }
    };
    if let Some(active) = info.schemes.iter().find(|s| s.2) {
        info.scheme_guid = active.0.clone();
        info.scheme_name = active.1.clone();
    }

    let base = Path::new("/sys/devices/system/cpu/cpufreq");
    let dirs = subdirs("/sys/devices/system/cpu/cpufreq", "policy");
    let readers: Vec<_> = dirs.iter().map(|d| move |k: &str| attr(d, k)).collect();
    let refs: Vec<parse::Attr> = readers.iter().map(|f| f as parse::Attr).collect();
    info.policy = parse::cpufreq_policy(&refs, &|k| attr(base, k));

    info.note = match profile_src {
        "" => "Read from sysfs. No power profiles are offered (power-profiles-daemon is not running).".into(),
        src => format!("Profiles from {src}; switch them in the desktop's power settings or with powerprofilesctl."),
    };
    Ok(info)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_error() {
        let p = super::read().unwrap();
        assert!(!p.note.is_empty());
        assert!(p.schemes.iter().filter(|s| s.2).count() <= 1);
    }
}
