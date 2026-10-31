//! Windows metric collection.

use super::clock::{clock_base, clock_mhz, perf_mhz};
use super::common::{cpu_brand, fold_gpu, net_delta, tick_delta_pct, NetMark, Ticks};
use super::*;
use crate::process::{Enumerator, ProcessRow};
use std::collections::HashMap;
use windows::core::PCWSTR;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
};

/// PDH_FMT_DOUBLE | PDH_FMT_NOCAP100
const FMT: PDH_FMT = PDH_FMT(0x0000_0200 | 0x0000_8000);
type PdhHandle = isize;
use windows::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};
use windows::Win32::System::SystemInformation::{GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO};
use windows::Win32::System::Threading::GetSystemTimes;

const PDH_MORE_DATA: u32 = 0x8000_07D2;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

pub const SOURCES: Sources = Sources {
    disk: "Active time (all physical disks)",
    network: "Throughput (GetIfTable2, all adapters)",
    gpu: "Busiest engine of the busiest adapter (PDH GPU Engine counters)",
    no_gpu: "No GPU Engine performance counters were reported.",
    thermal: "ACPI thermal zones (firmware sensors, not the CPU package)",
    thermal_hint: WINDOWS_THERMAL_HINT,
    sensor: "ACPI zone",
    handles: "Handles",
};

/// Windows reports clocks, and the thermal card always has ACPI zones or the PawnIO/HWiNFO hint.
pub fn probe() -> Probe {
    Probe { clocks: true, thermal: true }
}

const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut std::ffi::c_void, len: u32, ret_len: *mut u32) -> windows::Win32::Foundation::NTSTATUS;
}

/// Raw SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION array (processor group 0). `size` should be
/// entry size x processors: the call fails unless the length is a whole number of entries, so
/// on a mismatch it retries with the length the system names (else twice the size).
pub fn query_processor_performance(size: usize) -> Result<Vec<u8>, String> {
    let mut size = size.max(1);
    for _ in 0..4 {
        let mut buf = vec![0u8; size];
        let mut needed = 0u32;
        // SAFETY: buf is writable for size bytes; needed receives the length.
        let st = unsafe { NtQuerySystemInformation(SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS, buf.as_mut_ptr().cast(), size as u32, &mut needed) };
        if st.0 == STATUS_INFO_LENGTH_MISMATCH {
            size = if needed as usize > size { needed as usize } else { size * 2 };
            continue;
        }
        if st.0 < 0 {
            return Err(format!("NtQuerySystemInformation({SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS}) failed: 0x{:08X}", st.0 as u32));
        }
        buf.truncate(needed.max(1) as usize);
        return Ok(buf);
    }
    Err("SystemProcessorPerformanceInformation kept growing".into())
}

/// A PDH query with named counters.
struct Pdh {
    query: PdhHandle,
    counters: HashMap<&'static str, PdhHandle>,
}

impl Pdh {
    fn new(paths: &[(&'static str, &str)]) -> Option<Self> {
        let mut query = PdhHandle::default();
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) } != 0 {
            return None;
        }
        let mut counters = HashMap::new();
        for (key, path) in paths {
            let w = wide(path);
            let mut c = PdhHandle::default();
            if unsafe { PdhAddEnglishCounterW(query, PCWSTR(w.as_ptr()), 0, &mut c) } == 0 {
                counters.insert(*key, c);
            }
        }
        unsafe {
            PdhCollectQueryData(query);
        }
        Some(Self { query, counters })
    }

    fn collect(&self) {
        unsafe {
            PdhCollectQueryData(self.query);
        }
    }

    fn value(&self, key: &str) -> Option<f64> {
        let c = *self.counters.get(key)?;
        let mut v = PDH_FMT_COUNTERVALUE::default();
        let st = unsafe { PdhGetFormattedCounterValue(c, FMT, None, &mut v) };
        if st != 0 || v.CStatus > 1 {
            return None;
        }
        Some(unsafe { v.Anonymous.doubleValue })
    }

    fn array(&self, key: &str) -> Vec<(String, f64)> {
        let Some(&c) = self.counters.get(key) else { return Vec::new() };
        let mut size = 0u32;
        let mut count = 0u32;
        let st = unsafe { PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, None) };
        if st != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // u64 backing keeps the item structs aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        let st = unsafe { PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, Some(items)) };
        if st != 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let it = unsafe { &*items.add(i) };
            if it.FmtValue.CStatus > 1 {
                continue;
            }
            let name = unsafe { it.szName.to_string() }.unwrap_or_default();
            out.push((name, unsafe { it.FmtValue.Anonymous.doubleValue }));
        }
        out
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPerformance {
    idle: i64,
    kernel: i64, // includes idle
    user: i64,
    dpc: i64,
    interrupt: i64,
    interrupt_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPowerInformation {
    number: u32,
    max_mhz: u32,
    current_mhz: u32,
    mhz_limit: u32,
    max_idle_state: u32,
    current_idle_state: u32,
}

pub struct Collector {
    seq: u64,
    logical: usize,
    prev_sys: Option<Ticks>,
    prev_cores: Vec<ProcessorPerformance>,
    prev_net: Option<NetMark>,
    pdh: Option<Pdh>,
    procs: Enumerator,
    /// Registry `~MHz` of CPU 0 (read once).
    reg_mhz: Option<u32>,
    hwinfo: HwinfoCache,
    /// Opt-in PawnIO CPU temperature (closed while the Settings switch is off).
    pawnio: super::pawnio::Source,
}

/// Last HWiNFO read; the mapping is polled at most every 2 s (30 s while not running).
struct HwinfoCache {
    at: Option<Instant>,
    cpu: Option<(String, f32)>,
    status: String,
    running: bool,
}

fn registry_base_mhz() -> Option<u32> {
    use crate::pro::winutil::{Key, View};
    Key::open(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", View::Default)?.u32("~MHz")
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        let mut si = SYSTEM_INFO::default();
        unsafe { GetSystemInfo(&mut si) };
        let pdh = Pdh::new(&[
            ("disk_idle", r"\PhysicalDisk(_Total)\% Idle Time"),
            ("disk_read", r"\PhysicalDisk(_Total)\Disk Read Bytes/sec"),
            ("disk_write", r"\PhysicalDisk(_Total)\Disk Write Bytes/sec"),
            ("gpu_engine", r"\GPU Engine(*)\Utilization Percentage"),
            ("gpu_mem", r"\GPU Adapter Memory(*)\Dedicated Usage"),
            ("thermal", r"\Thermal Zone Information(*)\Temperature"),
            ("pagefile", r"\Paging File(_Total)\% Usage"),
            // Live clock as a share of base; CurrentMhz from CallNtPowerInformation stays at base.
            ("cpu_perf_total", r"\Processor Information(_Total)\% Processor Performance"),
            ("cpu_perf", r"\Processor Information(*)\% Processor Performance"),
            ("cpu_freq", r"\Processor Information(_Total)\Processor Frequency"),
        ]);
        Self {
            seq: 0,
            logical: (si.dwNumberOfProcessors as usize).max(1),
            prev_sys: None,
            prev_cores: Vec::new(),
            prev_net: None,
            pdh,
            procs: Enumerator::new(),
            reg_mhz: registry_base_mhz(),
            hwinfo: HwinfoCache { at: None, cpu: None, status: "not running".into(), running: false },
            pawnio: super::pawnio::Source::default(),
        }
    }

    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, ..Default::default() };
        snap.uptime = Duration::from_millis(unsafe { GetTickCount64() });
        self.cpu(&mut snap);
        self.memory(&mut snap);
        self.network(&mut snap);
        let mut gpu_by_pid = HashMap::new();
        // Windows always shows the disk readouts (0% when PDH or its PhysicalDisk counter is
        // missing), as before 0.6; the PDH error below says why they stay at zero.
        snap.disk.available = true;
        if let Some(pdh) = &self.pdh {
            pdh.collect();
            // Disk
            if let Some(idle) = pdh.value("disk_idle") {
                snap.disk.active_pct = (100.0 - idle).clamp(0.0, 100.0) as f32;
            }
            snap.disk.read_bps = pdh.value("disk_read").unwrap_or(0.0);
            let pagefile_size = snap.memory.swap_total.unwrap_or(0);
            snap.memory.swap_used = pdh.value("pagefile").map(|pct| (pct / 100.0 * pagefile_size as f64) as u64);
            snap.disk.write_bps = pdh.value("disk_write").unwrap_or(0.0);
            // GPU
            let engines = pdh.array("gpu_engine");
            if !engines.is_empty() {
                let (gpu, by_pid) = aggregate_gpu(&engines);
                snap.gpu = gpu;
                gpu_by_pid = by_pid;
            }
            snap.gpu.dedicated_used = pdh.array("gpu_mem").iter().map(|(_, v)| *v as u64).sum();
            // Thermal (Kelvin -> Celsius); zones read 0 when the sensor is not implemented.
            snap.thermal.zones = pdh
                .array("thermal")
                .into_iter()
                .filter(|(_, k)| *k > 200.0)
                .map(|(n, k)| (n, (k - 273.15) as f32))
                .collect();
            // CPU clock: live MHz = base * % Processor Performance.
            snap.cpu.perf_pct = pdh.value("cpu_perf_total");
            snap.cpu.pdh_freq_mhz = pdh.value("cpu_freq").filter(|f| *f > 0.0).map(|f| f.round() as u32);
            let base = clock_base(self.reg_mhz, snap.cpu.pdh_freq_mhz, snap.cpu.mhz_limit, snap.cpu.max_mhz);
            if let Some(mhz) = clock_mhz(base, snap.cpu.perf_pct) {
                snap.cpu.current_mhz = Some(mhz);
                snap.cpu.clock_live = true;
                if let Some(cores) = base.and_then(|b| per_core_perf_mhz(&pdh.array("cpu_perf"), b, snap.cpu.per_core_mhz.len())) {
                    snap.cpu.per_core_mhz = cores;
                }
            }
        } else {
            snap.errors.push("PDH unavailable: disk, GPU and thermal readings disabled".into());
        }
        snap.cpu.base_mhz = clock_base(self.reg_mhz, snap.cpu.pdh_freq_mhz, snap.cpu.mhz_limit, snap.cpu.max_mhz);
        snap.cpu.boost_mhz = snap.cpu.max_mhz.filter(|m| snap.cpu.base_mhz.is_some_and(|b| *m > b));
        if !snap.cpu.clock_live {
            // No live counter: report the static base clock instead of pretending.
            snap.cpu.current_mhz = snap.cpu.base_mhz.or(snap.cpu.current_mhz);
        }
        self.thermal(&mut snap);
        let rows = match self.procs.collect(self.logical, &gpu_by_pid) {
            Ok((rows, t)) => {
                snap.process_count = t.processes;
                snap.thread_count = t.threads;
                snap.handle_count = Some(t.handles);
                rows
            }
            Err(e) => {
                snap.errors.push(e);
                Vec::new()
            }
        };
        (snap, rows)
    }

    fn cpu(&mut self, snap: &mut Snapshot) {
        snap.cpu.logical_count = self.logical;
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_ok() {
            let cur = Ticks { idle: ft(idle), kernel: ft(kernel), user: ft(user) };
            if let Some((busy, kern)) = self.prev_sys.and_then(|p| tick_delta_pct(p, cur)) {
                snap.cpu.total_pct = busy;
                snap.cpu.kernel_pct = kern;
            }
            self.prev_sys = Some(cur);
        }
        // Per logical processor (processor group 0 only).
        let entry = std::mem::size_of::<ProcessorPerformance>();
        if let Ok(buf) = query_processor_performance(entry * self.logical) {
            let n = buf.len() / entry;
            let cores: Vec<ProcessorPerformance> =
                (0..n).map(|i| unsafe { std::ptr::read_unaligned(buf.as_ptr().add(i * entry).cast()) }).collect();
            if self.prev_cores.len() == cores.len() {
                let ticks = |c: &ProcessorPerformance| Ticks { idle: c.idle as u64, kernel: c.kernel as u64, user: c.user as u64 };
                let shares: Vec<(f32, f32)> = cores.iter().zip(&self.prev_cores).map(|(c, p)| tick_delta_pct(ticks(p), ticks(c)).unwrap_or((0.0, 0.0))).collect();
                snap.cpu.per_core_pct = shares.iter().map(|s| s.0).collect();
                snap.cpu.per_core_kernel_pct = shares.iter().map(|s| s.1).collect();
            }
            self.prev_cores = cores;
        }
        // Clock speed.
        let mut ppi = vec![ProcessorPowerInformation::default(); self.logical];
        let st = unsafe {
            CallNtPowerInformation(
                ProcessorInformation,
                None,
                0,
                Some(ppi.as_mut_ptr().cast()),
                (ppi.len() * std::mem::size_of::<ProcessorPowerInformation>()) as u32,
            )
        };
        if st.0 >= 0 && !ppi.is_empty() {
            let avg = ppi.iter().map(|p| p.current_mhz as u64).sum::<u64>() / ppi.len() as u64;
            snap.cpu.current_mhz = Some(avg as u32);
            snap.cpu.max_mhz = ppi.iter().map(|p| p.max_mhz).max();
            snap.cpu.mhz_limit = ppi.iter().map(|p| p.mhz_limit).max();
            snap.cpu.per_core_mhz = ppi.iter().map(|p| p.current_mhz).collect();
        }
        snap.cpu.name = cpu_brand();
    }

    /// Adds the CPU temperature (PawnIO when switched on and working, else HWiNFO's shared
    /// memory when it is up) and the source summary.
    fn thermal(&mut self, snap: &mut Snapshot) {
        let wait = Duration::from_secs(if self.hwinfo.running { 2 } else { 30 });
        if self.hwinfo.at.is_none_or(|t| t.elapsed() >= wait) {
            self.hwinfo.at = Some(Instant::now());
            self.hwinfo.running = true;
            self.hwinfo.cpu = None;
            match super::hwinfo::read() {
                Ok(r) => match super::hwinfo::cpu_temp(&r) {
                    Some(t) => {
                        self.hwinfo.status = t.0.clone();
                        self.hwinfo.cpu = Some(t);
                    }
                    None => self.hwinfo.status = "no CPU temperature reading".into(),
                },
                Err(super::hwinfo::Error::NotRunning) => {
                    self.hwinfo.status = "not running".into();
                    self.hwinfo.running = false;
                }
                Err(super::hwinfo::Error::Parse(e)) => self.hwinfo.status = format!("unreadable ({e})"),
            }
        }
        let (pawnio_cpu, pawnio) = self.pawnio.sample(super::pawnio::enabled());
        snap.thermal.cpu = pawnio_cpu.or_else(|| self.hwinfo.cpu.clone());
        let acpi = match snap.thermal.zones.len() {
            0 => "no zones".to_string(),
            1 => "1 zone".to_string(),
            n => format!("{n} zones"),
        };
        let pawnio = pawnio.map(|s| format!(" \u{b7} PawnIO: {s}")).unwrap_or_default();
        snap.thermal.sources = format!("ACPI: {acpi}{pawnio} \u{b7} HWiNFO: {}", self.hwinfo.status);
    }

    fn memory(&mut self, snap: &mut Snapshot) {
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() {
            snap.memory.total = m.ullTotalPhys;
            snap.memory.available = m.ullAvailPhys;
            snap.memory.used_pct = if m.ullTotalPhys > 0 {
                ((m.ullTotalPhys - m.ullAvailPhys) as f64 / m.ullTotalPhys as f64 * 100.0) as f32
            } else {
                0.0
            };
            snap.memory.commit_limit = Some(m.ullTotalPageFile);
            snap.memory.commit_used = Some(m.ullTotalPageFile.saturating_sub(m.ullAvailPageFile));
            // The commit limit is physical memory plus the page files.
            snap.memory.swap_total = Some(m.ullTotalPageFile.saturating_sub(m.ullTotalPhys));
            let mut pi = windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION {
                cb: std::mem::size_of::<windows::Win32::System::ProcessStatus::PERFORMANCE_INFORMATION>() as u32,
                ..Default::default()
            };
            // SAFETY: the struct is sized as declared in cb.
            if unsafe { windows::Win32::System::ProcessStatus::GetPerformanceInfo(&mut pi, pi.cb) }.is_ok() {
                snap.memory.cached = (pi.SystemCache as u64) * (pi.PageSize as u64);
            }
        } else {
            snap.errors.push("GlobalMemoryStatusEx failed".into());
        }
    }

    fn network(&mut self, snap: &mut Snapshot) {
        // Like the disk: Windows always shows the network readouts (0 B/s if GetIfTable2 fails).
        snap.network.available = true;
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
            snap.errors.push("GetIfTable2 failed".into());
            return;
        }
        let (mut rx, mut tx, mut link) = (0u64, 0u64, 0u64);
        unsafe {
            let n = (*table).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), n);
            for r in rows {
                let hardware = r.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
                if !hardware || r.Type == IF_TYPE_SOFTWARE_LOOPBACK || r.OperStatus != IfOperStatusUp {
                    continue;
                }
                rx += r.InOctets;
                tx += r.OutOctets;
                link += r.ReceiveLinkSpeed.max(r.TransmitLinkSpeed);
            }
            FreeMibTable(table as *const _);
        }
        let now = Instant::now();
        snap.network = net_delta(self.prev_net, now, rx, tx, link);
        self.prev_net = Some((now, rx, tx));
    }
}

/// Per-core MHz from `Processor Information(*)` instances (`"group,index"`), group 0 only to
/// match the logical-processor count; `None` unless each of the `n` cores has a reading.
pub(crate) fn per_core_perf_mhz(items: &[(String, f64)], base_mhz: u32, n: usize) -> Option<Vec<u32>> {
    if n == 0 {
        return None;
    }
    let mut out = vec![None; n];
    for (name, pct) in items {
        let Some(("0", idx)) = name.split_once(',') else { continue };
        let Ok(i) = idx.parse::<usize>() else { continue };
        if let Some(slot) = out.get_mut(i) {
            *slot = perf_mhz(base_mhz, *pct);
        }
    }
    out.into_iter().collect()
}

/// Parses `pid_1234_luid_0x..._0x..._phys_0_eng_3_engtype_3D` instance names.
pub(crate) fn parse_gpu_instance(name: &str) -> Option<(u32, String, String)> {
    let pid: u32 = name.strip_prefix("pid_")?.split('_').next()?.parse().ok()?;
    let luid_start = name.find("luid_")? + 5;
    let luid: String = name[luid_start..].split('_').take(2).collect::<Vec<_>>().join("_");
    let engtype = name.rsplit_once("engtype_")?.1.to_string();
    Some((pid, luid, engtype))
}

/// The PDH `GPU Engine(*)` instances folded by [`fold_gpu`] (unparsable names are skipped).
pub(crate) fn aggregate_gpu(items: &[(String, f64)]) -> (GpuSample, HashMap<u32, f32>) {
    fold_gpu(items.iter().filter_map(|(name, v)| parse_gpu_instance(name).map(|(pid, luid, eng)| (pid, luid, eng, *v))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_instance_parsing_and_aggregation() {
        let n = "pid_1234_luid_0x00000000_0x0000D1B5_phys_0_eng_3_engtype_3D";
        assert_eq!(parse_gpu_instance(n), Some((1234, "0x00000000_0x0000D1B5".into(), "3D".into())));
        let items = vec![
            (n.to_string(), 30.0),
            ("pid_1_luid_0x00000000_0x0000D1B5_phys_0_eng_3_engtype_3D".into(), 25.0),
            ("pid_1_luid_0x00000000_0x0000D1B5_phys_0_eng_5_engtype_VideoDecode".into(), 40.0),
        ];
        let (g, per) = aggregate_gpu(&items);
        assert!((g.utilization_pct - 55.0).abs() < 0.01);
        assert_eq!(per[&1], 40.0);
        assert_eq!(per[&1234], 30.0);
    }

    #[test]
    fn clock_from_processor_performance() {
        assert_eq!(perf_mhz(3000, 100.0), Some(3000));
        assert_eq!(perf_mhz(3000, 150.0), Some(4500));
        assert_eq!(perf_mhz(3000, 1e9), Some(30000));
        assert_eq!(perf_mhz(0, 150.0), None);
        assert_eq!(perf_mhz(3000, 0.0), None);
        assert_eq!(perf_mhz(3000, f64::NAN), None);
        let items = vec![
            ("_Total".to_string(), 120.0),
            ("0,_Total".into(), 120.0),
            ("0,1".into(), 110.0),
            ("0,0".into(), 130.0),
            ("1,0".into(), 90.0),
        ];
        assert_eq!(per_core_perf_mhz(&items, 2000, 2), Some(vec![2600, 2200]));
        assert_eq!(per_core_perf_mhz(&items, 2000, 3), None);
        assert_eq!(per_core_perf_mhz(&items, 2000, 0), None);
    }

    /// The call wants a whole number of entries (a 4 KiB buffer fails with a length mismatch);
    /// one entry per logical processor reaches the per-core graphs.
    #[test]
    fn processor_performance_per_core() {
        let e = std::mem::size_of::<ProcessorPerformance>();
        let mut c = Collector::new();
        let buf = query_processor_performance(e * c.logical).unwrap();
        assert_eq!(buf.len(), e * c.logical);
        let _ = c.collect();
        std::thread::sleep(Duration::from_millis(200));
        let (s, _) = c.collect();
        assert_eq!(s.cpu.per_core_pct.len(), s.cpu.logical_count);
        assert_eq!(s.cpu.per_core_kernel_pct.len(), s.cpu.logical_count);
    }
}
