//! macOS metric collection.
//!
//! * CPU total, kernel and per core: `host_processor_info(PROCESSOR_CPU_LOAD_INFO)` (mach ticks;
//!   the total is the sum over processors). Brand: `machdep.cpu.brand_string`. Clocks: Intel
//!   Macs report a static `hw.cpufrequency` / `hw.cpufrequency_max`; Apple Silicon has no public
//!   clock, so the clock fields stay None.
//! * Memory: `hw.memsize`, `host_statistics64(HOST_VM_INFO64)` counted like Activity Monitor,
//!   `vm.swapusage`. No commit charge (None).
//! * Disk: IOKit `IOBlockStorageDriver` "Statistics" (bytes and service time).
//! * Network: `NET_RT_IFLIST2` sysctl (64-bit `if_data64` counters) over the up, physical
//!   interfaces.
//! * GPU: IOKit `IOAccelerator` "PerformanceStatistics" (device utilisation and memory in use;
//!   no engine breakdown).
//! * Uptime: `kern.boottime`; open files: `kern.num_files`; tasks and threads:
//!   `processor_set_statistics(PROCESSOR_SET_LOAD_INFO)`.
//! * No temperatures (no public sensors).
//!
//! Process rows come from the libproc enumerator of work package PR-M; until it lands the list
//! is empty. The FFI below is declared by hand (libSystem, IOKit, CoreFoundation) so no binding
//! crate is needed; the decoding is pure and lives in [`super::mac_parse`].

use super::common::{brand_from_bytes, cpu_brand, net_delta, tick_delta_pct, NetMark, Ticks};
use super::mac_parse::*;
use super::{MemorySample, Probe, Snapshot, Sources};
use crate::process::ProcessRow;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::mem::MaybeUninit;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const SOURCES: Sources = Sources {
    disk: "Active time (all block storage drivers)",
    network: "Throughput (all interfaces)",
    gpu: "GPU utilization (IOAccelerator)",
    no_gpu: "No GPU utilization was reported.",
    thermal: "Temperatures are not available on macOS",
    thermal_hint: "macOS offers no public temperature sensors",
    sensor: "sensor",
    handles: "Open files",
};

/// Clocks only on Intel Macs (`hw.cpufrequency`); no public temperature sensors.
pub fn probe() -> Probe {
    Probe { clocks: sysctl_mhz(c"hw.cpufrequency").is_some(), thermal: false }
}

#[allow(non_camel_case_types, non_upper_case_globals, dead_code)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type kern_return_t = c_int;
    pub type mach_port_t = u32;
    pub type natural_t = u32;
    pub type integer_t = c_int;
    pub type io_object_t = mach_port_t;
    pub type CFTypeRef = *const c_void;
    pub type CFIndex = isize;

    pub const KERN_SUCCESS: kern_return_t = 0;
    pub const PROCESSOR_CPU_LOAD_INFO: c_int = 2;
    pub const HOST_VM_INFO64: c_int = 4;
    pub const PROCESSOR_SET_LOAD_INFO: c_int = 4;
    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    pub const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;

    /// `struct vm_statistics64` up to `HOST_VM_INFO64_REV1` (mach/vm_statistics.h). Newer
    /// kernels append fields; passing this count makes them fill exactly these.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct vm_statistics64 {
        pub free_count: natural_t,
        pub active_count: natural_t,
        pub inactive_count: natural_t,
        pub wire_count: natural_t,
        pub zero_fill_count: u64,
        pub reactivations: u64,
        pub pageins: u64,
        pub pageouts: u64,
        pub faults: u64,
        pub cow_faults: u64,
        pub lookups: u64,
        pub hits: u64,
        pub purges: u64,
        pub purgeable_count: natural_t,
        pub speculative_count: natural_t,
        pub decompressions: u64,
        pub compressions: u64,
        pub swapins: u64,
        pub swapouts: u64,
        pub compressor_page_count: natural_t,
        pub throttled_count: natural_t,
        pub external_page_count: natural_t,
        pub internal_page_count: natural_t,
        pub total_uncompressed_pages_in_compressor: u64,
    }
    pub const HOST_VM_INFO64_REV1_COUNT: u32 = (size_of::<vm_statistics64>() / size_of::<integer_t>()) as u32;

    /// `struct processor_set_load_info` (mach/processor_info.h).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct processor_set_load_info {
        pub task_count: c_int,
        pub thread_count: c_int,
        pub load_average: integer_t,
        pub mach_factor: integer_t,
    }
    pub const PROCESSOR_SET_LOAD_INFO_COUNT: u32 = (size_of::<processor_set_load_info>() / size_of::<natural_t>()) as u32;

    // libSystem (mach).
    extern "C" {
        pub static mach_task_self_: mach_port_t;
        /// Kernel page size: the unit of `vm_statistics64`, also under Rosetta.
        pub static vm_kernel_page_size: usize;
        pub fn mach_host_self() -> mach_port_t;
        pub fn mach_port_deallocate(task: mach_port_t, name: mach_port_t) -> kern_return_t;
        pub fn host_processor_info(host: mach_port_t, flavor: c_int, out_count: *mut natural_t, out_info: *mut *mut integer_t, out_info_cnt: *mut u32) -> kern_return_t;
        pub fn host_statistics64(host: mach_port_t, flavor: c_int, info: *mut integer_t, count: *mut u32) -> kern_return_t;
        pub fn vm_deallocate(task: mach_port_t, address: usize, size: usize) -> kern_return_t;
        pub fn processor_set_default(host: mach_port_t, out: *mut mach_port_t) -> kern_return_t;
        pub fn processor_set_statistics(pset: mach_port_t, flavor: c_int, info: *mut integer_t, count: *mut u32) -> kern_return_t;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
        pub fn CFDictionaryGetTypeID() -> usize;
        pub fn CFNumberGetTypeID() -> usize;
        pub fn CFDictionaryGetValue(dict: CFTypeRef, key: *const c_void) -> *const c_void;
        pub fn CFNumberGetValue(number: CFTypeRef, the_type: CFIndex, value_ptr: *mut c_void) -> u8;
        pub fn CFStringCreateWithCString(alloc: CFTypeRef, c_str: *const c_char, encoding: u32) -> CFTypeRef;
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        /// Returns a +1 dictionary that `IOServiceGetMatchingServices` consumes.
        pub fn IOServiceMatching(name: *const c_char) -> CFTypeRef;
        pub fn IOServiceGetMatchingServices(main_port: mach_port_t, matching: CFTypeRef, existing: *mut io_object_t) -> kern_return_t;
        pub fn IOIteratorNext(iterator: io_object_t) -> io_object_t;
        pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
        pub fn IORegistryEntryCreateCFProperty(entry: io_object_t, key: CFTypeRef, allocator: CFTypeRef, options: u32) -> CFTypeRef;
        pub fn IORegistryEntryGetRegistryEntryID(entry: io_object_t, entry_id: *mut u64) -> kern_return_t;
    }
}

// The hand-written offsets in mac_parse and the structs above against libc's definitions, checked
// at compile time on every macOS target (cross clippy included).
const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<libc::if_msghdr2>() == IF_MSGHDR2_LEN);
    assert!(offset_of!(libc::if_msghdr2, ifm_type) == 3);
    assert!(offset_of!(libc::if_msghdr2, ifm_flags) == OFF_FLAGS);
    assert!(offset_of!(libc::if_msghdr2, ifm_index) == OFF_INDEX);
    assert!(offset_of!(libc::if_msghdr2, ifm_data) == OFF_DATA);
    assert!(offset_of!(libc::if_data64, ifi_type) == OFF_TYPE - OFF_DATA);
    assert!(offset_of!(libc::if_data64, ifi_baudrate) == OFF_BAUDRATE - OFF_DATA);
    assert!(offset_of!(libc::if_data64, ifi_ibytes) == OFF_IBYTES - OFF_DATA);
    assert!(offset_of!(libc::if_data64, ifi_obytes) == OFF_OBYTES - OFF_DATA);
    assert!(libc::RTM_IFINFO2 as u8 == RTM_IFINFO2);
    assert!(libc::IFF_UP == IFF_UP && libc::IFF_RUNNING == IFF_RUNNING && libc::IFF_LOOPBACK == IFF_LOOPBACK);
    assert!(size_of::<ffi::vm_statistics64>() == 152 && ffi::HOST_VM_INFO64_REV1_COUNT == 38);
    assert!(offset_of!(ffi::vm_statistics64, purgeable_count) == offset_of!(libc::vm_statistics64, purgeable_count));
    assert!(offset_of!(ffi::vm_statistics64, compressor_page_count) == offset_of!(libc::vm_statistics64, compressor_page_count));
    assert!(offset_of!(ffi::vm_statistics64, internal_page_count) == offset_of!(libc::vm_statistics64, internal_page_count));
    assert!(offset_of!(ffi::vm_statistics64, total_uncompressed_pages_in_compressor) == offset_of!(libc::vm_statistics64, total_uncompressed_pages_in_compressor));
    assert!(size_of::<ffi::processor_set_load_info>() == size_of::<libc::processor_set_load_info>());
    assert!(ffi::PROCESSOR_CPU_LOAD_INFO == libc::PROCESSOR_CPU_LOAD_INFO && ffi::HOST_VM_INFO64 == libc::HOST_VM_INFO64);
    assert!(size_of::<libc::processor_cpu_load_info>() == 4 * size_of::<ffi::integer_t>());
    assert!(size_of::<libc::xsw_usage>() == 32 && size_of::<libc::timeval>() == 16);
};

/// `mach_task_self()`: the task's own port name (a macro in C, not a call).
fn task_self() -> ffi::mach_port_t {
    // SAFETY: set by libSystem before main and never changed.
    unsafe { ffi::mach_task_self_ }
}

/// A send right this process holds; its user reference is dropped once.
struct Port(ffi::mach_port_t);

impl Drop for Port {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: we own one user reference to this right.
            unsafe { ffi::mach_port_deallocate(task_self(), self.0) };
        }
    }
}

/// An owned (+1) CoreFoundation object.
struct Cf(ffi::CFTypeRef);

// SAFETY: only immutable CF objects (strings, property copies) are held, and CF reference
// counting is thread-safe.
unsafe impl Send for Cf {}

impl Cf {
    fn string(s: &CStr) -> Cf {
        // SAFETY: s is NUL-terminated; a null result is handled by every user.
        Cf(unsafe { ffi::CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), ffi::K_CF_STRING_ENCODING_UTF8) })
    }

    /// An integer under `key` when `self` is a dictionary and the value a number.
    fn dict_i64(&self, key: &Cf) -> Option<i64> {
        if self.0.is_null() || key.0.is_null() {
            return None;
        }
        // SAFETY: both are live CF objects; the dictionary value is borrowed (+0) and only read
        // while `self` keeps the dictionary alive.
        unsafe {
            if ffi::CFGetTypeID(self.0) != ffi::CFDictionaryGetTypeID() {
                return None;
            }
            let v = ffi::CFDictionaryGetValue(self.0, key.0);
            if v.is_null() || ffi::CFGetTypeID(v) != ffi::CFNumberGetTypeID() {
                return None;
            }
            let mut out = 0i64;
            // Returns false for a lossy conversion (a float) but still stores the value.
            ffi::CFNumberGetValue(v, ffi::K_CF_NUMBER_SINT64_TYPE, (&mut out as *mut i64).cast());
            Some(out)
        }
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own one reference.
            unsafe { ffi::CFRelease(self.0) };
        }
    }
}

/// Runs `f` on every IOKit service matching `class` and releases each; false when the
/// registry could not be queried.
fn each_service(class: &CStr, mut f: impl FnMut(ffi::io_object_t)) -> bool {
    // SAFETY: the matching dictionary is consumed by IOServiceGetMatchingServices (even on
    // failure); the iterator and every service it yields are released exactly once.
    unsafe {
        let matching = ffi::IOServiceMatching(class.as_ptr());
        if matching.is_null() {
            return false;
        }
        let mut iter: ffi::io_object_t = 0;
        // kIOMainPortDefault is MACH_PORT_NULL.
        if ffi::IOServiceGetMatchingServices(0, matching, &mut iter) != ffi::KERN_SUCCESS {
            return false;
        }
        loop {
            let svc = ffi::IOIteratorNext(iter);
            if svc == 0 {
                break;
            }
            f(svc);
            ffi::IOObjectRelease(svc);
        }
        ffi::IOObjectRelease(iter);
    }
    true
}

/// A copy (+1) of a registry property of `svc`.
fn property(svc: ffi::io_object_t, key: &Cf) -> Option<Cf> {
    if key.0.is_null() {
        return None;
    }
    // SAFETY: svc is a live registry entry, key a live CFString.
    let v = unsafe { ffi::IORegistryEntryCreateCFProperty(svc, key.0, std::ptr::null(), 0) };
    (!v.is_null()).then_some(Cf(v))
}

/// Writes up to `len` bytes of sysctl `name` into `buf`; the length written.
fn sysctl_into(name: &CStr, buf: *mut c_void, len: usize) -> Option<usize> {
    let mut n = len;
    // SAFETY: buf is writable for len bytes (or null with len 0 to ask for the size).
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), buf, &mut n, std::ptr::null_mut(), 0) };
    (r == 0).then_some(n)
}

fn sysctl_bytes(name: &CStr) -> Option<Vec<u8>> {
    let len = sysctl_into(name, std::ptr::null_mut(), 0)?;
    let mut buf = vec![0u8; len];
    let n = sysctl_into(name, buf.as_mut_ptr().cast(), len)?;
    buf.truncate(n);
    Some(buf)
}

/// A plain-data sysctl value that must fill `T` exactly.
fn sysctl_val<T: Copy>(name: &CStr) -> Option<T> {
    let mut v = MaybeUninit::<T>::zeroed();
    let n = sysctl_into(name, v.as_mut_ptr().cast(), size_of::<T>())?;
    // SAFETY: zero-initialised plain data (integers and C structs of integers), fully written.
    (n == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

fn sysctl_uint(name: &CStr) -> Option<u64> {
    ne_uint(&sysctl_bytes(name)?)
}

fn sysctl_mhz(name: &CStr) -> Option<u32> {
    hz_mhz(sysctl_uint(name)?)
}

/// The `NET_RT_IFLIST2` dump, retried while interfaces appear between the size query and the read.
fn iflist2() -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    for _ in 0..4 {
        let mut len = 0usize;
        // SAFETY: a size query (null buffer).
        if unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) } != 0 {
            return None;
        }
        len += len / 8 + 512;
        let mut buf = vec![0u8; len];
        // SAFETY: buf is writable for len bytes.
        if unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } == 0 {
            buf.truncate(len);
            return Some(buf);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

fn if_name(index: u16) -> String {
    let mut buf = [0 as c_char; libc::IFNAMSIZ];
    // SAFETY: buf holds IFNAMSIZ bytes, as if_indextoname requires.
    let p = unsafe { libc::if_indextoname(index as u32, buf.as_mut_ptr()) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: on success buf holds a NUL-terminated name.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// CF keys created once per collector.
struct Keys {
    statistics: Cf,
    bytes_read: Cf,
    bytes_write: Cf,
    time_read: Cf,
    time_write: Cf,
    perf: Cf,
    utilization: Cf,
    vram: Cf,
    system_mem: Cf,
}

impl Keys {
    fn new() -> Self {
        Self {
            statistics: Cf::string(c"Statistics"),
            bytes_read: Cf::string(c"Bytes (Read)"),
            bytes_write: Cf::string(c"Bytes (Write)"),
            time_read: Cf::string(c"Total Time (Read)"),
            time_write: Cf::string(c"Total Time (Write)"),
            perf: Cf::string(c"PerformanceStatistics"),
            utilization: Cf::string(c"Device Utilization %"),
            vram: Cf::string(c"vramUsedBytes"),
            system_mem: Cf::string(c"In use system memory"),
        }
    }
}

pub struct Collector {
    seq: u64,
    host: Port,
    logical: usize,
    brand: String,
    base_mhz: Option<u32>,
    max_mhz: Option<u32>,
    mem_total: u64,
    page: u64,
    prev_cores: Vec<[u32; 4]>,
    prev_net: Option<NetMark>,
    prev_disk: Option<(Instant, HashMap<u64, DiskCounters>)>,
    keys: Keys,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        let brand = sysctl_bytes(c"machdep.cpu.brand_string").map(|b| brand_from_bytes(&b)).filter(|b| !b.is_empty()).unwrap_or_else(cpu_brand);
        let logical = sysctl_val::<c_int>(c"hw.logicalcpu").filter(|n| *n > 0).map(|n| n as usize).unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        // SAFETY: plain reads of a libSystem global and a port-returning call.
        let (page, host) = unsafe { (ffi::vm_kernel_page_size as u64, Port(ffi::mach_host_self())) };
        Self {
            seq: 0,
            host,
            logical,
            brand,
            base_mhz: sysctl_mhz(c"hw.cpufrequency"),
            max_mhz: sysctl_mhz(c"hw.cpufrequency_max"),
            mem_total: sysctl_uint(c"hw.memsize").unwrap_or(0),
            page: if page == 0 { 4096 } else { page },
            prev_cores: Vec::new(),
            prev_net: None,
            prev_disk: None,
            keys: Keys::new(),
        }
    }

    pub fn collect(&mut self) -> (Snapshot, Vec<ProcessRow>) {
        self.seq += 1;
        let mut snap = Snapshot { seq: self.seq, ..Default::default() };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        if let Some(tv) = sysctl_val::<libc::timeval>(c"kern.boottime") {
            snap.uptime = uptime_since(tv.tv_sec, tv.tv_usec as i64, now);
        }
        self.cpu(&mut snap);
        self.memory(&mut snap);
        self.disk(&mut snap);
        self.network(&mut snap);
        self.gpu(&mut snap);
        snap.thermal.sources = SOURCES.thermal.into();
        snap.handle_count = sysctl_val::<c_int>(c"kern.num_files").map(|n| n.max(0) as usize);
        if let Some(l) = self.load_info() {
            snap.process_count = l.task_count.max(0) as usize;
            snap.thread_count = l.thread_count.max(0) as usize;
        }
        (snap, Vec::new())
    }

    /// The per-processor tick counters; the kernel's copy is freed again.
    fn cpu_loads(&self) -> Result<Vec<[u32; 4]>, String> {
        let (mut n, mut info, mut cnt): (ffi::natural_t, *mut ffi::integer_t, u32) = (0, std::ptr::null_mut(), 0);
        // SAFETY: out-pointers to locals; on success `info` is a vm allocation of `cnt` integers in our task.
        let kr = unsafe { ffi::host_processor_info(self.host.0, ffi::PROCESSOR_CPU_LOAD_INFO, &mut n, &mut info, &mut cnt) };
        if kr != ffi::KERN_SUCCESS || info.is_null() {
            return Err(format!("host_processor_info failed: {kr}"));
        }
        // SAFETY: `info` holds `cnt` integers (n x processor_cpu_load_info); copied before freeing.
        let loads = unsafe { std::slice::from_raw_parts(info as *const u32, cnt as usize) }.as_chunks::<4>().0.iter().take(n as usize).copied().collect();
        // SAFETY: frees exactly the region host_processor_info allocated.
        unsafe { ffi::vm_deallocate(task_self(), info as usize, cnt as usize * size_of::<ffi::integer_t>()) };
        Ok(loads)
    }

    fn cpu(&mut self, snap: &mut Snapshot) {
        let c = &mut snap.cpu;
        c.name = self.brand.clone();
        c.logical_count = self.logical;
        c.base_mhz = self.base_mhz;
        c.max_mhz = self.max_mhz.or(self.base_mhz);
        c.boost_mhz = self.max_mhz.filter(|m| self.base_mhz.is_some_and(|b| *m > b));
        // Static: macOS reports no live clock.
        c.current_mhz = self.base_mhz;
        c.clock_live = false;
        let loads = match self.cpu_loads() {
            Ok(l) if !l.is_empty() => l,
            Ok(_) => return,
            Err(e) => {
                snap.errors.push(e);
                return;
            }
        };
        c.logical_count = loads.len();
        c.per_core_pct = vec![0.0; loads.len()];
        c.per_core_kernel_pct = vec![0.0; loads.len()];
        if self.prev_cores.len() == loads.len() {
            let deltas: Vec<Ticks> = self.prev_cores.iter().zip(&loads).map(|(p, l)| load_delta(*p, *l)).collect();
            for (i, d) in deltas.iter().enumerate() {
                if let Some((busy, kern)) = tick_delta_pct(Ticks::default(), *d) {
                    c.per_core_pct[i] = busy;
                    c.per_core_kernel_pct[i] = kern;
                }
            }
            if let Some((busy, kern)) = tick_delta_pct(Ticks::default(), sum_ticks(deltas)) {
                c.total_pct = busy;
                c.kernel_pct = kern;
            }
        }
        self.prev_cores = loads;
    }

    fn memory(&self, snap: &mut Snapshot) {
        let mut vm = ffi::vm_statistics64::default();
        let mut count = ffi::HOST_VM_INFO64_REV1_COUNT;
        // SAFETY: vm is writable for `count` integers; the kernel lowers count to what it wrote.
        let kr = unsafe { ffi::host_statistics64(self.host.0, ffi::HOST_VM_INFO64, (&mut vm as *mut ffi::vm_statistics64).cast(), &mut count) };
        let total = self.mem_total;
        let mut m = MemorySample { total, ..Default::default() };
        if kr == ffi::KERN_SUCCESS && total > 0 {
            let v = VmCounts {
                free: vm.free_count as u64,
                wire: vm.wire_count as u64,
                purgeable: vm.purgeable_count as u64,
                compressor: vm.compressor_page_count as u64,
                external: vm.external_page_count as u64,
                internal: vm.internal_page_count as u64,
            };
            let (available, cached) = memory_figures(total, self.page, &v);
            m.available = available;
            m.cached = cached;
            m.used_pct = ((total - available) as f64 / total as f64 * 100.0) as f32;
        } else {
            snap.errors.push(format!("host_statistics64(HOST_VM_INFO64) failed: {kr}"));
        }
        if let Some(sw) = sysctl_val::<libc::xsw_usage>(c"vm.swapusage") {
            m.swap_total = Some(sw.xsu_total);
            m.swap_used = Some(sw.xsu_used);
        }
        snap.memory = m;
    }

    fn disk(&mut self, snap: &mut Snapshot) {
        let k = &self.keys;
        let mut cur = HashMap::new();
        let ok = each_service(c"IOBlockStorageDriver", |svc| {
            let Some(stats) = property(svc, &k.statistics) else { return };
            let mut id = 0u64;
            // SAFETY: svc is live; id is an out-pointer.
            if unsafe { ffi::IORegistryEntryGetRegistryEntryID(svc, &mut id) } != ffi::KERN_SUCCESS {
                return;
            }
            let get = |key: &Cf| stats.dict_i64(key).unwrap_or(0).max(0) as u64;
            cur.insert(id, DiskCounters { read_bytes: get(&k.bytes_read), write_bytes: get(&k.bytes_write), busy_ns: get(&k.time_read) + get(&k.time_write) });
        });
        if !ok {
            snap.errors.push("IOKit: IOBlockStorageDriver query failed".into());
        }
        let now = Instant::now();
        snap.disk.available = !cur.is_empty();
        if let Some((t, prev)) = &self.prev_disk {
            let (active, read, write) = disk_rates(prev, &cur, now.duration_since(*t));
            snap.disk.active_pct = active;
            snap.disk.read_bps = read;
            snap.disk.write_bps = write;
        }
        self.prev_disk = Some((now, cur));
    }

    fn network(&mut self, snap: &mut Snapshot) {
        let Some(buf) = iflist2() else {
            snap.errors.push("sysctl NET_RT_IFLIST2 failed".into());
            return;
        };
        let (mut rx, mut tx, mut link) = (0u64, 0u64, 0u64);
        for r in parse_iflist2(&buf) {
            if counts_interface(&if_name(r.index), r.flags) {
                rx = rx.wrapping_add(r.ibytes);
                tx = tx.wrapping_add(r.obytes);
                link = link.saturating_add(r.baudrate);
            }
        }
        let now = Instant::now();
        snap.network = net_delta(self.prev_net, now, rx, tx, link);
        self.prev_net = Some((now, rx, tx));
    }

    fn gpu(&self, snap: &mut Snapshot) {
        let k = &self.keys;
        let mut stats = Vec::new();
        each_service(c"IOAccelerator", |svc| {
            if let Some(p) = property(svc, &k.perf) {
                stats.push(AccelStats { utilization: p.dict_i64(&k.utilization), vram_used: p.dict_i64(&k.vram), system_used: p.dict_i64(&k.system_mem) });
            }
        });
        snap.gpu = fold_accel(&stats);
    }

    /// Task and thread counts of the default processor set (what `top` shows).
    fn load_info(&self) -> Option<ffi::processor_set_load_info> {
        let mut pset: ffi::mach_port_t = 0;
        // SAFETY: out-pointer; the returned name right is released by `Port`.
        if unsafe { ffi::processor_set_default(self.host.0, &mut pset) } != ffi::KERN_SUCCESS {
            return None;
        }
        let pset = Port(pset);
        let mut info = ffi::processor_set_load_info::default();
        let mut count = ffi::PROCESSOR_SET_LOAD_INFO_COUNT;
        // SAFETY: info is writable for `count` integers.
        let kr = unsafe { ffi::processor_set_statistics(pset.0, ffi::PROCESSOR_SET_LOAD_INFO, (&mut info as *mut ffi::processor_set_load_info).cast(), &mut count) };
        (kr == ffi::KERN_SUCCESS).then_some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Runs on the macOS CI runner: every source the backend reads answers sensibly.
    #[test]
    fn macos_sources_answer() {
        let mut c = Collector::new();
        let _ = c.collect();
        // Keep a core busy so the second sample sees CPU time.
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(300) {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(1));
        }
        let (s, _) = c.collect();
        eprintln!("{:?}\n{:?}\n{:?}\n{:?}\n{:?}\nuptime {:?} procs {} threads {} files {:?} errors {:?}", s.cpu, s.memory, s.disk, s.network, s.gpu, s.uptime, s.process_count, s.thread_count, s.handle_count, s.errors);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert!(!s.cpu.name.is_empty());
        assert!(s.cpu.logical_count >= 1 && s.cpu.per_core_pct.len() == s.cpu.logical_count);
        assert!(s.cpu.total_pct > 0.0 && s.cpu.total_pct <= 100.0, "{}", s.cpu.total_pct);
        assert!(s.memory.total > 0 && s.memory.available > 0 && s.memory.available < s.memory.total);
        assert!(s.memory.swap_total.is_some() && s.memory.commit_used.is_none());
        assert!(s.disk.available, "no IOBlockStorageDriver statistics");
        assert!(s.network.available);
        assert!(s.uptime > Duration::from_secs(1));
        assert!(s.process_count > 1 && s.thread_count >= s.process_count);
        assert!(s.handle_count.is_some_and(|n| n > 0));
        assert_eq!(probe().clocks, s.cpu.base_mhz.is_some());
        // Repeated sampling must not leak ports or CF objects into a failure.
        for _ in 0..50 {
            let _ = c.collect();
        }
    }
}
