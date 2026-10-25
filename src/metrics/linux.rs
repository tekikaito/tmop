//! Linux metric collection: plain `std::fs` reads of `/proc` and `/sys`, parsed by the pure
//! functions in [`super::procfs`].
//!
//! * CPU: `/proc/stat` (`cpu`, `cpuN`); clocks from cpufreq (`scaling_cur_freq`, `cpuinfo_max_freq`,
//!   `base_frequency` / `amd_pstate_nominal_freq`), else `/proc/cpuinfo` "cpu MHz" (static).
//! * Memory: `/proc/meminfo`. Disk: `/proc/diskstats`, whole physical disks (`/sys/block`).
//! * Network: `/sys/class/net/<if>/statistics`, up interfaces backed by a device.
//! * GPU: amdgpu sysfs (`gpu_busy_percent`, `mem_info_vram_used`) and NVIDIA NVML (`dlopen`).
//! * Thermal: hwmon (`k10temp`, `zenpower`, `coretemp` for the CPU) and `thermal_zone*`.
//! * Uptime `/proc/uptime`, open files `/proc/sys/fs/file-nr`, threads `/proc/loadavg`.
//!
//! Process rows come from the Linux process enumerator ([`crate::process::Enumerator`]); the
//! system-wide counts above stay the snapshot's process, thread and open-file totals.

use super::common::{cpu_brand, net_delta, tick_delta_pct, NetMark, Ticks};
use super::nvml::Nvml;
use super::procfs::{self, DiskTotals, GpuDev, HwmonChip, NetIf};
use super::{Probe, Snapshot, Sources};
use crate::process::{Enumerator, ProcessRow};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SOURCES: Sources = Sources {
    disk: "Active time (all physical disks, /proc/diskstats)",
    network: "Throughput (all adapters, sysfs)",
    gpu: "Busiest engine of the busiest GPU (amdgpu sysfs, NVIDIA NVML)",
    no_gpu: "No GPU utilization was reported (amdgpu and NVIDIA GPUs are supported).",
    thermal: "hwmon sensors and thermal zones",
    thermal_hint: "Load the k10temp or coretemp module (lm-sensors) for a CPU temperature",
    sensor: "hwmon",
    handles: "Open files",
};

const CPU_DIR: &str = "/sys/devices/system/cpu";

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
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
    procs: Enumerator,
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
            prev_net: None,
            freq_max: read_u64(cpu0.join("cpuinfo_max_freq")),
            freq_base: read_u64(cpu0.join("base_frequency")).or_else(|| read_u64(cpu0.join("amd_pstate_nominal_freq"))),
            name,
            nvml: NvmlState::Untried,
            nvidia_pci: None,
            sleepy: HashMap::new(),
            sleepy_temps: HashMap::new(),
            nvidia_last: None,
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
        snap.gpu = procfs::fold_gpus(&self.gpus());
        snap.thermal = self.thermal();
        snap.handle_count = read("/proc/sys/fs/file-nr").and_then(|f| procfs::parse_file_nr(&f));
        snap.thread_count = read("/proc/loadavg").and_then(|l| procfs::parse_loadavg_threads(&l)).unwrap_or(0);
        snap.process_count = fs::read_dir("/proc").map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())).count()).unwrap_or(0);
        // No per-process GPU source yet (DRM fdinfo), so the GPU column stays 0.
        let rows = match self.procs.collect(snap.cpu.logical_count.max(1), &HashMap::new()) {
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
        snap.disk = procfs::disk_delta(self.prev_disk.as_ref().map(|p| &p.1), &cur, self.prev_disk.as_ref().map_or(Duration::ZERO, |p| now.duration_since(p.0)));
        self.prev_disk = Some((now, cur));
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
        let idle = || GpuDev { engines: vec![("Graphics".into(), 0.0)], vram_used: None };
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
                procfs::GpuPoll::Idle => devs.push(idle()),
                procfs::GpuPoll::Keep => devs.extend(self.sleepy.get(&card).map(|c| c.1.clone())),
                procfs::GpuPoll::Read => {
                    let Some(busy) = read_u64(d.join("gpu_busy_percent")) else { continue };
                    let dev = GpuDev { engines: vec![("Graphics".into(), busy as f32)], vram_used: read_u64(d.join("mem_info_vram_used")) };
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
                procfs::GpuPoll::Idle => devs.extend(powers.iter().map(|_| idle())),
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

    /// hwmon chips and thermal zones, folded by [`procfs::fold_thermal`]. A chip of a powered-down
    /// device is not read (that would wake it); one of a GPU that may sleep is read seldom.
    fn thermal(&mut self) -> super::ThermalSample {
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
            chips.push(HwmonChip { name, temps });
        }
        thermal_fold(&chips)
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
