//! NVIDIA GPUs through NVML, loaded at run time: `dlopen("libnvidia-ml.so.1")` on Linux (installed
//! with the driver, also under WSL) and `nvml.dll` from System32 on Windows, so the binary has no
//! link-time NVIDIA dependency and runs without it. Plain library loading keeps this to a handful
//! of symbols with no new crate.

use super::procfs::GpuDev;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};

type Device = *mut c_void;
const SUCCESS: c_int = 0;
/// `NVML_TEMPERATURE_GPU`.
const TEMPERATURE_GPU: c_uint = 0;
/// `NVML_CLOCK_GRAPHICS`.
const CLOCK_GRAPHICS: c_uint = 0;
/// `NVML_DEVICE_NAME_V2_BUFFER_SIZE`.
const NAME_LEN: usize = 96;

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
type NameFn = unsafe extern "C" fn(Device, *mut c_char, c_uint) -> c_int;
/// `nvmlDeviceGetTemperature` (°C) / `nvmlDeviceGetClockInfo` (MHz): (device, which, value).
type SensorFn = unsafe extern "C" fn(Device, c_uint, *mut c_uint) -> c_int;
/// `nvmlDeviceGetPowerUsage` (mW).
type PowerFn = unsafe extern "C" fn(Device, *mut c_uint) -> c_int;

/// A loaded and initialised NVML with the devices it found. The library stays loaded for the
/// life of the process (never `dlclose`d: NVML may keep threads of its own).
pub struct Nvml {
    devices: Vec<(Device, String)>,
    util: UtilFn,
    mem: MemFn,
    encoder: Option<CodecFn>,
    decoder: Option<CodecFn>,
    temperature: Option<SensorFn>,
    clock: Option<SensorFn>,
    power: Option<PowerFn>,
    shutdown: InitFn,
}

#[cfg(unix)]
fn open_library() -> Option<*mut c_void> {
    // SAFETY: dlopen with a valid C string.
    let lib = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    (!lib.is_null()).then_some(lib)
}

#[cfg(windows)]
fn open_library() -> Option<*mut c_void> {
    use windows::core::w;
    use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
    // System32 only: a nvml.dll beside TMOP or in the current directory must not be loaded.
    // SAFETY: a valid wide string and no reserved handle.
    unsafe { LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok().map(|m| m.0)
}

/// The symbol `name` of `lib`, null when it is missing.
#[cfg(unix)]
unsafe fn symbol(lib: *mut c_void, name: &CStr) -> *mut c_void {
    libc::dlsym(lib, name.as_ptr())
}

#[cfg(windows)]
unsafe fn symbol(lib: *mut c_void, name: &CStr) -> *mut c_void {
    use windows::core::PCSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::GetProcAddress;
    GetProcAddress(HMODULE(lib), PCSTR(name.as_ptr().cast())).map_or(std::ptr::null_mut(), |f| f as *mut c_void)
}

/// The symbol `name` (NUL-terminated) of `lib` as a function pointer of type `F`.
///
/// # Safety
/// `F` must be the symbol's C signature.
unsafe fn sym<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    let p = symbol(lib, name);
    // SAFETY: a non-null symbol of a function type is a function pointer of the same size.
    (!p.is_null()).then(|| std::mem::transmute_copy::<*mut c_void, F>(&p))
}

impl Nvml {
    /// None when the library is missing (no NVIDIA driver), fails to initialise or finds no GPU.
    pub fn load() -> Option<Nvml> {
        let lib = open_library()?;
        // SAFETY: the signatures below are NVML's (nvml.h, API v2+).
        unsafe {
            let init: InitFn = sym(lib, c"nvmlInit_v2")?;
            let shutdown: InitFn = sym(lib, c"nvmlShutdown")?;
            let count: CountFn = sym(lib, c"nvmlDeviceGetCount_v2")?;
            let handle: HandleFn = sym(lib, c"nvmlDeviceGetHandleByIndex_v2")?;
            let util: UtilFn = sym(lib, c"nvmlDeviceGetUtilizationRates")?;
            let mem: MemFn = sym(lib, c"nvmlDeviceGetMemoryInfo")?;
            let name: Option<NameFn> = sym(lib, c"nvmlDeviceGetName");
            if init() != SUCCESS {
                return None;
            }
            let mut n: c_uint = 0;
            let devices: Vec<(Device, String)> = if count(&mut n) == SUCCESS {
                (0..n.min(64))
                    .filter_map(|i| {
                        let mut d: Device = std::ptr::null_mut();
                        (handle(i, &mut d) == SUCCESS && !d.is_null()).then(|| (d, device_name(name, d)))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            if devices.is_empty() {
                shutdown();
                return None;
            }
            Some(Nvml {
                devices,
                util,
                mem,
                encoder: sym(lib, c"nvmlDeviceGetEncoderUtilization"),
                decoder: sym(lib, c"nvmlDeviceGetDecoderUtilization"),
                temperature: sym(lib, c"nvmlDeviceGetTemperature"),
                clock: sym(lib, c"nvmlDeviceGetClockInfo"),
                power: sym(lib, c"nvmlDeviceGetPowerUsage"),
                shutdown,
            })
        }
    }

    /// One reading per GPU: "Graphics" (time a kernel ran), "Encode" and "Decode" where the GPU
    /// reports them, memory and the sensors it has. A GPU whose utilisation query fails (e.g.
    /// lost) has no engines but may still report memory.
    pub fn read(&self) -> Vec<GpuDev> {
        self.devices
            .iter()
            .enumerate()
            .map(|(i, (d, name))| {
                let d = *d;
                let mut dev = GpuDev { id: format!("nvidia{i}"), name: name.clone(), ..Default::default() };
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
                        dev.vram_total = Some(m.total);
                    }
                    let sensor = |f: Option<SensorFn>, which: c_uint| {
                        let mut v = 0;
                        f.is_some_and(|f| f(d, which, &mut v) == SUCCESS).then_some(v)
                    };
                    dev.temperature_c = sensor(self.temperature, TEMPERATURE_GPU).map(|t| t as f32);
                    dev.clock_mhz = sensor(self.clock, CLOCK_GRAPHICS);
                    let mut mw = 0;
                    if self.power.is_some_and(|f| f(d, &mut mw) == SUCCESS) {
                        dev.power_w = Some(mw as f32 / 1000.0);
                    }
                }
                dev
            })
            .collect()
    }
}

/// The device's marketing name; "NVIDIA GPU" when NVML gives none.
///
/// # Safety
/// `d` must be a handle NVML returned.
unsafe fn device_name(f: Option<NameFn>, d: Device) -> String {
    let mut buf = [0 as c_char; NAME_LEN];
    if f.is_some_and(|f| f(d, buf.as_mut_ptr(), NAME_LEN as c_uint) == SUCCESS) {
        // SAFETY: NVML NUL-terminates what it writes on success.
        let s = CStr::from_ptr(buf.as_ptr()).to_string_lossy().trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    "NVIDIA GPU".into()
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: balances the successful nvmlInit_v2 in `load`.
        unsafe {
            (self.shutdown)();
        }
    }
}
