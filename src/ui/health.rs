//! A one-glance verdict on how the machine is doing, built only from the numbers in the
//! snapshot and the process list (Summary hero, popout health line).

use crate::metrics::Snapshot;
use crate::process::ProcessRow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Memory use (percent) from which the machine counts as elevated / critical.
pub const MEMORY_LEVELS: (f32, f32) = (75.0, 90.0);
/// CPU use (percent).
pub const CPU_LEVELS: (f32, f32) = (80.0, 95.0);
/// Disk active time (percent).
pub const DISK_LEVELS: (f32, f32) = (60.0, 90.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Good,
    Elevated,
    Critical,
}

/// The verdict: a level, a headline and a sentence of detail, and the busiest app.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub level: Level,
    /// "Everything looks good.", "Memory is almost full."
    pub headline: String,
    pub detail: String,
    /// Display name of the app group using the most CPU (summed over its processes).
    pub busiest: Option<String>,
    /// That group's CPU use in percent.
    pub busiest_cpu: f32,
}

/// The level of one resource at `pct` against its (elevated, critical) thresholds.
pub fn level_of(pct: f32, (elevated, critical): (f32, f32)) -> Level {
    if pct >= critical {
        Level::Critical
    } else if pct >= elevated {
        Level::Elevated
    } else {
        Level::Good
    }
}

/// The app group with the most CPU: processes are grouped by lowercase image name, a group is
/// named after its first description (else its image name).
pub fn busiest_app(rows: &[ProcessRow]) -> Option<(String, f32)> {
    let mut groups: HashMap<String, (String, f32)> = HashMap::new();
    for r in rows {
        let e = groups.entry(r.name.to_lowercase()).or_insert_with(|| (r.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| r.name.clone()), 0.0));
        e.1 += r.cpu_pct;
    }
    groups.into_values().filter(|(_, cpu)| *cpu > 0.0).max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
}

fn lasted(since: Option<Instant>) -> String {
    match since.map(|t| t.elapsed()) {
        Some(d) if d >= Duration::from_secs(60) => format!(" This has lasted {} min.", d.as_secs() / 60),
        Some(d) if d >= Duration::from_secs(10) => format!(" This has lasted {} s.", d.as_secs()),
        _ => String::new(),
    }
}

/// How the machine is doing. `since` is when the current Critical level began, if the caller
/// tracks it.
pub fn health(snap: &Snapshot, rows: &[ProcessRow], since: Option<Instant>) -> Health {
    let (mem, cpu, disk) = (snap.memory.used_pct, snap.cpu.total_pct, snap.disk.active_pct);
    let levels = [(level_of(mem, MEMORY_LEVELS), 0), (level_of(cpu, CPU_LEVELS), 1), (level_of(disk, DISK_LEVELS), 2)];
    // The worst resource decides; on a tie memory beats CPU beats disk.
    let (level, which) = levels.iter().rev().fold((Level::Good, 0), |best, &(l, i)| if l >= best.0 && l != Level::Good { (l, i) } else { best });
    let busiest = busiest_app(rows);
    let busy = busiest.as_ref().map(|(n, c)| format!("{n} is the busiest app right now ({c:.0}% CPU)."));
    let (headline, detail) = match (level, which) {
        (Level::Good, _) => ("Everything looks good.".to_string(), format!("{} No resource is under pressure.", busy.unwrap_or_default()).trim().to_string()),
        (Level::Elevated, 0) => ("Memory is getting full.".into(), format!("{mem:.0}% of memory is in use. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, 0) => ("Memory is almost full.".into(), format!("{mem:.0}% of memory is in use, so Windows moves data to disk to free some, which makes apps feel slow.{}", lasted(since))),
        (Level::Elevated, 1) => ("CPU is busy.".into(), format!("The processor is at {cpu:.0}%. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, 1) => ("CPU is under heavy load.".into(), format!("The processor is at {cpu:.0}%. {}{}", busy.unwrap_or_default(), lasted(since)).trim().to_string()),
        (Level::Elevated, _) => ("Disk is busy.".into(), format!("The disk is active {disk:.0}% of the time. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, _) => ("Disk is saturated.".into(), format!("The disk is active {disk:.0}% of the time, so anything waiting for it feels slow.{}", lasted(since))),
    };
    let (busiest, busiest_cpu) = match busiest {
        Some((n, c)) => (Some(n), c),
        None => (None, 0.0),
    };
    Health { level, headline, detail, busiest, busiest_cpu }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, cpu: f32, desc: Option<&str>) -> ProcessRow {
        ProcessRow { name: name.into(), cpu_pct: cpu, description: desc.map(String::from), ..Default::default() }
    }

    fn snap(mem: f32, cpu: f32, disk: f32) -> Snapshot {
        let mut s = Snapshot::default();
        s.memory.used_pct = mem;
        s.cpu.total_pct = cpu;
        s.disk.active_pct = disk;
        s
    }

    #[test]
    fn thresholds_pick_the_level() {
        assert_eq!(level_of(74.9, MEMORY_LEVELS), Level::Good);
        assert_eq!(level_of(75.0, MEMORY_LEVELS), Level::Elevated);
        assert_eq!(level_of(90.0, MEMORY_LEVELS), Level::Critical);
        assert_eq!(level_of(80.0, CPU_LEVELS), Level::Elevated);
        assert_eq!(level_of(95.0, CPU_LEVELS), Level::Critical);
        assert_eq!(level_of(60.0, DISK_LEVELS), Level::Elevated);
        assert_eq!(level_of(90.0, DISK_LEVELS), Level::Critical);
    }

    #[test]
    fn busiest_app_sums_a_group() {
        let rows = [row("chrome.exe", 4.0, Some("Google Chrome")), row("Chrome.exe", 9.0, None), row("code.exe", 10.0, None), row("idle.exe", 0.0, None)];
        assert_eq!(busiest_app(&rows), Some(("Google Chrome".to_string(), 13.0)));
        assert_eq!(busiest_app(&[row("a.exe", 0.0, None)]), None);
    }

    #[test]
    fn verdict_names_the_worst_resource() {
        let rows = [row("chrome.exe", 18.0, Some("Chrome"))];
        let good = health(&snap(40.0, 20.0, 5.0), &rows, None);
        assert_eq!((good.level, good.headline.as_str()), (Level::Good, "Everything looks good."));
        assert!(good.detail.starts_with("Chrome is the busiest app right now (18% CPU)."), "{}", good.detail);
        assert_eq!((good.busiest.as_deref(), good.busiest_cpu), (Some("Chrome"), 18.0));
        let slow = health(&snap(93.0, 97.0, 10.0), &rows, None);
        assert_eq!((slow.level, slow.headline.as_str()), (Level::Critical, "Memory is almost full."));
        let cpu = health(&snap(50.0, 97.0, 10.0), &rows, None);
        assert_eq!(cpu.headline, "CPU is under heavy load.");
        assert_eq!(health(&snap(80.0, 10.0, 10.0), &[], None).level, Level::Elevated);
        assert_eq!(health(&snap(10.0, 10.0, 65.0), &[], None).headline, "Disk is busy.");
        assert_eq!(health(&snap(10.0, 10.0, 10.0), &[], None).detail, "No resource is under pressure.");
    }
}
