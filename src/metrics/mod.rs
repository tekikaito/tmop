//! System metrics: snapshot types, history buffers and the background sampler.
//!
//! One backend per target, chosen at compile time like [`crate::platform`]: `win.rs`,
//! `linux.rs`, `macos.rs` and `stub.rs` for anything else. Each provides the same items:
//! * `Collector` with `new()` and `collect(&mut self) -> (Snapshot, Vec<ProcessRow>)`;
//! * `probe() -> Probe`, what this machine reports (narrows [`crate::platform::Caps`] once);
//! * `SOURCES: Sources`, how the UI names the backend's sources and gaps.
//!
//! Shared, pure helpers (CPUID brand, tick deltas, network rates, GPU fold, CPU temperature
//! labels) live in [`common`]. A value an OS cannot provide is `None` / `available: false` in the
//! snapshot, never 0, and the UI hides it.
//!
//! Windows sources:
//! * CPU total/kernel: `GetSystemTimes`; per logical CPU: `NtQuerySystemInformation`
//!   (`SystemProcessorPerformanceInformation`); clock: `CallNtPowerInformation(ProcessorInformation)`.
//! * Memory: `GlobalMemoryStatusEx`.
//! * Disk: PDH `\PhysicalDisk(_Total)\...` (aggregated over all disks) and `\PhysicalDisk(*)\...`
//!   (each disk); the disks' own counters while PDH's are missing or stay at zero.
//! * Network: `GetIfTable2` octet deltas.
//! * GPU: PDH `\GPU Engine(*)\Utilization Percentage` and `\GPU Adapter Memory(*)\Dedicated Usage`.
//!   GPU temperature: NVML while it is open, else the driver's report to Windows
//!   (`D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` by LUID; on a machine with a battery
//!   an idle GPU driving no display is not asked, see `common::wddm_poll`); hot spot and memory
//!   temperatures from HWiNFO's shared memory when it runs, matched to a GPU by name.
//! * Thermal: PDH `\Thermal Zone Information(*)\Temperature` (ACPI firmware zones, Kelvin); CPU
//!   temperature from the PawnIO driver when switched on in Settings ([`pawnio`]), else from
//!   HWiNFO's shared memory ([`hwinfo`]).
//! * Processes: `NtQuerySystemInformation(SystemProcessInformation)`.

pub mod common;
pub mod cost;
pub mod drm_fdinfo;
pub mod hwinfo;
#[cfg(target_os = "linux")]
mod drm_clients;
pub mod pawnio;
pub mod procfs;
#[cfg(any(windows, target_os = "linux"))]
mod nvml;
pub mod mac_parse;

#[cfg_attr(windows, path = "win.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), path = "stub.rs")]
mod imp;

pub use imp::{probe, Collector, SOURCES};
#[cfg(windows)]
pub use imp::query_processor_performance;

use crate::process::ProcessRow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct CpuSample {
    pub total_pct: f32,
    pub kernel_pct: f32,
    pub per_core_pct: Vec<f32>,
    /// Kernel share of each logical processor (%).
    pub per_core_kernel_pct: Vec<f32>,
    /// Average clock (live when `clock_live`, else the static base). None where the OS reports
    /// no clocks (Apple Silicon).
    pub current_mhz: Option<u32>,
    /// Highest clock a core may reach, as the OS reports it.
    pub max_mhz: Option<u32>,
    /// Base clock (Windows: registry `~MHz`, cross-checked with PDH; see `clock::clock_base`).
    pub base_mhz: Option<u32>,
    /// The boost clock: the maximum when it exceeds the base.
    pub boost_mhz: Option<u32>,
    /// Windows: `MhzLimit` from the power API (max over cores). None elsewhere.
    pub mhz_limit: Option<u32>,
    /// Windows: raw `\Processor Information(_Total)\% Processor Performance`. None elsewhere.
    pub perf_pct: Option<f64>,
    /// Windows: raw `\Processor Information(_Total)\Processor Frequency`. None elsewhere.
    pub pdh_freq_mhz: Option<u32>,
    /// Other backends' clock diagnostics for `--dump`, as (label, value).
    pub diag: Vec<(&'static str, f64)>,
    /// True when `current_mhz` came from a live reading; false = static base clock.
    pub clock_live: bool,
    /// Logical processors; at least 1 in every published snapshot.
    pub logical_count: usize,
    /// Current clock of each logical processor (MHz), when available.
    pub per_core_mhz: Vec<u32>,
    /// Brand string ("AMD Ryzen 7 ..."), empty when unknown.
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct MemorySample {
    pub total: u64,
    pub available: u64,
    pub used_pct: f32,
    /// Committed memory and its limit (Windows commit charge, Linux `Committed_AS` /
    /// `CommitLimit`). None where the OS has no such figure (macOS).
    pub commit_used: Option<u64>,
    pub commit_limit: Option<u64>,
    /// File cache.
    pub cached: u64,
    /// Size of the page file (Windows) or swap, when known.
    pub swap_total: Option<u64>,
    /// Page file / swap bytes in use, when known.
    pub swap_used: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    /// The disk counters were read; false = unsupported or failed (not idle).
    pub available: bool,
    pub active_pct: f32,
    pub read_bps: f64,
    pub write_bps: f64,
    /// Used and total bytes of all the mounted file systems / volumes, each counted once (one that
    /// spans disks is not counted per disk). `None` when none can be asked.
    pub space: Option<(u64, u64)>,
    /// Each physical disk on its own; the fields above stay the grouped view.
    pub drives: Vec<DiskDrive>,
}

/// One physical disk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskDrive {
    /// Stable across samples (Windows: the disk number, Linux: the device name).
    pub id: String,
    /// "Disk 0" (Windows, macOS) or the device ("nvme0n1").
    pub name: String,
    /// The drive letters on it (Windows: "C: D:"); empty elsewhere and for a disk without one.
    pub volumes: String,
    /// The name with its volumes, as Windows Task Manager writes it: "Disk 0 (C: D:)". Without
    /// volumes the name.
    pub title: String,
    /// Empty when the system names none.
    pub model: String,
    /// The firmware revision the disk reports; empty when the system gives none.
    pub firmware: String,
    /// Used and total bytes of the mounted volumes lying wholly on this disk (Windows: those with a
    /// drive letter): their sum, not the size of the disk. A volume shared with other disks (RAID,
    /// LVM, spanned or mirrored) is left out, as its size is not this disk's. `None` when it has
    /// none or they cannot be asked.
    pub space: Option<(u64, u64)>,
    pub active_pct: f32,
    pub read_bps: f64,
    pub write_bps: f64,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSample {
    /// The interface counters were read; false = unsupported or failed (not idle).
    pub available: bool,
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// Sum of link speeds of up hardware interfaces (bits/s); 0 when unknown.
    pub link_bps: f64,
    pub utilization_pct: f32,
}

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub available: bool,
    /// Busiest engine type of the busiest adapter, like Windows Task Manager.
    pub utilization_pct: f32,
    pub dedicated_used: u64,
    /// The GPUs' dedicated memory added up, when a size is known.
    pub dedicated_total: Option<u64>,
    /// Shared (system) memory the GPUs use, added up.
    pub shared_used: Option<u64>,
    /// How much system memory a GPU may share: the largest GPU's limit, as they draw on the same
    /// RAM.
    pub shared_total: Option<u64>,
    /// (engine type, %) for the busiest adapter.
    pub engines: Vec<(String, f32)>,
    /// Each GPU on its own; the fields above stay the grouped view.
    pub adapters: Vec<GpuAdapter>,
    /// What each process uses of each GPU (Windows, while [`gpu_detail`] is on); a process on two
    /// GPUs has an entry for each.
    pub processes: Vec<GpuProcessUsage>,
    /// One plain sentence about what the figures leave out, when they do (Linux Intel GPUs:
    /// other users' programs). Shown on the Performance GPU card and by `--dump` only.
    pub note: Option<String>,
}

/// One GPU. A figure its source does not report is `None`, never 0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuAdapter {
    /// Stable across samples (Windows: the LUID).
    pub id: String,
    pub name: String,
    /// Busiest engine type.
    pub utilization_pct: f32,
    /// (engine type, %).
    pub engines: Vec<(String, f32)>,
    pub dedicated_used: Option<u64>,
    pub dedicated_total: Option<u64>,
    pub shared_used: Option<u64>,
    /// System memory this GPU may use (Windows: DXGI's shared system memory, amdgpu: GTT).
    pub shared_total: Option<u64>,
    /// The GPU temperature ([`GpuTempKind::Gpu`] of [`GpuAdapter::temps`]), kept for readers that
    /// want the one figure.
    pub temperature_c: Option<f32>,
    /// Each temperature a source reports for this GPU, one per kind; none is a 0 °C placeholder.
    pub temps: Vec<GpuTemp>,
    pub power_w: Option<f32>,
    pub clock_mhz: Option<u32>,
    /// Its figures cover only the processes TMOP could read ([`GpuSample::note`] says so).
    pub partial: bool,
}

impl GpuAdapter {
    /// The reading of `kind`, when a source reports one.
    pub fn temp(&self, kind: GpuTempKind) -> Option<&GpuTemp> {
        self.temps.iter().find(|t| t.kind == kind)
    }

    /// Sets the reading of `kind` unless one is already there: sources are offered best first.
    pub fn offer_temp(&mut self, kind: GpuTempKind, c: f32, source: &'static str) {
        if self.temp(kind).is_none() && c.is_finite() {
            self.temps.push(GpuTemp { kind, c, source });
            self.temps.sort_by_key(|t| t.kind);
            if kind == GpuTempKind::Gpu {
                self.temperature_c = Some(c);
            }
        }
    }
}

/// What a GPU temperature reading measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GpuTempKind {
    /// The GPU's main sensor: what the driver reports as the GPU temperature (amdgpu "edge",
    /// NVML's GPU die sensor, WDDM's main sensor).
    Gpu,
    /// The hottest point of the die (AMD "junction", HWiNFO "GPU Hot Spot Temperature").
    HotSpot,
    /// The graphics memory (amdgpu "mem", Intel xe "vram", HWiNFO "GPU Memory Temperature").
    Memory,
    /// The graphics memory's junction (HWiNFO "GPU Memory Junction Temperature", GDDR6X).
    MemoryJunction,
}

impl GpuTempKind {
    pub fn label(self) -> &'static str {
        match self {
            GpuTempKind::Gpu => "GPU",
            GpuTempKind::HotSpot => "Hot spot",
            GpuTempKind::Memory => "Memory",
            GpuTempKind::MemoryJunction => "Memory junction",
        }
    }
}

/// One temperature of one GPU: what it measures, °C, and where it came from ("WDDM", "NVML",
/// "HWiNFO", "hwmon amdgpu").
#[derive(Clone, Debug, PartialEq)]
pub struct GpuTemp {
    pub kind: GpuTempKind,
    pub c: f32,
    pub source: &'static str,
}

/// One process on one GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuProcessUsage {
    pub pid: u32,
    /// [`GpuAdapter::id`].
    pub gpu_id: String,
    /// Busiest engine type: engines run side by side, so they are not added up.
    pub utilization_pct: f32,
    /// (engine type, %), busiest first.
    pub engines: Vec<(String, f32)>,
    pub dedicated: Option<u64>,
    pub shared: Option<u64>,
}

static GPU_DETAIL: AtomicBool = AtomicBool::new(false);

/// Asks the sampler for what only the separated GPUs and the activity view show: per-process
/// memory counters and NVIDIA sensors. Off by default, as opening NVML keeps a hybrid laptop's
/// discrete GPU powered.
pub fn set_gpu_detail(on: bool) {
    GPU_DETAIL.store(on, Ordering::Relaxed);
}

pub fn gpu_detail() -> bool {
    GPU_DETAIL.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Default)]
pub struct ThermalSample {
    /// (zone name, °C)
    pub zones: Vec<(String, f32)>,
    /// CPU temperature (PawnIO or HWiNFO shared memory on Windows, hwmon on Linux): (reading label, °C).
    pub cpu: Option<(String, f32)>,
    /// Where [`ThermalSample::cpu`] comes from: "PawnIO", "HWiNFO", "hwmon k10temp".
    pub cpu_source: Option<String>,
    /// The GPUs' temperatures ([`GpuAdapter::temps`]).
    pub gpu: Vec<GpuThermal>,
    /// Which sources were checked, e.g. "ACPI: no zones · HWiNFO: not running".
    pub sources: String,
}

/// One GPU temperature in the thermal sample.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuThermal {
    /// "<GPU name> · <reading> (<source>)", for a list of all readings.
    pub label: String,
    pub kind: GpuTempKind,
    pub c: f32,
    pub source: &'static str,
}

/// The reading behind [`ThermalSample::max_c`]: what it is ("CPU", "GPU" or the zone's name),
/// °C and its source ("PawnIO", "WDDM", "ACPI zone" ...).
#[derive(Clone, Debug, PartialEq)]
pub struct Hottest {
    pub what: String,
    pub c: f32,
    pub source: String,
}

impl ThermalSample {
    /// The hottest of the CPU, board and GPU-core readings: the Thermals headline, its history,
    /// the popout tile, the recorder and the temperature alert. A GPU's hot spot and memory
    /// readings are left out: they run well above the core in normal use (a GDDR6X memory
    /// junction near 90-100 °C), so they would set off a limit meant for a part that is too hot.
    pub fn max_c(&self) -> Option<f32> {
        self.hottest().map(|h| h.c)
    }

    /// What [`ThermalSample::max_c`] is, named.
    pub fn hottest(&self) -> Option<Hottest> {
        let cpu = self.cpu.iter().map(|c| ("CPU".to_string(), c.1, self.cpu_source.clone().unwrap_or_else(|| SOURCES.sensor.to_string())));
        let gpu = self.gpu.iter().filter(|g| g.kind == GpuTempKind::Gpu).map(|g| ("GPU".to_string(), g.c, g.source.to_string()));
        let zones = self.zones.iter().map(|z| (z.0.clone(), z.1, SOURCES.sensor.to_string()));
        // The first of equals: CPU, then GPU, then the zones.
        cpu.chain(gpu).chain(zones).fold(None::<Hottest>, |best, (what, c, source)| match best {
            Some(b) if b.c >= c => Some(b),
            _ => Some(Hottest { what, c, source }),
        })
    }

    /// The hottest reading of the CPU and the board (thermal zones, other hwmon chips), GPUs left
    /// out: what a CPU readout falls back to when there is no CPU temperature.
    pub fn board_max_c(&self) -> Option<f32> {
        self.zones.iter().map(|z| z.1).chain(self.cpu.as_ref().map(|c| c.1)).fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
    }
}

/// Windows' hint where no temperature source is available (see [`Sources::thermal_hint`]).
pub const WINDOWS_THERMAL_HINT: &str =
    "For CPU temperature, see PawnIO in Settings \u{2192} General, or HWiNFO's Shared Memory Support";

/// How the UI names a backend's sources and gaps; each backend exports one as `SOURCES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sources {
    /// Disk card subtitle.
    pub disk: &'static str,
    /// Network card subtitle.
    pub network: &'static str,
    /// GPU card subtitle.
    pub gpu: &'static str,
    /// GPU card note when no GPU reading exists.
    pub no_gpu: &'static str,
    /// Thermal card subtitle while the snapshot names no sources.
    pub thermal: &'static str,
    /// What to do when no temperature is available (no trailing period).
    pub thermal_hint: &'static str,
    /// Short name of the sensor behind the hottest reading (popout).
    pub sensor: &'static str,
    /// What `Snapshot::handle_count` counts: "Handles" or "Open files".
    pub handles: &'static str,
}

/// What this machine's metrics backend reports; [`crate::platform::Caps::get`] narrows its
/// constant `clocks` and `thermal` flags with it once at start-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Clock speeds (a base or a current clock).
    pub clocks: bool,
    /// A temperature source, or at least one worth a hint.
    pub thermal: bool,
}

/// Pure CPU clock conversions.
pub mod clock {
    /// MHz from a `% Processor Performance` reading (share of base clock; above 100 while boosting).
    pub fn perf_mhz(base_mhz: u32, pct: f64) -> Option<u32> {
        if base_mhz == 0 || !pct.is_finite() || pct <= 0.0 {
            return None;
        }
        Some((base_mhz as f64 * pct.min(1000.0) / 100.0).round() as u32)
    }

    /// The multiplier for `% Processor Performance`: registry base, else PDH Processor Frequency,
    /// else MhzLimit (when 0 < limit <= max), else MaxMhz. When registry and PDH disagree by
    /// more than 10 % the smaller wins.
    pub fn clock_base(reg_mhz: Option<u32>, pdh_freq: Option<u32>, limit: Option<u32>, max: Option<u32>) -> Option<u32> {
        let reg = reg_mhz.filter(|m| *m > 0);
        let pdh = pdh_freq.filter(|m| *m > 0);
        match (reg, pdh) {
            (Some(a), Some(b)) => {
                let (lo, hi) = (a.min(b), a.max(b));
                Some(if hi as f64 > lo as f64 * 1.1 { lo } else { a })
            }
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => limit.filter(|l| *l > 0 && max.is_none_or(|m| *l <= m)).or(max.filter(|m| *m > 0)),
        }
    }

    /// Live MHz = base * pct / 100 (pct clamped to 10x). None without a base or a positive pct.
    pub fn clock_mhz(base: Option<u32>, pct: Option<f64>) -> Option<u32> {
        perf_mhz(base?, pct?)
    }

    /// Meter ceiling: the boost clock when known (above base), otherwise base * 1.25.
    pub fn clock_ceiling(base: Option<u32>, boost: Option<u32>) -> Option<f32> {
        let b = base.filter(|b| *b > 0)?;
        Some(match boost {
            Some(x) if x > b => x as f32,
            _ => b as f32 * 1.25,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub seq: u64,
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub disk: DiskSample,
    pub network: NetworkSample,
    pub gpu: GpuSample,
    pub thermal: ThermalSample,
    pub uptime: Duration,
    pub process_count: usize,
    pub thread_count: usize,
    /// Open handles (Windows) or open files; None when the OS has no system-wide count.
    pub handle_count: Option<usize>,
    pub errors: Vec<String>,
}

impl Snapshot {
    /// Whether disk figures should show. The placeholder before the first sample counts as
    /// known, so readouts do not jump when it arrives.
    pub fn disk_known(&self) -> bool {
        self.seq == 0 || self.disk.available
    }

    /// Whether network figures should show; see [`Snapshot::disk_known`].
    pub fn net_known(&self) -> bool {
        self.seq == 0 || self.network.available
    }
}

/// Ring buffer of recent values for graphs.
#[derive(Clone, Debug)]
pub struct Series {
    pub values: VecDeque<f32>,
    pub capacity: usize,
    /// Total number of values ever pushed. The newest value has absolute index `pushed - 1`;
    /// graphs anchor their time grid to it so grid lines don't flicker as samples arrive.
    pub pushed: u64,
}

impl Series {
    pub fn new(capacity: usize) -> Self {
        Self { values: VecDeque::with_capacity(capacity), capacity: capacity.max(2), pushed: 0 }
    }
    pub fn push(&mut self, v: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(v);
        self.pushed += 1;
    }
    pub fn last(&self) -> f32 {
        self.values.back().copied().unwrap_or(0.0)
    }
    pub fn max(&self) -> f32 {
        self.values.iter().copied().fold(0.0, f32::max)
    }
    pub fn resize(&mut self, capacity: usize) {
        self.capacity = capacity.max(2);
        while self.values.len() > self.capacity {
            self.values.pop_front();
        }
    }
}

/// The history of one disk.
#[derive(Clone, Debug)]
pub struct DiskSeries {
    pub active: Series,
    pub read: Series,
    pub write: Series,
}

#[derive(Clone, Debug)]
pub struct History {
    pub cpu: Series,
    pub kernel: Series,
    pub per_core: Vec<Series>,
    pub per_core_kernel: Vec<Series>,
    pub memory: Series,
    pub disk: Series,
    pub disk_read: Series,
    pub disk_write: Series,
    /// Active time and rates of each disk, keyed by [`DiskDrive::id`].
    pub disk_drives: Vec<(String, DiskSeries)>,
    pub net_rx: Series,
    pub net_tx: Series,
    pub gpu: Series,
    /// Utilisation of each GPU, keyed by [`GpuAdapter::id`].
    pub gpu_adapters: Vec<(String, Series)>,
    pub thermal: Series,
    /// Average core clock in MHz.
    pub clock: Series,
    /// Fastest core clock in MHz (0 where the OS reports no per-core clocks).
    pub clock_max: Series,
    /// Used physical memory in bytes.
    pub memory_bytes: Series,
    /// When each sample was taken, oldest first (#201). Every series above is pushed once per
    /// sample, so the newest value of each lines up with the newest stamp, the one before with
    /// the stamp before, and so on; a series that started later is just shorter.
    pub times: VecDeque<Stamp>,
    /// The sampling interval of the newest sample taken while not paused, in seconds (0 before
    /// the first). Graphs count their px-per-update scale in it.
    pub step: f32,
    /// The interval in force at the newest sample (`None`: paused).
    last_interval: Option<Duration>,
}

/// When a sample of a [`History`] was taken: seconds on the sampler's monotonic clock, and
/// whether a gap (pause, sleep, a stalled sampler) lies between it and the sample before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    pub t: f64,
    pub gap: bool,
}

/// Slack on top of twice the interval before a pause between samples counts as a gap: a busy
/// machine can delay a sample by a few hundred ms without breaking the trace.
const GAP_MARGIN: f64 = 0.25;

/// Whether `dt` seconds between two samples is a gap rather than the sampling rate: more than
/// twice the slower of the intervals in force at the two samples (plus [`GAP_MARGIN`]). After a
/// change of interval the slower one is what the time between them may legitimately be; two
/// samples taken while paused (refreshes) always have a gap between them.
pub fn is_gap(dt: f64, before: Option<Duration>, now: Option<Duration>) -> bool {
    // `None` orders below any `Some`: the slower interval of those known.
    match before.max(now) {
        Some(iv) => dt > 2.0 * iv.as_secs_f64() + GAP_MARGIN,
        None => true,
    }
}

/// Where the samples of a [`History`] sit in time, for graphs (#201). Ages are counted back
/// from the newest sample, in seconds or in updates of [`History::step`] seconds (fractional),
/// the unit graph scales use. Evenly spaced samples have ages 0, 1, 2, ... updates.
#[derive(Clone, Copy, Debug)]
pub struct Clock<'a> {
    times: &'a VecDeque<Stamp>,
    step: f64,
    /// `1 / step`, so placing a point costs a multiplication.
    per_sec: f64,
}

impl<'a> Clock<'a> {
    /// A clock over `times` counting updates of `step` seconds; `None` without either.
    pub fn new(times: &'a VecDeque<Stamp>, step: f32) -> Option<Self> {
        (step > 0.0 && !times.is_empty()).then(|| Self { times, step: step as f64, per_sec: 1.0 / step as f64 })
    }

    /// Whether a series of `n` values pushed along with these stamps can be placed by them. A
    /// series filled some other way (a preview) can be longer and keeps the evenly spaced layout.
    pub fn covers(&self, n: usize) -> bool {
        n <= self.times.len()
    }

    /// Seconds per update.
    pub fn step(&self) -> f32 {
        self.step as f32
    }

    fn newest(&self) -> f64 {
        self.times.back().map_or(0.0, |s| s.t)
    }

    /// Seconds from the sample `age` samples back to the newest one (the oldest beyond).
    pub fn secs(&self, age: usize) -> f64 {
        let i = self.times.len().saturating_sub(1 + age);
        self.newest() - self.times.get(i).map_or(0.0, |s| s.t)
    }

    /// The same in updates.
    pub fn age(&self, age: usize) -> f32 {
        (self.secs(age) * self.per_sec) as f32
    }

    /// Newest first: each sample's age in updates, and whether a gap lies between it and the
    /// next older sample (where the trace breaks).
    pub fn ages(&self) -> impl Iterator<Item = (f32, bool)> + 'a {
        let (newest, per_sec) = (self.newest(), self.per_sec);
        self.times.iter().rev().map(move |s| (((newest - s.t) * per_sec) as f32, s.gap))
    }

    /// Index of the stamp at or before which a time `u` updates before the newest falls.
    fn split(&self, u: f32) -> usize {
        let target = self.newest() - u as f64 * self.step;
        self.times.partition_point(|s| s.t < target)
    }

    /// The sample (age in samples) of a series of `n` values nearest to `u` updates before the
    /// newest; `None` more than half an update beyond its oldest, as an evenly spaced graph has
    /// no sample there either.
    pub fn nearest(&self, u: f32, n: usize) -> Option<usize> {
        let n = n.min(self.times.len());
        if n == 0 || u >= self.age(n - 1) + 0.5 {
            return None;
        }
        let len = self.times.len();
        let first = len - n;
        let i = self.split(u).clamp(first, len - 1);
        let target = self.newest() - u as f64 * self.step;
        // The older neighbour wins a tie, as `round` sends x.5 to the older sample.
        let i = if i > first && target - self.times[i - 1].t <= self.times[i].t - target { i - 1 } else { i };
        Some(len - 1 - i)
    }

    /// Seconds a graph of `n` values `window` updates wide shows: from the oldest of its samples
    /// inside the window to the newest.
    pub fn span(&self, n: usize, window: f32) -> f64 {
        let n = n.min(self.times.len());
        if n == 0 {
            return 0.0;
        }
        let len = self.times.len();
        // A sample right on the edge counts, as with whole updates.
        let i = self.split(window + 1e-3).clamp(len - n, len - 1);
        self.newest() - self.times[i].t
    }
}

impl History {
    pub fn new(capacity: usize) -> Self {
        let s = || Series::new(capacity);
        Self {
            cpu: s(),
            kernel: s(),
            per_core: Vec::new(),
            per_core_kernel: Vec::new(),
            memory: s(),
            disk: s(),
            disk_read: s(),
            disk_write: s(),
            disk_drives: Vec::new(),
            net_rx: s(),
            net_tx: s(),
            gpu: s(),
            gpu_adapters: Vec::new(),
            thermal: s(),
            clock: s(),
            clock_max: s(),
            memory_bytes: s(),
            times: VecDeque::with_capacity(capacity),
            step: 0.0,
            last_interval: None,
        }
    }

    pub fn capacity(&self) -> usize {
        self.cpu.capacity
    }

    /// Where the samples sit in time; `None` before the first sample taken at an interval.
    pub fn sample_clock(&self) -> Option<Clock<'_>> {
        Clock::new(&self.times, self.step)
    }

    /// Adds a sample one second after the previous one (tests and previews).
    pub fn push(&mut self, snap: &Snapshot) {
        let t = self.times.back().map_or(0.0, |s| s.t + 1.0);
        self.push_at(snap, t, Some(Duration::from_secs(1)));
    }

    /// Adds a sample taken at `t` seconds (a monotonic clock) while sampling every `interval`
    /// (`None`: paused, a refresh).
    pub fn push_at(&mut self, snap: &Snapshot, t: f64, interval: Option<Duration>) {
        let gap = self.times.back().is_some_and(|b| is_gap(t - b.t, self.last_interval, interval));
        self.push_values(snap);
        if self.times.len() >= self.capacity() {
            self.times.pop_front();
        }
        self.times.push_back(Stamp { t, gap });
        self.last_interval = interval;
        if let Some(iv) = interval {
            self.step = iv.as_secs_f32();
        }
    }

    fn push_values(&mut self, snap: &Snapshot) {
        let cap = self.capacity();
        self.cpu.push(snap.cpu.total_pct);
        self.kernel.push(snap.cpu.kernel_pct);
        if self.per_core.len() != snap.cpu.per_core_pct.len() {
            self.per_core = (0..snap.cpu.per_core_pct.len()).map(|_| Series::new(cap)).collect();
        }
        for (s, v) in self.per_core.iter_mut().zip(&snap.cpu.per_core_pct) {
            s.push(*v);
        }
        if self.per_core_kernel.len() != snap.cpu.per_core_kernel_pct.len() {
            self.per_core_kernel = (0..snap.cpu.per_core_kernel_pct.len()).map(|_| Series::new(cap)).collect();
        }
        for (s, v) in self.per_core_kernel.iter_mut().zip(&snap.cpu.per_core_kernel_pct) {
            s.push(*v);
        }
        self.memory.push(snap.memory.used_pct);
        self.disk.push(snap.disk.active_pct);
        self.disk_read.push(snap.disk.read_bps as f32);
        self.disk_write.push(snap.disk.write_bps as f32);
        // Like a GPU below: a disk that is unplugged keeps its history, reading 0.
        for d in &snap.disk.drives {
            if !self.disk_drives.iter().any(|(id, _)| *id == d.id) {
                self.disk_drives.push((d.id.clone(), DiskSeries { active: Series::new(cap), read: Series::new(cap), write: Series::new(cap) }));
            }
        }
        for (id, s) in &mut self.disk_drives {
            let d = snap.disk.drives.iter().find(|d| d.id == *id);
            s.active.push(d.map_or(0.0, |d| d.active_pct));
            s.read.push(d.map_or(0.0, |d| d.read_bps as f32));
            s.write.push(d.map_or(0.0, |d| d.write_bps as f32));
        }
        self.net_rx.push(snap.network.rx_bps as f32);
        self.net_tx.push(snap.network.tx_bps as f32);
        self.gpu.push(snap.gpu.utilization_pct);
        // A GPU that vanishes (eGPU, driver reset) keeps its history, reading 0, for when it returns.
        for a in &snap.gpu.adapters {
            if !self.gpu_adapters.iter().any(|(id, _)| *id == a.id) {
                self.gpu_adapters.push((a.id.clone(), Series::new(cap)));
            }
        }
        for (id, s) in &mut self.gpu_adapters {
            s.push(snap.gpu.adapters.iter().find(|a| a.id == *id).map_or(0.0, |a| a.utilization_pct));
        }
        // No reading is a gap (NaN), not 0 °C: graphs leave it out and the readout says so.
        self.thermal.push(snap.thermal.max_c().unwrap_or(f32::NAN));
        self.clock.push(snap.cpu.current_mhz.unwrap_or(0) as f32);
        self.clock_max.push(snap.cpu.per_core_mhz.iter().copied().max().unwrap_or(0) as f32);
        self.memory_bytes.push(snap.memory.total.saturating_sub(snap.memory.available) as f32);
    }

    pub fn resize(&mut self, capacity: usize) {
        for s in [
            &mut self.cpu,
            &mut self.kernel,
            &mut self.memory,
            &mut self.disk,
            &mut self.disk_read,
            &mut self.disk_write,
            &mut self.net_rx,
            &mut self.net_tx,
            &mut self.gpu,
            &mut self.thermal,
            &mut self.clock,
            &mut self.clock_max,
            &mut self.memory_bytes,
        ] {
            s.resize(capacity);
        }
        for s in self.per_core.iter_mut().chain(self.per_core_kernel.iter_mut()).chain(self.gpu_adapters.iter_mut().map(|(_, s)| s)) {
            s.resize(capacity);
        }
        for (_, d) in &mut self.disk_drives {
            for s in [&mut d.active, &mut d.read, &mut d.write] {
                s.resize(capacity);
            }
        }
        while self.times.len() > self.cpu.capacity {
            self.times.pop_front();
        }
    }

    /// The history of disk `id`.
    pub fn disk_drive(&self, id: &str) -> Option<&DiskSeries> {
        self.disk_drives.iter().find(|(i, _)| i == id).map(|(_, s)| s)
    }

    /// The utilisation history of GPU `id`.
    pub fn gpu_adapter(&self, id: &str) -> Option<&Series> {
        self.gpu_adapters.iter().find(|(i, _)| i == id).map(|(_, s)| s)
    }
}

/// History length in samples. Design choice: `graphHistoryMultiplier` (default 15)
/// times 64. With compressed graph time the live quarter of a graph holds 1x and the
/// older quarters 2x, 4x and 8x history, so 15x the live window is what a wide graph
/// can show; 64 samples per unit covers 1024 px of live area at 16 px per update.
pub fn history_capacity(multiplier: u32) -> usize {
    (multiplier.clamp(1, 240) as usize) * 64
}

/// State shared between the sampler thread and the UI. Readers clone the `Arc`s (cheap);
/// the sampler replaces them, so a frame never copies the history or the process list.
pub struct Shared {
    pub latest: Arc<Snapshot>,
    pub history: Arc<History>,
    pub processes: Arc<Vec<ProcessRow>>,
}

pub struct Sampler {
    pub shared: Arc<Mutex<Shared>>,
    interval_ms: Arc<AtomicU64>,
    refresh_now: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Arc<Wake>,
}

/// Wakes the sampler thread before its next sample is due: a refresh, a new interval, the end
/// (#194). The thread sleeps until then instead of looking every 25 ms.
#[derive(Default)]
struct Wake {
    lock: Mutex<()>,
    cv: Condvar,
}

impl Wake {
    /// Call after changing what the sleeper checks: taking the lock first means a sleeper that
    /// checked before the change is already waiting, so the notification cannot be lost.
    fn notify(&self) {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }

    /// Sleeps up to `wait`, or until [`Wake::notify`]; returns at once when `ready()` already
    /// holds (checked under the lock).
    fn sleep(&self, wait: Duration, ready: impl Fn() -> bool) {
        let held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if ready() {
            return;
        }
        let _ = self.cv.wait_timeout(held, wait);
    }
}

/// How long the sampler sleeps before its next look: until the next sample is due, or (paused,
/// `interval_ms` 0) until woken. `since_last` is the time since the last sample.
fn sampler_wait(interval_ms: u64, since_last: Duration) -> Duration {
    if interval_ms == 0 {
        Duration::from_secs(3600)
    } else {
        Duration::from_millis(interval_ms).saturating_sub(since_last)
    }
}

impl Sampler {
    /// Starts the background sampler. `on_sample` runs on the sampler thread after each publication
    /// (repaint hook, alerts) with the sample and the process list that were published.
    pub fn start(interval: Option<Duration>, history_len: usize, on_sample: impl Fn(&Snapshot, &[ProcessRow]) + Send + 'static) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            latest: Arc::new(Snapshot::default()),
            history: Arc::new(History::new(history_len)),
            processes: Arc::new(Vec::new()),
        }));
        let interval_ms = Arc::new(AtomicU64::new(interval.map(|d| d.as_millis() as u64).unwrap_or(0)));
        let refresh_now = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Wake::default());
        {
            let shared = shared.clone();
            let interval_ms = interval_ms.clone();
            let refresh_now = refresh_now.clone();
            let stop = stop.clone();
            let wake = wake.clone();
            std::thread::Builder::new()
                .name("tmop-sampler".into())
                .spawn(move || {
                    let mut collector = Collector::new();
                    // None before the first sample. Not "now minus an hour": an Instant cannot go back
                    // before the boot on some platforms, and that would panic in the first hour.
                    let mut last: Option<Instant> = None;
                    // The history's clock (#201): seconds since the thread started.
                    let epoch = Instant::now();
                    let since_last = |last: Option<Instant>| last.map_or(Duration::MAX, |t| t.elapsed());
                    while !stop.load(Ordering::Relaxed) {
                        let ms = interval_ms.load(Ordering::Relaxed);
                        let due = (ms > 0 && since_last(last) >= Duration::from_millis(ms)) || refresh_now.swap(false, Ordering::Relaxed);
                        if !due {
                            // Until the next sample is due, or a refresh, a new interval or the end.
                            let ready = || stop.load(Ordering::Relaxed) || refresh_now.load(Ordering::Relaxed) || interval_ms.load(Ordering::Relaxed) != ms;
                            wake.sleep(sampler_wait(ms, since_last(last)), ready);
                            continue;
                        }
                        {
                            let at = Instant::now();
                            last = Some(at);
                            let (mut snap, procs) = cost::time(cost::Part::Collect, || collector.collect());
                            // Invariant: CPU% normalisers divide by it.
                            snap.cpu.logical_count = snap.cpu.logical_count.max(1);
                            let (snap, procs) = (Arc::new(snap), Arc::new(procs));
                            if let Ok(mut g) = shared.lock() {
                                // Copy-on-write: clones only if a frame still holds the old one.
                                let interval = (ms > 0).then(|| Duration::from_millis(ms));
                                Arc::make_mut(&mut g.history).push_at(&snap, at.duration_since(epoch).as_secs_f64(), interval);
                                g.latest = snap.clone();
                                g.processes = procs.clone();
                            }
                            on_sample(&snap, &procs);
                            cost::note_sampler_cpu();
                        }
                    }
                })
                .expect("spawn sampler");
        }
        Self { shared, interval_ms, refresh_now, stop, wake }
    }

    /// Called every frame; wakes the sampler only when the interval changes.
    pub fn set_interval(&self, interval: Option<Duration>) {
        let ms = interval.map(|d| d.as_millis() as u64).unwrap_or(0);
        if self.interval_ms.swap(ms, Ordering::Relaxed) != ms {
            self.wake.notify();
        }
    }

    pub fn refresh(&self) {
        self.refresh_now.store(true, Ordering::Relaxed);
        self.wake.notify();
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sampler_sleeps_until_the_next_sample_is_due() {
        assert_eq!(sampler_wait(500, Duration::from_millis(120)), Duration::from_millis(380));
        assert_eq!(sampler_wait(500, Duration::from_millis(900)), Duration::ZERO, "overdue");
        assert!(sampler_wait(0, Duration::ZERO) >= Duration::from_secs(3600), "paused: until woken");
    }

    /// A paused sampler takes its first sample and then sleeps; a refresh and a new interval wake
    /// it at once instead of after its long wait (#194).
    #[test]
    fn a_sleeping_sampler_wakes_for_a_refresh_and_a_new_interval() {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let s = Sampler::start(None, 8, move |snap, _| {
            let _ = tx.lock().map(|t| t.send(snap.seq));
        });
        let wait = Duration::from_secs(20);
        assert_eq!(rx.recv_timeout(wait), Ok(1), "the first sample");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "paused: no more");
        s.refresh();
        assert_eq!(rx.recv_timeout(wait), Ok(2), "woken by a refresh");
        s.set_interval(Some(Duration::from_millis(50)));
        assert_eq!(rx.recv_timeout(wait), Ok(3), "woken by the new interval");
        assert_eq!(rx.recv_timeout(wait), Ok(4), "and sampling at it");
        drop(s);
    }

    /// Dropping the sampler ends its thread, also while it sleeps: the thread owns the callback,
    /// so the callback's channel disconnects once the thread is gone.
    #[test]
    fn dropping_the_sampler_ends_its_thread() {
        for interval in [Some(Duration::from_millis(50)), None, Some(Duration::from_secs(3600))] {
            let (tx, rx) = std::sync::mpsc::channel::<u64>();
            let tx = Mutex::new(tx);
            let s = Sampler::start(interval, 8, move |snap, _| {
                let _ = tx.lock().map(|t| t.send(snap.seq));
            });
            assert_eq!(rx.recv_timeout(Duration::from_secs(20)), Ok(1), "{interval:?}: the first sample");
            drop(s);
            let gone = (0..200).any(|_| matches!(rx.recv_timeout(Duration::from_millis(100)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected)));
            assert!(gone, "{interval:?}: the thread ended");
        }
    }

    #[test]
    fn a_gap_is_more_than_the_slower_interval_allows() {
        let (half, one, five) = (Some(Duration::from_millis(500)), Some(Duration::from_secs(1)), Some(Duration::from_secs(5)));
        assert!(!is_gap(1.0, one, one));
        assert!(!is_gap(2.2, one, one), "a late sample");
        assert!(is_gap(2.3, one, one));
        assert!(!is_gap(5.0, five, one), "back from the notification area at 5 s");
        assert!(!is_gap(1.0, half, one), "into low-power mode");
        assert!(is_gap(30.0, one, None), "a refresh after a pause");
        assert!(is_gap(30.0, None, one), "resumed after a pause");
        assert!(!is_gap(0.8, None, one), "resumed at once");
        assert!(is_gap(0.1, None, None), "two refreshes while paused");
    }

    fn stamped(cap: usize, times: &[(f64, Option<Duration>)]) -> History {
        let mut h = History::new(cap);
        for (t, iv) in times {
            h.push_at(&Snapshot::default(), *t, *iv);
        }
        h
    }

    #[test]
    fn the_history_stamps_each_sample() {
        let (one, two) = (Some(Duration::from_secs(1)), Some(Duration::from_secs(2)));
        assert!(History::new(4).sample_clock().is_none(), "no clock before the first sample");
        let h = stamped(8, &[(0.0, one), (1.0, one), (3.0, two), (5.0, two), (20.0, None), (21.0, None)]);
        assert_eq!(h.times.iter().map(|s| s.gap).collect::<Vec<_>>(), vec![false, false, false, false, true, true]);
        assert_eq!(h.step, 2.0, "the last interval while sampling; paused keeps it");
        let c = h.sample_clock().expect("a clock");
        assert_eq!((0..6).map(|a| c.secs(a)).collect::<Vec<_>>(), vec![0.0, 1.0, 16.0, 18.0, 20.0, 21.0]);
        assert_eq!(c.age(3), 9.0, "in updates of 2 s");
        // Wraps with the series and shrinks with them.
        let mut h = stamped(4, &(0..10).map(|i| (i as f64, one)).collect::<Vec<_>>());
        assert_eq!((h.times.len(), h.times.front().map(|s| s.t)), (4, Some(6.0)));
        h.resize(2);
        assert_eq!((h.times.len(), h.cpu.values.len()), (2, 2));
        // Previews push without a time: one second apart.
        let mut h = History::new(4);
        h.push(&Snapshot::default());
        h.push(&Snapshot::default());
        assert_eq!(h.sample_clock().map(|c| c.secs(1)), Some(1.0));
    }

    #[test]
    fn the_clock_finds_samples_and_spans_by_time() {
        let (one, two) = (Some(Duration::from_secs(1)), Some(Duration::from_secs(2)));
        // 1 s samples, then 2 s ones: newest first, ages in updates of 2 s are 0, 1, 2, 3, 3.5, 4, 4.5.
        let h = stamped(16, &[(0.0, one), (1.0, one), (2.0, one), (3.0, one), (5.0, two), (7.0, two), (9.0, two)]);
        let c = h.sample_clock().expect("a clock");
        let n = h.cpu.values.len();
        assert_eq!([0.0, 0.4, 0.6, 2.2, 3.4, 3.8, 4.9].map(|u| c.nearest(u, n)), [0, 0, 1, 2, 4, 5, 6].map(Some));
        assert_eq!(c.nearest(-3.0, n), Some(0), "right of the newest sample");
        assert_eq!(c.nearest(5.0, n), None, "beyond the oldest");
        assert_eq!(c.nearest(1.4, 2), Some(1), "only the newest two belong to a short series");
        assert_eq!(c.nearest(1.5, 2), None);
        assert_eq!(c.span(n, 100.0), 9.0, "the whole history");
        assert_eq!(c.span(n, 2.0), 4.0, "two updates of 2 s");
        assert_eq!(c.span(n, 2.6), 4.0, "the newest three are inside");
        assert_eq!(c.span(n, 3.0), 6.0);
        assert_eq!(c.span(3, 100.0), 4.0, "a short series");
        // Evenly spaced: the same as counting samples.
        let h = stamped(64, &(0..40).map(|i| (100.0 + i as f64 * 0.5, Some(Duration::from_millis(500)))).collect::<Vec<_>>());
        let c = h.sample_clock().expect("a clock");
        for u in [0.0_f32, 0.49, 0.5, 0.51, 7.2, 38.6, 39.4] {
            assert_eq!(c.nearest(u, 40), Some((u.round() as usize).min(39)), "u {u}");
        }
        assert_eq!(c.nearest(39.5, 40), None);
        assert_eq!(c.span(40, 10.0), 5.0);
        assert_eq!(c.span(40, 10.5), 5.0);
    }

    #[test]
    fn series_ring() {
        let mut s = Series::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            s.push(v);
        }
        assert_eq!(s.values.iter().copied().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(s.max(), 4.0);
        s.resize(2);
        assert_eq!(s.last(), 4.0);
        assert_eq!(s.values.len(), 2);
    }

    #[test]
    fn fastest_core_history_runs_with_the_average_clock() {
        // Both clock traces are collected with every sample, whatever page is shown, so a page
        // switch cannot restart one of them.
        let mut h = History::new(8);
        let mut snap = Snapshot::default();
        for per_core in [vec![3000, 4200], vec![3100, 3900], vec![]] {
            snap.cpu.current_mhz = Some(3500);
            snap.cpu.per_core_mhz = per_core;
            h.push(&snap);
        }
        assert_eq!(h.clock.values.iter().copied().collect::<Vec<_>>(), vec![3500.0; 3]);
        assert_eq!(h.clock_max.values.iter().copied().collect::<Vec<_>>(), vec![4200.0, 3900.0, 0.0]);
        h.resize(2);
        assert_eq!((h.clock.capacity, h.clock_max.capacity), (2, 2));
    }

    #[test]
    fn history_tracks_each_gpu_by_id() {
        let snap = |ids: &[(&str, f32)]| {
            let adapters = ids.iter().map(|(id, u)| GpuAdapter { id: id.to_string(), utilization_pct: *u, ..Default::default() }).collect();
            Snapshot { gpu: GpuSample { adapters, ..Default::default() }, ..Default::default() }
        };
        let mut h = History::new(8);
        h.push(&snap(&[("a", 10.0), ("b", 20.0)]));
        h.push(&snap(&[("b", 30.0)]));
        assert_eq!(h.gpu_adapter("a").unwrap().last(), 0.0);
        assert_eq!(h.gpu_adapter("b").unwrap().last(), 30.0);
        assert!(h.gpu_adapter("c").is_none());
        h.resize(1);
        assert_eq!(h.gpu_adapter("b").unwrap().capacity, 2);
    }

    #[test]
    fn history_tracks_each_disk_by_id() {
        let snap = |ids: &[(&str, f32, f64)]| {
            let drives = ids.iter().map(|(id, a, r)| DiskDrive { id: id.to_string(), active_pct: *a, read_bps: *r, write_bps: r * 2.0, ..Default::default() }).collect();
            Snapshot { disk: DiskSample { drives, ..Default::default() }, ..Default::default() }
        };
        let mut h = History::new(8);
        h.push(&snap(&[("0", 10.0, 100.0), ("1", 20.0, 200.0)]));
        h.push(&snap(&[("1", 30.0, 300.0)]));
        let (gone, kept) = (h.disk_drive("0").unwrap(), h.disk_drive("1").unwrap());
        assert_eq!((gone.active.last(), gone.read.last(), gone.active.values.len()), (0.0, 0.0, 2));
        assert_eq!((kept.active.last(), kept.read.last(), kept.write.last()), (30.0, 300.0, 600.0));
        assert!(h.disk_drive("2").is_none());
        h.resize(1);
        assert_eq!(h.disk_drive("1").unwrap().write.capacity, 2);
    }

    #[test]
    fn clock_base_prefers_registry_then_pdh() {
        use clock::clock_base;
        assert_eq!(clock_base(Some(3400), Some(3400), None, None), Some(3400));
        assert_eq!(clock_base(None, Some(3000), None, None), Some(3000));
        assert_eq!(clock_base(Some(3400), Some(2900), None, None), Some(2900));
        assert_eq!(clock_base(Some(2900), Some(3400), None, None), Some(2900));
        assert_eq!(clock_base(None, None, Some(3600), Some(5000)), Some(3600));
        assert_eq!(clock_base(None, None, Some(0), Some(5000)), Some(5000));
        assert_eq!(clock_base(None, None, None, None), None);
    }

    #[test]
    fn clock_mhz_from_pct() {
        use clock::clock_mhz;
        assert_eq!(clock_mhz(Some(3400), Some(141.2)), Some(4801));
        assert_eq!(clock_mhz(Some(3400), Some(0.0)), None);
        assert_eq!(clock_mhz(Some(3400), Some(f64::NAN)), None);
        assert_eq!(clock_mhz(Some(3400), None), None);
        assert_eq!(clock_mhz(None, Some(100.0)), None);
        assert_eq!(clock_mhz(Some(3400), Some(1e9)), Some(34000));
    }

    #[test]
    fn clock_ceiling_boost_or_125() {
        use clock::clock_ceiling;
        assert_eq!(clock_ceiling(Some(3400), Some(5000)), Some(5000.0));
        assert_eq!(clock_ceiling(Some(3400), None), Some(4250.0));
        assert_eq!(clock_ceiling(Some(3400), Some(3400)), Some(4250.0));
        assert_eq!(clock_ceiling(None, Some(5000)), None);
    }

    #[test]
    fn thermal_max_includes_hwinfo() {
        let t = ThermalSample { zones: vec![("TZ00".into(), 45.0)], cpu: Some(("CPU Package".into(), 70.0)), ..Default::default() };
        assert_eq!(t.max_c(), Some(70.0));
        assert_eq!(ThermalSample::default().max_c(), None);
    }

    #[test]
    fn only_the_gpu_core_counts_as_hottest_and_never_for_the_cpu() {
        let g = |kind, c, source| GpuThermal { label: String::new(), kind, c, source };
        let t = ThermalSample { zones: vec![("TZ00".into(), 45.0)], gpu: vec![g(GpuTempKind::Gpu, 80.0, "WDDM"), g(GpuTempKind::MemoryJunction, 96.0, "HWiNFO"), g(GpuTempKind::HotSpot, 92.0, "HWiNFO")], ..Default::default() };
        assert_eq!((t.max_c(), t.board_max_c()), (Some(80.0), Some(45.0)));
        assert_eq!(t.hottest(), Some(Hottest { what: "GPU".into(), c: 80.0, source: "WDDM".into() }));
        // Hot spot and memory alone: no hottest reading at all, rather than an alarming one.
        let t = ThermalSample { gpu: vec![g(GpuTempKind::Memory, 90.0, "hwmon amdgpu")], ..Default::default() };
        assert_eq!((t.max_c(), t.hottest()), (None, None));
        // The CPU names its source and wins a tie; a zone is named by itself.
        let t = ThermalSample { cpu: Some(("CPU Package".into(), 70.0)), cpu_source: Some("PawnIO".into()), gpu: vec![g(GpuTempKind::Gpu, 70.0, "NVML")], zones: vec![("TZ00".into(), 30.0)], ..Default::default() };
        assert_eq!(t.hottest(), Some(Hottest { what: "CPU".into(), c: 70.0, source: "PawnIO".into() }));
        let t = ThermalSample { zones: vec![("TZ00".into(), 30.0), ("TZ01".into(), 31.0)], ..Default::default() };
        assert_eq!(t.hottest().map(|h| (h.what, h.source)), Some(("TZ01".to_string(), SOURCES.sensor.to_string())));
    }

    #[test]
    fn a_gpu_keeps_the_first_reading_of_each_kind() {
        let mut a = GpuAdapter::default();
        a.offer_temp(GpuTempKind::HotSpot, 75.0, "HWiNFO");
        a.offer_temp(GpuTempKind::Gpu, 61.0, "NVML");
        a.offer_temp(GpuTempKind::Gpu, 60.0, "WDDM");
        a.offer_temp(GpuTempKind::Memory, f32::NAN, "HWiNFO");
        assert_eq!(a.temperature_c, Some(61.0));
        assert_eq!(a.temps.iter().map(|t| (t.kind, t.source)).collect::<Vec<_>>(), [(GpuTempKind::Gpu, "NVML"), (GpuTempKind::HotSpot, "HWiNFO")]);
        assert_eq!(a.temp(GpuTempKind::HotSpot).map(|t| t.c), Some(75.0));
    }

    #[test]
    fn placeholder_counts_as_known_until_a_sample_says_otherwise() {
        let mut s = Snapshot::default();
        assert!(s.disk_known() && s.net_known());
        s.seq = 1;
        assert!(!s.disk_known() && !s.net_known());
        s.disk.available = true;
        s.network.available = true;
        assert!(s.disk_known() && s.net_known());
    }

    /// Every backend, stubs included: collects without panicking, keeps the logical-count
    /// invariant and names its sources.
    #[test]
    fn backend_contract() {
        let mut c = Collector::new();
        let (snap, _) = c.collect();
        assert!(snap.cpu.logical_count >= 1);
        assert_eq!(snap.seq, 1);
        let s = SOURCES;
        assert!([s.disk, s.network, s.gpu, s.no_gpu, s.thermal, s.thermal_hint, s.sensor, s.handles].iter().all(|t| !t.is_empty()));
        assert!(!s.thermal_hint.ends_with('.'));
        let _ = probe();
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn collector_smoke() {
        let mut c = Collector::new();
        let _ = c.collect();
        // Keep one core busy so the CPU reading cannot be a legitimate 0.
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(300) {
            std::hint::black_box(t.elapsed());
        }
        let (snap, procs) = c.collect();
        assert!(snap.memory.total > 0 && snap.memory.available > 0);
        assert!(snap.cpu.logical_count > 0);
        assert!(snap.cpu.total_pct > 0.0 && snap.cpu.total_pct <= 100.0, "cpu {}", snap.cpu.total_pct);
        assert!(snap.uptime > Duration::ZERO);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert_eq!(snap.cpu.per_core_pct.len(), snap.cpu.logical_count);
        }
        if cfg!(target_os = "linux") {
            assert!(snap.process_count > 0 && snap.thread_count >= snap.process_count);
            assert!(snap.disk.available && snap.network.available, "{:?}", snap.errors);
            assert!(snap.errors.is_empty(), "{:?}", snap.errors);
        }
        assert!(procs.iter().any(|p| p.pid == std::process::id()), "own process not enumerated");
    }
}
