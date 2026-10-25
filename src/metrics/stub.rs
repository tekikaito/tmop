//! Fallback for targets other than Windows, Linux and macOS: no metrics, so the library (and
//! its tests) still build. Only the logical-processor count is published.

use super::{Probe, Snapshot, Sources};
use crate::process::ProcessRow;

pub const SOURCES: Sources = Sources {
    disk: "Disk activity",
    network: "Network throughput",
    gpu: "GPU utilization",
    no_gpu: "No GPU utilization was reported.",
    thermal: "Temperature sensors",
    thermal_hint: "No temperature source is supported on this system",
    sensor: "sensor",
    handles: "Open files",
};

pub fn probe() -> Probe {
    Probe { clocks: false, thermal: false }
}

pub struct Collector {
    seq: u64,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        Collector { seq: 0 }
    }

    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, errors: vec![format!("metrics are not implemented on {}", crate::platform::os_name())], ..Default::default() };
        snap.cpu.logical_count = std::thread::available_parallelism().map_or(1, |n| n.get());
        (snap, Vec::new())
    }
}
