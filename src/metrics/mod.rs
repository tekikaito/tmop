//! System metrics: snapshot types, history buffers and the background sampler.
//!
//! Sources:
//! * CPU total/kernel: `GetSystemTimes`; per logical CPU: `NtQuerySystemInformation`
//!   (`SystemProcessorPerformanceInformation`); clock: `CallNtPowerInformation(ProcessorInformation)`.
//! * Memory: `GlobalMemoryStatusEx`.
//! * Disk: PDH `\PhysicalDisk(_Total)\...` (aggregated over all disks).
//! * Network: `GetIfTable2` octet deltas.
//! * GPU: PDH `\GPU Engine(*)\Utilization Percentage` and `\GPU Adapter Memory(*)\Dedicated Usage`.
//! * Thermal: PDH `\Thermal Zone Information(*)\Temperature` (ACPI firmware zones, Kelvin).
//! * Processes: `NtQuerySystemInformation(SystemProcessInformation)`.

#[cfg(windows)]
mod win;

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
    pub current_mhz: Option<u32>,
    pub max_mhz: Option<u32>,
    pub logical_count: usize,
    /// Current clock of each logical processor (MHz), when available.
    pub per_core_mhz: Vec<u32>,
    /// CPUID brand string ("AMD Ryzen 7 ..."), empty when unknown.
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct MemorySample {
    pub total: u64,
    pub available: u64,
    pub used_pct: f32,
    pub commit_used: u64,
    pub commit_limit: u64,
    /// System file cache (`GetPerformanceInfo`).
    pub cached: u64,
    /// Page file bytes in use (PDH `\\Paging File(_Total)\\% Usage`), when known.
    pub swap_used: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    pub active_pct: f32,
    pub read_bps: f64,
    pub write_bps: f64,
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSample {
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// Sum of link speeds of up hardware interfaces (bits/s).
    pub link_bps: f64,
    pub utilization_pct: f32,
}

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub available: bool,
    /// Busiest engine type of the busiest adapter, like Windows Task Manager.
    pub utilization_pct: f32,
    pub dedicated_used: u64,
    /// (engine type, %) for the busiest adapter.
    pub engines: Vec<(String, f32)>,
}

#[derive(Clone, Debug, Default)]
pub struct ThermalSample {
    /// (zone name, °C)
    pub zones: Vec<(String, f32)>,
}

impl ThermalSample {
    pub fn max_c(&self) -> Option<f32> {
        self.zones.iter().map(|z| z.1).fold(None, |m, v| Some(m.map_or(v, |m: f32| m.max(v))))
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
    pub handle_count: usize,
    pub errors: Vec<String>,
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
    pub net_rx: Series,
    pub net_tx: Series,
    pub gpu: Series,
    pub thermal: Series,
    /// Average core clock in MHz.
    pub clock: Series,
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
            net_rx: s(),
            net_tx: s(),
            gpu: s(),
            thermal: s(),
            clock: s(),
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
        self.net_rx.push(snap.network.rx_bps as f32);
        self.net_tx.push(snap.network.tx_bps as f32);
        self.gpu.push(snap.gpu.utilization_pct);
        self.thermal.push(snap.thermal.max_c().unwrap_or(0.0));
        self.clock.push(snap.cpu.current_mhz.unwrap_or(0) as f32);
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
            &mut self.memory_bytes,
        ] {
            s.resize(capacity);
        }
        for s in self.per_core.iter_mut().chain(self.per_core_kernel.iter_mut()) {
            s.resize(capacity);
        }
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
    /// Starts the background sampler. `on_sample` runs after each publication (repaint hook).
    pub fn start(interval: Option<Duration>, history_len: usize, on_sample: impl Fn() + Send + 'static) -> Self {
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
                            let (snap, procs) = collector.collect();
                            if let Ok(mut g) = shared.lock() {
                                // Copy-on-write: clones only if a frame still holds the old one.
                                Arc::make_mut(&mut g.history).push(&snap);
                                g.latest = Arc::new(snap);
                                g.processes = Arc::new(procs);
                            }
                            on_sample();
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

#[cfg(windows)]
pub use win::Collector;

/// Non-Windows stub so the library (and its tests) still compile elsewhere.
#[cfg(not(windows))]
pub struct Collector;
#[cfg(not(windows))]
impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(not(windows))]
impl Collector {
    pub fn new() -> Self {
        Collector
    }
    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        (Snapshot { errors: vec!["metrics are only implemented on Windows".into()], ..Default::default() }, Vec::new())
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

    #[cfg(windows)]
    #[test]
    fn collector_smoke() {
        let mut c = Collector::new();
        let _ = c.collect();
        std::thread::sleep(Duration::from_millis(300));
        let (snap, procs) = c.collect();
        assert!(snap.memory.total > 0);
        assert!(snap.cpu.logical_count > 0);
        assert!((0.0..=100.0).contains(&snap.cpu.total_pct));
        assert!(procs.iter().any(|p| p.pid == std::process::id()), "own process not enumerated");
    }
}
