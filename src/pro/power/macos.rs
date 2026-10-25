//! macOS power: `pmset -g batt` for the battery, `pmset -g custom` for the AC and battery
//! settings, `ioreg` for the charge rate, Low Power Mode as the energy mode.

use super::parse;
use super::PowerInfo;
use crate::platform::run_capture;
use std::time::Duration;

pub fn read() -> Result<PowerInfo, String> {
    let t = Duration::from_secs(5);
    let mut info = PowerInfo::default();
    let batt = run_capture("pmset", &["-g", "batt"], t)?;
    (info.ac_online, info.battery) = parse::parse_pmset_batt(&batt);
    if let Some(b) = info.battery.as_mut() {
        let smart = run_capture("ioreg", &["-rn", "AppleSmartBattery", "-a"], t).ok().and_then(|x| crate::pro::sysfmt::parse_plist_xml(&x));
        if let Some((rate, now, full)) = smart.as_ref().and_then(parse::ioreg_battery) {
            (b.rate_mw, b.remaining_mwh, b.full_mwh) = (rate, now, full);
        }
    }
    if let Ok(custom) = run_capture("pmset", &["-g", "custom"], t) {
        info.policy = parse::pmset_policy(&parse::parse_pmset_custom(&custom));
    }
    if let Some(low) = run_capture("pmset", &["-g"], t).ok().and_then(|s| parse::pmset_low_power(&s)) {
        info.schemes = vec![("automatic".into(), "Automatic".into(), !low), ("lowpower".into(), "Low Power".into(), low)];
        let active = &info.schemes[usize::from(low)];
        (info.scheme_guid, info.scheme_name) = (active.0.clone(), active.1.clone());
    }
    info.note = "Read from pmset; change these in System Settings \u{2192} Battery (or Energy).".into();
    Ok(info)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_error() {
        let p = super::read().unwrap();
        assert!(!p.note.is_empty());
    }
}
