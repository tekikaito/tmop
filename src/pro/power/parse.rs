//! Pure parsers behind the Linux (sysfs, power-profiles-daemon) and macOS (`pmset`, `ioreg`)
//! power readers (tested on every host).

use super::{describe, Battery, PolicyRow};
use crate::pro::sysfmt::Plist;

/// A sysfs directory: attribute file name to its trimmed content.
pub type Attr<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One `/sys/class/power_supply/<name>` as read through `attr` (file name to trimmed content).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Supply {
    /// "Mains", "USB", "Battery", ...
    pub kind: String,
    pub online: Option<bool>,
    pub battery: Option<Battery>,
}

/// Reads a supply. Batteries of devices (`scope` = Device: mice, headsets) are skipped.
pub fn supply(attr: Attr) -> Supply {
    let kind = attr("type").unwrap_or_default();
    let num = |k: &str| attr(k).and_then(|v| v.trim().parse::<i64>().ok());
    let mut s = Supply { online: num("online").map(|v| v == 1), ..Default::default() };
    if kind == "Battery" && attr("scope").is_none_or(|s| s != "Device") && attr("present").is_none_or(|p| p == "1") {
        // energy_* in µWh and power_now in µW, or charge_* in µAh and current_now in µA (times µV).
        let volts = num("voltage_now").map(|v| v as f64 / 1e6);
        let wh = |e: &str, c: &str| num(e).map(|v| v as f64 / 1e6).or_else(|| Some(num(c)? as f64 / 1e6 * volts?));
        let now = wh("energy_now", "charge_now");
        let full = wh("energy_full", "charge_full");
        let watts = num("power_now").map(|v| v.unsigned_abs() as f64 / 1e6).or_else(|| Some(num("current_now")?.unsigned_abs() as f64 / 1e6 * volts?));
        let status = attr("status").unwrap_or_default();
        let charging = status == "Charging";
        let percent = num("capacity").map(|c| c.clamp(0, 100) as u8).or_else(|| Some((now? / full?.max(1e-9) * 100.0).round().clamp(0.0, 100.0) as u8)).unwrap_or(0);
        let mw = watts.map(|w| (w * 1000.0).round() as i32).unwrap_or(0);
        let rate_mw = match status.as_str() {
            "Charging" => mw,
            "Discharging" => -mw,
            _ => 0,
        };
        let seconds_left = num("time_to_empty_now").filter(|s| *s > 0).map(|s| s as u32).or_else(|| match (status.as_str(), now, watts) {
            ("Discharging", Some(e), Some(w)) if w > 0.05 => Some((e / w * 3600.0) as u32),
            _ => None,
        });
        s.battery = Some(Battery {
            percent,
            charging,
            rate_mw,
            remaining_mwh: now.map(|e| (e * 1000.0) as u32).unwrap_or(0),
            full_mwh: full.map(|e| (e * 1000.0) as u32).unwrap_or(0),
            seconds_left,
        });
    }
    s.kind = kind;
    s
}

/// The AC state and the combined battery of all supplies.
pub fn combine(supplies: &[Supply]) -> (Option<bool>, Option<Battery>) {
    let batteries: Vec<&Battery> = supplies.iter().filter_map(|s| s.battery.as_ref()).collect();
    let mains: Vec<bool> = supplies.iter().filter(|s| matches!(s.kind.as_str(), "Mains" | "USB" | "USB_C" | "USB_PD")).filter_map(|s| s.online).collect();
    let mut ac = if mains.is_empty() { None } else { Some(mains.iter().any(|o| *o)) };
    let battery = match batteries.as_slice() {
        [] => None,
        [one] => Some((*one).clone()),
        many => {
            let full: u32 = many.iter().map(|b| b.full_mwh).sum();
            let now: u32 = many.iter().map(|b| b.remaining_mwh).sum();
            Some(Battery {
                percent: if full > 0 { (now as u64 * 100 / full as u64).min(100) as u8 } else { (many.iter().map(|b| b.percent as u32).sum::<u32>() / many.len() as u32) as u8 },
                charging: many.iter().any(|b| b.charging),
                rate_mw: many.iter().map(|b| b.rate_mw).sum(),
                remaining_mwh: now,
                full_mwh: full,
                seconds_left: many.iter().filter_map(|b| b.seconds_left).reduce(|a, b| a + b),
            })
        }
    };
    if ac.is_none() {
        if let Some(b) = &battery {
            ac = if b.rate_mw < 0 { Some(false) } else if b.charging { Some(true) } else { None };
        }
    }
    (ac, battery)
}

/// `powerprofilesctl list`: (profile, active), the active one marked with `*`.
pub fn parse_profiles(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let active = l.starts_with('*');
            let name = l.trim_start_matches('*').trim().strip_suffix(':')?;
            (!name.is_empty() && !l.starts_with("    ") && !l.starts_with('\t') && !name.contains(' ')).then(|| (name.to_string(), active))
        })
        .collect()
}

/// `"power-saver"` to `"Power saver"`, `"low-power"` to `"Low power"`.
pub fn profile_title(id: &str) -> String {
    let words = id.replace(['-', '_'], " ");
    let mut c = words.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// kHz (cpufreq) as GHz text.
pub fn khz_text(khz: &str) -> Option<String> {
    khz.trim().parse::<u64>().ok().filter(|k| *k > 0).map(|k| format!("{:.2} GHz", k as f64 / 1e6))
}

/// The cpufreq policy rows from `/sys/devices/system/cpu/cpufreq` (paths relative to it). The
/// governors of all policies are listed when they differ. No AC/DC split on Linux: `dc` is empty.
pub fn cpufreq_policy(policies: &[Attr], global: Attr) -> Vec<PolicyRow> {
    let mut rows = Vec::new();
    let mut row = |name: &'static str, v: String| {
        if !v.is_empty() {
            rows.push(PolicyRow { name, ac: v, dc: String::new() });
        }
    };
    let all = |k: &str| {
        let mut vals: Vec<String> = Vec::new();
        for p in policies {
            if let Some(v) = p(k).filter(|v| !v.is_empty()) {
                if !vals.contains(&v) {
                    vals.push(v);
                }
            }
        }
        vals.join(", ")
    };
    let first = |k: &str| policies.iter().find_map(|p| p(k)).unwrap_or_default();
    row("Scaling driver", all("scaling_driver"));
    row("Governor", all("scaling_governor"));
    row("Minimum frequency", khz_text(&first("scaling_min_freq")).unwrap_or_default());
    row("Maximum frequency", khz_text(&first("scaling_max_freq")).unwrap_or_default());
    row("Energy performance preference", all("energy_performance_preference"));
    let boost = global("boost").map(|b| b == "1").or_else(|| global("../intel_pstate/no_turbo").map(|n| n == "0"));
    row("Performance boost", boost.map(|b| if b { "Enabled" } else { "Disabled" }.to_string()).unwrap_or_default());
    rows
}

/// Battery state from `pmset -g batt`, and whether power comes from AC.
pub fn parse_pmset_batt(text: &str) -> (Option<bool>, Option<Battery>) {
    let ac = text.lines().find_map(|l| l.split_once("drawing from '")).map(|(_, r)| r.starts_with("AC"));
    let line = text.lines().find(|l| l.contains("InternalBattery") || (l.contains('%') && l.contains(';')));
    let Some(line) = line else { return (ac, None) };
    let parts: Vec<&str> = line.split(['\t', ';']).map(str::trim).collect();
    let percent = parts.iter().find_map(|p| p.strip_suffix('%').and_then(|n| n.rsplit(' ').next()?.parse::<u8>().ok()));
    let Some(percent) = percent else { return (ac, None) };
    let state = parts.iter().find(|p| ["charging", "discharging", "charged", "finishing charge", "AC attached"].iter().any(|s| p.starts_with(s))).copied().unwrap_or("");
    let left = parts.iter().find_map(|p| {
        let t = p.split_whitespace().next()?;
        let (h, m) = t.split_once(':')?;
        let secs = h.parse::<u32>().ok()? * 3600 + m.parse::<u32>().ok()? * 60;
        p.contains("remaining").then_some(secs)
    });
    let charging = state.starts_with("charging") || state.starts_with("finishing");
    let battery = Battery { percent: percent.min(100), charging, seconds_left: left.filter(|s| *s > 0 && !charging), ..Default::default() };
    (ac, Some(battery))
}

/// Rate, remaining and full charge from `ioreg -rn AppleSmartBattery -a` (an array of dicts):
/// `(rate_mw, remaining_mwh, full_mwh)`; the rate is negative while discharging.
pub fn ioreg_battery(p: &Plist) -> Option<(i32, u32, u32)> {
    let b = p.array().first().unwrap_or(p);
    let get = |k: &str| b.get(k).and_then(Plist::int);
    let mv = get("Voltage")? as f64;
    let amps = get("InstantAmperage").or_else(|| get("Amperage")).unwrap_or(0) as f64;
    let now = get("AppleRawCurrentCapacity").or_else(|| get("CurrentCapacity"))? as f64;
    let full = get("AppleRawMaxCapacity").or_else(|| get("MaxCapacity"))? as f64;
    let v = mv / 1000.0;
    Some(((amps * v).round() as i32, (now * v) as u32, (full * v) as u32))
}

/// `pmset -g custom`: the settings per source, ("AC Power" | "Battery Power" | "UPS Power", pairs).
pub fn parse_pmset_custom(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            if let Some(src) = line.trim().strip_suffix(':') {
                out.push((src.to_string(), Vec::new()));
            }
            continue;
        }
        let mut it = line.split_whitespace();
        if let (Some(k), Some(v), Some(cur)) = (it.next(), it.next(), out.last_mut()) {
            cur.1.push((k.to_string(), v.to_string()));
        }
    }
    out
}

/// The `pmset` settings shown as policy rows, AC and battery side by side.
pub fn pmset_policy(custom: &[(String, Vec<(String, String)>)]) -> Vec<PolicyRow> {
    const ROWS: [(&str, &str, &str); 6] = [
        ("Sleep after", "sleep", "min"),
        ("Display sleep after", "displaysleep", "min"),
        ("Disk sleep after", "disksleep", "min"),
        ("Low Power Mode", "lowpowermode", "flag"),
        ("Power Nap", "powernap", "flag"),
        ("Wake for network access", "womp", "flag"),
    ];
    let value = |src: &str, key: &str, kind: &str| -> String {
        let Some(v) = custom.iter().find(|c| c.0.starts_with(src)).and_then(|c| c.1.iter().find(|(k, _)| k == key)).map(|(_, v)| v.as_str()) else { return String::new() };
        match (kind, v.parse::<u32>()) {
            ("min", Ok(m)) => describe("sleep", m * 60),
            ("flag", Ok(f)) => if f == 0 { "Off" } else { "On" }.into(),
            _ => v.to_string(),
        }
    };
    let has_battery = custom.iter().any(|c| c.0.starts_with("Battery"));
    ROWS.iter()
        .filter_map(|(name, key, kind)| {
            let ac = value("AC", key, kind);
            let dc = value("Battery", key, kind);
            if ac.is_empty() && dc.is_empty() {
                return None;
            }
            let dash = |s: String| if s.is_empty() { "\u{2014}".to_string() } else { s };
            Some(PolicyRow { name, ac: dash(ac), dc: if has_battery { dash(dc) } else { String::new() } })
        })
        .collect()
}

/// The `lowpowermode` (or `powermode`) value in effect, from `pmset -g`.
pub fn pmset_low_power(text: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        match (it.next(), it.next()) {
            (Some("lowpowermode"), Some(v)) => Some(v != "0"),
            (Some("powermode"), Some(v)) => Some(v == "1"),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::sysfmt::parse_plist_xml;
    use std::collections::HashMap;

    fn sysfs(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn sysfs_batteries() {
        // Energy-reporting battery, discharging at 12.5 W.
        let bat0 = supply(&sysfs(&[("type", "Battery"), ("status", "Discharging"), ("present", "1"), ("capacity", "80"), ("energy_now", "40000000"), ("energy_full", "50000000"), ("power_now", "12500000")]));
        let b = bat0.battery.clone().unwrap();
        assert_eq!((b.percent, b.charging, b.rate_mw, b.remaining_mwh, b.full_mwh), (80, false, -12_500, 40_000, 50_000));
        assert_eq!(b.seconds_left, Some(11_520));
        // Charge-reporting battery, charging at 2 A x 12 V.
        let bat1 = supply(&sysfs(&[("type", "Battery"), ("status", "Charging"), ("charge_now", "2000000"), ("charge_full", "4000000"), ("current_now", "2000000"), ("voltage_now", "12000000")]));
        let b1 = bat1.battery.clone().unwrap();
        assert_eq!((b1.percent, b1.charging, b1.rate_mw, b1.remaining_mwh, b1.full_mwh, b1.seconds_left), (50, true, 24_000, 24_000, 48_000, None));
        let mouse = supply(&sysfs(&[("type", "Battery"), ("scope", "Device"), ("capacity", "20")]));
        assert!(mouse.battery.is_none());
        let ac = supply(&sysfs(&[("type", "Mains"), ("online", "1")]));
        assert_eq!((ac.kind.as_str(), ac.online), ("Mains", Some(true)));

        assert_eq!(combine(&[]), (None, None));
        let (on_ac, one) = combine(&[ac.clone(), bat0.clone()]);
        assert_eq!((on_ac, one.map(|b| b.percent)), (Some(true), Some(80)));
        let (on_ac, both) = combine(&[bat0, bat1, mouse]);
        let both = both.unwrap();
        assert_eq!((on_ac, both.full_mwh, both.remaining_mwh, both.percent, both.rate_mw), (Some(true), 98_000, 64_000, 65, 11_500));
    }

    #[test]
    fn profiles() {
        let text = "  performance:\n    CpuDriver:\tamd_pstate\n    PlatformDriver:\tplatform_profile\n    Degraded:   no\n\n* balanced:\n    CpuDriver:\tamd_pstate\n\n  power-saver:\n    CpuDriver:\tamd_pstate\n";
        assert_eq!(parse_profiles(text), [("performance".to_string(), false), ("balanced".to_string(), true), ("power-saver".to_string(), false)]);
        assert_eq!(profile_title("power-saver"), "Power saver");
        assert_eq!(profile_title("balanced_performance"), "Balanced performance");
        assert_eq!(khz_text("3800000"), Some("3.80 GHz".into()));
        assert_eq!(khz_text("x"), None);
    }

    #[test]
    fn cpufreq_rows() {
        let p0 = sysfs(&[("scaling_driver", "amd-pstate-epp"), ("scaling_governor", "powersave"), ("scaling_min_freq", "400000"), ("scaling_max_freq", "5083000"), ("energy_performance_preference", "balance_performance")]);
        let p1 = sysfs(&[("scaling_driver", "amd-pstate-epp"), ("scaling_governor", "performance"), ("energy_performance_preference", "balance_performance")]);
        let global = sysfs(&[("boost", "1")]);
        let rows = cpufreq_policy(&[&p0, &p1], &global);
        let get = |n: &str| rows.iter().find(|r| r.name == n).map(|r| r.ac.clone()).unwrap_or_default();
        assert_eq!(get("Governor"), "powersave, performance");
        assert_eq!(get("Scaling driver"), "amd-pstate-epp");
        assert_eq!(get("Maximum frequency"), "5.08 GHz");
        assert_eq!(get("Performance boost"), "Enabled");
        assert!(rows.iter().all(|r| r.dc.is_empty()));
        let intel = cpufreq_policy(&[], &sysfs(&[("../intel_pstate/no_turbo", "1")]));
        assert_eq!((intel.len(), intel[0].ac.as_str()), (1, "Disabled"));
    }

    #[test]
    fn pmset() {
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t85%; discharging; 4:12 remaining present: true\n";
        let (ac, b) = parse_pmset_batt(batt);
        let b = b.unwrap();
        assert_eq!((ac, b.percent, b.charging, b.seconds_left), (Some(false), 85, false, Some(4 * 3600 + 12 * 60)));
        let (ac, b) = parse_pmset_batt("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n");
        assert_eq!((ac, b.as_ref().map(|b| (b.percent, b.charging, b.seconds_left))), (Some(true), Some((100, false, None))));
        let (ac, b) = parse_pmset_batt("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t42%; charging; (no estimate) present: true\n");
        assert_eq!((ac, b.map(|b| b.charging)), (Some(true), Some(true)));
        assert_eq!(parse_pmset_batt("Now drawing from 'AC Power'\n"), (Some(true), None));

        let custom = "Battery Power:\n Sleep On Power Button 1\n lowpowermode         1\n standby              1\n powernap             0\n sleep                1\n displaysleep         2\nAC Power:\n Sleep On Power Button 1\n lowpowermode         0\n powernap             1\n sleep                0 (sleep prevented by coreaudiod)\n displaysleep         10\n womp                 1\n";
        let parsed = parse_pmset_custom(custom);
        assert_eq!(parsed.len(), 2);
        let rows = pmset_policy(&parsed);
        let get = |n: &str| rows.iter().find(|r| r.name == n).map(|r| (r.ac.clone(), r.dc.clone())).unwrap();
        assert_eq!(get("Sleep after"), ("Never".into(), "1 min".into()));
        assert_eq!(get("Display sleep after"), ("10 min".into(), "2 min".into()));
        assert_eq!(get("Low Power Mode"), ("Off".into(), "On".into()));
        assert_eq!(get("Wake for network access"), ("On".into(), "\u{2014}".into()));
        let desktop = pmset_policy(&parse_pmset_custom("AC Power:\n sleep 0\n"));
        assert_eq!((desktop[0].ac.as_str(), desktop[0].dc.as_str()), ("Never", ""));
        assert_eq!(pmset_low_power("System-wide power settings:\nCurrently in use:\n standby 1\n lowpowermode 1\n"), Some(true));
        assert_eq!(pmset_low_power(" powermode 0\n"), Some(false));
        assert_eq!(pmset_low_power(""), None);
    }

    #[test]
    fn ioreg() {
        let p = parse_plist_xml("<plist><array><dict><key>Voltage</key><integer>12000</integer><key>InstantAmperage</key><integer>18446744073709550616</integer><key>AppleRawCurrentCapacity</key><integer>4000</integer><key>AppleRawMaxCapacity</key><integer>5000</integer></dict></array></plist>").unwrap();
        assert_eq!(ioreg_battery(&p), Some((-12_000, 48_000, 60_000)));
        assert_eq!(ioreg_battery(&Plist::Array(vec![])), None);
    }
}
