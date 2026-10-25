//! Pieces every metrics backend shares: the CPUID brand string, tick-counter deltas, network
//! rates, the GPU engine fold and the CPU temperature label pick. Pure (bar CPUID) and tested on
//! every host.

use super::{GpuSample, NetworkSample};
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
