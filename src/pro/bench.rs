//! Benchmarks: CPU (single and all threads), memory bandwidth and latency, disk
//! sequential write/read and random 4K reads.
//!
//! Every test has a hard time budget and checks the cancel flag between short work
//! slices, so a run is always bounded and stops promptly. The disk tests only touch one
//! temporary file in the chosen folder (default: the system temp folder); a guard
//! deletes it when the test ends, fails or is cancelled.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Test {
    CpuSingle,
    CpuMulti,
    MemBandwidth,
    MemLatency,
    DiskWrite,
    DiskRead,
    DiskRandom,
}

impl Test {
    pub const ALL: [Test; 7] = [Test::CpuSingle, Test::CpuMulti, Test::MemBandwidth, Test::MemLatency, Test::DiskWrite, Test::DiskRead, Test::DiskRandom];

    pub fn id(self) -> &'static str {
        match self {
            Test::CpuSingle => "cpu-single",
            Test::CpuMulti => "cpu-multi",
            Test::MemBandwidth => "mem-bandwidth",
            Test::MemLatency => "mem-latency",
            Test::DiskWrite => "disk-write",
            Test::DiskRead => "disk-read",
            Test::DiskRandom => "disk-random",
        }
    }

    pub fn from_id(s: &str) -> Option<Test> {
        Test::ALL.into_iter().find(|t| t.id() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Test::CpuSingle => "CPU single thread",
            Test::CpuMulti => "CPU all threads",
            Test::MemBandwidth => "Memory bandwidth",
            Test::MemLatency => "Memory latency",
            Test::DiskWrite => "Disk sequential write",
            Test::DiskRead => "Disk sequential read",
            Test::DiskRandom => "Disk random 4K read",
        }
    }

    /// Short name for the dot-matrix card title.
    pub fn short(self) -> &'static str {
        match self {
            Test::CpuSingle => "CPU 1 thread",
            Test::CpuMulti => "CPU all",
            Test::MemBandwidth => "Mem bandwidth",
            Test::MemLatency => "Mem latency",
            Test::DiskWrite => "Disk write",
            Test::DiskRead => "Disk read",
            Test::DiskRandom => "Disk 4K random",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Test::CpuSingle | Test::CpuMulti => "Mops/s",
            Test::MemBandwidth => "GB/s",
            Test::MemLatency => "ns",
            Test::DiskWrite | Test::DiskRead => "MB/s",
            Test::DiskRandom => "IOPS",
        }
    }

    pub fn group(self) -> &'static str {
        match self {
            Test::CpuSingle | Test::CpuMulti => "cpu",
            Test::MemBandwidth | Test::MemLatency => "memory",
            _ => "disk",
        }
    }

    pub fn higher_is_better(self) -> bool {
        self != Test::MemLatency
    }

    /// Hard time budget of the measuring phase.
    pub fn budget(self) -> Duration {
        match self {
            Test::CpuSingle | Test::CpuMulti => Duration::from_secs(3),
            Test::MemBandwidth => Duration::from_secs(2),
            Test::MemLatency => Duration::from_millis(1500),
            Test::DiskWrite | Test::DiskRead => Duration::from_secs(4),
            Test::DiskRandom => Duration::from_secs(3),
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Test::CpuSingle => "Integer and floating-point mix on one thread",
            Test::CpuMulti => "The same mix on every logical processor",
            Test::MemBandwidth => "64 MiB block copies (read + write traffic)",
            Test::MemLatency => "Dependent random loads across 64 MiB",
            Test::DiskWrite => "Unbuffered 4 MiB writes to a temporary file",
            Test::DiskRead => "Unbuffered 4 MiB reads of that file",
            Test::DiskRandom => "Unbuffered 4 KiB reads at random offsets, queue depth 1",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BenchResult {
    pub test: Test,
    pub value: f64,
    /// Local time the test finished (`YYYY-MM-DD HH:MM:SS`).
    pub when: String,
    pub detail: String,
    /// A disk result measured through the OS page cache (direct I/O refused).
    pub buffered: bool,
}

/// Live state of a run, shared with the UI.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub running: Option<Test>,
    pub queue: Vec<Test>,
    /// 0..1 progress of the current test.
    pub fraction: f32,
    /// Throughput samples of the current test (unit of the test), about every 100 ms.
    pub live: Vec<f32>,
    pub message: String,
    pub cancelled: bool,
}

#[derive(Default)]
struct Shared {
    progress: Progress,
    results: Vec<BenchResult>,
}

#[derive(Default)]
pub struct Bench {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    /// All results, newest last (loaded from and saved to `benchmarks.json`).
    pub history: Vec<BenchResult>,
    loaded: bool,
}


pub fn results_path() -> PathBuf {
    crate::platform::data_dir().join("benchmarks.json")
}

impl Bench {
    /// Loads saved results once.
    pub fn ensure_loaded(&mut self) {
        if !self.loaded {
            self.loaded = true;
            self.history = load_results(&results_path());
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> Progress {
        self.shared.lock().map(|g| g.progress.clone()).unwrap_or_default()
    }

    /// Starts the given tests in order on a worker thread. `disk_dir` hosts the temp file.
    pub fn start(&mut self, tests: Vec<Test>, disk_dir: PathBuf) {
        if self.is_running() || tests.is_empty() {
            return;
        }
        self.ensure_loaded();
        self.cancel.store(false, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.shared.lock() {
            g.progress = Progress { queue: tests.clone(), message: "Starting".into(), ..Default::default() };
        }
        let shared = self.shared.clone();
        let cancel = self.cancel.clone();
        let running = self.running.clone();
        let r = std::thread::Builder::new().name("tmop-bench".into()).spawn(move || {
            run_all(&tests, &disk_dir, &shared, &cancel);
            running.store(false, Ordering::Relaxed);
        });
        if r.is_err() {
            self.running.store(false, Ordering::Relaxed);
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Moves finished results into `history` and saves them. Returns true if any arrived.
    pub fn poll(&mut self) -> bool {
        let new: Vec<BenchResult> = self.shared.lock().map(|mut g| std::mem::take(&mut g.results)).unwrap_or_default();
        if new.is_empty() {
            return false;
        }
        self.ensure_loaded();
        self.history.extend(new);
        let excess = self.history.len().saturating_sub(200);
        self.history.drain(..excess);
        let _ = save_results(&results_path(), &self.history);
        true
    }

    /// Latest result of a test.
    pub fn latest(&self, t: Test) -> Option<&BenchResult> {
        self.history.iter().rev().find(|r| r.test == t)
    }

    /// Best result of a test across history.
    pub fn best(&self, t: Test) -> Option<f64> {
        let it = self.history.iter().filter(|r| r.test == t).map(|r| r.value);
        if t.higher_is_better() {
            it.fold(None, |m, v| Some(m.map_or(v, |m: f64| m.max(v))))
        } else {
            it.fold(None, |m, v| Some(m.map_or(v, |m: f64| m.min(v))))
        }
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        let _ = save_results(&results_path(), &self.history);
    }
}

fn set_progress(shared: &Mutex<Shared>, f: impl FnOnce(&mut Progress)) {
    if let Ok(mut g) = shared.lock() {
        f(&mut g.progress);
    }
}

fn run_all(tests: &[Test], disk_dir: &Path, shared: &Mutex<Shared>, cancel: &AtomicBool) {
    // Disk read/random need the file written by the write test; the guard is shared.
    let mut disk_file: Option<TempFile> = None;
    for (i, t) in tests.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        set_progress(shared, |p| {
            p.running = Some(*t);
            p.queue = tests[i + 1..].to_vec();
            p.fraction = 0.0;
            p.live.clear();
            p.message = t.description().into();
        });
        let live = |frac: f32, sample: Option<f32>| {
            set_progress(shared, |p| {
                p.fraction = frac.clamp(0.0, 1.0);
                if let Some(s) = sample {
                    p.live.push(s);
                }
            });
        };
        let mut buffered = false;
        let outcome: Result<(f64, String), String> = match t {
            Test::CpuSingle => Ok(cpu_test(1, t.budget(), cancel, &live)),
            Test::CpuMulti => {
                let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
                Ok(cpu_test(n, t.budget(), cancel, &live))
            }
            Test::MemBandwidth => Ok(mem_bandwidth(t.budget(), cancel, &live)),
            Test::MemLatency => Ok(mem_latency(t.budget(), cancel, &live)),
            Test::DiskWrite | Test::DiskRead | Test::DiskRandom => {
                if disk_file.is_none() {
                    match TempFile::create(disk_dir) {
                        Ok(f) => disk_file = Some(f),
                        Err(e) => {
                            set_progress(shared, |p| p.message = e.clone());
                            continue;
                        }
                    }
                }
                let f = disk_file.as_mut().expect("temp file");
                let r = match t {
                    Test::DiskWrite => f.write_test(t.budget(), cancel, &live),
                    Test::DiskRead => f.read_test(t.budget(), cancel, &live),
                    _ => f.random_test(t.budget(), cancel, &live),
                };
                buffered = f.buffered;
                r
            }
        };
        let cancelled = cancel.load(Ordering::Relaxed);
        match outcome {
            Ok((value, detail)) if !cancelled && value.is_finite() && value > 0.0 => {
                if let Ok(mut g) = shared.lock() {
                    g.results.push(BenchResult { test: *t, value, when: super::display_stamp(), detail, buffered });
                }
            }
            Ok(_) => {}
            Err(e) => set_progress(shared, |p| p.message = format!("{}: {e}", t.title())),
        }
    }
    drop(disk_file); // deletes the temp file
    let cancelled = cancel.load(Ordering::Relaxed);
    set_progress(shared, |p| {
        p.running = None;
        p.queue.clear();
        p.fraction = 1.0;
        p.cancelled = cancelled;
        p.message = if cancelled { "Cancelled".into() } else { "Finished".into() };
    });
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

/// One slice of the CPU workload: `n` rounds of xorshift, multiply-add and a short
/// floating-point recurrence. Returns a value so the work cannot be optimised away.
pub fn cpu_kernel(seed: u64, n: u32) -> u64 {
    let mut x = seed | 1;
    let mut acc = 0u64;
    let mut f = (seed % 1000) as f64 * 0.001 + 0.5;
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        acc = acc.wrapping_add(x.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        f = f * 1.000_000_1 + 0.000_001;
        if f > 2.0 {
            f -= 1.0;
        }
    }
    acc ^ f.to_bits()
}

const CPU_SLICE: u32 = 200_000;

fn cpu_test(threads: usize, budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> (f64, String) {
    let stop = AtomicBool::new(false);
    let counts: Vec<std::sync::atomic::AtomicU64> = (0..threads).map(|_| std::sync::atomic::AtomicU64::new(0)).collect();
    let start = Instant::now();
    std::thread::scope(|s| {
        for (i, c) in counts.iter().enumerate() {
            let stop = &stop;
            s.spawn(move || {
                let mut seed = 0x1234_5678 + i as u64;
                while !stop.load(Ordering::Relaxed) {
                    seed = std::hint::black_box(cpu_kernel(seed, CPU_SLICE));
                    c.fetch_add(CPU_SLICE as u64, Ordering::Relaxed);
                }
            });
        }
        let mut last = (Instant::now(), 0u64);
        while start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            let total: u64 = counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
            let dt = last.0.elapsed().as_secs_f64().max(1e-6);
            let rate = (total - last.1) as f64 / dt / 1e6;
            last = (Instant::now(), total);
            live(start.elapsed().as_secs_f32() / budget.as_secs_f32(), Some(rate as f32));
        }
        stop.store(true, Ordering::Relaxed);
    });
    let secs = start.elapsed().as_secs_f64();
    let total: u64 = counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    (total as f64 / secs / 1e6, format!("{threads} thread(s), {:.1} s", secs))
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

const MEM_BYTES: usize = 64 << 20;

fn mem_bandwidth(budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> (f64, String) {
    let n = MEM_BYTES / 8;
    let src: Vec<u64> = (0..n as u64).collect();
    let mut dst = vec![0u64; n];
    let start = Instant::now();
    let mut bytes = 0f64;
    let mut window = (Instant::now(), 0f64);
    while start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
        dst.copy_from_slice(&src);
        std::hint::black_box(&mut dst);
        bytes += 2.0 * MEM_BYTES as f64;
        if window.0.elapsed() >= Duration::from_millis(100) {
            let rate = (bytes - window.1) / window.0.elapsed().as_secs_f64() / 1e9;
            window = (Instant::now(), bytes);
            live(start.elapsed().as_secs_f32() / budget.as_secs_f32(), Some(rate as f32));
        }
    }
    (bytes / start.elapsed().as_secs_f64() / 1e9, "64 MiB copy, read + write bytes counted".into())
}

/// Builds a single random cycle over `n` slots (Sattolo's algorithm).
pub fn random_cycle(n: usize, seed: u64) -> Vec<u32> {
    let mut next: Vec<u32> = (0..n as u32).collect();
    let mut x = seed | 1;
    for i in (1..n).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let j = (x % i as u64) as usize;
        next.swap(i, j);
    }
    next
}

fn mem_latency(budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> (f64, String) {
    // Slots are 64 bytes apart so every load touches a new cache line.
    let slots = MEM_BYTES / 64;
    let order = random_cycle(slots, 0xC0FF_EE11);
    let mut table = vec![0u32; slots * 16];
    for (i, n) in order.iter().enumerate() {
        table[i * 16] = *n;
    }
    let start = Instant::now();
    let mut idx = 0u32;
    let mut steps = 0u64;
    let mut window = (Instant::now(), 0u64);
    while start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
        for _ in 0..200_000 {
            idx = table[idx as usize * 16];
        }
        steps += 200_000;
        if window.0.elapsed() >= Duration::from_millis(100) {
            let ns = window.0.elapsed().as_nanos() as f64 / (steps - window.1) as f64;
            window = (Instant::now(), steps);
            live(start.elapsed().as_secs_f32() / budget.as_secs_f32(), Some(ns as f32));
        }
    }
    std::hint::black_box(idx);
    (start.elapsed().as_nanos() as f64 / steps.max(1) as f64, "64 MiB pointer chase".into())
}

// ---------------------------------------------------------------------------
// Disk
// ---------------------------------------------------------------------------

const CHUNK: usize = 4 << 20;
const MAX_FILE: u64 = 1 << 30;
const ALIGN: usize = 4096;

/// A 4096-aligned heap buffer (unbuffered I/O needs sector alignment).
struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

impl AlignedBuf {
    fn new(len: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len, ALIGN).expect("layout");
        // SAFETY: non-zero size, valid layout.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        Self { ptr, len }
    }
    fn as_mut(&mut self) -> &mut [u8] {
        // SAFETY: ptr is valid for len bytes for the lifetime of self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { std::alloc::dealloc(self.ptr, std::alloc::Layout::from_size_align(self.len, ALIGN).expect("layout")) }
    }
}

/// The benchmark's only file. Deleted on drop.
pub struct TempFile {
    pub path: PathBuf,
    written: u64,
    /// The file system refused direct I/O: the tests ran through the OS page cache.
    pub buffered: bool,
}

/// Opens the test file bypassing the OS page cache where the file system allows it: no
/// buffering plus write-through on Windows, `O_DIRECT` (+ `O_DSYNC` for writes) on Linux,
/// `F_NOCACHE` (+ `O_DSYNC`) on macOS. The flag is false when the cache is not bypassed (some
/// file systems refuse `O_DIRECT`); the file is then opened buffered.
fn open_unbuffered(path: &Path, write: bool) -> std::io::Result<(std::fs::File, bool)> {
    let options = || {
        let mut o = std::fs::OpenOptions::new();
        if write {
            o.write(true).create(true).truncate(true);
        } else {
            o.read(true);
        }
        o
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
        let mut o = options();
        o.custom_flags(FILE_FLAG_NO_BUFFERING | if write { FILE_FLAG_WRITE_THROUGH } else { 0 });
        o.attributes(FILE_ATTRIBUTE_TEMPORARY);
        o.open(path).map(|f| (f, true))
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let open = |direct: bool| options().custom_flags(if write { libc::O_DSYNC } else { 0 } | if direct { libc::O_DIRECT } else { 0 }).open(path);
        match open(true) {
            Ok(f) => Ok((f, true)),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => open(false).map(|f| (f, false)),
            Err(e) => Err(e),
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        let f = options().custom_flags(if write { libc::O_DSYNC } else { 0 }).open(path)?;
        // SAFETY: fcntl on a descriptor we own; F_NOCACHE takes an int argument.
        let direct = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) } != -1;
        Ok((f, direct))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        options().open(path).map(|f| (f, false))
    }
}

/// Evicts a buffered test file's cached pages so reads reach the disk where they can (Linux;
/// nothing to evict on tmpfs, whose pages are the file). Only used when the cache is not bypassed.
fn drop_cache(f: &std::fs::File) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: advisory call on a descriptor we own; offset 0 and length 0 cover the whole file.
        unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    }
    #[cfg(not(target_os = "linux"))]
    let _ = f;
}

/// Ends the detail of a disk result measured through the OS page cache.
pub const BUFFERED_NOTE: &str = "buffered: this file system refuses direct I/O, so the OS cache may inflate the result";

/// True when the error is the file system refusing direct I/O on a file it let us open.
#[cfg(unix)]
fn refuses_direct(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EINVAL)
}

#[cfg(not(unix))]
fn refuses_direct(_e: &std::io::Error) -> bool {
    false
}

impl TempFile {
    pub fn create(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("tmop-bench-{}-{}.tmp", std::process::id(), super::file_stamp()));
        Ok(Self { path, written: 0, buffered: false })
    }

    /// Opens the file for reading the way the write test managed to write it.
    fn open_read(&self) -> Result<std::fs::File, String> {
        let f = if self.buffered { std::fs::File::open(&self.path) } else { open_unbuffered(&self.path, false).map(|(f, _)| f) };
        let f = f.map_err(|e| e.to_string())?;
        if self.buffered {
            drop_cache(&f);
        }
        Ok(f)
    }

    /// `detail` plus the buffered note when the cache was not bypassed.
    fn note(&self, detail: String) -> String {
        if self.buffered {
            format!("{detail}, {BUFFERED_NOTE}")
        } else {
            detail
        }
    }

    fn write_test(&mut self, budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> Result<(f64, String), String> {
        use std::io::Write;
        let free = super::diskspace::free_space(self.path.parent().unwrap_or(Path::new("."))).unwrap_or(0);
        // Never use more than a tenth of the free space.
        let cap = MAX_FILE.min(free / 10) / CHUNK as u64 * CHUNK as u64;
        if cap < CHUNK as u64 * 4 {
            return Err("not enough free space for the disk test".into());
        }
        let (mut f, direct) = open_unbuffered(&self.path, true).map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.buffered = !direct;
        let mut buf = AlignedBuf::new(CHUNK);
        let mut x = 0x9E37_79B9u64;
        for b in buf.as_mut().chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            b.copy_from_slice(&x.to_le_bytes()[..b.len()]);
        }
        let start = Instant::now();
        let mut window = (Instant::now(), 0u64);
        self.written = 0;
        while self.written < cap && start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
            match f.write_all(buf.as_mut()) {
                Ok(()) => {}
                // Opened with O_DIRECT but the file system refuses it on write: go buffered.
                Err(e) if !self.buffered && self.written == 0 && refuses_direct(&e) => {
                    self.buffered = true;
                    f = std::fs::OpenOptions::new().write(true).truncate(true).open(&self.path).map_err(|e| e.to_string())?;
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            }
            self.written += CHUNK as u64;
            if window.0.elapsed() >= Duration::from_millis(100) {
                let rate = (self.written - window.1) as f64 / window.0.elapsed().as_secs_f64() / 1e6;
                window = (Instant::now(), self.written);
                let frac = (start.elapsed().as_secs_f32() / budget.as_secs_f32()).max(self.written as f32 / cap as f32);
                live(frac, Some(rate as f32));
            }
        }
        f.sync_all().map_err(|e| e.to_string())?;
        let secs = start.elapsed().as_secs_f64();
        Ok((self.written as f64 / secs / 1e6, self.note(format!("{} MiB in {secs:.1} s, {}", self.written >> 20, self.path.parent().map(|p| p.display().to_string()).unwrap_or_default()))))
    }

    fn read_test(&mut self, budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> Result<(f64, String), String> {
        use std::io::Read;
        if self.written == 0 {
            return Err("run the write test first".into());
        }
        let mut f = self.open_read()?;
        let mut buf = AlignedBuf::new(CHUNK);
        let start = Instant::now();
        let mut read = 0u64;
        let mut window = (Instant::now(), 0u64);
        while start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
            let n = f.read(buf.as_mut()).map_err(|e| e.to_string())?;
            if n == 0 {
                use std::io::Seek;
                f.seek(std::io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                if self.buffered {
                    drop_cache(&f);
                }
                continue;
            }
            read += n as u64;
            if window.0.elapsed() >= Duration::from_millis(100) {
                let rate = (read - window.1) as f64 / window.0.elapsed().as_secs_f64() / 1e6;
                window = (Instant::now(), read);
                live(start.elapsed().as_secs_f32() / budget.as_secs_f32(), Some(rate as f32));
            }
        }
        let secs = start.elapsed().as_secs_f64();
        Ok((read as f64 / secs / 1e6, self.note(format!("{} MiB in {secs:.1} s", read >> 20))))
    }

    fn random_test(&mut self, budget: Duration, cancel: &AtomicBool, live: &dyn Fn(f32, Option<f32>)) -> Result<(f64, String), String> {
        use std::io::{Read, Seek};
        if self.written == 0 {
            return Err("run the write test first".into());
        }
        let mut f = self.open_read()?;
        let mut buf = AlignedBuf::new(ALIGN);
        let blocks = self.written / ALIGN as u64;
        let mut x = 0xDEAD_BEEFu64;
        let start = Instant::now();
        let mut ops = 0u64;
        let mut window = (Instant::now(), 0u64);
        while start.elapsed() < budget && !cancel.load(Ordering::Relaxed) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            f.seek(std::io::SeekFrom::Start((x % blocks) * ALIGN as u64)).map_err(|e| e.to_string())?;
            f.read_exact(buf.as_mut()).map_err(|e| e.to_string())?;
            ops += 1;
            if window.0.elapsed() >= Duration::from_millis(100) {
                let rate = (ops - window.1) as f64 / window.0.elapsed().as_secs_f64();
                window = (Instant::now(), ops);
                live(start.elapsed().as_secs_f32() / budget.as_secs_f32(), Some(rate as f32));
            }
        }
        let secs = start.elapsed().as_secs_f64();
        let iops = ops as f64 / secs;
        Ok((iops, self.note(format!("{:.1} MB/s, {:.0} \u{b5}s average", iops * ALIGN as f64 / 1e6, 1e6 / iops.max(1e-9)))))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn load_results(path: &Path) -> Vec<BenchResult> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    items
        .iter()
        .filter_map(|v| {
            Some(BenchResult {
                test: Test::from_id(v.get("test")?.as_str()?)?,
                value: v.get("value")?.as_f64()?,
                when: v.get("when").and_then(|w| w.as_str()).unwrap_or_default().to_string(),
                detail: v.get("detail").and_then(|w| w.as_str()).unwrap_or_default().to_string(),
                buffered: v.get("buffered").and_then(|b| b.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

fn save_results(path: &Path, results: &[BenchResult]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let arr: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let mut v = serde_json::json!({ "test": r.test.id(), "value": r.value, "when": r.when, "detail": r.detail });
            // Only written when set, so files stay as they were where the cache is always bypassed.
            if r.buffered {
                v["buffered"] = true.into();
            }
            v
        })
        .collect();
    std::fs::write(path, serde_json::to_string_pretty(&serde_json::Value::Array(arr)).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_visits_every_slot() {
        let n = 1000;
        let next = random_cycle(n, 7);
        let mut seen = vec![false; n];
        let mut i = 0usize;
        for _ in 0..n {
            assert!(!seen[i]);
            seen[i] = true;
            i = next[i] as usize;
        }
        assert_eq!(i, 0);
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn cpu_kernel_is_deterministic() {
        assert_eq!(cpu_kernel(1, 1000), cpu_kernel(1, 1000));
        assert_ne!(cpu_kernel(1, 1000), cpu_kernel(2, 1000));
    }

    #[test]
    fn disk_tests_are_bounded_and_clean_up() {
        let dir = std::env::temp_dir().join(format!("tmop-bench-test-{}", std::process::id()));
        let cancel = AtomicBool::new(false);
        let path;
        {
            let mut f = TempFile::create(&dir).unwrap();
            path = f.path.clone();
            let t = Instant::now();
            let w = f.write_test(Duration::from_millis(300), &cancel, &|_, _| {});
            assert!(t.elapsed() < Duration::from_secs(20));
            let (mbps, detail) = w.expect("write test");
            assert!(mbps > 0.0);
            assert!(path.exists());
            // Windows always bypasses the cache; elsewhere the result says when it could not.
            assert!(!(cfg!(windows) && f.buffered));
            assert_eq!(detail.ends_with(BUFFERED_NOTE), f.buffered, "{detail}");
            let (r, _) = f.read_test(Duration::from_millis(200), &cancel, &|_, _| {}).expect("read test");
            assert!(r > 0.0);
            let (iops, _) = f.random_test(Duration::from_millis(200), &cancel, &|_, _| {}).expect("random test");
            assert!(iops > 0.0);
            // The buffered fallback reads the same file and labels its results.
            f.buffered = true;
            let (r, detail) = f.read_test(Duration::from_millis(100), &cancel, &|_, _| {}).expect("buffered read test");
            assert!(r > 0.0 && detail.ends_with(BUFFERED_NOTE), "{detail}");
            let (iops, detail) = f.random_test(Duration::from_millis(100), &cancel, &|_, _| {}).expect("buffered random test");
            assert!(iops > 0.0 && detail.ends_with(BUFFERED_NOTE), "{detail}");
        }
        assert!(!path.exists(), "temp file must be deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_stops_a_run_quickly() {
        // loaded = true: do not touch the user's saved results.
        let mut b = Bench { loaded: true, ..Default::default() };
        b.start(vec![Test::CpuMulti], std::env::temp_dir());
        std::thread::sleep(Duration::from_millis(150));
        b.cancel();
        let t = Instant::now();
        while b.is_running() {
            assert!(t.elapsed() < Duration::from_secs(2), "cancel took too long");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(b.progress().cancelled);
    }

    #[test]
    fn results_roundtrip() {
        let p = std::env::temp_dir().join(format!("tmop-bench-results-{}.json", std::process::id()));
        let r = vec![
            BenchResult { test: Test::DiskRead, value: 123.5, when: "2026-01-01 00:00:00".into(), detail: "x".into(), buffered: false },
            BenchResult { test: Test::DiskWrite, value: 99.0, when: "2026-01-01 00:00:01".into(), detail: "y".into(), buffered: true },
        ];
        save_results(&p, &r).unwrap();
        assert_eq!(load_results(&p), r);
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("buffered").count(), 1, "only set flags are written: {text}");
        let _ = std::fs::remove_file(&p);
    }
}
