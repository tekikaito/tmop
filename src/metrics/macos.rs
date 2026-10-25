//! macOS metric collection.
//!
//! STUB until work package M-M fills it (`host_statistics` / `host_processor_info`, sysctl
//! clocks and brand, `vm_statistics64` and `vm.swapusage`, IOKit disk and GPU,
//! `NET_RT_IFLIST2`, `kern.boottime`) and wires the libproc process enumerator. Until then it
//! publishes the logical-processor count and the brand only; everything else stays None /
//! unavailable, so the UI hides it. Keep the public items (`Collector`, `probe`, `SOURCES`):
//! `metrics/mod.rs` re-exports exactly these. Shared helpers are in [`super::common`].

use super::common::cpu_brand;
use super::{Probe, Snapshot, Sources};
use crate::process::ProcessRow;

pub const SOURCES: Sources = Sources {
    disk: "Active time (all physical disks)",
    network: "Throughput (all interfaces)",
    gpu: "GPU utilization (IOAccelerator)",
    no_gpu: "No GPU utilization was reported.",
    thermal: "Temperatures are not available on macOS",
    thermal_hint: "macOS offers no public temperature sensors",
    sensor: "sensor",
    handles: "Open files",
};

/// What this machine reports. M-M: `clocks` on Intel Macs (`hw.cpufrequency`), none on Apple
/// Silicon; `thermal` stays false (no public sensors).
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

    /// M-M: fill every sample from mach, sysctl and IOKit (None / `available: false` for what
    /// is missing; `commit_*` stay None) and return the rows of the libproc enumerator.
    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, errors: vec!["macOS metrics are not implemented yet".into()], ..Default::default() };
        snap.cpu.logical_count = self.logical;
        snap.cpu.name = cpu_brand();
        (snap, Vec::new())
    }
}
