//! NVIDIA GPUs through NVML, loaded at run time: `dlopen("libnvidia-ml.so.1")` (installed with the
//! driver, also under WSL), so the binary has no link-time NVIDIA dependency and runs without it.
//! Plain libc `dlopen` keeps this to a handful of symbols with no new crate.

use super::procfs::GpuDev;
use std::ffi::{c_int, c_uint, c_void, CStr};

type Device = *mut c_void;
const SUCCESS: c_int = 0;

/// `nvmlUtilization_t`; `memory` (memory-controller busy %) is not shown.
#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

/// `nvmlMemory_t` (bytes).
#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

type InitFn = unsafe extern "C" fn() -> c_int;
type CountFn = unsafe extern "C" fn(*mut c_uint) -> c_int;
type HandleFn = unsafe extern "C" fn(c_uint, *mut Device) -> c_int;
type UtilFn = unsafe extern "C" fn(Device, *mut Utilization) -> c_int;
type MemFn = unsafe extern "C" fn(Device, *mut Memory) -> c_int;
/// `nvmlDeviceGetEncoderUtilization` / `...Decoder...`: (device, %, sampling period in µs).
type CodecFn = unsafe extern "C" fn(Device, *mut c_uint, *mut c_uint) -> c_int;

/// A loaded and initialised NVML with the devices it found. The library stays loaded for the
/// life of the process (never `dlclose`d: NVML may keep threads of its own).
pub struct Nvml {
    devices: Vec<Device>,
    util: UtilFn,
    mem: MemFn,
    encoder: Option<CodecFn>,
    decoder: Option<CodecFn>,
    shutdown: InitFn,
}

/// The symbol `name` (NUL-terminated) of `lib` as a function pointer of type `F`.
///
/// # Safety
/// `F` must be the symbol's C signature.
unsafe fn sym<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    let p = libc::dlsym(lib, name.as_ptr());
    // SAFETY: a non-null symbol of a function type is a function pointer of the same size.
    (!p.is_null()).then(|| std::mem::transmute_copy::<*mut c_void, F>(&p))
}

impl Nvml {
    /// None when the library is missing (no NVIDIA driver), fails to initialise or finds no GPU.
    pub fn load() -> Option<Nvml> {
        let name: &CStr = c"libnvidia-ml.so.1";
        // SAFETY: dlopen with a valid C string; the signatures below are NVML's (nvml.h, API v2+).
        unsafe {
            let lib = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if lib.is_null() {
                return None;
            }
            let init: InitFn = sym(lib, c"nvmlInit_v2")?;
            let shutdown: InitFn = sym(lib, c"nvmlShutdown")?;
            let count: CountFn = sym(lib, c"nvmlDeviceGetCount_v2")?;
            let handle: HandleFn = sym(lib, c"nvmlDeviceGetHandleByIndex_v2")?;
            let util: UtilFn = sym(lib, c"nvmlDeviceGetUtilizationRates")?;
            let mem: MemFn = sym(lib, c"nvmlDeviceGetMemoryInfo")?;
            if init() != SUCCESS {
                return None;
            }
            let mut n: c_uint = 0;
            let devices: Vec<Device> = if count(&mut n) == SUCCESS {
                (0..n.min(64))
                    .filter_map(|i| {
                        let mut d: Device = std::ptr::null_mut();
                        (handle(i, &mut d) == SUCCESS && !d.is_null()).then_some(d)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if devices.is_empty() {
                shutdown();
                return None;
            }
            Some(Nvml { devices, util, mem, encoder: sym(lib, c"nvmlDeviceGetEncoderUtilization"), decoder: sym(lib, c"nvmlDeviceGetDecoderUtilization"), shutdown })
        }
    }

    /// One reading per GPU: "Graphics" (time a kernel ran), "Encode" and "Decode" where the GPU
    /// reports them, and memory in use. A GPU whose utilisation query fails (e.g. lost) has no
    /// engines but may still report memory.
    pub fn read(&self) -> Vec<GpuDev> {
        self.devices
            .iter()
            .map(|&d| {
                let mut dev = GpuDev::default();
                // SAFETY: `d` is a handle NVML returned; out-pointers are valid locals.
                unsafe {
                    let mut u = Utilization::default();
                    if (self.util)(d, &mut u) == SUCCESS {
                        dev.engines.push(("Graphics".into(), u.gpu as f32));
                        for (name, f) in [("Encode", self.encoder), ("Decode", self.decoder)] {
                            let (mut pct, mut period) = (0, 0);
                            if f.is_some_and(|f| f(d, &mut pct, &mut period) == SUCCESS) {
                                dev.engines.push((name.into(), pct as f32));
                            }
                        }
                    }
                    let mut m = Memory::default();
                    if (self.mem)(d, &mut m) == SUCCESS {
                        dev.vram_used = Some(m.used);
                    }
                }
                dev
            })
            .collect()
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the successful nvmlInit_v2 in `load`.
        unsafe {
            (self.shutdown)();
        }
    }
}
