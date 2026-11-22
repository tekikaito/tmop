//! DRM client usage stats: the `drm-*` lines a DRM driver prints in `/proc/<pid>/fdinfo/<fd>` for
//! an open GPU device, and the busy shares between two readings. Pure functions, so they are
//! tested on every platform; the Linux reader is [`super::intel_gpu`].
//!
//! The format is the kernel's "DRM client usage stats" (`Documentation/gpu/drm-usage-stats.rst`):
//! * `drm-driver` names the driver; `drm-client-id` identifies one open `struct drm_file`, which
//!   several descriptors and processes may share, unique per device (`drm-pdev`), so a client is
//!   counted once per (device, client id).
//! * `drm-engine-<e>: <n> ns` is the time the client kept engine class `e` busy (i915, since Linux
//!   5.19). `drm-cycles-<e>` with `drm-total-cycles-<e>` are busy cycles and a GPU timestamp in the
//!   same unit (xe, since 6.11): the share is computed in the GPU's clock, not the wall clock.
//! * `drm-engine-capacity-<e>` is how many identical engines the class has (1 when absent); the
//!   busy figure is their sum, so the share is divided by it.
//! * Counters need not be monotonic: a lower value than before means the previous one stands
//!   until the counter catches up.
//! * `drm-total-<r>`, `drm-shared-<r>`, `drm-resident-<r>` are buffer sizes per memory region, in
//!   bytes or with a `KiB` / `MiB` suffix.

use std::collections::{BTreeMap, HashMap};

/// One engine class of one client. A reading has either the time (`ns`) or the cycle pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    /// `drm-engine-<e>`: busy nanoseconds.
    pub ns: Option<u64>,
    /// `drm-cycles-<e>`: busy cycles.
    pub cycles: Option<u64>,
    /// `drm-total-cycles-<e>`: the GPU timestamp the cycles are counted against.
    pub total_cycles: Option<u64>,
    /// `drm-engine-capacity-<e>`; 1 when absent.
    pub capacity: u32,
}

/// Buffer sizes of one memory region of one client, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub total: Option<u64>,
    pub shared: Option<u64>,
    pub resident: Option<u64>,
}

/// One DRM client as one fdinfo file shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Client {
    pub driver: String,
    pub pdev: Option<String>,
    pub id: u64,
    /// By engine class key (`render`, `rcs`, ...).
    pub engines: BTreeMap<String, Engine>,
    /// By region key (`system0`, `vram0`, ...).
    pub regions: BTreeMap<String, Region>,
}

/// A number with an optional unit: `ns` (or none) for times, `KiB` / `MiB` (or none) for sizes.
fn value(text: &str, units: &[(&str, u64)]) -> Option<u64> {
    let mut it = text.split_ascii_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    match it.next() {
        None => Some(n),
        Some(u) => units.iter().find(|(name, _)| *name == u).and_then(|(_, f)| n.checked_mul(*f)),
    }
}

const TIME: &[(&str, u64)] = &[("ns", 1)];
const SIZE: &[(&str, u64)] = &[("KiB", 1024), ("MiB", 1024 * 1024)];

/// The client an fdinfo text describes; `None` when it is not a DRM file, or lacks the client id
/// (without it nothing can be told apart, so nothing is counted).
pub fn parse(text: &str) -> Option<Client> {
    let mut c = Client::default();
    let mut id = None;
    for line in text.lines() {
        let Some((key, v)) = line.split_once(':') else { continue };
        let (key, v) = (key.trim(), v.trim());
        let Some(key) = key.strip_prefix("drm-") else { continue };
        match key {
            "driver" => c.driver = v.to_string(),
            "pdev" => c.pdev = Some(v.to_string()),
            "client-id" => id = v.parse::<u64>().ok(),
            _ => {
                if let Some(e) = key.strip_prefix("engine-capacity-") {
                    if let Some(n) = v.parse::<u32>().ok().filter(|n| *n > 0) {
                        c.engines.entry(e.to_string()).or_default().capacity = n;
                    }
                } else if let Some(e) = key.strip_prefix("engine-") {
                    c.engines.entry(e.to_string()).or_default().ns = value(v, TIME);
                } else if let Some(e) = key.strip_prefix("total-cycles-") {
                    c.engines.entry(e.to_string()).or_default().total_cycles = v.parse().ok();
                } else if let Some(e) = key.strip_prefix("cycles-") {
                    c.engines.entry(e.to_string()).or_default().cycles = v.parse().ok();
                } else {
                    // Memory: drm-total-<r>, drm-shared-<r>, drm-resident-<r>.
                    for (prefix, field) in [("total-", 0), ("shared-", 1), ("resident-", 2)] {
                        if let Some(r) = key.strip_prefix(prefix) {
                            let reg = c.regions.entry(r.to_string()).or_default();
                            let slot = match field {
                                0 => &mut reg.total,
                                1 => &mut reg.shared,
                                _ => &mut reg.resident,
                            };
                            *slot = value(v, SIZE);
                        }
                    }
                }
            }
        }
    }
    for e in c.engines.values_mut() {
        e.capacity = e.capacity.max(1);
    }
    c.id = id?;
    (!c.driver.is_empty()).then_some(c)
}

/// Where a client sits: the device it is open on (the provider's id for it) and its client id.
pub type ClientKey = (String, u64);

/// Counts each client once, however many descriptors and processes hold it: the first reading of
/// a (device, client id) is kept, with the PIDs that hold it (sorted, no repeats).
pub fn dedupe(readings: impl IntoIterator<Item = (u32, String, Client)>) -> HashMap<ClientKey, (Vec<u32>, Client)> {
    let mut out: HashMap<ClientKey, (Vec<u32>, Client)> = HashMap::new();
    for (pid, dev, c) in readings {
        let e = out.entry((dev, c.id)).or_insert_with(|| (Vec::new(), c));
        if let Err(at) = e.0.binary_search(&pid) {
            e.0.insert(at, pid);
        }
    }
    out
}

/// `cur` with every busy counter raised to at least `prev`'s: the spec asks a reader to stay with
/// the previous larger value until the driver catches up. A GPU timestamp that went back (a reset)
/// is not held: [`client_busy`] then starts the client afresh instead of stalling.
pub fn hold_max(prev: &Client, mut cur: Client) -> Client {
    for (name, e) in &mut cur.engines {
        let Some(p) = prev.engines.get(name) else { continue };
        let max = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        e.ns = max(e.ns, p.ns);
        if e.total_cycles >= p.total_cycles {
            e.cycles = max(e.cycles, p.cycles);
        }
    }
    cur
}

/// Busy share (%) of each engine class of one client between `prev` and `cur` (the latter
/// already through [`hold_max`]), `wall_ns` apart. Cycles are set against the GPU timestamp,
/// nanoseconds against the wall clock; both are divided by the class's capacity. An engine
/// without two comparable readings gets no entry. Not clamped: the device sum is.
pub fn client_busy(prev: &Client, cur: &Client, wall_ns: u64) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for (name, e) in &cur.engines {
        let Some(p) = prev.engines.get(name) else { continue };
        let cap = f64::from(e.capacity.max(1));
        let share = match (e.cycles, e.total_cycles, p.cycles, p.total_cycles) {
            (Some(c), Some(t), Some(pc), Some(pt)) => match t.checked_sub(pt) {
                Some(dt) if dt > 0 => Some(c.saturating_sub(pc) as f64 / (dt as f64 * cap)),
                _ => None,
            },
            _ => match (e.ns, p.ns) {
                (Some(n), Some(pn)) if wall_ns > 0 => Some(n.saturating_sub(pn) as f64 / (wall_ns as f64 * cap)),
                _ => None,
            },
        };
        if let Some(s) = share {
            out.push((name.clone(), s * 100.0));
        }
    }
    out
}

/// One device between two readings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceBusy {
    /// (engine class key, %) for every class a current client reports, by key; summed over the
    /// clients and clamped to 0..=100.
    pub engines: Vec<(String, f32)>,
    /// Each client's busiest class (%), by client id; only clients read both times.
    pub clients: HashMap<u64, f32>,
    /// What to compare the next reading with.
    pub state: HashMap<u64, Client>,
}

/// The busy shares of one device from its clients' last readings (`prev`, by client id) and the
/// current ones, `wall_ns` apart. A client that is new has nothing to compare with and counts 0
/// this time (no spike from its whole lifetime); one that is gone simply drops out (no negative
/// delta). The sum over clients of a class is clamped to 100 %.
pub fn device_busy(prev: &HashMap<u64, Client>, cur: impl IntoIterator<Item = Client>, wall_ns: u64) -> DeviceBusy {
    let mut sums: BTreeMap<String, f64> = BTreeMap::new();
    let mut out = DeviceBusy::default();
    for c in cur {
        for name in c.engines.keys() {
            sums.entry(name.clone()).or_insert(0.0);
        }
        let c = match prev.get(&c.id) {
            Some(p) => {
                let c = hold_max(p, c);
                let shares = client_busy(p, &c, wall_ns);
                for (name, s) in &shares {
                    *sums.entry(name.clone()).or_insert(0.0) += s;
                }
                let top = shares.iter().map(|s| s.1).fold(0.0, f64::max);
                out.clients.insert(c.id, top.clamp(0.0, 100.0) as f32);
                c
            }
            None => c,
        };
        out.state.insert(c.id, c);
    }
    out.engines = sums.into_iter().map(|(n, s)| (n, s.clamp(0.0, 100.0) as f32)).collect();
    out
}

/// Memory in use per region, over all clients of one device: what each holds on its own
/// (resident minus shared) plus the largest shared set once, as shared buffers appear in every
/// client that holds them. A lower bound; regions no client reports as resident are left out.
pub fn memory<'a>(clients: impl IntoIterator<Item = &'a Client>) -> BTreeMap<String, u64> {
    let mut own: BTreeMap<String, u64> = BTreeMap::new();
    let mut shared: BTreeMap<String, u64> = BTreeMap::new();
    for c in clients {
        for (name, r) in &c.regions {
            let Some(res) = r.resident else { continue };
            let sh = r.shared.unwrap_or(0).min(res);
            *own.entry(name.clone()).or_insert(0) += res - sh;
            let s = shared.entry(name.clone()).or_insert(0);
            *s = (*s).max(sh);
        }
    }
    for (name, s) in shared {
        *own.entry(name).or_insert(0) += s;
    }
    own
}

/// The display name of an Intel engine class key (i915: `render`, `copy`, `video`,
/// `video-enhance`; xe: `rcs`, `bcs`, `vcs`, `vecs`, `ccs`, `other`).
pub fn intel_engine_name(key: &str) -> String {
    match key {
        "render" | "rcs" => "Render".into(),
        "copy" | "bcs" => "Copy".into(),
        "video" | "vcs" => "Video".into(),
        "video-enhance" | "vecs" => "Video enhance".into(),
        "ccs" | "compute" => "Compute".into(),
        "other" => "Other".into(),
        k => k.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The i915 example of `Documentation/gpu/i915.rst` ("i915 DRM client usage stats
    /// implementation"), as printed there.
    pub const I915_DOC: &str = "pos:    0
flags:  0100002
mnt_id: 21
drm-driver: i915
drm-pdev:   0000:00:02.0
drm-client-id:      7
drm-engine-render:  9288864723 ns
drm-engine-copy:    2035071108 ns
drm-engine-video:   0 ns
drm-engine-capacity-video:   2
drm-engine-video-enhance:   0 ns
";

    /// The xe example of `drivers/gpu/drm/xe/xe_drm_client.c` ("DRM Client usage stats",
    /// rendered as `Documentation/gpu/xe/xe-drm-usage-stats.rst`), as printed there.
    pub const XE_DOC: &str = "pos:    0
flags:  0100002
mnt_id: 26
ino:    685
drm-driver:     xe
drm-client-id:  3
drm-pdev:       0000:03:00.0
drm-total-system:       0
drm-shared-system:      0
drm-active-system:      0
drm-resident-system:    0
drm-purgeable-system:   0
drm-total-gtt:  192 KiB
drm-shared-gtt: 0
drm-active-gtt: 0
drm-resident-gtt:       192 KiB
drm-total-vram0:        23992 KiB
drm-shared-vram0:       16 MiB
drm-active-vram0:       0
drm-resident-vram0:     23992 KiB
drm-total-stolen:       0
drm-shared-stolen:      0
drm-active-stolen:      0
drm-resident-stolen:    0
drm-cycles-rcs: 28257900
drm-total-cycles-rcs:   7655183225
drm-cycles-bcs: 0
drm-total-cycles-bcs:   7655183225
drm-cycles-vcs: 0
drm-total-cycles-vcs:   7655183225
drm-engine-capacity-vcs:        2
drm-cycles-vecs:        0
drm-total-cycles-vecs:  7655183225
drm-engine-capacity-vecs:       2
drm-cycles-ccs: 0
drm-total-cycles-ccs:   7655183225
drm-engine-capacity-ccs:        4
";

    /// `text` with every number of `key` (exact key, before the colon) replaced by `n`.
    pub fn with(text: &str, key: &str, n: u64) -> String {
        text.lines()
            .map(|l| match l.split_once(':') {
                Some((k, v)) if k.trim() == key => {
                    let unit = v.split_ascii_whitespace().nth(1).map(|u| format!(" {u}")).unwrap_or_default();
                    format!("{k}: {n}{unit}")
                }
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn eng(ns: Option<u64>, cycles: Option<u64>, total: Option<u64>, capacity: u32) -> Engine {
        Engine { ns, cycles, total_cycles: total, capacity }
    }

    #[test]
    fn i915_doc_example_parses() {
        let c = parse(I915_DOC).unwrap();
        assert_eq!((c.driver.as_str(), c.pdev.as_deref(), c.id), ("i915", Some("0000:00:02.0"), 7));
        assert_eq!(c.engines.keys().map(String::as_str).collect::<Vec<_>>(), ["copy", "render", "video", "video-enhance"]);
        assert_eq!(c.engines["render"], eng(Some(9288864723), None, None, 1));
        assert_eq!(c.engines["copy"], eng(Some(2035071108), None, None, 1));
        assert_eq!(c.engines["video"], eng(Some(0), None, None, 2));
        assert!(c.regions.is_empty());
    }

    #[test]
    fn xe_doc_example_parses() {
        let c = parse(XE_DOC).unwrap();
        assert_eq!((c.driver.as_str(), c.pdev.as_deref(), c.id), ("xe", Some("0000:03:00.0"), 3));
        assert_eq!(c.engines["rcs"], eng(None, Some(28257900), Some(7655183225), 1));
        assert_eq!(c.engines["vcs"], eng(None, Some(0), Some(7655183225), 2));
        assert_eq!(c.engines["ccs"].capacity, 4);
        assert_eq!(c.engines.len(), 5);
        assert_eq!(c.regions["gtt"], Region { total: Some(192 * 1024), shared: Some(0), resident: Some(192 * 1024) });
        assert_eq!(c.regions["vram0"], Region { total: Some(23992 * 1024), shared: Some(16 << 20), resident: Some(23992 * 1024) });
        // `drm-active-*` and `drm-purgeable-*` are not memory in use beyond resident.
        assert_eq!(c.regions.len(), 4);
    }

    #[test]
    fn non_drm_and_unidentified_files_are_skipped() {
        assert_eq!(parse("pos:\t0\nflags:\t02\nmnt_id:\t15\nino:\t1234\n"), None);
        assert_eq!(parse("drm-driver: i915\ndrm-engine-render: 5 ns\n"), None, "no client id: cannot be deduplicated");
        assert_eq!(parse("drm-client-id: 4\ndrm-engine-render: 5 ns\n"), None, "no driver");
        // A unit outside the spec is not guessed at; zero capacity is not allowed.
        let c = parse("drm-driver: i915\ndrm-client-id: 1\ndrm-engine-render: 5 us\ndrm-engine-capacity-render: 0\n").unwrap();
        assert_eq!(c.engines["render"], eng(None, None, None, 1));
    }

    #[test]
    fn i915_share_is_busy_time_over_wall_time() {
        let a = parse(I915_DOC).unwrap();
        // 0.5 s later: render 250 ms busier, copy unchanged, video 500 ms over its 2 engines.
        let b = parse(&with(&with(I915_DOC, "drm-engine-render", 9288864723 + 250_000_000), "drm-engine-video", 500_000_000)).unwrap();
        let mut s = client_busy(&a, &b, 500_000_000);
        s.sort_by(|x, y| x.0.cmp(&y.0));
        let pct: Vec<(&str, i64)> = s.iter().map(|(n, v)| (n.as_str(), v.round() as i64)).collect();
        assert_eq!(pct, [("copy", 0), ("render", 50), ("video", 50), ("video-enhance", 0)]);
        assert!(client_busy(&a, &b, 0).is_empty(), "no wall time: no share");
    }

    #[test]
    fn xe_share_is_cycles_over_gpu_timestamp() {
        let a = parse(XE_DOC).unwrap();
        let t = 7655183225 + 1_000_000;
        let mut text = XE_DOC.to_string();
        for e in ["rcs", "bcs", "vcs", "vecs", "ccs"] {
            text = with(&text, &format!("drm-total-cycles-{e}"), t);
        }
        // rcs busy for a quarter of the interval; ccs (4 engines) for 3 engine-intervals.
        let text = with(&with(&text, "drm-cycles-rcs", 28257900 + 250_000), "drm-cycles-ccs", 3_000_000);
        let b = parse(&text).unwrap();
        // The wall time does not matter: the GPU's own clock is used.
        for wall in [0, 1, 10_000_000_000] {
            let s: BTreeMap<String, f64> = client_busy(&a, &b, wall).into_iter().collect();
            assert_eq!((s["rcs"].round(), s["ccs"].round(), s["vcs"]), (25.0, 75.0, 0.0), "wall {wall}");
        }
    }

    #[test]
    fn capacity_keeps_a_class_at_or_below_100() {
        // Two clients each keep both video engines fully busy for the whole interval: 2 × 100 %
        // of a capacity-2 class is reported as 100 %, not 200 % or 400 %.
        let one = |id: u64, ns: u64| parse(&format!("drm-driver: i915\ndrm-client-id: {id}\ndrm-engine-video: {ns} ns\ndrm-engine-capacity-video: 2\n")).unwrap();
        let prev: HashMap<u64, Client> = [(1, one(1, 0)), (2, one(2, 0))].into();
        let d = device_busy(&prev, [one(1, 2_000_000_000), one(2, 2_000_000_000)], 1_000_000_000);
        assert_eq!(d.engines, vec![("video".to_string(), 100.0)]);
        assert_eq!((d.clients[&1], d.clients[&2]), (100.0, 100.0));
        // One client, one of the two engines busy: 50 %.
        let d = device_busy(&prev, [one(1, 1_000_000_000)], 1_000_000_000);
        assert_eq!(d.engines, vec![("video".to_string(), 50.0)]);
    }

    #[test]
    fn new_and_vanished_clients_give_no_spike_and_no_negative() {
        let one = |id: u64, ns: u64| parse(&format!("drm-driver: i915\ndrm-client-id: {id}\ndrm-engine-render: {ns} ns\n")).unwrap();
        let prev: HashMap<u64, Client> = [(1, one(1, 100)), (2, one(2, 900_000_000_000))].into();
        // Client 2 (huge lifetime total) is gone; client 3 is new with a huge total.
        let d = device_busy(&prev, [one(1, 100 + 100_000_000), one(3, 800_000_000_000)], 1_000_000_000);
        assert_eq!(d.engines, vec![("render".to_string(), 10.0)]);
        assert_eq!(d.clients.keys().copied().collect::<Vec<_>>(), [1], "a new client has no share yet");
        assert_eq!(d.state.keys().copied().collect::<std::collections::BTreeSet<_>>(), [1, 3].into());
        // Next time the new client counts from its first reading.
        let d2 = device_busy(&d.state, [one(1, 100 + 100_000_000), one(3, 800_000_000_000 + 300_000_000)], 1_000_000_000);
        assert_eq!(d2.engines, vec![("render".to_string(), 30.0)]);
        // No clients at all: no engines, nothing negative.
        assert_eq!(device_busy(&d2.state, [], 1_000_000_000).engines, vec![]);
    }

    #[test]
    fn a_counter_that_goes_back_holds_the_larger_value() {
        let one = |ns: u64| parse(&format!("drm-driver: i915\ndrm-client-id: 1\ndrm-engine-render: {ns} ns\n")).unwrap();
        let d = device_busy(&[(1, one(1_000_000_000))].into(), [one(400_000_000)], 1_000_000_000);
        assert_eq!(d.engines, vec![("render".to_string(), 0.0)], "never negative");
        assert_eq!(d.state[&1].engines["render"].ns, Some(1_000_000_000), "the larger value stands");
        // It catches up: only what lies beyond the held value counts.
        let d = device_busy(&d.state, [one(1_200_000_000)], 1_000_000_000);
        assert_eq!(d.engines, vec![("render".to_string(), 20.0)]);
        // A GPU timestamp that went back (xe) starts that engine afresh instead of stalling.
        let xe = |c: u64, t: u64| parse(&format!("drm-driver: xe\ndrm-client-id: 1\ndrm-cycles-rcs: {c}\ndrm-total-cycles-rcs: {t}\n")).unwrap();
        let d = device_busy(&[(1, xe(5_000, 1_000_000))].into(), [xe(10, 100)], 1);
        assert!(d.clients[&1] == 0.0 && d.state[&1].engines["rcs"].total_cycles == Some(100));
        let d = device_busy(&d.state, [xe(60, 200)], 1);
        assert_eq!(d.engines, vec![("rcs".to_string(), 50.0)]);
    }

    #[test]
    fn a_client_shared_by_descriptors_and_processes_counts_once() {
        let a = parse(I915_DOC).unwrap();
        let other_dev = a.clone();
        let mut other_id = a.clone();
        other_id.id = 8;
        let all = dedupe([(40, "card0".to_string(), a.clone()), (40, "card0".into(), a.clone()), (12, "card0".into(), a.clone()), (40, "card1".into(), other_dev), (40, "card0".into(), other_id)]);
        assert_eq!(all.len(), 3);
        assert_eq!(all[&("card0".to_string(), 7)].0, [12, 40]);
        assert_eq!(all[&("card1".to_string(), 7)].0, [40]);
        // Summed over the deduplicated clients, the doubled descriptor adds nothing.
        let prev: HashMap<u64, Client> = [(7, a.clone())].into();
        let b = parse(&with(I915_DOC, "drm-engine-render", 9288864723 + 100_000_000)).unwrap();
        let readings = dedupe([(1, "card0".to_string(), b.clone()), (2, "card0".into(), b)]);
        let d = device_busy(&prev, readings.into_values().map(|r| r.1), 1_000_000_000);
        assert_eq!(d.engines.iter().find(|e| e.0 == "render").unwrap().1, 10.0);
    }

    #[test]
    fn memory_counts_shared_buffers_once() {
        let c = |id: u64, res: u64, sh: u64| parse(&format!("drm-driver: xe\ndrm-client-id: {id}\ndrm-shared-vram0: {sh} KiB\ndrm-resident-vram0: {res} KiB\ndrm-total-system: 9 MiB\n")).unwrap();
        // Two clients hold the same 16 KiB shared buffer and 100 / 50 KiB of their own.
        let m = memory(&[c(1, 116, 16), c(2, 66, 16)]);
        assert_eq!(m.get("vram0"), Some(&((100 + 50 + 16) * 1024)));
        assert_eq!(m.get("system"), None, "no resident figure: not counted");
        let doc = parse(XE_DOC).unwrap();
        // One client alone: its own plus its shared buffers, i.e. its resident size.
        assert_eq!(memory([&doc]).get("vram0"), Some(&(23992 * 1024)));
        // A shared size above the resident one is clamped, never wraps.
        assert_eq!(memory([&c(3, 10, 40)]).get("vram0"), Some(&(10 * 1024)));
        assert_eq!(memory([&parse(I915_DOC).unwrap()]), BTreeMap::new());
    }

    #[test]
    fn engine_names_read_like_the_other_gpus() {
        assert_eq!(["render", "rcs", "copy", "bcs", "video", "vcs", "video-enhance", "vecs", "ccs", "other", "x"].map(intel_engine_name), ["Render", "Render", "Copy", "Copy", "Video", "Video", "Video enhance", "Video enhance", "Compute", "Other", "x"]);
    }
}
