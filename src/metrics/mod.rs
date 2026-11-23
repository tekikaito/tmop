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
//!   (`D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERPERFDATA)` by LUID); hot spot and memory
//!   temperatures from HWiNFO's shared memory when it runs, matched to a GPU by name.
//! * Thermal: PDH `\Thermal Zone Information(*)\Temperature` (ACPI firmware zones, Kelvin); CPU
//!   temperature from the PawnIO driver when switched on in Settings ([`pawnio`]), else from
//!   HWiNFO's shared memory ([`hwinfo`]).
//! * Processes: `NtQuerySystemInformation(SystemProcessInformation)`.

pub mod common;
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
use std::sync::{Arc, Mutex};
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
    /// The GPUs' temperatures ([`GpuAdapter::temps`]), as ("<GPU name> <reading>", °C).
    pub gpu: Vec<(String, f32)>,
    /// Which sources were checked, e.g. "ACPI: no zones · HWiNFO: not running".
    pub sources: String,
}

impl ThermalSample {
    /// The hottest reading of all, GPUs included: the Thermals figure, its history, the popout
    /// tile and the temperature alert.
    pub fn max_c(&self) -> Option<f32> {
        hottest(self.board_readings().chain(self.gpu.iter().map(|g| g.1)))
    }

    /// The hottest reading of the CPU and the board (thermal zones, other hwmon chips), GPUs left
    /// out: what a CPU readout falls back to when there is no CPU temperature.
    pub fn board_max_c(&self) -> Option<f32> {
        hottest(self.board_readings())
    }

    fn board_readings(&self) -> impl Iterator<Item = f32> + '_ {
        self.zones.iter().map(|z| z.1).chain(self.cpu.as_ref().map(|c| c.1))
    }
}

fn hottest(v: impl Iterator<Item = f32>) -> Option<f32> {
    v.fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
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
        }
    }

    pub fn capacity(&self) -> usize {
        self.cpu.capacity
    }

    pub fn push(&mut self, snap: &Snapshot) {
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
        self.thermal.push(snap.thermal.max_c().unwrap_or(0.0));
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
        {
            let shared = shared.clone();
            let interval_ms = interval_ms.clone();
            let refresh_now = refresh_now.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("tmop-sampler".into())
                .spawn(move || {
                    let mut collector = Collector::new();
                    let mut last = Instant::now() - Duration::from_secs(3600);
                    while !stop.load(Ordering::Relaxed) {
                        let ms = interval_ms.load(Ordering::Relaxed);
                        let due = (ms > 0 && last.elapsed() >= Duration::from_millis(ms)) || refresh_now.swap(false, Ordering::Relaxed);
                        if due {
                            last = Instant::now();
                            let (mut snap, procs) = collector.collect();
                            // Invariant: CPU% normalisers divide by it.
                            snap.cpu.logical_count = snap.cpu.logical_count.max(1);
                            let (snap, procs) = (Arc::new(snap), Arc::new(procs));
                            if let Ok(mut g) = shared.lock() {
                                // Copy-on-write: clones only if a frame still holds the old one.
                                Arc::make_mut(&mut g.history).push(&snap);
                                g.latest = snap.clone();
                                g.processes = procs.clone();
                            }
                            on_sample(&snap, &procs);
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                })
                .expect("spawn sampler");
        }
        Self { shared, interval_ms, refresh_now, stop }
    }

    pub fn set_interval(&self, interval: Option<Duration>) {
        self.interval_ms.store(interval.map(|d| d.as_millis() as u64).unwrap_or(0), Ordering::Relaxed);
    }

    pub fn refresh(&self) {
        self.refresh_now.store(true, Ordering::Relaxed);
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn gpu_readings_count_as_hottest_but_not_for_the_cpu() {
        let t = ThermalSample { zones: vec![("TZ00".into(), 45.0)], gpu: vec![("RTX GPU".into(), 80.0)], ..Default::default() };
        assert_eq!((t.max_c(), t.board_max_c()), (Some(80.0), Some(45.0)));
        let t = ThermalSample { gpu: vec![("RTX GPU".into(), 60.0)], ..Default::default() };
        assert_eq!((t.max_c(), t.board_max_c()), (Some(60.0), None));
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
