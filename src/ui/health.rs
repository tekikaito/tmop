//! A one-glance verdict on how the machine is doing, built only from the numbers in the
//! snapshot and the process list (Summary hero, popout health line), and the process list
//! grouped into apps (Summary top consumers, Performance top CPU consumers).

use crate::metrics::Snapshot;
use crate::process::ProcessRow;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Memory use (percent) from which the machine counts as elevated / critical.
pub const MEMORY_LEVELS: (f32, f32) = (75.0, 90.0);
/// CPU use (percent).
pub const CPU_LEVELS: (f32, f32) = (80.0, 95.0);
/// Disk active time (percent).
pub const DISK_LEVELS: (f32, f32) = (60.0, 90.0);
/// GPU use (percent). Only marks the resource; the verdict ignores it (a game at 99% is fine).
pub const GPU_LEVELS: (f32, f32) = CPU_LEVELS;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    Good,
    Elevated,
    Critical,
}

/// The resource that decided a verdict that is not [`Level::Good`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottleneck {
    Memory,
    Cpu,
    Disk,
}

/// The verdict: a level, a headline and a sentence of detail, and the busiest app.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub level: Level,
    /// "Everything looks good.", "Memory is almost full."
    pub headline: String,
    /// The part of the headline to highlight ("good", "almost full"); a substring of it.
    pub emphasis: &'static str,
    pub detail: String,
    /// What the verdict is about; `None` when everything is fine.
    pub bottleneck: Option<Bottleneck>,
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

/// How long CPU or disk load must stay at its Critical level before the verdict calls it Critical;
/// a shorter spike (an app launch, a scan burst) stays Elevated. Memory is Critical at once.
pub const CRITICAL_AFTER: Duration = Duration::from_secs(30);

/// How long CPU and disk have been at their Critical level without a break (zero when they are not).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lasting {
    pub cpu: Duration,
    pub disk: Duration,
}

/// Follows [`Lasting`] from one sample to the next.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    cpu: Option<Instant>,
    disk: Option<Instant>,
}

impl Tracker {
    pub fn update(&mut self, snap: &Snapshot, now: Instant) -> Lasting {
        fn track(since: &mut Option<Instant>, pct: f32, levels: (f32, f32), now: Instant) -> Duration {
            if level_of(pct, levels) == Level::Critical {
                now.saturating_duration_since(*since.get_or_insert(now))
            } else {
                *since = None;
                Duration::ZERO
            }
        }
        Lasting { cpu: track(&mut self.cpu, snap.cpu.total_pct, CPU_LEVELS, now), disk: track(&mut self.disk, snap.disk.active_pct, DISK_LEVELS, now) }
    }
}

/// `level` for a CPU or disk load that has been at its Critical level for `held`: Critical only
/// from [`CRITICAL_AFTER`] on, Elevated before.
pub fn settled(level: Level, held: Duration) -> Level {
    if level == Level::Critical && held < CRITICAL_AFTER {
        Level::Elevated
    } else {
        level
    }
}

/// One app: the processes sharing an image name, summed.
#[derive(Clone, Debug, PartialEq)]
pub struct AppGroup {
    /// Lowercase image name, the group's identity.
    pub key: String,
    /// Description of the image when it has one, else its name.
    pub name: String,
    pub count: usize,
    pub cpu: f32,
    /// Private working set, bytes.
    pub memory: u64,
    /// Read plus write rate, bytes/s.
    pub io_bps: f64,
    pub gpu: f32,
    /// Some member owns a window.
    pub has_window: bool,
    /// The image lives under %SystemRoot%.
    pub system: bool,
    /// Index (into the rows the groups were built from) of the member using the most CPU.
    pub top: usize,
    /// Index of the member to end to close the whole app: the heaviest one that was not started
    /// by another member.
    pub root: usize,
    /// Index of the first member with an image path (for the icon).
    pub icon: Option<usize>,
}

/// Groups `rows` by lowercase image name. The idle process is left out, and so is TMOP itself
/// unless `show_self`.
pub fn app_groups(rows: &[ProcessRow], show_self: bool) -> Vec<AppGroup> {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase();
    let me = std::process::id();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<AppGroup> = Vec::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    let mut described: Vec<bool> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.pid == 0 || (!show_self && r.pid == me) {
            continue;
        }
        let key = r.name.to_lowercase();
        let g = *index.entry(key.clone()).or_insert_with(|| {
            groups.push(AppGroup {
                key,
                name: r.name.clone(),
                count: 0,
                cpu: 0.0,
                memory: 0,
                io_bps: 0.0,
                gpu: 0.0,
                has_window: false,
                system: r.path.as_deref().is_some_and(|p| p.to_lowercase().starts_with(&system_root)),
                top: i,
                root: i,
                icon: None,
            });
            members.push(Vec::new());
            described.push(false);
            groups.len() - 1
        });
        let e = &mut groups[g];
        if let Some(d) = r.description.as_deref().filter(|d| !d.is_empty()) {
            if !described[g] {
                e.name = d.to_string();
                described[g] = true;
            }
        }
        e.count += 1;
        e.cpu += r.cpu_pct;
        e.memory += r.private_working_set;
        e.io_bps += r.io_bps;
        e.gpu += r.gpu_pct;
        e.has_window |= r.has_window;
        if e.icon.is_none() && r.path.is_some() {
            e.icon = Some(i);
        }
        let t = &rows[e.top];
        if r.cpu_pct > t.cpu_pct || (r.cpu_pct == t.cpu_pct && r.private_working_set > t.private_working_set) {
            e.top = i;
        }
        members[g].push(i);
    }
    for (g, m) in groups.iter_mut().zip(&members) {
        let pids: HashSet<u32> = m.iter().map(|i| rows[*i].pid).collect();
        g.root = m.iter().copied().filter(|i| !pids.contains(&rows[*i].parent_pid) || rows[*i].parent_pid == rows[*i].pid).max_by_key(|i| rows[*i].private_working_set).unwrap_or(g.top);
    }
    groups
}

/// The app group with the most CPU: processes are grouped by lowercase image name, a group is
/// named after its description (else its image name).
pub fn busiest_app(rows: &[ProcessRow]) -> Option<(String, f32)> {
    app_groups(rows, true).into_iter().filter(|g| g.cpu > 0.0).max_by(|a, b| a.cpu.total_cmp(&b.cpu).then_with(|| b.name.cmp(&a.name))).map(|g| (g.name, g.cpu))
}

fn lasted(since: Option<Instant>) -> String {
    match since.map(|t| t.elapsed()) {
        Some(d) if d >= Duration::from_secs(60) => format!(" This has lasted {} min.", d.as_secs() / 60),
        Some(d) if d >= Duration::from_secs(10) => format!(" This has lasted {} s.", d.as_secs()),
        _ => String::new(),
    }
}

/// How the machine is doing. `since` is when the current Critical level began, if the caller
/// tracks it; `lasting` says how long CPU and disk have been at their Critical level, which they
/// need to be for [`CRITICAL_AFTER`] before the verdict is Critical.
pub fn health(snap: &Snapshot, rows: &[ProcessRow], since: Option<Instant>, lasting: Lasting) -> Health {
    let (mem, cpu, disk) = (snap.memory.used_pct, snap.cpu.total_pct, snap.disk.active_pct);
    let levels = [(level_of(mem, MEMORY_LEVELS), 0), (settled(level_of(cpu, CPU_LEVELS), lasting.cpu), 1), (settled(level_of(disk, DISK_LEVELS), lasting.disk), 2)];
    // The worst resource decides; on a tie memory beats CPU beats disk.
    let (level, which) = levels.iter().rev().fold((Level::Good, 0), |best, &(l, i)| if l >= best.0 && l != Level::Good { (l, i) } else { best });
    let busiest = busiest_app(rows);
    let busy = busiest.as_ref().map(|(n, c)| format!("{n} is the busiest app right now ({c:.0}% CPU)."));
    let (headline, emphasis, detail) = match (level, which) {
        (Level::Good, _) => ("Everything looks good.".to_string(), "good", format!("{} No resource is under pressure.", busy.unwrap_or_default()).trim().to_string()),
        (Level::Elevated, 0) => ("Memory is getting full.".into(), "getting full", format!("{mem:.0}% of memory is in use. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, 0) => (
            "Memory is almost full.".into(),
            "almost full",
            format!("{mem:.0}% of memory is in use, so Windows moves data to disk to free some, which makes apps feel slow.{}", lasted(since)),
        ),
        (Level::Elevated, 1) => ("CPU is busy.".into(), "busy", format!("The processor is at {cpu:.0}%. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, 1) => ("CPU is under heavy load.".into(), "under heavy load", format!("The processor is at {cpu:.0}%. {}{}", busy.unwrap_or_default(), lasted(since)).trim().to_string()),
        (Level::Elevated, _) => ("Disk is busy.".into(), "busy", format!("The disk is active {disk:.0}% of the time. {}", busy.unwrap_or_default()).trim().to_string()),
        (Level::Critical, _) => ("Disk is saturated.".into(), "saturated", format!("The disk is active {disk:.0}% of the time, so anything waiting for it feels slow.{}", lasted(since))),
    };
    let bottleneck = match (level, which) {
        (Level::Good, _) => None,
        (_, 0) => Some(Bottleneck::Memory),
        (_, 1) => Some(Bottleneck::Cpu),
        _ => Some(Bottleneck::Disk),
    };
    let (busiest, busiest_cpu) = match busiest {
        Some((n, c)) => (Some(n), c),
        None => (None, 0.0),
    };
    Health { level, headline, emphasis, detail, bottleneck, busiest, busiest_cpu }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, name: &str, cpu: f32, desc: Option<&str>) -> ProcessRow {
        ProcessRow { pid, name: name.into(), cpu_pct: cpu, description: desc.map(String::from), ..Default::default() }
    }

    /// CPU and disk have been critical for as long as it takes to count.
    const LONG: Lasting = Lasting { cpu: CRITICAL_AFTER, disk: CRITICAL_AFTER };

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
        let rows = [row(1, "chrome.exe", 4.0, Some("Google Chrome")), row(2, "Chrome.exe", 9.0, None), row(3, "code.exe", 10.0, None), row(4, "idle.exe", 0.0, None)];
        assert_eq!(busiest_app(&rows), Some(("Google Chrome".to_string(), 13.0)));
        assert_eq!(busiest_app(&[row(1, "a.exe", 0.0, None)]), None);
    }

    #[test]
    fn groups_sum_members_and_name_them() {
        let mut a = row(10, "app.exe", 3.0, None);
        a.private_working_set = 100;
        a.has_window = true;
        let mut b = row(11, "APP.exe", 5.0, Some("The App"));
        b.private_working_set = 300;
        b.parent_pid = 10;
        let mut c = row(12, "other.exe", 1.0, None);
        c.private_working_set = 50;
        let idle = row(0, "System Idle Process", 90.0, None);
        let rows = [a, b, c, idle];
        let groups = app_groups(&rows, true);
        assert_eq!(groups.len(), 2, "the idle process is not an app");
        let g = groups.iter().find(|g| g.key == "app.exe").expect("group");
        assert_eq!((g.name.as_str(), g.count, g.cpu, g.memory, g.has_window), ("The App", 2, 8.0, 400, true));
        // The heaviest member is the child, but the one to end is its parent (the root).
        assert_eq!((rows[g.top].pid, rows[g.root].pid), (11, 10));
    }

    #[test]
    fn verdict_names_the_worst_resource() {
        let rows = [row(1, "chrome.exe", 18.0, Some("Chrome"))];
        let good = health(&snap(40.0, 20.0, 5.0), &rows, None, Lasting::default());
        assert_eq!((good.level, good.headline.as_str(), good.emphasis, good.bottleneck), (Level::Good, "Everything looks good.", "good", None));
        assert!(good.detail.starts_with("Chrome is the busiest app right now (18% CPU)."), "{}", good.detail);
        assert_eq!((good.busiest.as_deref(), good.busiest_cpu), (Some("Chrome"), 18.0));
        let slow = health(&snap(93.0, 97.0, 10.0), &rows, None, LONG);
        assert_eq!((slow.level, slow.headline.as_str(), slow.bottleneck), (Level::Critical, "Memory is almost full.", Some(Bottleneck::Memory)));
        let cpu = health(&snap(50.0, 97.0, 10.0), &rows, None, LONG);
        assert_eq!((cpu.headline.as_str(), cpu.bottleneck), ("CPU is under heavy load.", Some(Bottleneck::Cpu)));
        assert_eq!(health(&snap(80.0, 10.0, 10.0), &[], None, LONG).level, Level::Elevated);
        assert_eq!(health(&snap(10.0, 10.0, 65.0), &[], None, LONG).headline, "Disk is busy.");
        assert_eq!(health(&snap(10.0, 10.0, 10.0), &[], None, LONG).detail, "No resource is under pressure.");
    }

    #[test]
    fn a_short_spike_is_not_critical() {
        let spike = snap(40.0, 99.0, 95.0);
        let fresh = health(&spike, &[], None, Lasting::default());
        assert_eq!((fresh.level, fresh.headline.as_str(), fresh.bottleneck), (Level::Elevated, "CPU is busy.", Some(Bottleneck::Cpu)));
        let old = health(&spike, &[], None, Lasting { cpu: CRITICAL_AFTER, disk: Duration::ZERO });
        assert_eq!((old.level, old.headline.as_str()), (Level::Critical, "CPU is under heavy load."));
        let disk = health(&snap(40.0, 10.0, 95.0), &[], None, Lasting { cpu: Duration::ZERO, disk: CRITICAL_AFTER });
        assert_eq!((disk.level, disk.headline.as_str()), (Level::Critical, "Disk is saturated."));
        // Memory does not wait.
        assert_eq!(health(&snap(95.0, 10.0, 10.0), &[], None, Lasting::default()).level, Level::Critical);
    }

    #[test]
    fn the_tracker_counts_an_unbroken_critical_stretch() {
        let t0 = Instant::now();
        let secs = Duration::from_secs;
        let mut t = Tracker::default();
        assert_eq!(t.update(&snap(10.0, 99.0, 10.0), t0).cpu, Duration::ZERO);
        assert_eq!(t.update(&snap(10.0, 99.0, 10.0), t0 + secs(31)).cpu, secs(31));
        assert_eq!(t.update(&snap(10.0, 50.0, 10.0), t0 + secs(32)), Lasting::default());
        assert_eq!(t.update(&snap(10.0, 99.0, 10.0), t0 + secs(33)).cpu, Duration::ZERO, "a break restarts the count");
    }

    #[test]
    fn the_emphasis_is_part_of_the_headline() {
        for s in [snap(10.0, 10.0, 10.0), snap(80.0, 10.0, 10.0), snap(95.0, 10.0, 10.0), snap(10.0, 85.0, 10.0), snap(10.0, 99.0, 10.0), snap(10.0, 10.0, 65.0), snap(10.0, 10.0, 95.0)] {
            let h = health(&s, &[], None, LONG);
            assert!(h.headline.contains(h.emphasis), "{} / {}", h.headline, h.emphasis);
        }
    }
}
