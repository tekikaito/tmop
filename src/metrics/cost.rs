//! What sampling costs, for the `TMOP_PROFILE` line (#194): for each part of a sample the number
//! of runs and their wall time, and the sampler thread's own CPU time. Counting is a few atomic
//! adds per sample, so it always runs; only the profile line reads (and resets) the totals.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A part of one sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The whole `Collector::collect`.
    Collect,
    /// The PDH query (disk, GPU engines and memory, thermal zones, clock).
    Pdh,
    /// Folding the GPU counters into adapters, sensors and per-process GPU memory.
    Gpu,
    /// The process list.
    Processes,
}

impl Part {
    pub const ALL: [Part; 4] = [Part::Collect, Part::Pdh, Part::Gpu, Part::Processes];

    pub fn name(self) -> &'static str {
        match self {
            Part::Collect => "collect",
            Part::Pdh => "pdh",
            Part::Gpu => "gpu",
            Part::Processes => "procs",
        }
    }
}

static RUNS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static NANOS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// The sampler thread's CPU time so far, in nanoseconds (0 where the OS does not say).
static SAMPLER_CPU: AtomicU64 = AtomicU64::new(0);

/// Adds one run of `part` that took `d`.
pub fn add(part: Part, d: Duration) {
    RUNS[part as usize].fetch_add(1, Ordering::Relaxed);
    NANOS[part as usize].fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
}

/// Runs `f` and counts it as one run of `part`.
pub fn time<T>(part: Part, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    add(part, t.elapsed());
    out
}

/// Called on the sampler thread after each sample.
pub fn note_sampler_cpu() {
    if let Some(d) = thread_cpu() {
        SAMPLER_CPU.store(d.as_nanos() as u64, Ordering::Relaxed);
    }
}

/// The sampler thread's CPU time so far (zero where the OS does not say).
pub fn sampler_cpu() -> Duration {
    Duration::from_nanos(SAMPLER_CPU.load(Ordering::Relaxed))
}

/// Runs and total time of each part since the last call, in the order of [`Part::ALL`].
pub fn take() -> [(u64, Duration); 4] {
    Part::ALL.map(|p| (RUNS[p as usize].swap(0, Ordering::Relaxed), Duration::from_nanos(NANOS[p as usize].swap(0, Ordering::Relaxed))))
}

/// CPU time (user + kernel) of the calling thread; `None` off Windows.
#[cfg(windows)]
pub fn thread_cpu() -> Option<Duration> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let zero = FILETIME::default();
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo handle of the calling thread and four valid out pointers.
    unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user).ok()? };
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Some(Duration::from_nanos((ticks(kernel) + ticks(user)) * 100))
}

/// CPU time of the calling thread; `None` off Windows.
#[cfg(not(windows))]
pub fn thread_cpu() -> Option<Duration> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_returns_what_was_added_and_resets() {
        // Other tests may sample at the same time; only this test adds to `Gpu` with these sizes.
        add(Part::Gpu, Duration::from_millis(3));
        time(Part::Gpu, || ());
        let after = take();
        let (runs, total) = after[Part::Gpu as usize];
        assert!(runs >= 2 && total >= Duration::from_millis(3), "{runs} {total:?}");
        assert_eq!(Part::ALL.map(Part::name), ["collect", "pdh", "gpu", "procs"]);
    }

    #[cfg(windows)]
    #[test]
    fn thread_cpu_grows_with_work() {
        let a = thread_cpu().expect("GetThreadTimes");
        let mut x = 0u64;
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(60) {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        assert!(thread_cpu().expect("GetThreadTimes") > a);
    }
}
