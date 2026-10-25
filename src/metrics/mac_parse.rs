//! Pure decoding for the macOS backend (`macos.rs`): mach CPU ticks, `vm_statistics64` memory
//! figures, `NET_RT_IFLIST2` route messages, IOKit disk and GPU statistics, sysctl integers and
//! `kern.boottime`. No FFI here, so it compiles and its tests run on every host and CI.

use super::common::Ticks;
use super::GpuSample;
use std::collections::HashMap;
use std::time::Duration;

/// `processor_cpu_load_info.cpu_ticks` / `host_cpu_load_info` indices (mach/machine.h).
pub const CPU_STATE_USER: usize = 0;
pub const CPU_STATE_SYSTEM: usize = 1;
pub const CPU_STATE_IDLE: usize = 2;
pub const CPU_STATE_NICE: usize = 3;

/// Tick delta between two mach load readings as [`Ticks`] (kernel includes idle, user includes
/// nice). The counters are 32-bit and wrap, so the difference wraps too.
pub fn load_delta(prev: [u32; 4], cur: [u32; 4]) -> Ticks {
    let d = |i: usize| cur[i].wrapping_sub(prev[i]) as u64;
    Ticks { idle: d(CPU_STATE_IDLE), kernel: d(CPU_STATE_SYSTEM) + d(CPU_STATE_IDLE), user: d(CPU_STATE_USER) + d(CPU_STATE_NICE) }
}

/// Element-wise sum of tick deltas (all processors).
pub fn sum_ticks(t: impl IntoIterator<Item = Ticks>) -> Ticks {
    t.into_iter().fold(Ticks::default(), |a, b| Ticks { idle: a.idle + b.idle, kernel: a.kernel + b.kernel, user: a.user + b.user })
}

/// The `vm_statistics64` page counts the memory figures need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmCounts {
    pub free: u64,
    pub wire: u64,
    pub purgeable: u64,
    pub compressor: u64,
    pub external: u64,
    pub internal: u64,
}

/// (available, cached) bytes the way Activity Monitor counts them: used = app memory (internal
/// minus purgeable) + wired + compressed; cached files = external + purgeable.
pub fn memory_figures(total: u64, page: u64, v: &VmCounts) -> (u64, u64) {
    let used = (v.internal.saturating_sub(v.purgeable) + v.wire + v.compressor).saturating_mul(page).min(total);
    (total - used, (v.external + v.purgeable).saturating_mul(page))
}

/// An unsigned sysctl value in native byte order (4 or 8 bytes).
pub fn ne_uint(b: &[u8]) -> Option<u64> {
    match b.len() {
        4 => Some(u32::from_ne_bytes(b.try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

/// MHz from Hz (rounded), None for nothing worth a clock (below 0.5 MHz).
pub fn hz_mhz(hz: u64) -> Option<u32> {
    (hz >= 500_000).then(|| ((hz as f64) / 1e6).round() as u32)
}

/// Time since `kern.boottime` (seconds, microseconds) at `now` (since the Unix epoch); zero when
/// the clock reads earlier than the boot time.
pub fn uptime_since(boot_sec: i64, boot_usec: i64, now: Duration) -> Duration {
    let boot = Duration::from_secs(boot_sec.max(0) as u64) + Duration::from_micros(boot_usec.clamp(0, 999_999) as u64);
    now.saturating_sub(boot)
}

/// `rt_msghdr` type of the `if_msghdr2` messages in a `NET_RT_IFLIST2` dump.
pub const RTM_IFINFO2: u8 = 0x12;
/// `sizeof(struct if_msghdr2)` under net/if.h's `#pragma pack(4)` (checked against libc in macos.rs).
pub const IF_MSGHDR2_LEN: usize = 160;
/// Byte offsets inside `if_msghdr2` (the `if_data64` member starts at 32).
pub const OFF_FLAGS: usize = 8;
pub const OFF_INDEX: usize = 12;
pub const OFF_DATA: usize = 32;
pub const OFF_TYPE: usize = OFF_DATA;
pub const OFF_BAUDRATE: usize = OFF_DATA + 16;
pub const OFF_IBYTES: usize = OFF_DATA + 64;
pub const OFF_OBYTES: usize = OFF_DATA + 72;

pub const IFF_UP: i32 = 0x1;
pub const IFF_LOOPBACK: i32 = 0x8;
pub const IFF_RUNNING: i32 = 0x40;

/// One interface of a `NET_RT_IFLIST2` dump, with 64-bit counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IfRow {
    pub index: u16,
    pub flags: i32,
    pub ifi_type: u8,
    pub baudrate: u64,
    pub ibytes: u64,
    pub obytes: u64,
}

/// The `RTM_IFINFO2` messages of a `NET_RT_IFLIST2` sysctl buffer; other messages (addresses,
/// multicast) are skipped by their `ifm_msglen`. A truncated tail ends the walk.
pub fn parse_iflist2(buf: &[u8]) -> Vec<IfRow> {
    let u16_at = |o: usize| u16::from_ne_bytes([buf[o], buf[o + 1]]);
    let i32_at = |o: usize| i32::from_ne_bytes(buf[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_ne_bytes(buf[o..o + 8].try_into().unwrap());
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 4 <= buf.len() {
        let len = u16_at(off) as usize;
        if len < 4 || off + len > buf.len() {
            break;
        }
        if buf[off + 3] == RTM_IFINFO2 && len >= IF_MSGHDR2_LEN {
            out.push(IfRow {
                index: u16_at(off + OFF_INDEX),
                flags: i32_at(off + OFF_FLAGS),
                ifi_type: buf[off + OFF_TYPE],
                baudrate: u64_at(off + OFF_BAUDRATE),
                ibytes: u64_at(off + OFF_IBYTES),
                obytes: u64_at(off + OFF_OBYTES),
            });
        }
        off += len;
    }
    out
}

/// Name prefixes of virtual interfaces whose traffic is already counted on a physical one (VPN
/// tunnels, bridges, AWDL/AirDrop, hotspot, 6to4) or that never leaves the machine.
const VIRTUAL_IFS: [&str; 12] = ["lo", "gif", "stf", "utun", "ipsec", "bridge", "awdl", "llw", "anpi", "ap", "vmenet", "feth"];

/// Whether an interface counts toward the network totals: up, running, not loopback, not virtual.
pub fn counts_interface(name: &str, flags: i32) -> bool {
    let virt = VIRTUAL_IFS.iter().any(|p| name.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_digit())));
    flags & (IFF_UP | IFF_RUNNING) == (IFF_UP | IFF_RUNNING) && flags & IFF_LOOPBACK == 0 && !virt
}

/// Cumulative counters of one `IOBlockStorageDriver` ("Statistics": bytes and the summed
/// read/write service time in ns).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskCounters {
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub busy_ns: u64,
}

/// (active %, read B/s, write B/s) between two readings keyed by registry entry ID. Only disks
/// present in both count, so attaching or ejecting one causes no jump. Active is the average
/// over those disks of service time per wall time, clamped to 100 (queued I/O overlaps).
pub fn disk_rates(prev: &HashMap<u64, DiskCounters>, cur: &HashMap<u64, DiskCounters>, dt: Duration) -> (f32, f64, f64) {
    let secs = dt.as_secs_f64().max(1e-3);
    let (mut active, mut n, mut rd, mut wr) = (0.0f64, 0usize, 0u64, 0u64);
    for (id, c) in cur {
        let Some(p) = prev.get(id) else { continue };
        rd += c.read_bytes.saturating_sub(p.read_bytes);
        wr += c.write_bytes.saturating_sub(p.write_bytes);
        active += (c.busy_ns.saturating_sub(p.busy_ns) as f64 / 1e9 / secs).min(1.0);
        n += 1;
    }
    let pct = if n == 0 { 0.0 } else { (active / n as f64 * 100.0).clamp(0.0, 100.0) as f32 };
    (pct, rd as f64 / secs, wr as f64 / secs)
}

/// One IOAccelerator's "PerformanceStatistics": "Device Utilization %", "vramUsedBytes"
/// (discrete GPUs) and "In use system memory" (Apple Silicon, unified memory).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccelStats {
    pub utilization: Option<i64>,
    pub vram_used: Option<i64>,
    pub system_used: Option<i64>,
}

/// The busiest accelerator's utilisation and the memory in use summed over all of them. No
/// engine breakdown exists on macOS, so `engines` stays empty; `available` needs a utilisation.
pub fn fold_accel(stats: &[AccelStats]) -> GpuSample {
    let util = stats.iter().filter_map(|s| s.utilization).max();
    GpuSample {
        available: util.is_some(),
        utilization_pct: util.unwrap_or(0).clamp(0, 100) as f32,
        dedicated_used: stats.iter().filter_map(|s| s.vram_used.or(s.system_used)).map(|b| b.max(0) as u64).sum(),
        engines: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::tick_delta_pct;
    use super::*;

    #[test]
    fn load_delta_wraps_and_maps_states() {
        // user 30, system 20, idle 40, nice 10 ticks.
        let d = load_delta([100, 200, 300, 400], [130, 220, 340, 410]);
        assert_eq!(d, Ticks { idle: 40, kernel: 60, user: 40 });
        let (busy, kern) = tick_delta_pct(Ticks::default(), d).unwrap();
        assert!((busy - 60.0).abs() < 1e-4 && (kern - 20.0).abs() < 1e-4, "{busy} {kern}");
        // The 32-bit idle counter wrapped.
        let d = load_delta([0, 0, u32::MAX - 4, 0], [0, 0, 5, 0]);
        assert_eq!(d.idle, 10);
        assert_eq!(sum_ticks([d, d]).idle, 20);
        assert_eq!(sum_ticks([]), Ticks::default());
    }

    #[test]
    fn memory_like_activity_monitor() {
        let v = VmCounts { free: 1000, wire: 100, purgeable: 50, compressor: 25, external: 200, internal: 450 };
        // used = (450 - 50 + 100 + 25) pages = 525 * 16 KiB.
        let (avail, cached) = memory_figures(1_000 * 16384, 16384, &v);
        assert_eq!(avail, (1000 - 525) * 16384);
        assert_eq!(cached, 250 * 16384);
        // Never more used than installed, never a negative app memory.
        let v = VmCounts { wire: 10_000, purgeable: 500, internal: 100, ..Default::default() };
        assert_eq!(memory_figures(4096 * 100, 4096, &v).0, 0);
    }

    #[test]
    fn sysctl_ints_and_clocks() {
        assert_eq!(ne_uint(&7u32.to_ne_bytes()), Some(7));
        assert_eq!(ne_uint(&2_400_000_000u64.to_ne_bytes()), Some(2_400_000_000));
        assert_eq!(ne_uint(&[1, 2]), None);
        assert_eq!(hz_mhz(2_400_000_000), Some(2400));
        assert_eq!(hz_mhz(2_299_999_999), Some(2300));
        assert_eq!(hz_mhz(0), None);
    }

    #[test]
    fn uptime_from_boottime() {
        let now = Duration::from_secs(1_000_100) + Duration::from_millis(500);
        assert_eq!(uptime_since(1_000_000, 250_000, now), Duration::from_millis(100_250));
        assert_eq!(uptime_since(2_000_000, 0, now), Duration::ZERO);
    }

    fn msg(ty: u8, flags: i32, index: u16, ibytes: u64, obytes: u64, baud: u64, len: usize) -> Vec<u8> {
        let mut m = vec![0u8; len];
        m[0..2].copy_from_slice(&(len as u16).to_ne_bytes());
        m[2] = 5; // RTM_VERSION
        m[3] = ty;
        if len >= IF_MSGHDR2_LEN {
            m[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&flags.to_ne_bytes());
            m[OFF_INDEX..OFF_INDEX + 2].copy_from_slice(&index.to_ne_bytes());
            m[OFF_TYPE] = 6; // IFT_ETHER
            m[OFF_BAUDRATE..OFF_BAUDRATE + 8].copy_from_slice(&baud.to_ne_bytes());
            m[OFF_IBYTES..OFF_IBYTES + 8].copy_from_slice(&ibytes.to_ne_bytes());
            m[OFF_OBYTES..OFF_OBYTES + 8].copy_from_slice(&obytes.to_ne_bytes());
        }
        m
    }

    #[test]
    fn iflist2_walks_messages() {
        let up = IFF_UP | IFF_RUNNING;
        let mut buf = msg(RTM_IFINFO2, up | IFF_LOOPBACK, 1, 10, 10, 0, 160 + 20); // lo0 plus its sockaddr_dl
        buf.extend(msg(0x13, 0, 0, 0, 0, 0, 24)); // RTM_NEWMADDR2
        buf.extend(msg(RTM_IFINFO2, up, 4, 5_000_000_000, 7, 1_000_000_000, 160));
        let rows = parse_iflist2(&buf);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].index, 1);
        assert_eq!(rows[1], IfRow { index: 4, flags: up, ifi_type: 6, baudrate: 1_000_000_000, ibytes: 5_000_000_000, obytes: 7 });
        // A truncated tail and a zero length stop the walk instead of looping or panicking.
        assert_eq!(parse_iflist2(&buf[..buf.len() - 1]).len(), 1);
        assert!(parse_iflist2(&[0, 0, 5, RTM_IFINFO2]).is_empty());
        assert!(parse_iflist2(&[]).is_empty());
    }

    #[test]
    fn interface_filter() {
        let up = IFF_UP | IFF_RUNNING;
        assert!(counts_interface("en0", up));
        assert!(counts_interface("en12", up | 0x8000));
        assert!(!counts_interface("en1", IFF_UP));
        assert!(!counts_interface("lo0", up | IFF_LOOPBACK));
        for v in ["utun3", "awdl0", "llw0", "bridge100", "ap1", "anpi0", "gif0", "stf0"] {
            assert!(!counts_interface(v, up), "{v}");
        }
        // Prefix match is by family, not by any string start.
        assert!(counts_interface("apple0", up));
    }

    #[test]
    fn disk_rates_over_common_disks() {
        let d = |r, w, b| DiskCounters { read_bytes: r, write_bytes: w, busy_ns: b };
        let prev = HashMap::from([(1, d(0, 0, 0)), (2, d(100, 100, 0)), (3, d(0, 0, 0))]);
        let cur = HashMap::from([(1, d(2000, 1000, 500_000_000)), (2, d(100, 100, 5_000_000_000)), (4, d(9_999, 9_999, 9_999))]);
        let (act, r, w) = disk_rates(&prev, &cur, Duration::from_secs(1));
        // Disk 1 50 % busy, disk 2 saturated (clamped to 100 %); disks 3 and 4 not in both.
        assert!((act - 75.0).abs() < 1e-4, "{act}");
        assert_eq!((r, w), (2000.0, 1000.0));
        assert_eq!(disk_rates(&prev, &HashMap::new(), Duration::from_secs(1)), (0.0, 0.0, 0.0));
    }

    #[test]
    fn accelerators_fold() {
        let g = fold_accel(&[
            AccelStats { utilization: Some(12), vram_used: Some(1 << 30), system_used: Some(5) },
            AccelStats { utilization: Some(140), vram_used: None, system_used: Some(1 << 20) },
        ]);
        assert!(g.available && g.engines.is_empty());
        assert_eq!(g.utilization_pct, 100.0);
        assert_eq!(g.dedicated_used, (1 << 30) + (1 << 20));
        let g = fold_accel(&[AccelStats { utilization: None, vram_used: Some(10), system_used: None }]);
        assert!(!g.available);
        assert_eq!(g.dedicated_used, 10);
        assert!(!fold_accel(&[]).available);
    }
}
