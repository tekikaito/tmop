//! Pieces every metrics backend shares: the CPUID brand string, tick-counter deltas, network
//! rates, the GPU engine fold and the CPU temperature label pick. Pure (bar CPUID) and tested on
//! every host.

use super::procfs::GpuDev;
use super::{DiskDrive, GpuAdapter, GpuProcessUsage, GpuSample, GpuTempKind, NetworkSample};
use std::collections::HashMap;
use std::time::Instant;

/// Processor brand string from CPUID leaves 0x8000_0002..4 (cached). Empty off x86-64, where a
/// backend reads its OS's name instead.
pub fn cpu_brand() -> String {
    static BRAND: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BRAND
        .get_or_init(|| {
            #[cfg(target_arch = "x86_64")]
            {
                // `__cpuid` is safe since Rust 1.87; older toolchains still want the block.
                #[allow(unused_unsafe)]
                // SAFETY: CPUID is available on every x86-64 processor.
                let cpuid = |leaf: u32| unsafe { std::arch::x86_64::__cpuid(leaf) };
                let max = cpuid(0x8000_0000).eax;
                if max < 0x8000_0004 {
                    return String::new();
                }
                let mut bytes = Vec::with_capacity(48);
                for leaf in 0x8000_0002u32..=0x8000_0004 {
                    let r = cpuid(leaf);
                    for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                        bytes.extend_from_slice(&reg.to_le_bytes());
                    }
                }
                brand_from_bytes(&bytes)
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                String::new()
            }
        })
        .clone()
}

/// The brand bytes without NUL padding and with runs of spaces collapsed.
pub fn brand_from_bytes(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_end_matches('\0').split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cumulative CPU time of one processor (or all): idle, kernel and user, in any one unit.
/// `kernel` includes `idle` (the Windows convention); a backend whose counters keep them apart
/// (Linux jiffies, mach ticks) adds idle to its kernel figure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ticks {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

/// (busy %, kernel %) between two readings; None when no time passed.
pub fn tick_delta_pct(prev: Ticks, cur: Ticks) -> Option<(f32, f32)> {
    let di = cur.idle.saturating_sub(prev.idle) as f64;
    let dk = cur.kernel.saturating_sub(prev.kernel) as f64;
    let du = cur.user.saturating_sub(prev.user) as f64;
    let total = dk + du;
    if total <= 0.0 {
        return None;
    }
    Some(((((total - di) / total) * 100.0).clamp(0.0, 100.0) as f32, (((dk - di).max(0.0) / total) * 100.0).clamp(0.0, 100.0) as f32))
}

/// Last cumulative network reading: (when, received octets, sent octets).
pub type NetMark = (Instant, u64, u64);

/// Rates and link utilisation from cumulative octet counters summed over the up hardware
/// interfaces; `link_bps` is their summed link speed (0 when unknown). Rates stay 0 on the first
/// reading.
pub fn net_delta(prev: Option<NetMark>, now: Instant, rx: u64, tx: u64, link_bps: u64) -> NetworkSample {
    let mut n = NetworkSample { available: true, ..Default::default() };
    if let Some((t, prx, ptx)) = prev {
        let dt = now.duration_since(t).as_secs_f64().max(1e-3);
        n.rx_bps = rx.saturating_sub(prx) as f64 / dt;
        n.tx_bps = tx.saturating_sub(ptx) as f64 / dt;
    }
    n.link_bps = link_bps as f64;
    if link_bps > 0 {
        let bits = (n.rx_bps + n.tx_bps) * 8.0;
        n.utilization_pct = (bits / link_bps as f64 * 100.0).clamp(0.0, 100.0) as f32;
    }
    n
}

/// One physical disk's cumulative counters (Windows `IOCTL_DISK_PERFORMANCE`): bytes moved, and
/// idle and query time in 100 ns units.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskCounters {
    pub read: u64,
    pub written: u64,
    pub idle: i64,
    pub query: i64,
}

/// One Windows disk between two readings: (disk number, its volumes as "C: D:", active time in
/// %, read B/s, write B/s).
pub type DiskUse = (u32, String, f32, f64, f64);

/// Each disk's active time and throughput between two readings of the same disks, keyed by
/// device number and without volumes; a disk not read both times is left out.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn disk_deltas(prev: &[(u32, DiskCounters)], now: &[(u32, DiskCounters)]) -> Vec<DiskUse> {
    let mut out = Vec::with_capacity(now.len());
    for (id, n) in now {
        let Some((_, p)) = prev.iter().find(|(pid, _)| pid == id) else { continue };
        let span = n.query - p.query;
        if span <= 0 {
            continue;
        }
        let secs = span as f64 / 1e7;
        let idle = ((n.idle - p.idle) as f64 / span as f64).clamp(0.0, 1.0);
        out.push((*id, String::new(), ((1.0 - idle) * 100.0) as f32, n.read.saturating_sub(p.read) as f64 / secs, n.written.saturating_sub(p.written) as f64 / secs));
    }
    out
}

/// The grouped view of `disks`: active time as their average (as PDH's `_Total`) and summed
/// throughput; `None` without a disk.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn group_disks(disks: &[DiskUse]) -> Option<(f32, f64, f64)> {
    let busy: f64 = disks.iter().map(|d| d.2 as f64).sum();
    (!disks.is_empty()).then(|| ((busy / disks.len() as f64) as f32, disks.iter().map(|d| d.3).sum(), disks.iter().map(|d| d.4).sum()))
}

/// Disk number and volumes of a PDH `PhysicalDisk` instance: "0 C: D:" is disk 0 with "C: D:",
/// "1" a disk without a drive letter. `None` for `_Total`.
pub fn parse_disk_instance(name: &str) -> Option<(u32, &str)> {
    let (number, volumes) = name.split_once(' ').unwrap_or((name, ""));
    Some((number.parse().ok()?, volumes.trim()))
}

/// PDH's `PhysicalDisk(*)` idle time, read rate and write rate joined by instance into one
/// reading per disk, in disk order. A disk without an idle reading is left out.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn pdh_disks(idle: &[(String, f64)], read: &[(String, f64)], write: &[(String, f64)]) -> Vec<DiskUse> {
    let of = |items: &[(String, f64)], name: &str| items.iter().find(|(n, _)| n == name).map_or(0.0, |(_, v)| v.max(0.0));
    let mut out: Vec<DiskUse> = idle
        .iter()
        .filter_map(|(name, idle)| {
            let (number, volumes) = parse_disk_instance(name)?;
            Some((number, volumes.to_string(), (100.0 - idle).clamp(0.0, 100.0) as f32, of(read, name), of(write, name)))
        })
        .collect();
    out.sort_by_key(|d| d.0);
    out.dedup_by_key(|d| d.0);
    out
}

/// A disk with its volumes as Windows Task Manager writes it: "Disk 0 (C: D:)", or "Disk 1"
/// without a drive letter.
pub fn disk_title(name: &str, volumes: &str) -> String {
    if volumes.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({volumes})")
    }
}

/// How full the file systems (Windows: volumes) are: those wholly on one disk per disk, and all
/// of them together.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Storage {
    /// Per disk id, the (used, total) of the file systems lying on that disk alone. A disk whose
    /// file systems are all shared with other disks has no entry.
    pub each: HashMap<String, (u64, u64)>,
    /// Every file system once, whether it lies on one disk or several; `None` without any.
    pub all: Option<(u64, u64)>,
}

/// Splits file systems into [`Storage`]. `volumes` has, for each file system, the disks it lies
/// on and its (used, total). One on several disks (RAID, LVM, a spanned or mirrored volume) is
/// counted once in the total and on no single disk: its size is no disk's own, and adding it to
/// each would count it again.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub fn split_storage(volumes: &[(Vec<String>, (u64, u64))]) -> Storage {
    let mut s = Storage::default();
    let add = |sum: &mut (u64, u64), (used, total): (u64, u64)| *sum = (sum.0.saturating_add(used), sum.1.saturating_add(total));
    for (disks, space) in volumes {
        add(s.all.get_or_insert((0, 0)), *space);
        let mut disks = disks.clone();
        disks.sort();
        disks.dedup();
        if let [only] = disks.as_slice() {
            add(s.each.entry(only.clone()).or_default(), *space);
        }
    }
    s
}

/// The drive letters of Windows disks as [`Storage`]. Each letter is asked of `space_of` once
/// as (used, total); a letter that does not answer (locked, no medium) is left out. A letter
/// that more than one disk lists (a spanned or mirrored volume, Storage Spaces) is shared.
/// Disks are keyed by their number as text, as [`windows_drive`] does.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn volumes_storage(disks: &[DiskUse], space_of: impl Fn(&str) -> Option<(u64, u64)>) -> Storage {
    let mut letters: Vec<(&str, Vec<String>)> = Vec::new();
    for d in disks {
        for letter in d.1.split_whitespace() {
            match letters.iter_mut().find(|(l, _)| *l == letter) {
                Some((_, on)) => on.push(d.0.to_string()),
                None => letters.push((letter, vec![d.0.to_string()])),
            }
        }
    }
    let volumes: Vec<_> = letters.into_iter().filter_map(|(l, on)| space_of(l).map(|s| (on, s))).collect();
    split_storage(&volumes)
}

/// A Windows disk for the snapshot.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn windows_drive(d: &DiskUse, model: &str, firmware: &str) -> DiskDrive {
    let name = format!("Disk {}", d.0);
    DiskDrive { id: d.0.to_string(), title: disk_title(&name, &d.1), name, volumes: d.1.clone(), model: model.to_string(), firmware: firmware.to_string(), active_pct: d.2, read_bps: d.3, write_bps: d.4, space: None }
}

/// A disk's model from the vendor and product its device reports, either of which may be empty
/// or padded: "SanDisk" and "Ultra" give "SanDisk Ultra". A product that names its vendor
/// ("Samsung SSD 990 PRO") stays as it is, as does one under a bus name ("NVMe", "ATA") that
/// drivers put where the vendor goes.
pub fn disk_model(vendor: &str, product: &str) -> String {
    let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (vendor, product) = (squeeze(vendor), squeeze(product));
    let lower = vendor.to_ascii_lowercase();
    let bus = ["nvme", "ata", "usb", "generic"].contains(&lower.as_str());
    if product.is_empty() {
        if bus { String::new() } else { vendor }
    } else if vendor.is_empty() || bus || product.to_ascii_lowercase().starts_with(&lower) {
        product
    } else {
        format!("{vendor} {product}")
    }
}

/// The NUL-terminated string at `offset` of a storage descriptor's bytes, trimmed. An offset of 0
/// means the device has none; one past the end of what was returned gives nothing, not a panic.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn descriptor_text(bytes: &[u8], offset: u32) -> String {
    let tail = bytes.get(offset as usize..).filter(|_| offset > 0).unwrap_or_default();
    String::from_utf8_lossy(&tail[..tail.iter().position(|b| *b == 0).unwrap_or(tail.len())]).trim().to_string()
}

/// A disk's firmware revision as its device reports it: trimmed, inner runs of blanks kept as one.
pub fn disk_firmware(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One engine reading: (pid, adapter, engine type, %).
pub type EngineUse = (u32, String, String, f64);

/// System GPU % = busiest (adapter, engine type) summed over processes; per-process GPU % = that
/// process's busiest engine type. The engines listed are the busiest adapter's.
pub fn fold_gpu(items: impl IntoIterator<Item = EngineUse>) -> (GpuSample, HashMap<u32, f32>) {
    let mut by_engine: HashMap<(String, String), f64> = HashMap::new();
    let mut by_pid: HashMap<u32, HashMap<String, f64>> = HashMap::new();
    for (pid, adapter, eng, v) in items {
        *by_engine.entry((adapter, eng.clone())).or_default() += v;
        *by_pid.entry(pid).or_default().entry(eng).or_default() += v;
    }
    let mut gpu = GpuSample { available: true, ..Default::default() };
    let busiest = by_engine.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|((l, _), _)| l.clone());
    if let Some(adapter) = busiest {
        let mut engines: Vec<(String, f32)> = by_engine.iter().filter(|((l, _), _)| *l == adapter).map(|((_, e), v)| (e.clone(), v.min(100.0) as f32)).collect();
        engines.sort_by(|a, b| a.0.cmp(&b.0));
        gpu.utilization_pct = engines.iter().map(|e| e.1).fold(0.0, f32::max);
        gpu.engines = engines;
    }
    let per_pid = by_pid.into_iter().map(|(pid, e)| (pid, e.values().copied().fold(0.0, f64::max).min(100.0) as f32)).collect();
    (gpu, per_pid)
}

/// A GPU the OS lists: (id, name, total dedicated memory, shared system memory it may use).
pub type KnownAdapter = (String, String, Option<u64>, Option<u64>);

/// The physical memory size of a GPU named `name`, from the display drivers' (name, size) list in
/// the registry, when it is at least what DXGI reports. DXGI leaves out what the hardware reserves
/// (345 MB on a 12 GB card), Task Manager shows the full size. Several drivers of that name must
/// agree, so two cards of one name with different memory keep DXGI's figure.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn physical_vram(name: &str, dxgi: u64, drivers: &[(String, u64)]) -> Option<u64> {
    let mut sizes = drivers.iter().filter(|(n, s)| n.trim().eq_ignore_ascii_case(name.trim()) && *s > 0).map(|d| d.1);
    let first = sizes.next()?;
    (sizes.all(|s| s == first) && first >= dxgi).then_some(first)
}

/// The grouped memory figures from each GPU's: dedicated sizes and shared use add up; the shared
/// limit is the largest GPU's, since every GPU draws on the same system RAM.
pub fn group_memory(g: &mut GpuSample) {
    let sum = |v: Vec<u64>| (!v.is_empty()).then(|| v.iter().sum());
    g.dedicated_total = sum(g.adapters.iter().filter_map(|a| a.dedicated_total).filter(|t| *t > 0).collect());
    g.shared_used = sum(g.adapters.iter().filter_map(|a| a.shared_used).collect());
    g.shared_total = g.adapters.iter().filter_map(|a| a.shared_total).filter(|t| *t > 0).max();
}

/// Memory counters by adapter id. `None`: not read (see [`super::gpu_detail`]), not zero.
#[derive(Clone, Debug, Default)]
pub struct GpuMemory {
    pub adapter_dedicated: HashMap<String, u64>,
    pub adapter_shared: Option<HashMap<String, u64>>,
    /// (pid, adapter) -> (dedicated, shared).
    pub process: Option<HashMap<(u32, String), (u64, u64)>>,
}

/// Each known GPU with its engines and memory, and each process on each GPU. Same arithmetic as
/// [`fold_gpu`], one adapter at a time. Readings of adapters not in `known`, of pid 0 and of
/// processes that use nothing are left out; `with_processes` false leaves all processes out.
pub fn fold_adapters(known: &[KnownAdapter], engines: &[EngineUse], mem: &GpuMemory, with_processes: bool) -> (Vec<GpuAdapter>, Vec<GpuProcessUsage>) {
    // One card can be listed twice (an indirect display's second LUID); only its real LUID has
    // counters. Without any counters there is nothing to tell them apart by, so all stay.
    let counted = |id: &String| engines.iter().any(|u| u.1 == *id) || mem.adapter_dedicated.contains_key(id);
    let live: Vec<&KnownAdapter> = known.iter().filter(|k| counted(&k.0)).collect();
    let known: Vec<&KnownAdapter> = if live.is_empty() { known.iter().collect() } else { live };
    let sorted = |m: &HashMap<String, f64>| {
        let mut v: Vec<(String, f32)> = m.iter().map(|(e, u)| (e.clone(), u.min(100.0) as f32)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let busiest = |e: &[(String, f32)]| e.iter().map(|e| e.1).fold(0.0, f32::max);
    let adapters = known
        .iter()
        .map(|(id, name, total, shared_total)| {
            let mut by_engine: HashMap<String, f64> = HashMap::new();
            for (_, _, e, v) in engines.iter().filter(|u| u.1 == *id) {
                *by_engine.entry(e.clone()).or_default() += v;
            }
            let engines = sorted(&by_engine);
            GpuAdapter {
                id: id.clone(),
                name: name.clone(),
                utilization_pct: busiest(&engines),
                engines,
                dedicated_used: mem.adapter_dedicated.get(id).copied(),
                dedicated_total: total.filter(|t| *t > 0),
                shared_used: mem.adapter_shared.as_ref().and_then(|m| m.get(id).copied()),
                shared_total: shared_total.filter(|t| *t > 0),
                ..Default::default()
            }
        })
        .collect();
    let mut processes = Vec::new();
    if with_processes {
        let mut by_key: HashMap<(u32, String), HashMap<String, f64>> = HashMap::new();
        for (pid, a, e, v) in engines {
            *by_key.entry((*pid, a.clone())).or_default().entry(e.clone()).or_default() += v;
        }
        for key in mem.process.iter().flat_map(|m| m.keys()) {
            by_key.entry(key.clone()).or_default();
        }
        for (key, by_engine) in by_key {
            let (pid, gpu_id) = key.clone();
            let m = mem.process.as_ref().and_then(|m| m.get(&key));
            let mut engines = sorted(&by_engine);
            engines.sort_by(|a, b| b.1.total_cmp(&a.1));
            let utilization_pct = busiest(&engines);
            let idle = utilization_pct < 0.05 && m.is_none_or(|m| m.0 == 0 && m.1 == 0);
            if pid != 0 && !idle && known.iter().any(|k| k.0 == gpu_id) {
                processes.push(GpuProcessUsage { pid, gpu_id, utilization_pct, engines, dedicated: m.map(|m| m.0), shared: m.map(|m| m.1) });
            }
        }
    }
    (adapters, processes)
}

/// Adds the sensors of `devs` to the adapter of the same name. Sources do not share an index or
/// id, so a name that several GPUs have stays unattributed: no reading beats the other card's.
pub fn merge_sensors(adapters: &mut [GpuAdapter], devs: &[GpuDev]) {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let names: Vec<String> = adapters.iter().map(|a| a.name.clone()).collect();
    for a in adapters.iter_mut() {
        let mut matches = devs.iter().filter(|d| same(&d.name, &a.name));
        let (Some(d), None) = (matches.next(), matches.next()) else { continue };
        if names.iter().filter(|n| same(n, &a.name)).count() == 1 {
            if let Some(t) = d.temperature_c {
                a.offer_temp(GpuTempKind::Gpu, t, "NVML");
            }
            a.power_w = d.power_w;
            a.clock_mhz = d.clock_mhz;
            a.dedicated_total = a.dedicated_total.or(d.vram_total);
        }
    }
}

/// WDDM's `D3DKMT_ADAPTER_PERFDATA::Temperature` ("1 = 0.1C") as °C. 0 is what a driver that
/// fills nothing leaves (integrated GPUs), so it is no reading; nor is anything above 200 °C.
pub fn deci_celsius(v: u32) -> Option<f32> {
    (v > 0 && v <= 2000).then(|| v as f32 / 10.0)
}

/// The adapter id [`super::GpuAdapter::id`] Windows gives a GPU (`0x{high:08x}_0x{low:08x}`, the
/// LUID as DXGI and the GPU performance counters write it) back as (HighPart, LowPart).
pub fn parse_luid(id: &str) -> Option<(i32, u32)> {
    let (high, low) = id.split_once('_')?;
    let hex = |s: &str| u32::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?, 16).ok();
    Some((hex(high)? as i32, hex(low)?))
}

/// A GPU name for comparing sources: lowercase, without "(TM)" / "(R)", spaces collapsed.
fn gpu_name_key(name: &str) -> String {
    let lower = name.to_lowercase().replace("(tm)", " ").replace("(r)", " ");
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Which of `adapters` (by name) a sensor named `sensor` belongs to: HWiNFO names a GPU's sensor
/// "GPU [#0]: NVIDIA GeForce RTX 5070: " (the card's name between colons). The adapter whose whole
/// name is one of those parts ("RTX 5070" is not "RTX 5070 Ti"); none when that name belongs to
/// two adapters, as nothing tells which card the sensor is on.
pub fn match_gpu_sensor(adapters: &[&str], sensor: &str) -> Option<usize> {
    let parts: Vec<String> = sensor.split(':').map(gpu_name_key).filter(|p| !p.is_empty()).collect();
    let keys: Vec<String> = adapters.iter().map(|a| gpu_name_key(a)).collect();
    let i = keys.iter().position(|k| !k.is_empty() && parts.contains(k))?;
    (keys.iter().filter(|k| **k == keys[i]).count() == 1).then_some(i)
}

/// Offers HWiNFO's GPU temperatures to the adapter each sensor names ([`match_gpu_sensor`]); a
/// sensor no single adapter matches is left out.
pub fn offer_hwinfo_temps(adapters: &mut [GpuAdapter], readings: &[super::hwinfo::GpuReading]) {
    let names: Vec<String> = adapters.iter().map(|a| a.name.clone()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    for r in readings {
        if let Some(i) = match_gpu_sensor(&names, &r.sensor) {
            adapters[i].offer_temp(r.kind, r.c, "HWiNFO");
        }
    }
}

/// The sources of the GPUs' temperatures for the Thermals subtitle: "GPU: HWiNFO, WDDM", or
/// "GPU: not reported".
pub fn gpu_sources(adapters: &[GpuAdapter]) -> String {
    let mut by: Vec<&str> = adapters.iter().flat_map(|a| a.temps.iter().map(|t| t.source)).collect();
    by.sort_unstable();
    by.dedup();
    format!("GPU: {}", if by.is_empty() { "not reported".to_string() } else { by.join(", ") })
}

/// The GPUs' temperatures for [`super::ThermalSample::gpu`]: "<name> · <reading> (<source>)". A
/// name two GPUs share gets their position ("#2") so the rows stay apart.
pub fn gpu_thermal(adapters: &[GpuAdapter]) -> Vec<(String, f32)> {
    let mut out = Vec::new();
    for (i, a) in adapters.iter().enumerate() {
        let name = if a.name.is_empty() { "GPU" } else { a.name.as_str() };
        let twin = adapters.iter().filter(|b| b.name == a.name).count() > 1;
        let name = if twin { format!("{name} #{}", i + 1) } else { name.to_string() };
        out.extend(a.temps.iter().map(|t| (format!("{name} \u{b7} {} ({})", t.kind.label(), t.source), t.c)));
    }
    out
}

/// HWiNFO's CPU temperature readings, best first.
pub const HWINFO_CPU_LABELS: [&str; 5] = ["CPU Package", "CPU (Tctl/Tdie)", "CPU Tctl/Tdie", "CPU Die (average)", "CPU CCD1 (Tdie)"];

/// hwmon CPU temperature labels, best first: k10temp / zenpower, then coretemp.
pub const HWMON_CPU_LABELS: [&str; 4] = ["Tctl", "Tdie", "Package id 0", "Tccd1"];

/// The CPU temperature among `(label, °C)` readings: the first label of `priority` present (case
/// ignored), else the first reading whose label starts with "CPU".
pub fn pick_cpu_temp<'a>(temps: &[(&'a str, f32)], priority: &[&str]) -> Option<(&'a str, f32)> {
    priority
        .iter()
        .find_map(|l| temps.iter().find(|t| t.0.eq_ignore_ascii_case(l)))
        .or_else(|| temps.iter().find(|t| t.0.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("cpu"))))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn disk_counters_give_active_time_and_rates() {
        let c = |read, written, idle, query| DiskCounters { read, written, idle, query };
        // Disk 0: half a second busy of one second, 2 MB read; disk 1 idle; disk 2 new (skipped).
        let prev = [(0, c(1_000, 0, 0, 0)), (1, c(0, 0, 0, 0))];
        let now = [(0, c(2_001_000, 500, 5_000_000, 10_000_000)), (1, c(0, 0, 10_000_000, 10_000_000)), (2, c(9, 9, 0, 9))];
        let each = disk_deltas(&prev, &now);
        assert_eq!(each, [(0, String::new(), 50.0, 2_000_000.0, 500.0), (1, String::new(), 0.0, 0.0, 0.0)]);
        let (active, r, w) = group_disks(&each).unwrap();
        assert!((active - 25.0).abs() < 1e-3, "{active}");
        assert_eq!((r, w), (2_000_000.0, 500.0));
        // First reading, or a disk whose clock did not move: nothing to report.
        assert_eq!(group_disks(&disk_deltas(&[], &now)), None);
        assert_eq!(group_disks(&disk_deltas(&now, &now)), None);
    }

    #[test]
    fn pdh_disk_instances_become_one_reading_per_disk() {
        assert_eq!(parse_disk_instance("0 C: D:"), Some((0, "C: D:")));
        assert_eq!(parse_disk_instance("12"), Some((12, "")));
        assert_eq!(parse_disk_instance("_Total"), None);
        assert_eq!(parse_disk_instance(""), None);
        let v = |items: &[(&str, f64)]| items.iter().map(|(n, v)| (n.to_string(), *v)).collect::<Vec<_>>();
        // Out of order, with the total, an idle time above 100 and a disk without rates.
        let idle = v(&[("_Total", 70.0), ("1 E:", 40.0), ("0 C: D:", 100.4), ("2", 99.0)]);
        let read = v(&[("0 C: D:", 10.0), ("1 E:", 2000.0), ("_Total", 2010.0)]);
        let write = v(&[("1 E:", 300.0), ("0 C: D:", -1.0)]);
        let disks = pdh_disks(&idle, &read, &write);
        assert_eq!(disks, [(0, "C: D:".to_string(), 0.0, 10.0, 0.0), (1, "E:".to_string(), 60.0, 2000.0, 300.0), (2, String::new(), 1.0, 0.0, 0.0)]);
        assert!(pdh_disks(&[], &read, &write).is_empty());
        let d = windows_drive(&disks[1], "Samsung SSD 990 PRO 2TB", "4B2QJXD7");
        assert_eq!((d.id.as_str(), d.name.as_str(), d.volumes.as_str(), d.title.as_str(), d.model.as_str()), ("1", "Disk 1", "E:", "Disk 1 (E:)", "Samsung SSD 990 PRO 2TB"));
        assert_eq!((d.active_pct, d.read_bps, d.write_bps), (60.0, 2000.0, 300.0));
        assert_eq!(windows_drive(&disks[2], "", "").title, "Disk 2");
    }

    #[test]
    fn disk_titles_and_models() {
        assert_eq!(disk_title("Disk 0", "C: D:"), "Disk 0 (C: D:)");
        assert_eq!(disk_title("nvme0n1", ""), "nvme0n1");
        assert_eq!(disk_model("", " Lexar SSD NM790 4TB  "), "Lexar SSD NM790 4TB");
        assert_eq!(disk_model("SanDisk ", "Ultra   Fit"), "SanDisk Ultra Fit");
        assert_eq!(disk_model("Samsung", "SAMSUNG MZVL21T0"), "SAMSUNG MZVL21T0");
        assert_eq!(disk_model("NVMe", "KINGSTON SNV2S1000G"), "KINGSTON SNV2S1000G");
        assert_eq!(disk_model("ATA", "WDC WD40EFRX-68N"), "WDC WD40EFRX-68N");
        assert_eq!(disk_model("Msft", ""), "Msft");
        assert_eq!(disk_model("ATA", ""), "");
        assert_eq!(disk_model("", ""), "");
    }

    #[test]
    fn firmware_text() {
        assert_eq!(disk_firmware("  4B2QJXD7\n"), "4B2QJXD7");
        assert_eq!(disk_firmware(""), "");
        let d = b"hdr\0ABC \0xyz";
        assert_eq!(descriptor_text(d, 4), "ABC");
        assert_eq!(descriptor_text(d, 0), "");
        assert_eq!(descriptor_text(d, 99), "");
        assert_eq!(descriptor_text(d, 12), "");
        assert_eq!(descriptor_text(b"RV01", 0), "");
        assert_eq!(descriptor_text(b"x\0RV01", 2), "RV01");
    }

    #[test]
    fn a_volume_on_several_disks_counts_once_and_on_none() {
        let space = |v: &str| match v {
            "C:" => Some((300, 1000)),
            "D:" => Some((50, 500)),
            "S:" => Some((10, 2000)),
            _ => None,
        };
        let disk = |n: u32, v: &str| (n, v.to_string(), 0.0, 0.0, 0.0);
        // Disk 0 holds C: and D:; S: is spread over disks 1 and 2.
        let s = volumes_storage(&[disk(0, "C: D:"), disk(1, "S:"), disk(2, "S:")], space);
        assert_eq!(s.each.get("0"), Some(&(350, 1500)));
        assert_eq!(s.each.get("1"), None);
        assert_eq!(s.each.get("2"), None);
        assert_eq!(s.all, Some((360, 3500)));
        // A locked volume is left out; a disk without a volume that answers has no figure.
        let s = volumes_storage(&[disk(0, "E: D:"), disk(1, "E:"), disk(2, "")], space);
        assert_eq!((s.each.get("0").copied(), s.each.get("1"), s.each.get("2"), s.all), (Some((50, 500)), None, None, Some((50, 500))));
        assert_eq!(volumes_storage(&[disk(0, "E:")], space), Storage::default());
        assert_eq!(volumes_storage(&[], space), Storage::default());
    }

    #[test]
    fn storage_counts_each_file_system_once() {
        let on = |disks: &[&str], used, total| (disks.iter().map(|d| d.to_string()).collect::<Vec<_>>(), (used, total));
        let tb = 1u64 << 40;
        // RAID1 of two 4 TB disks with one 4 TB file system 1 TB full: df says 1 / 4 TB.
        let s = split_storage(&[on(&["sda", "sdb"], tb, 4 * tb)]);
        assert_eq!(s.all, Some((tb, 4 * tb)));
        assert!(s.each.is_empty());
        // RAID0 / LVM over two 1 TB disks: 2 TB on neither disk.
        let s = split_storage(&[on(&["sda", "sdb"], tb, 2 * tb), on(&["sdc"], 5, 10), on(&["sdc"], 1, 2)]);
        assert_eq!(s.all, Some((tb + 6, 2 * tb + 12)));
        assert_eq!(s.each.get("sdc"), Some(&(6, 12)));
        assert_eq!((s.each.get("sda"), s.each.get("sdb")), (None, None));
        // A disk listed twice for one file system is still one disk.
        let s = split_storage(&[on(&["sda", "sda"], 1, 2)]);
        assert_eq!(s.each.get("sda"), Some(&(1, 2)));
        assert_eq!(split_storage(&[]), Storage::default());
    }

    #[test]
    fn brand_bytes_are_trimmed_and_collapsed() {
        let mut b = b"  AMD Ryzen 7   7840U w/ Radeon  ".to_vec();
        b.resize(48, 0);
        assert_eq!(brand_from_bytes(&b), "AMD Ryzen 7 7840U w/ Radeon");
        assert_eq!(brand_from_bytes(&[0; 48]), "");
        // Cached and stable.
        assert_eq!(cpu_brand(), cpu_brand());
    }

    #[test]
    fn tick_deltas() {
        let t = |idle, kernel, user| Ticks { idle, kernel, user };
        // 1000 ticks: 600 idle, 300 kernel busy, 100 user.
        let (busy, kern) = tick_delta_pct(t(0, 0, 0), t(600, 900, 100)).unwrap();
        assert!((busy - 40.0).abs() < 1e-4 && (kern - 30.0).abs() < 1e-4, "{busy} {kern}");
        assert_eq!(tick_delta_pct(t(5, 5, 5), t(5, 5, 5)), None);
        // Counters that went backwards count as no time, never as negative shares.
        assert_eq!(tick_delta_pct(t(10, 10, 10), t(0, 0, 0)), None);
        let (busy, kern) = tick_delta_pct(t(100, 100, 0), t(50, 300, 0)).unwrap();
        assert_eq!((busy, kern), (100.0, 100.0));
    }

    #[test]
    fn net_rates_and_utilization() {
        let t0 = Instant::now();
        let first = net_delta(None, t0, 1000, 2000, 0);
        assert!(first.available);
        assert_eq!((first.rx_bps, first.tx_bps, first.utilization_pct), (0.0, 0.0, 0.0));
        let t1 = t0 + Duration::from_secs(2);
        let n = net_delta(Some((t0, 1000, 2000)), t1, 3000, 2500, 8_000);
        assert_eq!((n.rx_bps, n.tx_bps, n.link_bps), (1000.0, 250.0, 8000.0));
        assert!((n.utilization_pct - 100.0).abs() < 1e-4);
        let n = net_delta(Some((t0, 1000, 2000)), t1, 1000, 2000, 1_000_000_000);
        assert_eq!(n.utilization_pct, 0.0);
        // A counter reset reads as no traffic.
        assert_eq!(net_delta(Some((t0, 5000, 5000)), t1, 10, 10, 0).rx_bps, 0.0);
    }

    #[test]
    fn adapters_and_processes_fold_per_gpu() {
        let e = |p: u32, a: &str, n: &str, v: f64| (p, a.to_string(), n.to_string(), v);
        let known = vec![("a".to_string(), "RTX 4090".to_string(), Some(24), Some(32)), ("b".to_string(), "RTX 4070".to_string(), None, None)];
        let uses = [e(1, "a", "3D", 60.0), e(2, "a", "3D", 60.0), e(1, "a", "Copy", 5.0), e(1, "b", "Compute", 30.0), e(0, "a", "3D", 1.0), e(9, "ghost", "3D", 50.0), e(3, "b", "3D", 0.0)];
        let mem = GpuMemory {
            adapter_dedicated: [("a".to_string(), 7)].into(),
            adapter_shared: None,
            process: Some([((1, "a".to_string()), (5, 2)), ((4, "b".to_string()), (8, 0)), ((5, "b".to_string()), (0, 0))].into()),
        };
        let (adapters, mut procs) = fold_adapters(&known, &uses, &mem, true);
        assert_eq!(adapters[0].utilization_pct, 100.0, "summed over processes, capped");
        assert_eq!((adapters[0].dedicated_used, adapters[0].dedicated_total, adapters[0].shared_used), (Some(7), Some(24), None));
        assert_eq!((adapters[1].utilization_pct, adapters[1].dedicated_used), (30.0, None));
        assert_eq!((adapters[0].shared_total, adapters[1].shared_total), (Some(32), None));
        procs.sort_by_key(|p| (p.pid, p.gpu_id.clone()));
        let got: Vec<(u32, &str, f32)> = procs.iter().map(|p| (p.pid, p.gpu_id.as_str(), p.utilization_pct)).collect();
        // A process on both GPUs is listed for each; pid 0, unknown adapters and idle ones are not.
        assert_eq!(got, [(1, "a", 60.0), (1, "b", 30.0), (2, "a", 60.0), (4, "b", 0.0)]);
        assert_eq!(procs[0].engines[0].0, "3D", "busiest engine first");
        assert_eq!((procs[0].dedicated, procs[0].shared), (Some(5), Some(2)));
        assert_eq!(procs[1].dedicated, None, "no memory row is unknown, not zero");
        assert!(fold_adapters(&known, &uses, &mem, false).1.is_empty());
    }

    #[test]
    fn a_listed_adapter_without_counters_is_a_duplicate() {
        let k = |id: &str| (id.to_string(), "RTX".to_string(), None, None);
        let known = [k("real"), k("idle"), k("ghost")];
        let uses = [(1, "real".to_string(), "3D".to_string(), 5.0)];
        let mem = GpuMemory { adapter_dedicated: [("idle".to_string(), 1)].into(), ..Default::default() };
        let ids = |a: Vec<GpuAdapter>| a.into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(ids(fold_adapters(&known, &uses, &mem, false).0), ["real", "idle"]);
        assert_eq!(ids(fold_adapters(&known, &[], &GpuMemory::default(), false).0), ["real", "idle", "ghost"], "no counters at all");
    }

    #[test]
    fn physical_vram_takes_the_drivers_size() {
        let d = |n: &str, s: u64| (n.to_string(), s);
        let drivers = [d("NVIDIA GeForce RTX 5070", 12 << 30), d("AMD Radeon(TM) Graphics", 512 << 20), d("Twin", 8 << 30), d("Twin", 12 << 30)];
        // 12 GB card of which DXGI reports 11.66 GB.
        assert_eq!(physical_vram("NVIDIA GeForce RTX 5070", 12_520_000_000, &drivers), Some(12 << 30));
        assert_eq!(physical_vram(" amd radeon(tm) graphics", 509_400_000, &drivers), Some(512 << 20));
        // Ambiguous name, no driver entry, or a registry size below DXGI's: DXGI stays.
        assert_eq!(physical_vram("Twin", 1, &drivers), None);
        assert_eq!(physical_vram("Intel Arc", 1, &drivers), None);
        assert_eq!(physical_vram("AMD Radeon(TM) Graphics", 1 << 30, &drivers), None);
    }

    #[test]
    fn grouped_memory_adds_sizes_and_takes_the_largest_shared_limit() {
        let a = |d: Option<u64>, su: Option<u64>, st: Option<u64>| GpuAdapter { dedicated_total: d, shared_used: su, shared_total: st, ..Default::default() };
        let mut g = GpuSample { adapters: vec![a(Some(6), Some(1), Some(16)), a(Some(2), None, Some(14)), a(None, Some(3), None)], ..Default::default() };
        group_memory(&mut g);
        assert_eq!((g.dedicated_total, g.shared_used, g.shared_total), (Some(8), Some(4), Some(16)));
        // Nothing known stays unknown rather than 0.
        let mut g = GpuSample { adapters: vec![a(None, None, None)], ..Default::default() };
        group_memory(&mut g);
        assert_eq!((g.dedicated_total, g.shared_used, g.shared_total), (None, None, None));
    }

    #[test]
    fn sensors_go_only_to_an_unambiguous_name() {
        let a = |n: &str| GpuAdapter { name: n.into(), ..Default::default() };
        let d = |n: &str, t: f32| GpuDev { name: n.into(), temperature_c: Some(t), vram_total: Some(8), ..Default::default() };
        let mut adapters = [a("NVIDIA GeForce RTX 4090"), a("AMD Radeon"), a("Twin"), a("Twin")];
        merge_sensors(&mut adapters, &[d("nvidia geforce rtx 4090", 60.0), d("Twin", 70.0), d("Twin", 71.0)]);
        assert_eq!((adapters[0].temperature_c, adapters[0].dedicated_total), (Some(60.0), Some(8)));
        assert!(adapters[1..].iter().all(|a| a.temperature_c.is_none()));
        // One sensor but two cards of that name: still unknown which card it is.
        let mut adapters = [a("Twin"), a("Twin")];
        merge_sensors(&mut adapters, &[d("Twin", 70.0)]);
        assert!(adapters.iter().all(|a| a.temperature_c.is_none()));
        // NVML's reading is labelled as NVML's.
        let mut adapters = [a("RTX")];
        merge_sensors(&mut adapters, &[d("RTX", 55.0)]);
        assert_eq!(adapters[0].temp(GpuTempKind::Gpu).map(|t| (t.c, t.source)), Some((55.0, "NVML")));
    }

    #[test]
    fn wddm_temperature_is_in_tenths_of_a_degree() {
        assert_eq!(deci_celsius(452), Some(45.2));
        assert_eq!(deci_celsius(1), Some(0.1));
        assert_eq!(deci_celsius(2000), Some(200.0));
        // 0: the driver filled nothing in; above 200 °C: no working sensor.
        assert_eq!(deci_celsius(0), None);
        assert_eq!(deci_celsius(2001), None);
        assert_eq!(deci_celsius(u32::MAX), None);
    }

    #[test]
    fn adapter_ids_give_back_their_luid() {
        assert_eq!(parse_luid("0x00000000_0x0001a2b3"), Some((0, 0x0001_a2b3)));
        assert_eq!(parse_luid("0xffffffff_0x00000010"), Some((-1, 0x10)));
        assert_eq!(parse_luid("0x00000000"), None);
        assert_eq!(parse_luid("card0"), None);
        assert_eq!(parse_luid("nvidia0"), None);
        assert_eq!(parse_luid("0x1_0xzz"), None);
    }

    #[test]
    fn sensors_find_their_gpu_by_name() {
        let names = ["AMD Radeon(TM) Graphics", "NVIDIA GeForce RTX 5070", "NVIDIA GeForce RTX 5070 Ti"];
        assert_eq!(match_gpu_sensor(&names, "GPU [#0]: NVIDIA GeForce RTX 5070: "), Some(1));
        assert_eq!(match_gpu_sensor(&names, "GPU [#1]: NVIDIA GeForce RTX 5070 Ti: "), Some(2));
        assert_eq!(match_gpu_sensor(&names, "GPU [#2]: AMD Radeon Graphics: "), Some(0), "(TM) and spacing ignored");
        assert_eq!(match_gpu_sensor(&names, "GPU [#0]: Intel Arc A770: "), None);
        assert_eq!(match_gpu_sensor(&["NVIDIA GeForce RTX 5070"], "GPU [#0]: NVIDIA GeForce RTX 5070 Ti: "), None, "a whole name only");
        assert_eq!(match_gpu_sensor(&["RTX 4090", "RTX 4090"], "GPU [#0]: RTX 4090: "), None, "two cards of that name");
        assert_eq!(match_gpu_sensor(&["", "RTX"], "GPU [#0]: RTX"), Some(1));
        assert_eq!(match_gpu_sensor(&["NVIDIA GeForce RTX 5070"], "GPU [#0]: NVIDIA GeForce RTX 5070 (laptop): "), None, "not the same name");
        assert_eq!(match_gpu_sensor(&[], "GPU"), None);
    }

    #[test]
    fn hwinfo_readings_go_to_the_named_gpu_after_better_sources() {
        use super::super::hwinfo::GpuReading;
        let a = |n: &str| GpuAdapter { name: n.into(), ..Default::default() };
        let mut adapters = [a("AMD Radeon(TM) Graphics"), a("NVIDIA GeForce RTX 5070")];
        adapters[1].offer_temp(GpuTempKind::Gpu, 45.0, "WDDM");
        let r = |s: &str, kind, c| GpuReading { sensor: s.into(), kind, c };
        let nv = "GPU [#0]: NVIDIA GeForce RTX 5070: ";
        offer_hwinfo_temps(&mut adapters, &[r(nv, GpuTempKind::Gpu, 46.0), r(nv, GpuTempKind::HotSpot, 58.0), r("GPU [#9]: Unknown", GpuTempKind::Gpu, 30.0)]);
        let nvidia: Vec<(GpuTempKind, f32, &str)> = adapters[1].temps.iter().map(|t| (t.kind, t.c, t.source)).collect();
        assert_eq!(nvidia, [(GpuTempKind::Gpu, 45.0, "WDDM"), (GpuTempKind::HotSpot, 58.0, "HWiNFO")]);
        assert!(adapters[0].temps.is_empty());
    }

    #[test]
    fn gpu_readings_are_named_by_gpu_kind_and_source() {
        let mut a = GpuAdapter { name: "RTX 5070".into(), ..Default::default() };
        a.offer_temp(GpuTempKind::Gpu, 50.0, "WDDM");
        a.offer_temp(GpuTempKind::MemoryJunction, 70.0, "HWiNFO");
        let mut twin = GpuAdapter { name: "Twin".into(), ..Default::default() };
        twin.offer_temp(GpuTempKind::Gpu, 40.0, "NVML");
        let t = gpu_thermal(&[a, twin.clone(), twin, GpuAdapter::default()]);
        let labels: Vec<&str> = t.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(labels, ["RTX 5070 \u{b7} GPU (WDDM)", "RTX 5070 \u{b7} Memory junction (HWiNFO)", "Twin #2 \u{b7} GPU (NVML)", "Twin #3 \u{b7} GPU (NVML)"]);
        assert_eq!(t[1].1, 70.0);
    }

    #[test]
    fn gpu_fold_picks_the_busiest_adapter() {
        let e = |pid, a: &str, eng: &str, v| (pid, a.to_string(), eng.to_string(), v);
        let (g, per) = fold_gpu([e(1, "A", "3D", 25.0), e(2, "A", "3D", 30.0), e(1, "A", "Video", 40.0), e(3, "B", "3D", 50.0)]);
        assert!(g.available);
        assert_eq!(g.engines, vec![("3D".to_string(), 55.0), ("Video".to_string(), 40.0)]);
        assert_eq!(g.utilization_pct, 55.0);
        assert_eq!((per[&1], per[&2], per[&3]), (40.0, 30.0, 50.0));
        let (g, per) = fold_gpu([e(1, "A", "3D", 250.0)]);
        assert_eq!((g.utilization_pct, per[&1]), (100.0, 100.0));
        let (g, per) = fold_gpu([]);
        assert!(g.available && g.engines.is_empty() && per.is_empty());
    }

    #[test]
    fn cpu_temp_label_priority() {
        let r = [("Composite", 40.0), ("Tccd1", 61.0), ("Tctl", 65.5)];
        assert_eq!(pick_cpu_temp(&r, &HWMON_CPU_LABELS), Some(("Tctl", 65.5)));
        let r = [("GPU", 50.0), ("cpu core", 55.0), ("CPU Package", 70.0)];
        assert_eq!(pick_cpu_temp(&r, &HWINFO_CPU_LABELS), Some(("CPU Package", 70.0)));
        assert_eq!(pick_cpu_temp(&r[..2], &HWINFO_CPU_LABELS), Some(("cpu core", 55.0)));
        assert_eq!(pick_cpu_temp(&[("GPU", 50.0)], &HWMON_CPU_LABELS), None);
        assert_eq!(pick_cpu_temp(&[("package ID 0", 48.0)], &HWMON_CPU_LABELS), Some(("package ID 0", 48.0)));
    }
}
