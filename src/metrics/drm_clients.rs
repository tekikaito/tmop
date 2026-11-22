//! DRM clients on Linux: the GPU users in `/proc/<pid>/fdinfo`, parsed and compared by
//! [`super::drm_fdinfo`], for the drivers that print the generic usage keys:
//! * Intel (i915, xe): the whole device's figures come from here, as these drivers have no
//!   unprivileged system-wide busy counter: engine shares summed over the clients, name from
//!   `pci.ids`, frequency from sysfs (i915 `gt_act_freq_mhz`, xe `tile0/gt0/freq0/act_freq`),
//!   memory from the clients' `drm-resident-*` where the driver prints it.
//! * AMD (amdgpu): only the Processes column. The device's figures keep coming from sysfs
//!   (`gpu_busy_percent`, in `linux.rs`), which covers every user's work, so they are not partial.
//!
//! Every process's figure is [`drm_fdinfo::process_busy`]: its busiest engine class on its
//! busiest GPU. Only processes whose `/proc/<pid>/fd` TMOP may read count: without root the
//! user's own (where the compositor, browsers and games run). The others are counted
//! ([`Reading::hidden`], kernel threads left out); an Intel device is then marked
//! [`GpuDev::partial`], which the Performance card states in one plain line ([`note`]).
//!
//! Cost: a GPU client is found by its descriptor linking to `/dev/dri/*`. Listing every
//! descriptor of every process each tick is what costs; so a process's descriptors are listed
//! once and then again only every [`RESCAN`] (spread by PID), and in between only its known DRM
//! descriptors are read. One walk serves every supported device; without one nothing in `/proc`
//! is read at all.
//!
//! Everything is read below a root directory (`/` in use), so tests can point it at a fake
//! `/proc` and `/sys` tree.

use super::drm_fdinfo::{self, Client, ProcessShare};
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

/// The drivers whose fdinfo is read. radeon prints no fdinfo at all (its `drm_driver` has no
/// `show_fdinfo`), so it is not here.
const DRIVERS: [&str; 3] = ["i915", "xe", "amdgpu"];

/// Whether the device's own figures come from its clients (Intel) rather than from sysfs
/// (amdgpu, whose clients count only for the Processes column).
fn owns_figures(driver: &str) -> bool {
    driver != "amdgpu"
}

/// One DRM device of a supported driver.
#[derive(Clone, Debug)]
struct Device {
    /// Its card directory name (`card0`): the GPU's id, as the amdgpu sysfs path names it too.
    id: String,
    driver: String,
    /// The card directory in `/sys/class/drm`.
    card: PathBuf,
    /// Its DRM nodes (`card0`, `renderD128`).
    nodes: Vec<String>,
    /// Shown name (Intel only; amdgpu's comes from sysfs).
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
    /// The device as read (for amdgpu only its engines, which decide how often it is read).
    reading: GpuDev,
    /// Its clients' shares per process, kept for the ticks that reuse the reading.
    shares: Vec<ProcessShare>,
}

/// One read of all supported GPUs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// The Intel devices (amdgpu devices are read from sysfs, not here).
    pub devs: Vec<GpuDev>,
    /// The Processes column: each process's busiest engine on its busiest GPU (%).
    pub by_pid: HashMap<u32, f32>,
    /// Processes (not kernel threads) whose descriptors could not be read.
    pub hidden: usize,
}

pub struct DrmClients {
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

impl DrmClients {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), devices: Vec::new(), devices_at: None, pids: HashMap::new(), last: HashMap::new(), seen: HashMap::new(), pci_ids: None, hidden: 0 }
    }

    fn drm_dir(&self) -> PathBuf {
        self.root.join("sys/class/drm")
    }

    fn intel_name(&mut self, dev: &Path) -> String {
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

    /// The cards of `/sys/class/drm` with a supported driver, each with the nodes on the same
    /// device.
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
            let name_text = if owns_figures(&driver) { self.intel_name(&card.join("device")) } else { String::new() };
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
            let poll = match procfs::gpu_poll(power, last.map(|l| now.saturating_duration_since(l.at)), last.is_some_and(|l| procfs::gpu_busy(&l.reading))) {
                // amdgpu's fdinfo does not resume the device (drm_show_fdinfo and
                // amdgpu_show_fdinfo take no runtime-PM reference), so an awake one is read every
                // tick; only a suspended one, which runs nothing, is skipped.
                procfs::GpuPoll::Keep if !owns_figures(&d.driver) => procfs::GpuPoll::Read,
                p => p,
            };
            polls.push(poll);
        }
        let node_dev: HashMap<String, (String, String)> = self.devices.iter().zip(&polls).filter(|(_, p)| **p == procfs::GpuPoll::Read).flat_map(|(d, _)| d.nodes.iter().map(|n| (n.clone(), (d.id.clone(), d.driver.clone())))).collect();
        // One walk of /proc for every device read this tick.
        let readings = if node_dev.is_empty() { None } else { Some(self.clients(now, &node_dev)) };
        let (all, hidden) = match readings {
            Some((c, h)) => (drm_fdinfo::dedupe(c), h),
            None => (HashMap::new(), self.hidden),
        };
        self.hidden = hidden;
        r.hidden = hidden;
        let mut shares: Vec<ProcessShare> = Vec::new();
        for (d, poll) in self.devices.clone().iter().zip(polls) {
            let owned = owns_figures(&d.driver);
            let seen = self.seen.entry(d.id.clone()).or_default();
            let idle = |seen: &BTreeSet<String>| {
                let engines: Vec<(String, f32)> = if seen.is_empty() { vec![("Render".into(), 0.0)] } else { seen.iter().map(|k| (drm_fdinfo::intel_engine_name(k), 0.0)).collect() };
                GpuDev { engines, id: d.id.clone(), name: d.name.clone(), ..Default::default() }
            };
            match poll {
                procfs::GpuPoll::Idle => {
                    if owned {
                        r.devs.push(idle(seen));
                    }
                }
                procfs::GpuPoll::Keep => {
                    if let Some(l) = self.last.get(&d.id) {
                        shares.extend(l.shares.iter().cloned());
                        if owned {
                            r.devs.push(l.reading.clone());
                        }
                    }
                }
                procfs::GpuPoll::Read => {
                    let mine: Vec<(&Vec<u32>, &Client)> = all.iter().filter(|(k, _)| k.0 == d.id).map(|(_, v)| (&v.0, &v.1)).collect();
                    let (prev, wall) = match self.last.get(&d.id) {
                        Some(l) => (l.clients.clone(), now.saturating_duration_since(l.at).as_nanos() as u64),
                        None => (HashMap::new(), 0),
                    };
                    let busy = drm_fdinfo::device_busy(&prev, mine.iter().map(|m| m.1.clone()), wall);
                    // A client several processes share counts for the first of them only.
                    let dev_shares: Vec<ProcessShare> = mine
                        .iter()
                        .filter_map(|(pids, c)| Some((*pids.first()?, busy.clients.get(&c.id)?)))
                        .flat_map(|(pid, s)| s.iter().map(move |(e, v)| (pid, d.id.clone(), e.clone(), *v)))
                        .collect();
                    shares.extend(dev_shares.iter().cloned());
                    let dev = if owned {
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
                        dev.partial = hidden > 0;
                        r.devs.push(dev.clone());
                        dev
                    } else {
                        // Not shown: the sysfs reading is. Its engines only say whether it is busy.
                        GpuDev { engines: busy.engines.clone(), id: d.id.clone(), ..Default::default() }
                    };
                    self.last.insert(d.id.clone(), DevState { at: now, clients: busy.state, reading: dev, shares: dev_shares });
                }
            }
        }
        r.by_pid = drm_fdinfo::process_busy(shares);
        r
    }
}

/// The device's current frequency (MHz) from sysfs, where the driver has it: i915's
/// `gt_act_freq_mhz` on the card, xe's `act_freq` of its first GT.
fn clock(d: &Device) -> Option<u32> {
    let path = if d.driver == "xe" { d.card.join("device/tile0/gt0/freq0/act_freq") } else { d.card.join("gt_act_freq_mhz") };
    read(path)?.trim().parse().ok()
}

/// What the Performance GPU card and `--dump` say when some processes could not be read. No
/// count: nearly all of them are system daemons that never open the GPU, and from outside their
/// `/proc/<pid>/fd` there is no reliable way to tell, so a number would only alarm.
pub const PARTIAL_NOTE: &str = "Intel GPU usage counts the programs TMOP may read. Programs of other users are not included.";

/// The note for a set of GPU readings: only a device whose own figures come from its clients
/// (Intel) can be partial; amdgpu (sysfs) and NVIDIA (NVML) cover every user.
pub fn note(devs: &[GpuDev]) -> Option<&'static str> {
    devs.iter().any(|d| d.partial).then_some(PARTIAL_NOTE)
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

    /// amdgpu's fdinfo in the format of `drm_show_fdinfo()` and `amdgpu_show_fdinfo()`, as in
    /// [`drm_fdinfo`]'s tests: client `id`, `gfx` and `dec` busy time in ns.
    fn amdgpu(id: u64, gfx: u64, dec: u64) -> String {
        format!("pos:\t0\nflags:\t02100002\ndrm-driver:\tamdgpu\ndrm-client-id:\t{id}\ndrm-pdev:\t0000:03:00.0\npasid:\t32771\ndrm-total-vram:\t188 MiB\ndrm-shared-vram:\t0\ndrm-resident-vram:\t188 MiB\ndrm-memory-vram:\t192512 KiB\namd-requested-vram:\t192512 KiB\ndrm-engine-gfx:\t{gfx} ns\ndrm-engine-dec:\t{dec} ns\n")
    }

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

    /// A PCI device with `driver` and `files`, and its DRM nodes linked to it.
    fn card(root: &Path, slot: &str, driver: &str, files: &[(&str, &str)], nodes: &[&str]) {
        let dev = root.join("sys/devices/pci0000:00").join(slot);
        for (f, v) in files {
            write(&dev.join(f), v);
        }
        link(&format!("../../../bus/pci/drivers/{driver}"), &dev.join("driver"));
        for n in nodes {
            link(&format!("../../../devices/pci0000:00/{slot}"), &root.join("sys/class/drm").join(n).join("device"));
        }
    }

    /// A fake `/sys` and `/proc` of a hybrid laptop: an i915 iGPU (card0 + renderD128) and an
    /// amdgpu dGPU (card1 + renderD129, awake). One Intel client is held by two descriptors of
    /// one process and by a second process; that process also has an amdgpu client; another
    /// process uses the AMD GPU only. Also a radeon card (not read), a process that is not a GPU
    /// client, another user's process and kernel threads.
    #[test]
    fn reads_a_fake_proc_and_sys_tree() {
        let root = std::env::temp_dir().join(format!("tmop-drm-clients-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let drm = root.join("sys/class/drm");
        card(&root, "0000:00:02.0", "i915", &[("vendor", "0x8086\n"), ("device", "0x46a6\n"), ("boot_vga", "1\n"), ("power/runtime_status", "active\n"), ("power/control", "auto\n")], &["card0", "renderD128"]);
        write(&drm.join("card0/gt_act_freq_mhz"), "1300\n");
        card(&root, "0000:03:00.0", "amdgpu", &[("vendor", "0x1002\n"), ("boot_vga", "0\n"), ("power/runtime_status", "active\n"), ("power/control", "auto\n"), ("gpu_busy_percent", "37\n")], &["card1", "renderD129"]);
        card(&root, "0000:04:00.0", "radeon", &[("vendor", "0x1002\n")], &["card2"]);
        write(&root.join("usr/share/hwdata/pci.ids"), "# x\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n8086  Intel Corporation\n\t46a6  Alder Lake-P GT2 [Iris Xe Graphics]\n");
        let mem = |res: u64, sh: u64| format!("drm-total-system0: {res} KiB\ndrm-shared-system0: {sh} KiB\ndrm-resident-system0: {res} KiB\n");
        let first = format!("{I915}{}", mem(4096, 1024));
        let radeon = "drm-driver: radeon\ndrm-client-id: 1\ndrm-engine-gfx: 5 ns\n";
        process(&root, 100, 1, &[("3", "/dev/dri/renderD128", Some(&first)), ("4", "/dev/dri/renderD128", Some(&first)), ("5", "/dev/null", None), ("6", "/dev/dri/renderD129", Some(&amdgpu(7, 1_000_000_000, 0))), ("7", "/dev/dri/card2", Some(radeon))]);
        process(&root, 101, 100, &[("9", "/dev/dri/card0", Some(&first))]);
        process(&root, 102, 1, &[("1", "/home/u/file", None)]);
        // Uses the AMD GPU through two descriptors of the same client, and a second client.
        process(&root, 103, 1, &[("4", "/dev/dri/renderD129", Some(&amdgpu(8, 0, 0))), ("5", "/dev/dri/card1", Some(&amdgpu(8, 0, 0))), ("6", "/dev/dri/renderD129", Some(&amdgpu(9, 0, 0)))]);
        process(&root, 200, 1, &[]);
        process(&root, 2, 0, &[]);
        process(&root, 300, 2, &[]);
        for pid in [200, 2, 300] {
            deny(&root, pid);
        }
        let root_user = crate::platform::is_elevated();

        let t0 = Instant::now();
        let mut g = DrmClients::new(&root);
        let r = g.read(t0);
        assert_eq!(g.devices.iter().map(|d| (d.id.as_str(), d.driver.as_str())).collect::<Vec<_>>(), [("card0", "i915"), ("card1", "amdgpu")], "radeon prints no fdinfo");
        assert_eq!(r.devs.len(), 1, "only the Intel card has its figures from here: {:?}", r.devs);
        let d = &r.devs[0];
        assert_eq!((d.id.as_str(), d.name.as_str(), d.clock_mhz), ("card0", "Intel Alder Lake-P GT2 [Iris Xe Graphics]", Some(1300)));
        // First reading: nothing to compare with, all engines at 0 (not a spike).
        assert_eq!(d.engines, [("Copy".to_string(), 0.0), ("Render".into(), 0.0), ("Video".into(), 0.0), ("Video enhance".into(), 0.0)]);
        // One client, counted once although three descriptors in two processes hold it: 3 MiB
        // of its own plus 1 MiB shared.
        assert_eq!((d.shared_used, d.vram_used), (Some(4096 * 1024), None));
        // Only process 200 is hidden: kernel threads have no descriptors to hide.
        assert_eq!(r.hidden, if root_user { 0 } else { 1 });
        assert_eq!(d.partial, !root_user);
        assert!(r.by_pid.values().all(|v| *v == 0.0), "new clients start at 0: {:?}", r.by_pid);

        // One second later the Intel client kept render busy for 400 ms and both video engines
        // for 1.5 s together (capacity 2: 75 %). On the AMD GPU process 100 ran gfx for 200 ms
        // more; process 103's first client 300 ms of gfx and 600 ms of dec, its second 100 ms
        // of gfx.
        let busy = I915.replace("9288864723 ns", "9688864723 ns").replace("drm-engine-video:   0 ns", "drm-engine-video:   1500000000 ns");
        let second = format!("{busy}{}", mem(4096, 1024));
        for (pid, fd) in [(100, "3"), (100, "4"), (101, "9")] {
            write(&root.join(format!("proc/{pid}/fdinfo/{fd}")), &second);
        }
        write(&root.join("proc/100/fdinfo/6"), &amdgpu(7, 1_200_000_000, 0));
        for fd in ["4", "5"] {
            write(&root.join(format!("proc/103/fdinfo/{fd}")), &amdgpu(8, 300_000_000, 600_000_000));
        }
        write(&root.join("proc/103/fdinfo/6"), &amdgpu(9, 100_000_000, 0));
        let r = g.read(t0 + Duration::from_secs(1));
        let e: HashMap<&str, i64> = r.devs[0].engines.iter().map(|(n, v)| (n.as_str(), v.round() as i64)).collect();
        assert_eq!(e, [("Render", 40), ("Copy", 0), ("Video", 75), ("Video enhance", 0)].into());
        let by_pid: HashMap<u32, i64> = r.by_pid.iter().map(|(p, v)| (*p, v.round() as i64)).collect();
        // 100: Intel video 75 % beats AMD gfx 20 % (busiest GPU, not the sum). The shared Intel
        // client counts for the first process holding it only, so 101 has no entry. 103: dec
        // 60 % beats gfx 30 + 10 % (two clients on one engine add up); its doubled descriptor
        // adds nothing.
        assert_eq!(by_pid, [(100, 75), (103, 60)].into());
        // The amdgpu device itself is still read from sysfs: nothing here, and no note for it.
        assert!(r.devs.iter().all(|d| d.id != "card1"));
        let sysfs_amd = GpuDev { id: "card1".into(), engines: vec![("Graphics".into(), 37.0)], ..Default::default() };
        assert_eq!(note(std::slice::from_ref(&sysfs_amd)), None);
        assert_eq!(note(&[r.devs[0].clone(), sysfs_amd]), (!root_user).then_some(PARTIAL_NOTE));

        // The process closes the Intel GPU and the other one ends: the client is gone, nothing
        // goes negative and the engines stay listed at 0. Known descriptors are checked every
        // tick. Process 100's AMD client stays, idle this time.
        fs::remove_file(root.join("proc/100/fd/3")).unwrap();
        fs::remove_file(root.join("proc/100/fd/4")).unwrap();
        fs::remove_dir_all(root.join("proc/101")).unwrap();
        let r = g.read(t0 + Duration::from_secs(2));
        assert!(r.devs[0].engines.iter().all(|e| e.1 == 0.0) && r.devs[0].engines.len() == 4, "{:?}", r.devs[0].engines);
        assert_eq!(r.devs[0].shared_used, None, "no client: no memory figure");
        assert_eq!(r.by_pid.get(&100), Some(&0.0));
        assert!(!r.by_pid.contains_key(&101));

        // A process that opens the GPU later is found by the next full listing.
        link("/dev/dri/renderD128", &root.join("proc/102/fd/8"));
        write(&root.join("proc/102/fdinfo/8"), &second.replace("drm-client-id:      7", "drm-client-id:      9"));
        let r = g.read(t0 + Duration::from_secs(2) + RESCAN);
        assert!(r.devs[0].engines.iter().all(|e| e.1 == 0.0), "a new client starts at 0");
        assert!(g.pids[&102].fds.iter().any(|f| f.0 == "8"));
        assert!(!g.pids.contains_key(&101), "ended processes leave the cache");

        // The AMD GPU powers down: its clients are not read; its processes drop out.
        write(&root.join("sys/devices/pci0000:00/0000:03:00.0/power/runtime_status"), "suspended\n");
        write(&root.join("proc/100/fdinfo/6"), &amdgpu(7, 9_000_000_000, 0));
        let r = g.read(t0 + Duration::from_secs(3) + RESCAN);
        assert!(!r.by_pid.contains_key(&100) && !r.by_pid.contains_key(&103), "{:?}", r.by_pid);

        for pid in [200, 2, 300] {
            let _ = fs::set_permissions(root.join(format!("proc/{pid}/fd")), fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// A machine with only an AMD GPU: its clients count for the Processes column, the device
    /// figures stay sysfs's, and an unreadable process brings no note.
    #[test]
    fn an_amd_only_machine_fills_the_column_without_a_note() {
        let root = std::env::temp_dir().join(format!("tmop-drm-amd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        card(&root, "0000:03:00.0", "amdgpu", &[("vendor", "0x1002\n"), ("boot_vga", "1\n")], &["card0", "renderD128"]);
        process(&root, 10, 1, &[("3", "/dev/dri/renderD128", Some(&amdgpu(1, 0, 0)))]);
        process(&root, 20, 1, &[]);
        deny(&root, 20);
        let t0 = Instant::now();
        let mut g = DrmClients::new(&root);
        g.read(t0);
        write(&root.join("proc/10/fdinfo/3"), &amdgpu(1, 250_000_000, 0));
        let r = g.read(t0 + Duration::from_millis(500));
        assert!(r.devs.is_empty());
        assert_eq!(r.by_pid.get(&10).map(|v| v.round()), Some(50.0));
        assert_eq!(note(&r.devs), None);
        let _ = fs::set_permissions(root.join("proc/20/fd"), fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_supported_device_reads_nothing_in_proc() {
        let root = std::env::temp_dir().join(format!("tmop-drm-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        card(&root, "0000:04:00.0", "radeon", &[("vendor", "0x1002\n")], &["card0"]);
        process(&root, 10, 1, &[("3", "/dev/dri/renderD128", Some(I915))]);
        let mut g = DrmClients::new(&root);
        assert_eq!(g.read(Instant::now()), Reading::default());
        assert!(g.pids.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// The cost of the `/proc` walk on this machine's real `/proc`, as if it had a GPU at
    /// `/dev/dri/card0` and `renderD128`: a full listing of every descriptor, then the ticks in
    /// between that read only known DRM descriptors. Run with
    /// `cargo test --release --lib drm_clients::tests::scan_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, prints timings of the real /proc"]
    fn scan_cost() {
        let nodes: HashMap<String, (String, String)> = ["card0", "renderD128"].iter().map(|n| (n.to_string(), ("card0".to_string(), "i915".to_string()))).collect();
        let mut g = DrmClients::new("/");
        let t0 = Instant::now();
        let pids = fs::read_dir("/proc").unwrap().flatten().filter(|e| e.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))).count();
        let fds: usize = fs::read_dir("/proc").unwrap().flatten().filter_map(|e| fs::read_dir(e.path().join("fd")).ok()).map(|d| d.count()).sum();
        let time = |g: &mut DrmClients, now: Instant| {
            let start = Instant::now();
            let (c, hidden) = g.clients(now, &nodes);
            (start.elapsed(), c.len(), hidden)
        };
        // The first tick lists everything; then one tick a second, as the sampler runs.
        let first = time(&mut g, t0);
        let mut ticks: Vec<Duration> = (1..=30).map(|s| time(&mut g, t0 + Duration::from_secs(s)).0).collect();
        // A tick that lists nothing: known descriptors only.
        let mut quiet = DrmClients::new("/");
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
        // Calm and exact: no count, no advice to run a GUI app as root.
        assert!(PARTIAL_NOTE.contains("Programs of other users are not included."));
        assert!(!PARTIAL_NOTE.contains("root") && !PARTIAL_NOTE.contains("sudo") && !PARTIAL_NOTE.bytes().any(|b| b.is_ascii_digit()));
        assert_eq!(note(&[]), None);
        assert_eq!(note(&[GpuDev { partial: true, ..Default::default() }]), Some(PARTIAL_NOTE));
    }
}
