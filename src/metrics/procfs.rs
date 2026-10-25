//! Pure parsers for the Linux backend's `/proc` and `/sys` files, and the folds that turn their
//! readings into samples. No I/O here (`linux.rs` reads the files), so the fixture tests run on
//! every host.

use super::common::{pick_cpu_temp, Ticks, HWMON_CPU_LABELS};
use super::{DiskSample, GpuSample, MemorySample, ThermalSample};
use std::time::Duration;

/// One `cpu`/`cpuN` line of `/proc/stat` as [`Ticks`]: idle = idle + iowait; kernel = system + irq
/// + softirq + steal, plus idle (the Windows convention [`Ticks`] follows); user = user + nice
///   (guest time is already inside user).
fn stat_ticks(fields: &[u64]) -> Ticks {
    let f = |i: usize| fields.get(i).copied().unwrap_or(0);
    let idle = f(3) + f(4);
    Ticks { idle, kernel: f(2) + f(5) + f(6) + f(7) + idle, user: f(0) + f(1) }
}

/// `/proc/stat`: the aggregate `cpu` line and each `cpuN` line as (N, ticks), in file order.
pub fn parse_stat(text: &str) -> (Option<Ticks>, Vec<(u32, Ticks)>) {
    let (mut total, mut cores) = (None, Vec::new());
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        let Some(head) = it.next().and_then(|h| h.strip_prefix("cpu")) else { continue };
        let fields: Vec<u64> = it.map_while(|v| v.parse().ok()).collect();
        if fields.len() < 4 {
            continue;
        }
        if head.is_empty() {
            total = Some(stat_ticks(&fields));
        } else if let Ok(n) = head.parse() {
            cores.push((n, stat_ticks(&fields)));
        }
    }
    (total, cores)
}

/// `/proc/meminfo` (kB values). None without `MemTotal`. `MemAvailable` (kernel 3.14+) falls back to
/// free + buffers + cached; cached = Cached + Buffers + SReclaimable (what `free` calls buff/cache).
pub fn parse_meminfo(text: &str) -> Option<MemorySample> {
    let get = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k == key).then_some(v)?.split_ascii_whitespace().next()?.parse::<u64>().ok().map(|kb| kb * 1024)
        })
    };
    let total = get("MemTotal")?;
    let (buffers, cached) = (get("Buffers").unwrap_or(0), get("Cached").unwrap_or(0));
    let available = get("MemAvailable").unwrap_or_else(|| get("MemFree").unwrap_or(0) + buffers + cached).min(total);
    let swap_total = get("SwapTotal");
    Some(MemorySample {
        total,
        available,
        used_pct: if total > 0 { ((total - available) as f64 / total as f64 * 100.0) as f32 } else { 0.0 },
        commit_used: get("Committed_AS"),
        commit_limit: get("CommitLimit"),
        cached: cached + buffers + get("SReclaimable").unwrap_or(0),
        swap_total,
        swap_used: swap_total.map(|t| t.saturating_sub(get("SwapFree").unwrap_or(t))),
    })
}

/// The processor's name from `/proc/cpuinfo`: "model name" (x86, some arm64 kernels), else
/// "Hardware" or "Processor" (32-bit ARM), else the Arm core named by "CPU implementer" / "CPU part".
pub fn cpuinfo_name(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|v| !v.is_empty())
        })
    };
    let hex = |key: &str| field(key).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    field("model name").or_else(|| field("Hardware")).or_else(|| field("Processor")).or_else(|| {
        let part = match (hex("CPU implementer")?, hex("CPU part")?) {
            (0x41, 0xd03) => "Cortex-A53",
            (0x41, 0xd04) => "Cortex-A35",
            (0x41, 0xd05) => "Cortex-A55",
            (0x41, 0xd07) => "Cortex-A57",
            (0x41, 0xd08) => "Cortex-A72",
            (0x41, 0xd09) => "Cortex-A73",
            (0x41, 0xd0a) => "Cortex-A75",
            (0x41, 0xd0b) => "Cortex-A76",
            (0x41, 0xd0c) => "Neoverse-N1",
            (0x41, 0xd0d) => "Cortex-A77",
            (0x41, 0xd40) => "Neoverse-V1",
            (0x41, 0xd41) => "Cortex-A78",
            (0x41, 0xd44) => "Cortex-X1",
            (0x41, 0xd46) => "Cortex-A510",
            (0x41, 0xd47) => "Cortex-A710",
            (0x41, 0xd48) => "Cortex-X2",
            (0x41, 0xd49) => "Neoverse-N2",
            (0x41, 0xd4f) => "Neoverse-V2",
            (0x41, _) => "Arm",
            (0x61, _) => "Apple",
            (0x51, _) => "Qualcomm",
            _ => return None,
        };
        Some(part.to_string())
    })
}

/// "cpu MHz" of each processor in `/proc/cpuinfo`, keyed by its "processor" number.
pub fn cpuinfo_mhz(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut cpu = None;
    for l in text.lines() {
        let Some((k, v)) = l.split_once(':') else { continue };
        match k.trim() {
            "processor" => cpu = v.trim().parse().ok(),
            "cpu MHz" => {
                if let (Some(n), Ok(m)) = (cpu, v.trim().parse::<f64>()) {
                    if m.is_finite() && m > 0.0 {
                        out.push((n, m.round() as u32));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// One whole-disk line of `/proc/diskstats`: cumulative sectors (512 bytes each, whatever the
/// device's sector size) and milliseconds spent doing I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStat {
    pub name: String,
    pub sectors_read: u64,
    pub sectors_written: u64,
    pub io_ms: u64,
}

/// Block devices that are not physical disks: loop images, RAM disks, zram swap, optical and
/// floppy drives, and the eMMC boot / RPMB areas.
fn never_counted(name: &str) -> bool {
    ["loop", "ram", "zram", "sr", "fd"].iter().any(|p| name.strip_prefix(p).is_some_and(|r| r.chars().all(|c| c.is_ascii_digit()))) || (name.starts_with("mmcblk") && (name.contains("boot") || name.contains("rpmb")))
}

/// Stacked devices (device-mapper, md RAID, network block): counted only when no physical disk
/// is, else their I/O would count twice.
fn stacked(name: &str) -> bool {
    name.starts_with("dm-") || name.starts_with("md") || name.starts_with("nbd") || name.starts_with("bcache")
}

/// Whether a name looks like a whole disk rather than a partition, for when `/sys/block` (which
/// lists exactly the whole devices) cannot be read: `sda`, `vdb`, `nvme0n1`, `mmcblk0`, not
/// `sda1`, `nvme0n1p2` or `mmcblk0p1`.
pub fn looks_whole(name: &str) -> bool {
    let digits_after = |p: &str| name.strip_prefix(p).is_some_and(|r| !r.is_empty() && r.bytes().all(|c| c.is_ascii_lowercase()));
    if ["sd", "vd", "xvd", "hd"].iter().any(|p| digits_after(p)) {
        return true;
    }
    let rest = |p: &str| name.strip_prefix(p).filter(|r| !r.is_empty());
    if let Some(r) = rest("nvme") {
        return !r.contains('p') && r.contains('n');
    }
    rest("mmcblk").is_some_and(|r| r.bytes().all(|c| c.is_ascii_digit())) || stacked(name) || never_counted(name)
}

/// The disks `/proc/diskstats` totals should count: whole devices (`whole(name)`, normally
/// membership of `/sys/block`) that are physical; the stacked ones only when there is none.
pub fn parse_diskstats(text: &str, whole: impl Fn(&str) -> bool) -> Vec<DiskStat> {
    let all: Vec<DiskStat> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_ascii_whitespace().collect();
            let n = |i: usize| f.get(i)?.parse::<u64>().ok();
            let name = *f.get(2)?;
            if !whole(name) || never_counted(name) {
                return None;
            }
            Some(DiskStat { name: name.to_string(), sectors_read: n(5)?, sectors_written: n(9)?, io_ms: n(12)? })
        })
        .collect();
    if all.iter().any(|d| !stacked(&d.name)) {
        all.into_iter().filter(|d| !stacked(&d.name)).collect()
    } else {
        all
    }
}

/// Disk counters summed over the counted disks, kept between samples.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskTotals {
    pub disks: Vec<String>,
    pub sectors_read: u64,
    pub sectors_written: u64,
    pub io_ms: u64,
}

impl DiskTotals {
    pub fn of(stats: &[DiskStat]) -> Self {
        Self {
            disks: stats.iter().map(|d| d.name.clone()).collect(),
            sectors_read: stats.iter().map(|d| d.sectors_read).sum(),
            sectors_written: stats.iter().map(|d| d.sectors_written).sum(),
            io_ms: stats.iter().map(|d| d.io_ms).sum(),
        }
    }
}

/// Rates over `dt` and the active time averaged over the disks (like `PhysicalDisk(_Total)` on
/// Windows). Zero on the first reading or when the set of disks changed.
pub fn disk_delta(prev: Option<&DiskTotals>, cur: &DiskTotals, dt: Duration) -> DiskSample {
    let mut d = DiskSample { available: true, ..Default::default() };
    let secs = dt.as_secs_f64();
    let Some(p) = prev.filter(|p| p.disks == cur.disks && secs > 0.0 && !cur.disks.is_empty()) else { return d };
    d.read_bps = cur.sectors_read.saturating_sub(p.sectors_read) as f64 * 512.0 / secs;
    d.write_bps = cur.sectors_written.saturating_sub(p.sectors_written) as f64 * 512.0 / secs;
    let busy_ms = cur.io_ms.saturating_sub(p.io_ms) as f64;
    d.active_pct = (busy_ms / (secs * 1000.0 * cur.disks.len() as f64) * 100.0).clamp(0.0, 100.0) as f32;
    d
}

/// One interface under `/sys/class/net`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetIf {
    pub name: String,
    /// `operstate` reads "up".
    pub up: bool,
    /// Backed by a device (`/sys/class/net/<if>/device` exists): not loopback, bridge, veth, tun.
    pub hardware: bool,
    /// `speed` in Mb/s; -1 or unreadable (Wi-Fi, virtual) means unknown.
    pub speed_mbps: Option<i64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// (received, sent, summed link speed in bits/s) over the up hardware interfaces.
pub fn net_totals(ifs: &[NetIf]) -> (u64, u64, u64) {
    ifs.iter().filter(|i| i.up && i.hardware && i.name != "lo").fold((0, 0, 0), |(rx, tx, link), i| {
        (rx + i.rx_bytes, tx + i.tx_bytes, link + i.speed_mbps.filter(|s| *s > 0).map_or(0, |s| s as u64 * 1_000_000))
    })
}

/// A sysfs temperature (`temp*_input`, `thermal_zone*/temp`) in millidegrees as °C. None for
/// readings no working sensor reports (0, negative, above 200 °C).
pub fn millideg(text: &str) -> Option<f32> {
    let c = text.trim().parse::<i64>().ok()? as f32 / 1000.0;
    (c > 0.0 && c <= 200.0).then_some(c)
}

/// One hwmon chip: its `name` and (label, °C) of each temperature input (label = `temp*_label`,
/// else "tempN").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwmonChip {
    pub name: String,
    pub temps: Vec<(String, f32)>,
}

/// hwmon drivers of CPU sensors (AMD, Intel, Arm SoCs), best first.
const CPU_CHIPS: [&str; 5] = ["k10temp", "zenpower", "coretemp", "cpu_thermal", "soc_thermal"];

/// The thermal sample: the CPU reading from a CPU chip (by [`HWMON_CPU_LABELS`], else its first
/// reading), the ACPI thermal zones, then the other hwmon readings (NVMe, GPU, board) named
/// "chip label". `acpitz` is skipped: it mirrors the thermal zones.
pub fn fold_thermal(chips: &[HwmonChip], zones: Vec<(String, f32)>) -> ThermalSample {
    let mut t = ThermalSample { zones, ..Default::default() };
    let mut cpu_chip = None;
    for name in CPU_CHIPS {
        let Some((i, chip)) = chips.iter().enumerate().find(|(_, c)| c.name == name && !c.temps.is_empty()) else { continue };
        let temps: Vec<(&str, f32)> = chip.temps.iter().map(|(l, v)| (l.as_str(), *v)).collect();
        let (label, v) = pick_cpu_temp(&temps, &HWMON_CPU_LABELS).unwrap_or(temps[0]);
        t.cpu = Some((label.to_string(), v));
        cpu_chip = Some(i);
        break;
    }
    let zone_count = t.zones.len();
    for (i, chip) in chips.iter().enumerate() {
        if Some(i) == cpu_chip || chip.name == "acpitz" {
            continue;
        }
        t.zones.extend(chip.temps.iter().map(|(l, v)| (format!("{} {l}", chip.name), *v)));
    }
    let mut names: Vec<&str> = chips.iter().filter(|c| !c.temps.is_empty()).map(|c| c.name.as_str()).collect();
    names.dedup();
    let hw = if names.is_empty() { "no sensors".to_string() } else { names.join(", ") };
    t.sources = format!("hwmon: {hw} \u{b7} thermal zones: {}", if zone_count == 0 { "none".to_string() } else { zone_count.to_string() });
    t
}

/// One GPU's reading: amdgpu sysfs or NVML.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuDev {
    /// (engine, %); the busiest is the device's utilisation.
    pub engines: Vec<(String, f32)>,
    /// Dedicated memory in use (bytes), when known.
    pub vram_used: Option<u64>,
}

/// Utilisation and engines of the busiest device; dedicated memory summed over all (as Windows
/// sums its adapters). Unavailable without any utilisation reading.
pub fn fold_gpus(devs: &[GpuDev]) -> GpuSample {
    let busy = |d: &GpuDev| d.engines.iter().map(|e| e.1).fold(0.0, f32::max);
    let mut g = GpuSample { dedicated_used: devs.iter().filter_map(|d| d.vram_used).sum(), ..Default::default() };
    if let Some(d) = devs.iter().filter(|d| !d.engines.is_empty()).max_by(|a, b| busy(a).total_cmp(&busy(b))) {
        g.available = true;
        g.utilization_pct = busy(d).clamp(0.0, 100.0);
        g.engines = d.engines.iter().map(|(e, v)| (e.clone(), v.clamp(0.0, 100.0))).collect();
    }
    g
}

/// Runtime power management of a GPU's PCI device, from sysfs attributes of the PM core and PCI
/// (`power/runtime_status`, `power/control`, `boot_vga`) that never wake it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuPower {
    /// Powered down (`suspended`, or on its way there).
    pub suspended: bool,
    /// A GPU that is not the boot display device and that its driver may power down
    /// (`power/control` is `auto`): the discrete GPU of a hybrid laptop.
    pub may_sleep: bool,
}

/// [`GpuPower`] from the attribute texts (`None`: the file is missing).
pub fn gpu_power(status: Option<&str>, control: Option<&str>, boot_vga: Option<&str>) -> GpuPower {
    GpuPower {
        suspended: status.is_some_and(|s| matches!(s.trim(), "suspended" | "suspending")),
        may_sleep: control.is_some_and(|c| c.trim() == "auto") && !boot_vga.is_some_and(|b| b.trim() == "1"),
    }
}

/// How often a GPU that may sleep is read while it is awake but idle. Each read (NVML, amdgpu's
/// `gpu_busy_percent`, its hwmon) restarts its idle timer, so the gaps must be long enough for it
/// to power down. While it is at work it cannot power down anyway and is read every tick.
pub const SLEEPY_GPU_PERIOD: Duration = Duration::from_secs(10);

/// The device was at work when last read (any engine above 0 %).
pub fn gpu_busy(d: &GpuDev) -> bool {
    d.engines.iter().any(|e| e.1 > 0.0)
}

/// What the sampler does with a GPU (or its sensors) this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPoll {
    /// Read it now.
    Read,
    /// It is powered down: report it idle without touching it.
    Idle,
    /// Reuse the last reading.
    Keep,
}

/// The poll of a GPU with power state `p`, `since` its last read (`None`: never read), `busy`
/// when that read found it at work: then it is read every tick until a read finds it idle.
pub fn gpu_poll(p: GpuPower, since: Option<Duration>, busy: bool) -> GpuPoll {
    if p.suspended {
        GpuPoll::Idle
    } else if p.may_sleep && !busy && since.is_some_and(|d| d < SLEEPY_GPU_PERIOD) {
        GpuPoll::Keep
    } else {
        GpuPoll::Read
    }
}

/// `/proc/uptime`: seconds since boot (first field).
pub fn parse_uptime(text: &str) -> Option<Duration> {
    let s = text.split_ascii_whitespace().next()?.parse::<f64>().ok()?;
    (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s))
}

/// `/proc/sys/fs/file-nr`: allocated minus free file handles (free is 0 since Linux 2.6).
pub fn parse_file_nr(text: &str) -> Option<usize> {
    let mut f = text.split_ascii_whitespace().map(|v| v.parse::<usize>().ok());
    let (alloc, free) = (f.next()??, f.next().flatten().unwrap_or(0));
    Some(alloc.saturating_sub(free))
}

/// `/proc/loadavg`: the total of "running/total" scheduling entities, i.e. all threads.
pub fn parse_loadavg_threads(text: &str) -> Option<usize> {
    text.split_ascii_whitespace().nth(3)?.split_once('/')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeping_gpus_are_left_alone() {
        let dgpu = gpu_power(Some("suspended\n"), Some("auto\n"), None);
        assert_eq!(dgpu, GpuPower { suspended: true, may_sleep: true });
        assert_eq!(gpu_poll(dgpu, None, false), GpuPoll::Idle, "a powered-down GPU is never read");
        assert_eq!(gpu_poll(dgpu, Some(Duration::from_secs(1)), true), GpuPoll::Idle);
        let awake = gpu_power(Some("active\n"), Some("auto\n"), Some("0\n"));
        assert_eq!(gpu_poll(awake, None, false), GpuPoll::Read);
        assert_eq!(gpu_poll(awake, Some(Duration::from_secs(1)), false), GpuPoll::Keep, "idle: read seldom so it can power down");
        assert_eq!(gpu_poll(awake, Some(Duration::from_secs(1)), true), GpuPoll::Read, "at work: read every tick");
        assert_eq!(gpu_poll(awake, Some(SLEEPY_GPU_PERIOD), false), GpuPoll::Read);
        let dev = |pct: f32| GpuDev { engines: vec![("Graphics".into(), pct)], vram_used: None };
        assert!(gpu_busy(&dev(3.0)) && !gpu_busy(&dev(0.0)) && !gpu_busy(&GpuDev::default()));
        // The boot display GPU, a GPU whose power management is off, and one without the files
        // are read every tick as before.
        for p in [gpu_power(Some("active"), Some("auto"), Some("1")), gpu_power(Some("active"), Some("on"), Some("0")), gpu_power(None, None, None)] {
            assert!(!p.may_sleep && !p.suspended, "{p:?}");
            assert_eq!(gpu_poll(p, Some(Duration::from_millis(500)), false), GpuPoll::Read);
        }
    }

    const STAT: &str = "cpu  1183823 92 471691 34746123 37962 0 55759 0 0 0
cpu0 72012 54 32251 2161026 3036 0 25891 0 0 0
cpu1 69021 0 26773 2178740 3792 0 10254 0 0 0
cpu3 70383 3 26442 2178627 3854 0 4464 7 0 0
intr 123456 0 0
ctxt 987654
btime 1761400000
processes 60571
procs_running 1
procs_blocked 0
softirq 1 2 3
";

    #[test]
    fn stat_lines_map_to_windows_style_ticks() {
        let (total, cores) = parse_stat(STAT);
        let t = total.unwrap();
        assert_eq!(t.idle, 34746123 + 37962);
        assert_eq!(t.user, 1183823 + 92);
        assert_eq!(t.kernel, 471691 + 55759 + t.idle);
        // Offline CPU 2 is absent: numbers come from the line, not the position.
        assert_eq!(cores.iter().map(|c| c.0).collect::<Vec<_>>(), vec![0, 1, 3]);
        assert_eq!(cores[2].1, Ticks { idle: 2178627 + 3854, kernel: 26442 + 4464 + 7 + 2178627 + 3854, user: 70383 + 3 });
        // Old kernels: 4 fields only.
        let (t, c) = parse_stat("cpu 10 0 5 85\ncpu0 10 0 5 85\n");
        assert_eq!(t, Some(Ticks { idle: 85, kernel: 90, user: 10 }));
        assert_eq!(c.len(), 1);
        assert_eq!(parse_stat("garbage\ncpu x y\n"), (None, vec![]));
    }

    #[test]
    fn stat_deltas_give_busy_and_kernel_shares() {
        let (a, _) = parse_stat("cpu 100 0 50 800 50 0 0 0 0 0\n");
        // +100 user, +60 system, +10 softirq, +780 idle, +50 iowait.
        let (b, _) = parse_stat("cpu 200 0 110 1580 100 0 10 0 0 0\n");
        let (busy, kern) = super::super::common::tick_delta_pct(a.unwrap(), b.unwrap()).unwrap();
        assert!((busy - 17.0).abs() < 1e-3 && (kern - 7.0).abs() < 1e-3, "{busy} {kern}");
    }

    const MEMINFO: &str = "MemTotal:       16303580 kB
MemFree:         9812516 kB
MemAvailable:   13512300 kB
Buffers:          101236 kB
Cached:          3570440 kB
SwapCached:            0 kB
SReclaimable:     211848 kB
SwapTotal:       4194304 kB
SwapFree:        4094304 kB
CommitLimit:    12346092 kB
Committed_AS:    3866784 kB
HugePages_Total:       0
";

    #[test]
    fn meminfo_fields() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.total, 16303580 * 1024);
        assert_eq!(m.available, 13512300 * 1024);
        assert_eq!(m.cached, (3570440 + 101236 + 211848) * 1024);
        assert_eq!((m.swap_total, m.swap_used), (Some(4194304 * 1024), Some(100000 * 1024)));
        assert_eq!((m.commit_used, m.commit_limit), (Some(3866784 * 1024), Some(12346092 * 1024)));
        assert!((m.used_pct - 17.12).abs() < 0.01, "{}", m.used_pct);
        // Pre-3.14 kernel: no MemAvailable, no swap lines.
        let m = parse_meminfo("MemTotal: 1000 kB\nMemFree: 200 kB\nBuffers: 100 kB\nCached: 300 kB\n").unwrap();
        assert_eq!(m.available, 600 * 1024);
        assert_eq!((m.swap_total, m.swap_used, m.commit_used), (None, None, None));
        assert!(parse_meminfo("MemFree: 1 kB\n").is_none());
    }

    #[test]
    fn cpuinfo_names_and_clocks() {
        let x86 = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 9800X3D 8-Core Processor\ncpu MHz\t\t: 4699.871\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 9800X3D 8-Core Processor\ncpu MHz\t\t: 3000.000\n";
        assert_eq!(cpuinfo_name(x86).as_deref(), Some("AMD Ryzen 7 9800X3D 8-Core Processor"));
        assert_eq!(cpuinfo_mhz(x86), vec![(0, 4700), (1, 3000)]);
        let pi = "processor\t: 0\nBogoMIPS\t: 108.00\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU part\t: 0xd08\n\nHardware\t: \nModel\t\t: Raspberry Pi 4 Model B Rev 1.4\n";
        assert_eq!(cpuinfo_name(pi).as_deref(), Some("Cortex-A72"));
        assert!(cpuinfo_mhz(pi).is_empty());
        assert_eq!(cpuinfo_name("Processor\t: ARMv7 Processor rev 4 (v7l)\nHardware\t: BCM2835\n").as_deref(), Some("BCM2835"));
        assert_eq!(cpuinfo_name("CPU implementer\t: 0x99\nCPU part\t: 0x1\n"), None);
    }

    const DISKSTATS: &str = "   1       0 ram0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
   7       0 loop0 7368 0 1403580 28922 0 0 0 0 0 3180 28922 0 0 0 0 0 0
   8       0 sda 1536 489 184802 422 0 0 0 0 0 1000 422 0 0 0 0 0 0
   8       1 sda1 1400 489 180000 400 0 0 0 0 0 900 400 0 0 0 0 0 0
 259       0 nvme0n1 81355 13260 3389106 67259 519361 432832 16839248 2074221 0 408500 2384742
 259       1 nvme0n1p1 100 0 2000 10 0 0 0 0 0 10 10
 253       0 dm-0 90000 0 3000000 70000 600000 0 16000000 2000000 0 400000 2070000 0 0 0 0
  11       0 sr0 1 0 8 0 0 0 0 0 0 1 0
";

    #[test]
    fn diskstats_counts_whole_physical_disks_only() {
        let sys_block = ["ram0", "loop0", "sda", "nvme0n1", "dm-0", "sr0"];
        let d = parse_diskstats(DISKSTATS, |n| sys_block.contains(&n));
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["sda", "nvme0n1"]);
        assert_eq!(d[1], DiskStat { name: "nvme0n1".into(), sectors_read: 3389106, sectors_written: 16839248, io_ms: 408500 });
        // The name heuristic agrees when /sys/block is unreadable.
        assert_eq!(parse_diskstats(DISKSTATS, looks_whole), d);
        // Only stacked devices (a VM on LVM over virtio-less storage): count those.
        let d = parse_diskstats(DISKSTATS, |n| n == "dm-0" || n == "loop0");
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["dm-0"]);
        assert!(parse_diskstats("", |_| true).is_empty());
    }

    #[test]
    fn whole_disk_names() {
        for w in ["sda", "sdab", "vdb", "xvda", "nvme0n1", "nvme10n2", "mmcblk0", "dm-3", "md127"] {
            assert!(looks_whole(w), "{w}");
        }
        for p in ["sda1", "vdb15", "nvme0n1p2", "mmcblk0p1", "nvme0", "sd"] {
            assert!(!looks_whole(p), "{p}");
        }
        assert!(never_counted("mmcblk0boot0") && never_counted("zram0") && never_counted("loop12") && !never_counted("sda"));
    }

    #[test]
    fn disk_rates_and_active_time() {
        let a = DiskTotals { disks: vec!["sda".into(), "sdb".into()], sectors_read: 1000, sectors_written: 0, io_ms: 0 };
        let b = DiskTotals { sectors_read: 3000, sectors_written: 4000, io_ms: 1000, ..a.clone() };
        let d = disk_delta(Some(&a), &b, Duration::from_secs(2));
        assert!(d.available);
        assert_eq!((d.read_bps, d.write_bps), (512_000.0, 1_024_000.0));
        // 1000 ms busy over 2 s on 2 disks: 25 %.
        assert_eq!(d.active_pct, 25.0);
        let first = disk_delta(None, &b, Duration::from_secs(1));
        assert!(first.available && first.read_bps == 0.0);
        let other = DiskTotals { disks: vec!["sda".into()], ..b.clone() };
        assert_eq!(disk_delta(Some(&a), &other, Duration::from_secs(1)).read_bps, 0.0);
        let busy = DiskTotals { io_ms: 100_000, ..b };
        assert_eq!(disk_delta(Some(&a), &busy, Duration::from_secs(1)).active_pct, 100.0);
    }

    #[test]
    fn net_totals_count_up_hardware_interfaces() {
        let i = |name: &str, up, hardware, speed, rx, tx| NetIf { name: name.into(), up, hardware, speed_mbps: speed, rx_bytes: rx, tx_bytes: tx };
        let ifs = [
            i("lo", true, false, None, 9999, 9999),
            i("eth0", true, true, Some(1000), 100, 200),
            i("wlan0", true, true, Some(-1), 10, 20),
            i("eth1", false, true, Some(1000), 1, 1),
            i("docker0", true, false, None, 5000, 5000),
        ];
        assert_eq!(net_totals(&ifs), (110, 220, 1_000_000_000));
        assert_eq!(net_totals(&[]), (0, 0, 0));
    }

    #[test]
    fn temperatures() {
        assert_eq!(millideg("65500\n"), Some(65.5));
        assert_eq!(millideg("0"), None);
        assert_eq!(millideg("-273150"), None);
        assert_eq!(millideg("255000"), None);
        assert_eq!(millideg("n/a"), None);
        let chip = |name: &str, temps: &[(&str, f32)]| HwmonChip { name: name.into(), temps: temps.iter().map(|(l, v)| (l.to_string(), *v)).collect() };
        let chips = [
            chip("nvme", &[("Composite", 41.9)]),
            chip("acpitz", &[("temp1", 27.8)]),
            chip("k10temp", &[("Tccd1", 61.0), ("Tctl", 65.5)]),
            chip("amdgpu", &[("edge", 48.0)]),
        ];
        let t = fold_thermal(&chips, vec![("acpitz".into(), 27.8)]);
        assert_eq!(t.cpu, Some(("Tctl".to_string(), 65.5)));
        assert_eq!(t.zones, vec![("acpitz".to_string(), 27.8), ("nvme Composite".to_string(), 41.9), ("amdgpu edge".to_string(), 48.0)]);
        assert_eq!(t.sources, "hwmon: nvme, acpitz, k10temp, amdgpu \u{b7} thermal zones: 1");
        assert_eq!(t.max_c(), Some(65.5));
        // Intel without a package label match falls back to the chip's first reading.
        let t = fold_thermal(&[chip("coretemp", &[("Core 0", 50.0), ("Core 1", 52.0)])], vec![]);
        assert_eq!(t.cpu, Some(("Core 0".to_string(), 50.0)));
        assert!(t.zones.is_empty());
        let t = fold_thermal(&[], vec![]);
        assert_eq!((t.max_c(), t.cpu), (None, None));
        assert_eq!(t.sources, "hwmon: no sensors \u{b7} thermal zones: none");
    }

    #[test]
    fn gpus_fold_to_the_busiest_device() {
        let amd = GpuDev { engines: vec![("Graphics".into(), 30.0)], vram_used: Some(512 << 20) };
        let nv = GpuDev { engines: vec![("Graphics".into(), 70.0), ("Encode".into(), 10.0), ("Decode".into(), 120.0)], vram_used: Some(1 << 30) };
        let g = fold_gpus(&[amd.clone(), nv]);
        assert!(g.available);
        assert_eq!(g.utilization_pct, 100.0);
        assert_eq!(g.engines.len(), 3);
        assert_eq!(g.dedicated_used, (512 << 20) + (1 << 30));
        let vram_only = GpuDev { engines: vec![], vram_used: Some(100) };
        let g = fold_gpus(&[vram_only]);
        assert!(!g.available && g.dedicated_used == 100);
        assert_eq!(fold_gpus(&[amd]).utilization_pct, 30.0);
        assert!(!fold_gpus(&[]).available);
    }

    #[test]
    fn small_files() {
        assert_eq!(parse_uptime("22868.05 347728.41\n"), Some(Duration::from_secs_f64(22868.05)));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_file_nr("1888\t0\t9223372036854775807\n"), Some(1888));
        assert_eq!(parse_file_nr("1888\t88\t100\n"), Some(1800));
        assert_eq!(parse_file_nr("x"), None);
        assert_eq!(parse_loadavg_threads("1.50 4.79 5.75 1/608 60571\n"), Some(608));
        assert_eq!(parse_loadavg_threads("1.50 4.79"), None);
    }
}
