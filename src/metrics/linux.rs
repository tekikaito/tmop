//! Linux metric collection.
//!
//! STUB until work package M-L fills it (`/proc/stat`, cpufreq, `/proc/meminfo`,
//! `/proc/diskstats`, `/sys/class/net`, hwmon and thermal zones, amdgpu sysfs, `/proc/uptime`,
//! `/proc/sys/fs/file-nr`) and wires the Linux process enumerator. Until then it publishes the
//! logical-processor count and the brand only; everything else stays None / unavailable, so the
//! UI hides it. Keep the public items (`Collector`, `probe`, `SOURCES`): `metrics/mod.rs`
//! re-exports exactly these. Shared helpers are in [`super::common`].

use super::common::cpu_brand;
use super::{Probe, Snapshot, Sources};
use crate::process::ProcessRow;

pub const SOURCES: Sources = Sources {
    disk: "Active time (all physical disks)",
    network: "Throughput (all adapters)",
    gpu: "Busiest engine of the busiest GPU",
    no_gpu: "No GPU utilization was reported.",
    thermal: "hwmon sensors and thermal zones",
    thermal_hint: "Load the k10temp or coretemp module (lm-sensors) for a CPU temperature",
    sensor: "hwmon",
    handles: "Open files",
};

/// What this machine reports. M-L: `clocks` when cpufreq or `/proc/cpuinfo` "cpu MHz" exists,
/// `thermal` when hwmon or a thermal zone exists.
pub fn probe() -> Probe {
    Probe { clocks: false, thermal: false }
}

pub struct Collector {
    seq: u64,
    logical: usize,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        Self { seq: 0, logical: std::thread::available_parallelism().map_or(1, |n| n.get()) }
    }

    /// M-L: fill every sample from /proc and /sys (None / `available: false` for what is
    /// missing) and return the rows of the Linux process enumerator.
    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, errors: vec!["Linux metrics are not implemented yet".into()], ..Default::default() };
        snap.cpu.logical_count = self.logical;
        snap.cpu.name = cpu_brand();
        (snap, Vec::new())
    }
}
