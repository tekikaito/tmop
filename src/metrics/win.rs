//! Windows metric collection.

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

/// Processor brand string from CPUID leaves 0x8000_0002..4 (cached).
pub fn cpu_brand() -> String {
    static BRAND: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BRAND
        .get_or_init(|| {
            #[cfg(target_arch = "x86_64")]
            {
                // CPUID is available on every x86-64 processor.
                let max = std::arch::x86_64::__cpuid(0x8000_0000).eax;
                if max < 0x8000_0004 {
                    return String::new();
                }
                let mut bytes = Vec::with_capacity(48);
                for leaf in 0x8000_0002u32..=0x8000_0004 {
                    let r = std::arch::x86_64::__cpuid(leaf);
                    for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                        bytes.extend_from_slice(&reg.to_le_bytes());
                    }
                }
                let s = String::from_utf8_lossy(&bytes);
                s.trim_end_matches('\0').split_whitespace().collect::<Vec<_>>().join(" ")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                String::new()
            }
        })
        .clone()
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
    prev_sys: Option<(u64, u64, u64)>,
    prev_cores: Vec<ProcessorPerformance>,
    prev_net: Option<(Instant, u64, u64)>,
    pdh: Option<Pdh>,
    procs: Enumerator,
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
        ]);
        Self {
            seq: 0,
            logical: (si.dwNumberOfProcessors as usize).max(1),
            prev_sys: None,
            prev_cores: Vec::new(),
            prev_net: None,
            pdh,
            procs: Enumerator::new(),
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
        if let Some(pdh) = &self.pdh {
            pdh.collect();
            // Disk
            if let Some(idle) = pdh.value("disk_idle") {
                snap.disk.active_pct = (100.0 - idle).clamp(0.0, 100.0) as f32;
            }
            snap.disk.read_bps = pdh.value("disk_read").unwrap_or(0.0);
            let pagefile_size = snap.memory.commit_limit.saturating_sub(snap.memory.total);
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
        } else {
            snap.errors.push("PDH unavailable: disk, GPU and thermal readings disabled".into());
        }
        let rows = match self.procs.collect(self.logical, &gpu_by_pid) {
            Ok((rows, t)) => {
                snap.process_count = t.processes;
                snap.thread_count = t.threads;
                snap.handle_count = t.handles;
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
            let cur = (ft(idle), ft(kernel), ft(user));
            if let Some(p) = self.prev_sys {
                let di = cur.0.saturating_sub(p.0) as f64;
                let dk = cur.1.saturating_sub(p.1) as f64;
                let du = cur.2.saturating_sub(p.2) as f64;
                let total = dk + du;
                if total > 0.0 {
                    snap.cpu.total_pct = (((total - di) / total) * 100.0).clamp(0.0, 100.0) as f32;
                    snap.cpu.kernel_pct = (((dk - di).max(0.0) / total) * 100.0).clamp(0.0, 100.0) as f32;
                }
            }
            self.prev_sys = Some(cur);
        }
        // Per logical processor (processor group 0 only).
        let entry = std::mem::size_of::<ProcessorPerformance>();
        if let Ok(buf) = crate::process::query_processor_performance(entry * self.logical) {
            let n = buf.len() / entry;
            let cores: Vec<ProcessorPerformance> =
                (0..n).map(|i| unsafe { std::ptr::read_unaligned(buf.as_ptr().add(i * entry).cast()) }).collect();
            if self.prev_cores.len() == cores.len() {
                snap.cpu.per_core_kernel_pct = cores
                    .iter()
                    .zip(&self.prev_cores)
                    .map(|(c, p)| {
                        let total = (c.kernel - p.kernel + c.user - p.user) as f64;
                        let busy_kernel = ((c.kernel - p.kernel) - (c.idle - p.idle)).max(0) as f64;
                        if total > 0.0 {
                            ((busy_kernel / total) * 100.0).clamp(0.0, 100.0) as f32
                        } else {
                            0.0
                        }
                    })
                    .collect();
                snap.cpu.per_core_pct = cores
                    .iter()
                    .zip(&self.prev_cores)
                    .map(|(c, p)| {
                        let total = (c.kernel - p.kernel + c.user - p.user) as f64;
                        let idle = (c.idle - p.idle) as f64;
                        if total > 0.0 {
                            (((total - idle) / total) * 100.0).clamp(0.0, 100.0) as f32
                        } else {
                            0.0
                        }
                    })
                    .collect();
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
            snap.cpu.per_core_mhz = ppi.iter().map(|p| p.current_mhz).collect();
        }
        snap.cpu.name = cpu_brand();
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
            snap.memory.commit_limit = m.ullTotalPageFile;
            snap.memory.commit_used = m.ullTotalPageFile.saturating_sub(m.ullAvailPageFile);
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
        if let Some((t, prx, ptx)) = self.prev_net {
            let dt = now.duration_since(t).as_secs_f64().max(1e-3);
            snap.network.rx_bps = rx.saturating_sub(prx) as f64 / dt;
            snap.network.tx_bps = tx.saturating_sub(ptx) as f64 / dt;
        }
        snap.network.link_bps = link as f64;
        if link > 0 {
            let bits = (snap.network.rx_bps + snap.network.tx_bps) * 8.0;
            snap.network.utilization_pct = (bits / link as f64 * 100.0).clamp(0.0, 100.0) as f32;
        }
        self.prev_net = Some((now, rx, tx));
    }
}

/// Parses `pid_1234_luid_0x..._0x..._phys_0_eng_3_engtype_3D` instance names.
pub(crate) fn parse_gpu_instance(name: &str) -> Option<(u32, String, String)> {
    let pid: u32 = name.strip_prefix("pid_")?.split('_').next()?.parse().ok()?;
    let luid_start = name.find("luid_")? + 5;
    let luid: String = name[luid_start..].split('_').take(2).collect::<Vec<_>>().join("_");
    let engtype = name.rsplit_once("engtype_")?.1.to_string();
    Some((pid, luid, engtype))
}

/// System GPU % = busiest (adapter, engine type) summed over processes;
/// per-process GPU % = that process's busiest engine type.
pub(crate) fn aggregate_gpu(items: &[(String, f64)]) -> (GpuSample, HashMap<u32, f32>) {
    let mut by_engine: HashMap<(String, String), f64> = HashMap::new();
    let mut by_pid: HashMap<u32, HashMap<String, f64>> = HashMap::new();
    for (name, v) in items {
        let Some((pid, luid, eng)) = parse_gpu_instance(name) else { continue };
        *by_engine.entry((luid, eng.clone())).or_default() += v;
        *by_pid.entry(pid).or_default().entry(eng).or_default() += v;
    }
    let mut gpu = GpuSample { available: true, ..Default::default() };
    let busiest_luid = by_engine.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|((l, _), _)| l.clone());
    if let Some(luid) = busiest_luid {
        let mut engines: Vec<(String, f32)> = by_engine
            .iter()
            .filter(|((l, _), _)| *l == luid)
            .map(|((_, e), v)| (e.clone(), v.min(100.0) as f32))
            .collect();
        engines.sort_by(|a, b| a.0.cmp(&b.0));
        gpu.utilization_pct = engines.iter().map(|e| e.1).fold(0.0, f32::max);
        gpu.engines = engines;
    }
    let per_pid = by_pid
        .into_iter()
        .map(|(pid, e)| (pid, e.values().copied().fold(0.0, f64::max).min(100.0) as f32))
        .collect();
    (gpu, per_pid)
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
}
