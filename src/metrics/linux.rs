//! Linux metric collection: plain `std::fs` reads of `/proc` and `/sys`, parsed by the pure
//! functions in [`super::procfs`].
//!
//! * CPU: `/proc/stat` (`cpu`, `cpuN`); clocks from cpufreq (`scaling_cur_freq`, `cpuinfo_max_freq`,
//!   `base_frequency` / `amd_pstate_nominal_freq`), else `/proc/cpuinfo` "cpu MHz" (static).
//! * Memory: `/proc/meminfo`. Disk: `/proc/diskstats`, whole physical disks (`/sys/block`).
//! * Network: `/sys/class/net/<if>/statistics`, up interfaces backed by a device.
//! * GPU: amdgpu sysfs (`gpu_busy_percent`, `mem_info_vram_used`), Intel i915 / xe DRM client
//!   stats in `/proc/<pid>/fdinfo` ([`super::drm_clients`], which also gives the Processes
//!   column for Intel and amdgpu) and NVIDIA NVML (`dlopen`).
//! * Thermal: hwmon (`k10temp`, `zenpower`, `coretemp` for the CPU) and `thermal_zone*`. A GPU
//!   driver's hwmon chip (amdgpu edge / junction / mem, xe pkg / vram, i915 package) gives that
//!   GPU its temperatures, matched by PCI device; NVML gives NVIDIA's.
//! * Uptime `/proc/uptime`, open files `/proc/sys/fs/file-nr`, threads `/proc/loadavg`.
//!
//! Process rows come from the Linux process enumerator ([`crate::process::Enumerator`]); the
//! system-wide counts above stay the snapshot's process, thread and open-file totals.

use super::common::{cpu_brand, disk_firmware, disk_model, gpu_sources, gpu_thermal, net_delta, split_storage, tick_delta_pct, NetMark, Storage, Ticks};
use super::nvml::Nvml;
use super::procfs::{self, DiskStat, DiskTotals, GpuDev, HwmonChip, NetIf};
use super::{Probe, Snapshot, Sources};
use crate::process::{Enumerator, ProcessRow};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SOURCES: Sources = Sources {
    disk: "Active time (all physical disks, /proc/diskstats)",
    network: "Throughput (all adapters, sysfs)",
    gpu: "Busiest engine of the busiest GPU (amdgpu sysfs, Intel DRM fdinfo, NVIDIA NVML)",
    no_gpu: "No GPU utilization was reported (AMD, Intel and NVIDIA GPUs are supported).",
    thermal: "hwmon sensors and thermal zones",
    thermal_hint: "Load the k10temp or coretemp module (lm-sensors) for a CPU temperature",
    sensor: "hwmon",
    handles: "Open files",
};

const CPU_DIR: &str = "/sys/devices/system/cpu";

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// The firmware revision of the disk whose `device` directory of `/sys/block` is `dir`: `rev`
/// (SATA, SCSI), else `firmware_rev` (NVMe); empty when it has neither.
fn firmware_rev(dir: &Path) -> String {
    ["rev", "firmware_rev"].iter().map(|f| disk_firmware(&read(dir.join(f)).unwrap_or_default())).find(|s| !s.is_empty()).unwrap_or_default()
}

/// How often the file systems are asked how full they are.
const DISK_SPACE_PERIOD: Duration = Duration::from_secs(10);

/// The kernel's name of the block device a mount is on ("nvme0n1p2", "dm-0"): what its source
/// links to, else (`/dev/root`) the device of the mount.
fn block_name(m: &crate::pro::mounts::Mount) -> Option<String> {
    let name = |p: PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let known = |n: &String| Path::new("/sys/class/block").join(n).exists();
    fs::canonicalize(&m.source).ok().and_then(name).filter(known).or_else(|| fs::canonicalize(format!("/sys/dev/block/{}:{}", m.dev.0, m.dev.1)).ok().and_then(name))
}

/// The whole disks block device `name` lies on: "nvme0n1" for "nvme0n1p2", and for a
/// device-mapper or md device itself and the disks below it.
fn whole_disks(name: &str, depth: u32) -> Vec<String> {
    let Ok(dir) = fs::canonicalize(format!("/sys/class/block/{name}")) else { return Vec::new() };
    let dir = if dir.join("partition").exists() { dir.parent().map_or(dir.clone(), Path::to_path_buf) } else { dir };
    let Some(disk) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    let mut out = vec![disk];
    if depth < 4 {
        for below in fs::read_dir(dir.join("slaves")).into_iter().flatten().flatten() {
            out.extend(whole_disks(&below.file_name().to_string_lossy(), depth + 1));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Used and total bytes of the mounted file systems, as the Disk Space page counts them: on each
/// disk the ones wholly on it, and all of them once (a RAID or LVM volume on several disks is on
/// none of them). Only local block devices are asked, so no unreachable share can hold this up.
fn disk_spaces() -> Storage {
    let Some(text) = read("/proc/self/mountinfo") else { return Storage::default() };
    let mounts = crate::pro::mounts::parse_mountinfo(&text);
    let mut seen = HashSet::new();
    let mut volumes = Vec::new();
    for m in procfs::block_mounts(&mounts) {
        let Some(dev) = block_name(m).filter(|d| seen.insert(d.clone())) else { continue };
        let Some((total, free)) = crate::pro::mounts::space(Path::new(&m.point)).filter(|s| s.0 > 0) else { continue };
        volumes.push((procfs::physical_disks(whole_disks(&dev, 0)), (total.saturating_sub(free), total)));
    }
    split_storage(&volumes)
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read(path)?.trim().parse().ok()
}

/// Entries of `dir` whose file name starts with `prefix`, sorted by name.
fn entries(dir: &str, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(prefix)).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Clocks (cpufreq, or "cpu MHz" in `/proc/cpuinfo`) and a temperature source (hwmon or a
/// thermal zone).
pub fn probe() -> Probe {
    let clocks = Path::new(CPU_DIR).join("cpu0/cpufreq/scaling_cur_freq").exists() || read("/proc/cpuinfo").is_some_and(|c| !procfs::cpuinfo_mhz(&c).is_empty());
    let thermal = !entries("/sys/class/thermal", "thermal_zone").is_empty()
        || entries("/sys/class/hwmon", "hwmon").iter().any(|h| fs::read_dir(h).is_ok_and(|d| d.flatten().any(|e| is_temp_input(&e.file_name().to_string_lossy()))));
    Probe { clocks, thermal }
}

fn is_temp_input(name: &str) -> bool {
    name.starts_with("temp") && name.ends_with("_input")
}

/// The NVML library is probed once; a machine without it is not asked again.
enum NvmlState {
    Untried,
    Loaded(Nvml),
    Absent,
}

pub struct Collector {
    seq: u64,
    prev_sys: Option<Ticks>,
    prev_cores: Vec<(u32, Ticks)>,
    prev_disk: Option<(Instant, DiskTotals)>,
    /// The disks of the last reading, for each disk's own rates.
    prev_disk_stats: Vec<DiskStat>,
    /// The model and firmware revision of each disk listed (device -> both), read when the disk first shows.
    disk_models: HashMap<String, (String, String)>,
    /// Used and total bytes of the file systems on each disk (device -> bytes), and when asked.
    disk_spaces: Storage,
    disk_spaces_at: Option<Instant>,
    prev_net: Option<NetMark>,
    /// Static cpufreq figures (kHz) read once: max, base.
    freq_max: Option<u64>,
    freq_base: Option<u64>,
    name: String,
    nvml: NvmlState,
    /// The NVIDIA display devices on the PCI bus (looked up once).
    nvidia_pci: Option<Vec<PathBuf>>,
    /// Last reading of each GPU that may sleep (amdgpu card dir or hwmon dir -> when, reading),
    /// and of the NVIDIA GPUs read through a short-lived NVML.
    sleepy: HashMap<PathBuf, (Instant, GpuDev)>,
    sleepy_temps: HashMap<PathBuf, (Instant, Vec<(String, f32)>)>,
    nvidia_last: Option<(Instant, Vec<GpuDev>)>,
    /// The DRM clients in `/proc/<pid>/fdinfo`: Intel GPUs (i915, xe) and the Processes column
    /// (Intel and amdgpu).
    drm: super::drm_clients::DrmClients,
    procs: Enumerator,
}

/// A card's id: its DRM directory name (`card0`).
fn card_id(card: &Path) -> String {
    card.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        let cpu0 = Path::new(CPU_DIR).join("cpu0/cpufreq");
        let name = Some(cpu_brand()).filter(|b| !b.is_empty()).or_else(|| procfs::cpuinfo_name(&read("/proc/cpuinfo")?)).unwrap_or_default();
        Self {
            seq: 0,
            prev_sys: None,
            prev_cores: Vec::new(),
            prev_disk: None,
            prev_disk_stats: Vec::new(),
            disk_models: HashMap::new(),
            disk_spaces: Storage::default(),
            disk_spaces_at: None,
            prev_net: None,
            freq_max: read_u64(cpu0.join("cpuinfo_max_freq")),
            freq_base: read_u64(cpu0.join("base_frequency")).or_else(|| read_u64(cpu0.join("amd_pstate_nominal_freq"))),
            name,
            nvml: NvmlState::Untried,
            nvidia_pci: None,
            sleepy: HashMap::new(),
            sleepy_temps: HashMap::new(),
            nvidia_last: None,
            drm: super::drm_clients::DrmClients::new("/"),
            procs: Enumerator::new(),
        }
    }

    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, ..Default::default() };
        snap.uptime = read("/proc/uptime").and_then(|u| procfs::parse_uptime(&u)).unwrap_or_default();
        self.cpu(&mut snap);
        match read("/proc/meminfo").and_then(|m| procfs::parse_meminfo(&m)) {
            Some(m) => snap.memory = m,
            None => snap.errors.push("/proc/meminfo unreadable".into()),
        }
        self.disk(&mut snap);
        self.network(&mut snap);
        let drm = self.drm.read(Instant::now());
        let mut gpus = self.gpus();
        gpus.extend(drm.devs);
        snap.gpu = procfs::fold_gpus(&gpus);
        snap.gpu.note = super::drm_clients::note(&gpus).map(str::to_string);
        // A GPU driver's hwmon readings go to that GPU, matched by PCI device; the rest are zones.
        let card_devs: Vec<Option<PathBuf>> = snap.gpu.adapters.iter().map(|a| fs::canonicalize(Path::new("/sys/class/drm").join(&a.id).join("device")).ok()).collect();
        let chips = procfs::attach_gpu_hwmon(&mut snap.gpu.adapters, &card_devs, self.hwmon_chips());
        snap.thermal = thermal_fold(&chips);
        snap.thermal.gpu = gpu_thermal(&snap.gpu.adapters);
        snap.thermal.sources.push_str(&format!(" \u{b7} {}", gpu_sources(&snap.gpu.adapters)));
        snap.handle_count = read("/proc/sys/fs/file-nr").and_then(|f| procfs::parse_file_nr(&f));
        snap.thread_count = read("/proc/loadavg").and_then(|l| procfs::parse_loadavg_threads(&l)).unwrap_or(0);
        snap.process_count = fs::read_dir("/proc").map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())).count()).unwrap_or(0);
        // The GPU column: Intel and amdgpu clients (NVML gives no per-process figure here).
        let rows = match self.procs.collect(snap.cpu.logical_count.max(1), &drm.by_pid) {
            Ok((rows, _)) => rows,
            Err(e) => {
                snap.errors.push(e);
                Vec::new()
            }
        };
        (snap, rows)
    }

    fn cpu(&mut self, snap: &mut Snapshot) {
        snap.cpu.name = self.name.clone();
        let Some(stat) = read("/proc/stat") else {
            snap.cpu.logical_count = std::thread::available_parallelism().map_or(1, |n| n.get());
            snap.errors.push("/proc/stat unreadable".into());
            return;
        };
        let c = &mut snap.cpu;
        let (total, cores) = procfs::parse_stat(&stat);
        c.logical_count = if cores.is_empty() { std::thread::available_parallelism().map_or(1, |n| n.get()) } else { cores.len() };
        if let Some(cur) = total {
            if let Some((busy, kern)) = self.prev_sys.and_then(|p| tick_delta_pct(p, cur)) {
                c.total_pct = busy;
                c.kernel_pct = kern;
            }
            self.prev_sys = Some(cur);
        }
        // Per logical CPU, matched by number (CPUs can go offline between samples).
        let (mut pct, mut kpct) = (Vec::with_capacity(cores.len()), Vec::with_capacity(cores.len()));
        for (n, cur) in &cores {
            let d = self.prev_cores.iter().find(|p| p.0 == *n).and_then(|p| tick_delta_pct(p.1, *cur)).unwrap_or((0.0, 0.0));
            pct.push(d.0);
            kpct.push(d.1);
        }
        c.per_core_pct = pct;
        c.per_core_kernel_pct = kpct;
        // Clocks: cpufreq is live; /proc/cpuinfo only where there is no cpufreq driver (VMs, WSL).
        let ids: Vec<u32> = cores.iter().map(|c| c.0).collect();
        self.prev_cores = cores;
        let khz: Vec<Option<u64>> = ids.iter().map(|n| read_u64(format!("{CPU_DIR}/cpu{n}/cpufreq/scaling_cur_freq"))).collect();
        if khz.iter().any(Option::is_some) {
            c.per_core_mhz = khz.iter().map(|k| k.map_or(0, |k| (k / 1000) as u32)).collect();
            c.clock_live = true;
            if let Some(max) = read_u64(format!("{CPU_DIR}/cpu0/cpufreq/scaling_max_freq")) {
                c.diag.push(("scaling_max_freq MHz", (max / 1000) as f64));
            }
        } else if let Some(info) = read("/proc/cpuinfo") {
            let mhz = procfs::cpuinfo_mhz(&info);
            c.per_core_mhz = if mhz.is_empty() { Vec::new() } else { ids.iter().map(|n| mhz.iter().find(|m| m.0 == *n).map_or(0, |m| m.1)).collect() };
        }
        let known: Vec<u32> = c.per_core_mhz.iter().copied().filter(|m| *m > 0).collect();
        if !known.is_empty() {
            c.current_mhz = Some((known.iter().map(|m| *m as u64).sum::<u64>() / known.len() as u64) as u32);
        }
        if c.per_core_mhz.iter().all(|m| *m == 0) {
            c.per_core_mhz.clear();
        }
        c.max_mhz = self.freq_max.map(|k| (k / 1000) as u32).filter(|m| *m > 0);
        c.base_mhz = self.freq_base.map(|k| (k / 1000) as u32).filter(|m| *m > 0);
        c.boost_mhz = c.max_mhz.filter(|m| c.base_mhz.is_some_and(|b| *m > b));
    }

    fn disk(&mut self, snap: &mut Snapshot) {
        let Some(text) = read("/proc/diskstats") else { return };
        let block: HashSet<String> = fs::read_dir("/sys/block").map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        let stats = if block.is_empty() { procfs::parse_diskstats(&text, procfs::looks_whole) } else { procfs::parse_diskstats(&text, |n| block.contains(n)) };
        let now = Instant::now();
        let cur = DiskTotals::of(&stats);
        let dt = self.prev_disk.as_ref().map_or(Duration::ZERO, |p| now.duration_since(p.0));
        snap.disk = procfs::disk_delta(self.prev_disk.as_ref().map(|p| &p.1), &cur, dt);
        self.prev_disk = Some((now, cur));
        // Each disk on its own. A device name may pass to another disk once its own is gone.
        self.disk_models.retain(|dev, _| stats.iter().any(|d| d.name == *dev));
        let mut drives = procfs::disk_drives(&self.prev_disk_stats, &stats, dt);
        for d in &mut drives {
            if !self.disk_models.contains_key(&d.id) {
                let dir = Path::new("/sys/block").join(&d.id).join("device");
                let of = |file: &str| read(dir.join(file)).unwrap_or_default();
                self.disk_models.insert(d.id.clone(), (disk_model(&of("vendor"), &of("model")), firmware_rev(&dir)));
            }
            let (model, firmware) = &self.disk_models[&d.id];
            d.model.clone_from(model);
            d.firmware.clone_from(firmware);
        }
        if self.disk_spaces_at.is_none_or(|t| now.duration_since(t) >= DISK_SPACE_PERIOD) {
            self.disk_spaces = disk_spaces();
            self.disk_spaces_at = Some(now);
        }
        for d in &mut drives {
            d.space = self.disk_spaces.each.get(&d.id).copied();
        }
        snap.disk.space = self.disk_spaces.all;
        snap.disk.drives = drives;
        self.prev_disk_stats = stats;
    }

    fn network(&mut self, snap: &mut Snapshot) {
        let Ok(dir) = fs::read_dir("/sys/class/net") else { return };
        let ifs: Vec<NetIf> = dir
            .flatten()
            .map(|e| {
                let p = e.path();
                let stat = |f: &str| read_u64(p.join("statistics").join(f)).unwrap_or(0);
                NetIf {
                    name: e.file_name().to_string_lossy().into_owned(),
                    up: read(p.join("operstate")).is_some_and(|s| s.trim() == "up"),
                    hardware: p.join("device").exists(),
                    speed_mbps: read(p.join("speed")).and_then(|s| s.trim().parse().ok()),
                    rx_bytes: stat("rx_bytes"),
                    tx_bytes: stat("tx_bytes"),
                }
            })
            .collect();
        let (rx, tx, link) = procfs::net_totals(&ifs);
        let now = Instant::now();
        snap.network = net_delta(self.prev_net, now, rx, tx, link);
        self.prev_net = Some((now, rx, tx));
    }

    /// amdgpu cards from sysfs, then NVIDIA GPUs from NVML (loaded on first use). A hybrid
    /// laptop's discrete GPU must be able to power down: it is not touched while suspended, read
    /// every tick while at work, and read only every [`procfs::SLEEPY_GPU_PERIOD`] while awake but
    /// idle (each read restarts its idle timer), when NVML is not kept open for it either.
    fn gpus(&mut self) -> Vec<GpuDev> {
        let now = Instant::now();
        let idle = |name: &str| GpuDev { engines: vec![("Graphics".into(), 0.0)], name: name.into(), ..Default::default() };
        let mut devs = Vec::new();
        let cards = entries("/sys/class/drm", "card").into_iter().filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n[4..].bytes().all(|b| b.is_ascii_digit())));
        for card in cards {
            let d = card.join("device");
            // A stat of the attribute does not run the driver (and so does not wake the GPU).
            if !d.join("gpu_busy_percent").exists() {
                continue;
            }
            let since = self.sleepy.get(&card).map(|c| now.saturating_duration_since(c.0));
            let busy = self.sleepy.get(&card).is_some_and(|c| procfs::gpu_busy(&c.1));
            match procfs::gpu_poll(power_of(&d), since, busy) {
                procfs::GpuPoll::Idle => devs.push(GpuDev { id: card_id(&card), ..idle("AMD GPU") }),
                procfs::GpuPoll::Keep => devs.extend(self.sleepy.get(&card).map(|c| c.1.clone())),
                procfs::GpuPoll::Read => {
                    let Some(busy) = read_u64(d.join("gpu_busy_percent")) else { continue };
                    let name = read(d.join("product_name")).map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "AMD GPU".into());
                    let dev = GpuDev { engines: vec![("Graphics".into(), busy as f32)], vram_used: read_u64(d.join("mem_info_vram_used")), vram_total: read_u64(d.join("mem_info_vram_total")), shared_used: read_u64(d.join("mem_info_gtt_used")), shared_total: read_u64(d.join("mem_info_gtt_total")), id: card_id(&card), name, ..Default::default() };
                    self.sleepy.insert(card, (now, dev.clone()));
                    devs.push(dev);
                }
            }
        }
        let nvidia = self.nvidia_pci.get_or_insert_with(nvidia_pci_devices);
        let powers: Vec<procfs::GpuPower> = nvidia.iter().map(|d| power_of(d)).collect();
        if powers.iter().any(|p| p.may_sleep) {
            // Hybrid laptop: an open NVML keeps the GPU powered, so it stays open only while the
            // GPU is at work anyway (and is read every tick); an idle one is read seldom.
            let since = self.nvidia_last.as_ref().map(|c| now.saturating_duration_since(c.0));
            let busy = self.nvidia_last.as_ref().is_some_and(|c| c.1.iter().any(procfs::gpu_busy));
            let awake = procfs::GpuPower { suspended: powers.iter().all(|p| p.suspended), may_sleep: true };
            let open = std::mem::replace(&mut self.nvml, NvmlState::Untried);
            match procfs::gpu_poll(awake, since, busy) {
                procfs::GpuPoll::Idle => devs.extend(powers.iter().enumerate().map(|(i, _)| GpuDev { id: format!("nvidia{i}"), ..idle("NVIDIA GPU") })),
                procfs::GpuPoll::Keep => devs.extend(self.nvidia_last.iter().flat_map(|c| c.1.iter().cloned())),
                procfs::GpuPoll::Read => {
                    let nvml = if let NvmlState::Loaded(n) = open { Some(n) } else { Nvml::load() };
                    let read = nvml.as_ref().map(|n| n.read()).unwrap_or_default();
                    // Dropping the Nvml shuts it down again.
                    if let Some(n) = nvml.filter(|_| read.iter().any(procfs::gpu_busy)) {
                        self.nvml = NvmlState::Loaded(n);
                    }
                    devs.extend(read.iter().cloned());
                    self.nvidia_last = Some((now, read));
                }
            }
            return devs;
        }
        if matches!(self.nvml, NvmlState::Untried) {
            self.nvml = Nvml::load().map_or(NvmlState::Absent, NvmlState::Loaded);
        }
        if let NvmlState::Loaded(n) = &self.nvml {
            devs.extend(n.read());
        }
        devs
    }

    /// hwmon chips, each with its (resolved) device directory when it is a GPU's. A chip of a
    /// powered-down device is not read (that would wake it); one of a GPU that may sleep is read
    /// seldom.
    fn hwmon_chips(&mut self) -> Vec<(HwmonChip, Option<PathBuf>)> {
        let now = Instant::now();
        let mut chips = Vec::new();
        for h in entries("/sys/class/hwmon", "hwmon") {
            let Some(name) = read(h.join("name")).map(|n| n.trim().to_string()) else { continue };
            let dev = h.join("device");
            let gpu = read(dev.join("class")).is_some_and(|c| c.trim().starts_with("0x03"));
            let mut power = power_of(&dev);
            // Only a GPU is read seldom; a CPU sensor's PCI function may be `auto` under TLP.
            power.may_sleep &= gpu;
            let since = self.sleepy_temps.get(&h).map(|c| now.saturating_duration_since(c.0));
            let temps = match procfs::gpu_poll(power, since, false) {
                procfs::GpuPoll::Idle => Vec::new(),
                procfs::GpuPoll::Keep => self.sleepy_temps.get(&h).map(|c| c.1.clone()).unwrap_or_default(),
                procfs::GpuPoll::Read => {
                    let temps = hwmon_temps(&h);
                    if power.may_sleep {
                        self.sleepy_temps.insert(h.clone(), (now, temps.clone()));
                    }
                    temps
                }
            };
            let gpu_dev = if gpu { fs::canonicalize(&dev).ok() } else { None };
            chips.push((HwmonChip { name, temps }, gpu_dev));
        }
        chips
    }
}

/// Runtime power state of a (PCI) device.
fn power_of(dev: &Path) -> procfs::GpuPower {
    procfs::gpu_power(read(dev.join("power/runtime_status")).as_deref(), read(dev.join("power/control")).as_deref(), read(dev.join("boot_vga")).as_deref())
}

/// The NVIDIA display controllers (VGA or 3D) on the PCI bus.
fn nvidia_pci_devices() -> Vec<PathBuf> {
    entries("/sys/bus/pci/devices", "")
        .into_iter()
        .filter(|d| read(d.join("vendor")).is_some_and(|v| v.trim() == "0x10de") && read(d.join("class")).is_some_and(|c| c.trim().starts_with("0x03")))
        .collect()
}

/// (label, °C) of each temperature input of a hwmon chip.
fn hwmon_temps(h: &Path) -> Vec<(String, f32)> {
    let Ok(rd) = fs::read_dir(h) else { return Vec::new() };
    let mut inputs: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| is_temp_input(n)).collect();
    // temp2 before temp10.
    inputs.sort_by_key(|n| n.trim_start_matches("temp").trim_end_matches("_input").parse::<u32>().unwrap_or(u32::MAX));
    inputs
        .iter()
        .filter_map(|i| {
            let v = procfs::millideg(&read(h.join(i))?)?;
            let stem = i.trim_end_matches("_input");
            let label = read(h.join(format!("{stem}_label"))).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| stem.to_string());
            Some((label, v))
        })
        .collect()
}

/// The thermal zones with the hwmon `chips`, folded by [`procfs::fold_thermal`].
fn thermal_fold(chips: &[HwmonChip]) -> super::ThermalSample {
    let zones = entries("/sys/class/thermal", "thermal_zone")
        .iter()
        .filter_map(|z| {
            let v = procfs::millideg(&read(z.join("temp"))?)?;
            let kind = read(z.join("type")).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            Some((kind.unwrap_or_else(|| z.file_name().unwrap_or_default().to_string_lossy().into_owned()), v))
        })
        .collect();
    procfs::fold_thermal(chips, zones)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_revision_from_sysfs_files() {
        let root = std::env::temp_dir().join(format!("tmop-fw-{}", std::process::id()));
        let dev = |name: &str, files: &[(&str, &str)]| {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            for (f, text) in files {
                fs::write(dir.join(f), text).unwrap();
            }
            dir
        };
        assert_eq!(firmware_rev(&dev("sata", &[("rev", "3B6Q\n"), ("model", "x")])), "3B6Q");
        assert_eq!(firmware_rev(&dev("nvme", &[("firmware_rev", "4B2QJXD7\n")])), "4B2QJXD7");
        assert_eq!(firmware_rev(&dev("blank", &[("rev", " \n")])), "");
        assert_eq!(firmware_rev(&dev("none", &[])), "");
        assert_eq!(firmware_rev(&root.join("missing")), "");
        let _ = fs::remove_dir_all(&root);
    }

    /// A sysfs tree like the kernel's: two amdgpu cards and an NVMe drive, each with its hwmon
    /// chip; the cards' readings go to the card on the same PCI device, labelled, the drive's stay.
    #[test]
    fn gpu_hwmon_readings_go_to_their_card() {
        use super::super::{GpuAdapter, GpuTempKind};
        let root = std::env::temp_dir().join(format!("tmop-gpu-hwmon-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let pci = |slot: &str| root.join("devices/pci0000:00").join(slot);
        let hwmon = |dev: &Path, n: u32, name: &str, files: &[(&str, &str)]| {
            let h = dev.join(format!("hwmon/hwmon{n}"));
            fs::create_dir_all(&h).unwrap();
            fs::write(h.join("name"), format!("{name}\n")).unwrap();
            for (f, text) in files {
                fs::write(h.join(f), text).unwrap();
            }
            std::os::unix::fs::symlink(dev, h.join("device")).unwrap();
            h
        };
        let (gpu_a, gpu_b, nvme) = (pci("0000:03:00.0"), pci("0000:0a:00.0"), pci("0000:01:00.0"));
        let amd_files = [("temp1_input", "48000\n"), ("temp1_label", "edge\n"), ("temp2_input", "61000\n"), ("temp2_label", "junction\n"), ("temp3_input", "70000\n"), ("temp3_label", "mem\n")];
        let ha = hwmon(&gpu_a, 1, "amdgpu", &amd_files);
        let hb = hwmon(&gpu_b, 2, "amdgpu", &[("temp1_input", "39000\n"), ("temp1_label", "edge\n")]);
        let hn = hwmon(&nvme, 0, "nvme", &[("temp1_input", "41850\n"), ("temp1_label", "Composite\n")]);
        // hwmon_temps reads the tree in channel order with the kernel's labels.
        assert_eq!(hwmon_temps(&ha), [("edge".to_string(), 48.0), ("junction".to_string(), 61.0), ("mem".to_string(), 70.0)]);
        let chip = |h: &Path, gpu: bool| (HwmonChip { name: read(h.join("name")).unwrap().trim().to_string(), temps: hwmon_temps(h) }, gpu.then(|| fs::canonicalize(h.join("device")).unwrap()));
        let chips = vec![chip(&hn, false), chip(&ha, true), chip(&hb, true)];
        // Two cards of one model, listed in the other order: matched by device, not by position.
        let card = |id: &str| GpuAdapter { id: id.into(), name: "AMD Radeon RX 7900 XTX".into(), ..Default::default() };
        let mut adapters = [card("card1"), card("card0")];
        let devs = [Some(fs::canonicalize(&gpu_b).unwrap()), Some(fs::canonicalize(&gpu_a).unwrap())];
        let rest = procfs::attach_gpu_hwmon(&mut adapters, &devs, chips);
        assert_eq!(rest.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["nvme"]);
        let temps = |a: &GpuAdapter| a.temps.iter().map(|t| (t.kind, t.c, t.source)).collect::<Vec<_>>();
        assert_eq!(temps(&adapters[0]), [(GpuTempKind::Gpu, 39.0, "hwmon amdgpu")]);
        assert_eq!(temps(&adapters[1]), [(GpuTempKind::Gpu, 48.0, "hwmon amdgpu"), (GpuTempKind::HotSpot, 61.0, "hwmon amdgpu"), (GpuTempKind::Memory, 70.0, "hwmon amdgpu")]);
        assert_eq!(adapters[1].temperature_c, Some(48.0));
        let _ = fs::remove_dir_all(&root);
    }

    /// Runs on the Linux CI runners: the file system at `/` is traced to whole disks of
    /// `/sys/block`, and counts on its disk, or on none when it spans several.
    #[test]
    fn the_root_file_system_counts_on_its_disk() {
        let mounts = crate::pro::mounts::parse_mountinfo(&read("/proc/self/mountinfo").unwrap());
        // In a container `/` is an overlay, on no block device.
        let Some(root) = procfs::block_mounts(&mounts).into_iter().find(|m| m.point == "/") else { return };
        let dev = block_name(root).unwrap_or_else(|| panic!("no block device for {root:?}"));
        let disks = procfs::physical_disks(whole_disks(&dev, 0));
        let spaces = disk_spaces();
        eprintln!("{} is {dev} on {disks:?}: {spaces:?}", root.source);
        assert!(!disks.is_empty() && disks.iter().all(|d| Path::new("/sys/block").join(d).exists()), "{disks:?}");
        let (used, total) = spaces.all.expect("no space");
        assert!(total > 0 && used <= total, "{used} / {total}");
        // On one disk it counts there; on several (RAID, LVM) on none of them.
        for d in &disks {
            assert_eq!(spaces.each.contains_key(d), disks.len() == 1, "{d}");
        }
    }
}
