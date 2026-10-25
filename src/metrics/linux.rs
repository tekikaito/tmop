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
        snap.thermal = thermal();
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

    /// amdgpu cards from sysfs, then NVIDIA GPUs from NVML (loaded on first use).
    fn gpus(&mut self) -> Vec<GpuDev> {
        let mut devs: Vec<GpuDev> = entries("/sys/class/drm", "card")
            .into_iter()
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n[4..].bytes().all(|b| b.is_ascii_digit())))
            .filter_map(|p| {
                let d = p.join("device");
                let busy = read_u64(d.join("gpu_busy_percent"))?;
                Some(GpuDev { engines: vec![("Graphics".into(), busy as f32)], vram_used: read_u64(d.join("mem_info_vram_used")) })
            })
            .collect();
        if matches!(self.nvml, NvmlState::Untried) {
            self.nvml = Nvml::load().map_or(NvmlState::Absent, NvmlState::Loaded);
        }
        if let NvmlState::Loaded(n) = &self.nvml {
            devs.extend(n.read());
        }
        devs
    }
}

/// hwmon chips and thermal zones, folded by [`procfs::fold_thermal`].
fn thermal() -> super::ThermalSample {
    let chips: Vec<HwmonChip> = entries("/sys/class/hwmon", "hwmon")
        .iter()
        .filter_map(|h| {
            let name = read(h.join("name"))?.trim().to_string();
            let mut inputs: Vec<String> = fs::read_dir(h).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| is_temp_input(n)).collect();
            // temp2 before temp10.
            inputs.sort_by_key(|n| n.trim_start_matches("temp").trim_end_matches("_input").parse::<u32>().unwrap_or(u32::MAX));
            let temps = inputs
                .iter()
                .filter_map(|i| {
                    let v = procfs::millideg(&read(h.join(i))?)?;
                    let stem = i.trim_end_matches("_input");
                    let label = read(h.join(format!("{stem}_label"))).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| stem.to_string());
                    Some((label, v))
                })
                .collect();
            Some(HwmonChip { name, temps })
        })
        .collect();
    let zones = entries("/sys/class/thermal", "thermal_zone")
        .iter()
        .filter_map(|z| {
            let v = procfs::millideg(&read(z.join("temp"))?)?;
            let kind = read(z.join("type")).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
            Some((kind.unwrap_or_else(|| z.file_name().unwrap_or_default().to_string_lossy().into_owned()), v))
        })
        .collect();
    procfs::fold_thermal(&chips, zones)
}
