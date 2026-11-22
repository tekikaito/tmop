//! Intel GPUs on Linux (i915 and xe): engine busy shares summed over the DRM clients in
//! `/proc/<pid>/fdinfo`, parsed and compared by [`super::drm_fdinfo`]; name from `pci.ids`,
//! frequency from sysfs (i915 `gt_act_freq_mhz`, xe `tile0/gt0/freq0/act_freq`), memory from the
//! clients' `drm-resident-*` where the driver prints it.
//!
//! The kernel has no unprivileged system-wide busy counter for these drivers, so only the clients
//! of processes whose `/proc/<pid>/fd` TMOP may read count: without root the user's own. The
//! others are counted ([`Reading::hidden`]) and every device then reads as a lower bound.
//!
//! Cost: a GPU client is found by its descriptor linking to `/dev/dri/*`. Listing every
//! descriptor of every process each tick is what costs; so a process's descriptors are listed
//! once and then again only every [`RESCAN`], and in between only its known DRM descriptors are
//! read. Without an i915 or xe device nothing in `/proc` is read at all.
//!
//! Everything is read below a root directory (`/` in use), so tests can point it at a fake
//! `/proc` and `/sys` tree.

use super::drm_fdinfo::{self, Client};
use super::procfs::{self, GpuDev};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often all descriptors of a process are listed again; a process that opens the GPU later
/// shows up within this time. New processes are listed on first sight.
pub const RESCAN: Duration = Duration::from_secs(5);
/// How often the DRM devices are looked up again.
const DEVICES_PERIOD: Duration = Duration::from_secs(10);

const DRIVERS: [&str; 2] = ["i915", "xe"];

/// One Intel DRM device.
#[derive(Clone, Debug)]
struct Device {
    /// Its card directory name (`card0`): the GPU's id.
    id: String,
    driver: String,
    /// The card directory in `/sys/class/drm`.
    card: PathBuf,
    /// Its DRM nodes (`card0`, `renderD128`).
    nodes: Vec<String>,
    name: String,
}

/// What a process's descriptors were when last listed.
#[derive(Clone, Debug, Default)]
struct PidScan {
    at: Option<Instant>,
    /// (fd, DRM node) of its descriptors open on `/dev/dri/*`.
    fds: Vec<(String, String)>,
    /// Its descriptors may not be read (another user's process).
    denied: bool,
}

/// Last reading of one device.
#[derive(Clone, Debug)]
struct DevState {
    at: Instant,
    clients: HashMap<u64, Client>,
    reading: GpuDev,
}

/// One read of all Intel GPUs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub devs: Vec<GpuDev>,
    /// Busiest engine (%) of each process with a GPU client, summed over its clients.
    pub by_pid: HashMap<u32, f32>,
    /// Processes (not kernel threads) whose descriptors could not be read.
    pub hidden: usize,
}

pub struct IntelGpus {
    root: PathBuf,
    devices: Vec<Device>,
    devices_at: Option<Instant>,
    pids: HashMap<u32, PidScan>,
    last: HashMap<String, DevState>,
    /// Engine classes each device has reported, so one without clients still lists them (at 0).
    seen: HashMap<String, BTreeSet<String>>,
    pci_ids: Option<String>,
    /// Hidden processes of the last /proc walk, for the ticks that reuse a reading.
    hidden: usize,
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn file_name(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// `cardN` or `renderDN`.
fn is_node(name: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    name.strip_prefix("card").is_some_and(digits) || name.strip_prefix("renderD").is_some_and(digits)
}

/// A PID's share of [`RESCAN`] (below it): consecutive PIDs land far apart.
fn stagger(pid: u32) -> Duration {
    let step = (u64::from(pid).wrapping_mul(2_654_435_761) % 1000) as u32;
    RESCAN * step / 1000
}

/// A kernel thread (kthreadd, or one of its children): it has no descriptors to hide.
fn kernel_thread(pid: u32, stat: Option<&str>) -> bool {
    // "pid (comm) state ppid ...": comm may hold spaces and parentheses, so read after the last ')'.
    let ppid = stat.and_then(|s| s.rsplit_once(')')).and_then(|(_, rest)| rest.split_ascii_whitespace().nth(1)?.parse::<u32>().ok());
    pid == 2 || ppid == Some(2)
}

impl IntelGpus {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), devices: Vec::new(), devices_at: None, pids: HashMap::new(), last: HashMap::new(), seen: HashMap::new(), pci_ids: None, hidden: 0 }
    }

    fn drm_dir(&self) -> PathBuf {
        self.root.join("sys/class/drm")
    }

    fn device_name(&mut self, dev: &Path) -> String {
        let (Some(v), Some(d)) = (read(dev.join("vendor")).and_then(|v| crate::pro::inventory::parse::hex_id(&v)), read(dev.join("device")).and_then(|d| crate::pro::inventory::parse::hex_id(&d))) else {
            return "Intel GPU".into();
        };
        let root = self.root.clone();
        let ids = self.pci_ids.get_or_insert_with(|| ["usr/share/hwdata/pci.ids", "usr/share/misc/pci.ids", "usr/share/pci.ids"].iter().find_map(|p| read(root.join(p))).unwrap_or_default());
        match crate::pro::inventory::parse::pci_name(ids, v, d).1 {
            Some(name) => format!("Intel {name}"),
            None => format!("Intel GPU {v:04x}:{d:04x}"),
        }
    }

    /// The i915 and xe cards of `/sys/class/drm`, each with the nodes on the same device.
    fn find_devices(&mut self) -> Vec<Device> {
        let Ok(rd) = fs::read_dir(self.drm_dir()) else { return Vec::new() };
        let mut nodes: Vec<(String, PathBuf)> = rd.flatten().filter_map(|e| Some((e.file_name().to_str().filter(|n| is_node(n))?.to_string(), e.path()))).collect();
        nodes.sort();
        let dev_of = |p: &Path| fs::canonicalize(p.join("device")).ok();
        let mut out = Vec::new();
        for (name, card) in nodes.iter().filter(|n| n.0.starts_with("card")) {
            let Some(driver) = fs::read_link(card.join("device/driver")).ok().and_then(|p| file_name(&p)).filter(|d| DRIVERS.contains(&d.as_str())) else { continue };
            let Some(dev) = dev_of(card) else { continue };
            let on_dev: Vec<String> = nodes.iter().filter(|(_, p)| dev_of(p).as_ref() == Some(&dev)).map(|n| n.0.clone()).collect();
            let name_text = self.device_name(&card.join("device"));
            out.push(Device { id: name.clone(), driver, card: card.clone(), nodes: on_dev, name: name_text });
        }
        out
    }

    /// Updates the descriptor cache and returns (pid, device id, client) of every readable DRM
    /// client on a node of `node_dev` (node -> device id, driver), and how many processes were
    /// hidden.
    fn clients(&mut self, now: Instant, node_dev: &HashMap<String, (String, String)>) -> (Vec<(u32, String, Client)>, usize) {
        let proc_dir = self.root.join("proc");
        let mut alive = HashSet::new();
        let mut out = Vec::new();
        let mut hidden = 0;
        for e in fs::read_dir(&proc_dir).into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            alive.insert(pid);
            let dir = e.path();
            let scan = self.pids.entry(pid).or_default();
            if scan.at.is_none_or(|t| now.saturating_duration_since(t) >= RESCAN) {
                // After the first listing the next ones are spread over RESCAN by PID, so the
                // processes are not all listed again in the same tick.
                let at = if scan.at.is_none() { now.checked_sub(stagger(pid)).unwrap_or(now) } else { now };
                *scan = PidScan { at: Some(at), ..Default::default() };
                match fs::read_dir(dir.join("fd")) {
                    Ok(fds) => {
                        for fd in fds.flatten() {
                            let Ok(target) = fs::read_link(fd.path()) else { continue };
                            let Some(node) = target.to_str().and_then(|t| t.strip_prefix("/dev/dri/")) else { continue };
                            scan.fds.push((fd.file_name().to_string_lossy().into_owned(), node.to_string()));
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::PermissionDenied => scan.denied = !kernel_thread(pid, read(dir.join("stat")).as_deref()),
                    Err(_) => {}
                }
            } else {
                // Known descriptors only; one closed or reused for something else drops out.
                let fd_dir = dir.join("fd");
                scan.fds.retain(|(fd, node)| fs::read_link(fd_dir.join(fd)).ok().and_then(|t| t.to_str().map(|t| t.strip_prefix("/dev/dri/") == Some(node.as_str()))).unwrap_or(false));
            }
            hidden += usize::from(scan.denied);
            for (fd, node) in &scan.fds {
                let Some((dev, driver)) = node_dev.get(node) else { continue };
                let Some(c) = read(dir.join("fdinfo").join(fd)).and_then(|t| drm_fdinfo::parse(&t)) else { continue };
                if c.driver == *driver {
                    out.push((pid, dev.clone(), c));
                }
            }
        }
        self.pids.retain(|p, _| alive.contains(p));
        (out, hidden)
    }

    pub fn read(&mut self, now: Instant) -> Reading {
        if self.devices_at.is_none_or(|t| now.saturating_duration_since(t) >= DEVICES_PERIOD) {
            self.devices = self.find_devices();
            self.devices_at = Some(now);
        }
        let mut r = Reading::default();
        if self.devices.is_empty() {
            self.pids.clear();
            return r;
        }
        // Each device's poll: a powered-down one is not touched (an xe read wakes it), one that
        // may sleep is read seldom while idle, as the other GPUs are.
        let mut polls = Vec::new();
        for d in &self.devices {
            let dev = d.card.join("device");
            let power = procfs::gpu_power(read(dev.join("power/runtime_status")).as_deref(), read(dev.join("power/control")).as_deref(), read(dev.join("boot_vga")).as_deref());
            let last = self.last.get(&d.id);
            polls.push(procfs::gpu_poll(power, last.map(|l| now.saturating_duration_since(l.at)), last.is_some_and(|l| procfs::gpu_busy(&l.reading))));
        }
        let node_dev: HashMap<String, (String, String)> = self.devices.iter().zip(&polls).filter(|(_, p)| **p == procfs::GpuPoll::Read).flat_map(|(d, _)| d.nodes.iter().map(|n| (n.clone(), (d.id.clone(), d.driver.clone())))).collect();
        let readings = if node_dev.is_empty() { None } else { Some(self.clients(now, &node_dev)) };
        let (all, hidden) = match readings {
            Some((c, h)) => (drm_fdinfo::dedupe(c), h),
            None => (HashMap::new(), self.hidden),
        };
        self.hidden = hidden;
        r.hidden = hidden;
        for (d, poll) in self.devices.clone().iter().zip(polls) {
            let seen = self.seen.entry(d.id.clone()).or_default();
            let idle = |seen: &BTreeSet<String>| {
                let engines: Vec<(String, f32)> = if seen.is_empty() { vec![("Render".into(), 0.0)] } else { seen.iter().map(|k| (drm_fdinfo::intel_engine_name(k), 0.0)).collect() };
                GpuDev { engines, id: d.id.clone(), name: d.name.clone(), ..Default::default() }
            };
            match poll {
                procfs::GpuPoll::Idle => r.devs.push(idle(seen)),
                procfs::GpuPoll::Keep => r.devs.extend(self.last.get(&d.id).map(|l| l.reading.clone())),
                procfs::GpuPoll::Read => {
                    let mine: Vec<(&Vec<u32>, &Client)> = all.iter().filter(|(k, _)| k.0 == d.id).map(|(_, v)| (&v.0, &v.1)).collect();
                    let (prev, wall) = match self.last.get(&d.id) {
                        Some(l) => (l.clients.clone(), now.saturating_duration_since(l.at).as_nanos() as u64),
                        None => (HashMap::new(), 0),
                    };
                    let busy = drm_fdinfo::device_busy(&prev, mine.iter().map(|m| m.1.clone()), wall);
                    for (pids, c) in &mine {
                        // A client several processes share counts for the first of them only.
                        if let (Some(pct), Some(pid)) = (busy.clients.get(&c.id), pids.first()) {
                            let v = r.by_pid.entry(*pid).or_insert(0.0);
                            *v = (*v + pct).min(100.0);
                        }
                    }
                    seen.extend(busy.engines.iter().map(|e| e.0.clone()));
                    let mut dev = idle(seen);
                    for (k, v) in &busy.engines {
                        let name = drm_fdinfo::intel_engine_name(k);
                        if let Some(e) = dev.engines.iter_mut().find(|e| e.0 == name) {
                            e.1 = *v;
                        }
                    }
                    let mem = drm_fdinfo::memory(mine.iter().map(|m| m.1));
                    let sum = |pick: fn(&str) -> bool| {
                        let v: Vec<u64> = mem.iter().filter(|(k, _)| pick(k)).map(|(_, v)| *v).collect();
                        (!v.is_empty()).then(|| v.iter().sum())
                    };
                    dev.vram_used = sum(|k| k.starts_with("vram") || k.starts_with("local"));
                    dev.shared_used = sum(|k| k.starts_with("system") || k == "gtt");
                    dev.clock_mhz = clock(d);
                    dev.lower_bound = hidden > 0;
                    self.last.insert(d.id.clone(), DevState { at: now, clients: busy.state, reading: dev.clone() });
                    r.devs.push(dev);
                }
            }
        }
        r
    }
}

/// The device's current frequency (MHz) from sysfs, where the driver has it: i915's
/// `gt_act_freq_mhz` on the card, xe's `act_freq` of its first GT.
fn clock(d: &Device) -> Option<u32> {
    let path = if d.driver == "xe" { d.card.join("device/tile0/gt0/freq0/act_freq") } else { d.card.join("gt_act_freq_mhz") };
    read(path)?.trim().parse().ok()
}

/// The plain note for a GPU figure that may be low: TMOP could not read every process.
pub fn hidden_note(hidden: usize) -> Option<String> {
    let who = if hidden == 1 { "1 process of another user".to_string() } else { format!("{hidden} processes of other users") };
    (hidden > 0).then(|| format!("Intel GPU usage is at least the figure shown: {who} could not be read. Run TMOP as root (sudo) to include them."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    const I915: &str = "pos:    0
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

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn link(target: &str, at: &Path) {
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        symlink(target, at).unwrap();
    }

    /// A process with descriptors (fd, link target) and the fdinfo of some of them.
    fn process(root: &Path, pid: u32, ppid: u32, fds: &[(&str, &str, Option<&str>)]) {
        let p = root.join(format!("proc/{pid}"));
        write(&p.join("stat"), &format!("{pid} (some (odd) name) S {ppid} 1 1 0"));
        fs::create_dir_all(p.join("fd")).unwrap();
        for (fd, target, info) in fds {
            link(target, &p.join("fd").join(fd));
            write(&p.join("fdinfo").join(fd), info.unwrap_or("pos: 0\nflags: 02\n"));
        }
    }

    fn deny(root: &Path, pid: u32) {
        fs::set_permissions(root.join(format!("proc/{pid}/fd")), fs::Permissions::from_mode(0o000)).unwrap();
    }

    /// A fake `/sys` and `/proc`: an i915 iGPU (card0 + renderD128), an AMD card that is not
    /// ours, one client held by two descriptors of one process and by a second process, a
    /// process that is not a GPU client, another user's process and a kernel thread.
    #[test]
    fn reads_a_fake_proc_and_sys_tree() {
        let root = std::env::temp_dir().join(format!("tmop-intel-gpu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let drm = root.join("sys/class/drm");
        let dev = root.join("sys/devices/pci0000:00/0000:00:02.0");
        for (f, v) in [("vendor", "0x8086\n"), ("device", "0x46a6\n"), ("boot_vga", "1\n"), ("power/runtime_status", "active\n"), ("power/control", "auto\n")] {
            write(&dev.join(f), v);
        }
        link("../../../bus/pci/drivers/i915", &dev.join("driver"));
        link("../../../devices/pci0000:00/0000:00:02.0", &drm.join("card0/device"));
        link("../../../devices/pci0000:00/0000:00:02.0", &drm.join("renderD128/device"));
        write(&drm.join("card0/gt_act_freq_mhz"), "1300\n");
        let amd = root.join("sys/devices/pci0000:00/0000:03:00.0");
        write(&amd.join("vendor"), "0x1002\n");
        link("../../../bus/pci/drivers/amdgpu", &amd.join("driver"));
        link("../../../devices/pci0000:00/0000:03:00.0", &drm.join("card1/device"));
        write(&root.join("usr/share/hwdata/pci.ids"), "# x\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n8086  Intel Corporation\n\t46a6  Alder Lake-P GT2 [Iris Xe Graphics]\n");
        let mem = |res: u64, sh: u64| format!("drm-total-system0: {res} KiB\ndrm-shared-system0: {sh} KiB\ndrm-resident-system0: {res} KiB\n");
        let first = format!("{I915}{}", mem(4096, 1024));
        process(&root, 100, 1, &[("3", "/dev/dri/renderD128", Some(&first)), ("4", "/dev/dri/renderD128", Some(&first)), ("5", "/dev/null", None), ("6", "/dev/dri/card1", Some("drm-driver: amdgpu\ndrm-client-id: 7\ndrm-engine-gfx: 5 ns\n"))]);
        process(&root, 101, 100, &[("9", "/dev/dri/card0", Some(&first))]);
        process(&root, 102, 1, &[("1", "/home/u/file", None)]);
        process(&root, 200, 1, &[]);
        process(&root, 2, 0, &[]);
        process(&root, 300, 2, &[]);
        for pid in [200, 2, 300] {
            deny(&root, pid);
        }
        let root_user = crate::platform::is_elevated();

        let t0 = Instant::now();
        let mut g = IntelGpus::new(&root);
        let r = g.read(t0);
        assert_eq!(r.devs.len(), 1, "only the i915 card: {:?}", r.devs);
        let d = &r.devs[0];
        assert_eq!((d.id.as_str(), d.name.as_str(), d.clock_mhz), ("card0", "Intel Alder Lake-P GT2 [Iris Xe Graphics]", Some(1300)));
        // First reading: nothing to compare with, all engines at 0 (not a spike).
        assert_eq!(d.engines, [("Copy".to_string(), 0.0), ("Render".into(), 0.0), ("Video".into(), 0.0), ("Video enhance".into(), 0.0)]);
        // One client, counted once although three descriptors in two processes hold it: 3 MiB
        // of its own plus 1 MiB shared.
        assert_eq!((d.shared_used, d.vram_used), (Some(4096 * 1024), None));
        // Only process 200 is hidden: kernel threads have no descriptors to hide.
        assert_eq!(r.hidden, if root_user { 0 } else { 1 });
        assert_eq!(d.lower_bound, !root_user);

        // One second later the client kept render busy for 400 ms and both video engines for
        // 1.5 s together (capacity 2: 75 %).
        let busy = I915.replace("9288864723 ns", "9688864723 ns").replace("drm-engine-video:   0 ns", "drm-engine-video:   1500000000 ns");
        let second = format!("{busy}{}", mem(4096, 1024));
        for (pid, fd) in [(100, "3"), (100, "4"), (101, "9")] {
            write(&root.join(format!("proc/{pid}/fdinfo/{fd}")), &second);
        }
        let r = g.read(t0 + Duration::from_secs(1));
        let e: HashMap<&str, i64> = r.devs[0].engines.iter().map(|(n, v)| (n.as_str(), v.round() as i64)).collect();
        assert_eq!(e, [("Render", 40), ("Copy", 0), ("Video", 75), ("Video enhance", 0)].into());
        // The shared client counts for the first process holding it only.
        assert_eq!(r.by_pid.len(), 1);
        assert_eq!(r.by_pid[&100].round(), 75.0);

        // The process closes the GPU and the other one ends: the client is gone, nothing goes
        // negative and the engines stay listed at 0. Known descriptors are checked every tick.
        fs::remove_file(root.join("proc/100/fd/3")).unwrap();
        fs::remove_file(root.join("proc/100/fd/4")).unwrap();
        fs::remove_dir_all(root.join("proc/101")).unwrap();
        let r = g.read(t0 + Duration::from_secs(2));
        assert!(r.devs[0].engines.iter().all(|e| e.1 == 0.0) && r.devs[0].engines.len() == 4, "{:?}", r.devs[0].engines);
        assert_eq!(r.devs[0].shared_used, None, "no client: no memory figure");
        assert!(r.by_pid.is_empty());

        // A process that opens the GPU later is found by the next full listing.
        link("/dev/dri/renderD128", &root.join("proc/102/fd/8"));
        write(&root.join("proc/102/fdinfo/8"), &second.replace("drm-client-id:      7", "drm-client-id:      9"));
        let r = g.read(t0 + Duration::from_secs(2) + RESCAN);
        assert!(r.devs[0].engines.iter().all(|e| e.1 == 0.0), "a new client starts at 0");
        assert!(g.pids[&102].fds.iter().any(|f| f.0 == "8"));
        assert!(!g.pids.contains_key(&101), "ended processes leave the cache");

        for pid in [200, 2, 300] {
            let _ = fs::set_permissions(root.join(format!("proc/{pid}/fd")), fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_intel_device_reads_nothing_in_proc() {
        let root = std::env::temp_dir().join(format!("tmop-intel-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        process(&root, 10, 1, &[("3", "/dev/dri/renderD128", Some(I915))]);
        let mut g = IntelGpus::new(&root);
        assert_eq!(g.read(Instant::now()), Reading::default());
        assert!(g.pids.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// The cost of the `/proc` walk on this machine's real `/proc`, as if it had an Intel GPU at
    /// `/dev/dri/card0` and `renderD128`: a full listing of every descriptor, then the ticks in
    /// between that read only known DRM descriptors. Run with
    /// `cargo test --release --lib intel_gpu::tests::scan_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, prints timings of the real /proc"]
    fn scan_cost() {
        let nodes: HashMap<String, (String, String)> = ["card0", "renderD128"].iter().map(|n| (n.to_string(), ("card0".to_string(), "i915".to_string()))).collect();
        let mut g = IntelGpus::new("/");
        let t0 = Instant::now();
        let pids = fs::read_dir("/proc").unwrap().flatten().filter(|e| e.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))).count();
        let fds: usize = fs::read_dir("/proc").unwrap().flatten().filter_map(|e| fs::read_dir(e.path().join("fd")).ok()).map(|d| d.count()).sum();
        let time = |g: &mut IntelGpus, now: Instant| {
            let start = Instant::now();
            let (c, hidden) = g.clients(now, &nodes);
            (start.elapsed(), c.len(), hidden)
        };
        // The first tick lists everything; then one tick a second, as the sampler runs.
        let first = time(&mut g, t0);
        let mut ticks: Vec<Duration> = (1..=30).map(|s| time(&mut g, t0 + Duration::from_secs(s)).0).collect();
        // A tick that lists nothing: known descriptors only.
        let mut quiet = IntelGpus::new("/");
        time(&mut quiet, t0);
        for p in quiet.pids.values_mut() {
            p.at = Some(t0);
        }
        let known = time(&mut quiet, t0 + Duration::from_millis(10)).0;
        ticks.sort();
        eprintln!("{pids} processes, {fds} readable descriptors; first tick (all listed): {:?}; ticks a second apart: median {:?}, max {:?}; known descriptors only: {known:?}; clients {}, hidden {}", first.0, ticks[ticks.len() / 2], ticks[ticks.len() - 1], first.1, first.2);
    }

    #[test]
    fn kernel_threads_and_notes() {
        assert!(kernel_thread(2, None));
        assert!(kernel_thread(55, Some("55 (kworker/0:1-events) I 2 0 0")));
        assert!(!kernel_thread(900, Some("900 (a) b) (c) S 1 900 900")));
        assert!(!kernel_thread(901, None));
        assert_eq!(hidden_note(0), None);
        assert!(hidden_note(1).unwrap().contains("1 process of another user could not be read"));
        assert!(hidden_note(12).unwrap().contains("at least") && hidden_note(12).unwrap().contains("12 processes of other users"));
    }
}
