//! Optional PawnIO source for the CPU temperature on Windows: AMD Ryzen Tctl/Tdie (SMN register
//! `THM_TCON_CUR_TMP`) and the Intel package temperature (`IA32_PACKAGE_THERM_STATUS`).
//!
//! PawnIO (<https://pawnio.eu>) is an open-source, signed driver that runs small signed modules;
//! LibreHardwareMonitor and Fan Control use it in place of WinRing0. TMOP embeds the two
//! official modules it needs (`assets/pawnio`, LGPL-2.1-or-later) and talks to the driver
//! through its device I/O control interface only: no PawnIOLib.dll.
//!
//! Strictly opt-in ([`set_enabled`], Settings → General): the user installs PawnIO themselves,
//! TMOP must run as administrator (the device admits Administrators and SYSTEM only), and some
//! anti-cheat software refuses to run beside it. While the switch is off TMOP never opens the
//! device. The register decoding is pure and tested on every host.

use std::sync::atomic::{AtomicBool, Ordering};

/// The Settings switch, read by the sampler thread on every collect.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `AMDFamily17.bin` of PawnIO.Modules 0.2.11 (signed; see `assets/pawnio/README.md`). Only the
/// Windows program carries the modules.
#[cfg(any(windows, test))]
pub const AMD_FAMILY_17: &[u8] = include_bytes!("../../assets/pawnio/AMDFamily17.bin");
/// `IntelMSR.bin` of PawnIO.Modules 0.2.11.
#[cfg(any(windows, test))]
pub const INTEL_MSR: &[u8] = include_bytes!("../../assets/pawnio/IntelMSR.bin");

/// SMN address of `THM_TCON_CUR_TMP` (Zen 1 to Zen 5).
pub const SMN_THM_TCON_CUR_TMP: u32 = 0x0005_9800;
pub const MSR_IA32_TEMPERATURE_TARGET: u32 = 0x1A2;
pub const MSR_IA32_PACKAGE_THERM_STATUS: u32 = 0x1B1;

/// Which module and register this processor needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cpu {
    /// AMD family 17h to 1Ah (Zen 1 to Zen 5): `AMDFamily17` module.
    AmdZen,
    /// Intel with the package thermal sensor (CPUID 6 EAX bit 6, Sandy Bridge and later).
    IntelPackage,
}

impl Cpu {
    #[cfg(any(windows, test))]
    pub fn module(self) -> &'static [u8] {
        match self {
            Cpu::AmdZen => AMD_FAMILY_17,
            Cpu::IntelPackage => INTEL_MSR,
        }
    }
}

/// The supported CPU from the CPUID vendor string, leaf 1 EAX and leaf 6 EAX (0 when leaf 6 is
/// missing).
pub fn classify(vendor: &str, leaf1_eax: u32, leaf6_eax: u32) -> Option<Cpu> {
    let base = (leaf1_eax >> 8) & 0xF;
    let family = if base == 0xF { base + ((leaf1_eax >> 20) & 0xFF) } else { base };
    match vendor {
        "AuthenticAMD" if (0x17..=0x1A).contains(&family) => Some(Cpu::AmdZen),
        "GenuineIntel" if leaf6_eax & (1 << 6) != 0 => Some(Cpu::IntelPackage),
        _ => None,
    }
}

/// This machine's CPU (x86-64 only: the modules refuse every other architecture).
pub fn detect() -> Option<Cpu> {
    #[cfg(target_arch = "x86_64")]
    {
        // `__cpuid` is safe since Rust 1.87; older toolchains still want the block.
        #[allow(unused_unsafe)]
        // SAFETY: CPUID is available on every x86-64 processor.
        let cpuid = |leaf: u32| unsafe { std::arch::x86_64::__cpuid(leaf) };
        let l0 = cpuid(0);
        let vendor: Vec<u8> = [l0.ebx, l0.edx, l0.ecx].iter().flat_map(|r| r.to_le_bytes()).collect();
        let leaf6 = if l0.eax >= 6 { cpuid(6).eax } else { 0 };
        classify(&String::from_utf8_lossy(&vendor), cpuid(1).eax, leaf6)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

/// Tctl in °C from `THM_TCON_CUR_TMP`: CUR_TEMP [31:21] in 0.125 °C steps, 49 °C lower when
/// RANGE_SEL [19] is set or TJ_SEL [17:16] is 3 (Linux k10temp).
pub fn amd_tctl(reg: u32) -> f32 {
    let t = (reg >> 21) as f32 * 0.125;
    if reg & 0x8_0000 != 0 || reg & 0x3_0000 == 0x3_0000 {
        t - 49.0
    } else {
        t
    }
}

/// How far Tctl reads above Tdie on the early Zen parts that offset it (k10temp's table).
pub fn amd_tctl_offset(brand: &str) -> f32 {
    if ["1600X", "1700X", "1800X"].iter().any(|m| brand.contains(m)) {
        20.0
    } else if brand.contains("Threadripper 19") || brand.contains("Threadripper 29") {
        27.0
    } else if brand.contains("2700X") {
        10.0
    } else {
        0.0
    }
}

/// The AMD reading as (label, °C): Tdie where Tctl is offset, else the shared Tctl/Tdie.
pub fn amd_reading(reg: u32, brand: &str) -> (String, f32) {
    let (tctl, offset) = (amd_tctl(reg), amd_tctl_offset(brand));
    if offset > 0.0 {
        ("CPU (Tdie)".into(), tctl - offset)
    } else {
        ("CPU (Tctl/Tdie)".into(), tctl)
    }
}

/// TjMax in °C from `IA32_TEMPERATURE_TARGET` [23:16]; 100 °C when the field is empty.
pub fn intel_tjmax(target: u64) -> f32 {
    match (target >> 16) & 0xFF {
        0 => 100.0,
        t => t as f32,
    }
}

/// The package temperature: TjMax minus the digital readout [22:16] of `IA32_PACKAGE_THERM_STATUS`.
pub fn intel_package(status: u64, tjmax: f32) -> f32 {
    tjmax - ((status >> 16) & 0x7F) as f32
}

/// A reading worth showing (the registers read 0 or all ones when something is off).
pub fn plausible(c: f32) -> bool {
    c.is_finite() && (-20.0..=150.0).contains(&c)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No PawnIO device: the driver is not installed (or not running).
    NotInstalled,
    /// The device exists but this process may not open it.
    NeedsAdmin,
    /// No AMD Ryzen or Intel CPU with a package sensor, or the module refused the CPU.
    Unsupported,
    Failed(String),
}

impl Error {
    /// The status shown after "PawnIO:" on the Thermals card.
    pub fn status(&self) -> String {
        match self {
            Error::NotInstalled => "not installed (pawnio.eu)".into(),
            Error::NeedsAdmin => "run TMOP as administrator".into(),
            Error::Unsupported => "CPU not supported".into(),
            Error::Failed(e) => e.clone(),
        }
    }
}

#[cfg(windows)]
pub use sys::{Session, Source};

#[cfg(windows)]
mod sys {
    use super::*;
    use crate::pro::winutil::wide;
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::Threading::{CreateMutexW, OpenMutexW, ReleaseMutex, WaitForSingleObject, SYNCHRONIZATION_ACCESS_RIGHTS};
    use windows::Win32::System::IO::DeviceIoControl;

    /// `\Device\PawnIO` through the Win32 namespace (the `\\.\PawnIO` link is deprecated).
    const DEVICE: &str = r"\\?\GLOBALROOT\Device\PawnIO";
    const DEVICE_TYPE: u32 = 41394;
    /// CTL_CODE(DEVICE_TYPE, function, METHOD_BUFFERED, FILE_ANY_ACCESS).
    const fn ctl(function: u32) -> u32 {
        (DEVICE_TYPE << 16) | (function << 2)
    }
    const IOCTL_PIO_LOAD_BINARY: u32 = ctl(0x821);
    const IOCTL_PIO_EXECUTE_FN: u32 = ctl(0x841);
    /// The function name field in front of the input of an execute call.
    const FN_NAME_LENGTH: usize = 32;
    /// The mutex hardware monitors share around PCI config space (the SMN index/data pair).
    const PCI_MUTEX: &str = "Global\\Access_PCI";

    fn map_err(e: &windows::core::Error) -> Error {
        let code = e.code();
        if code == ERROR_FILE_NOT_FOUND.to_hresult() || code == ERROR_PATH_NOT_FOUND.to_hresult() {
            Error::NotInstalled
        } else if code == ERROR_ACCESS_DENIED.to_hresult() {
            Error::NeedsAdmin
        } else if code == ERROR_NOT_SUPPORTED.to_hresult() {
            Error::Unsupported
        } else {
            Error::Failed(format!("error 0x{:08X}", code.0 as u32))
        }
    }

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// An open PawnIO device with this CPU's module loaded; closing it unloads the module.
    pub struct Session {
        device: Handle,
        cpu: Cpu,
        /// `Global\Access_PCI`, held only around SMN reads (AMD).
        pci_mutex: Option<Handle>,
        tjmax: f32,
        brand: String,
    }

    impl Session {
        pub fn open() -> Result<Self, Error> {
            let cpu = detect().ok_or(Error::Unsupported)?;
            let path = wide(DEVICE);
            let device = unsafe {
                CreateFileW(PCWSTR(path.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)
            }
            .map_err(|e| map_err(&e))?;
            let device = Handle(device);
            let blob = cpu.module();
            unsafe { DeviceIoControl(device.0, IOCTL_PIO_LOAD_BINARY, Some(blob.as_ptr().cast()), blob.len() as u32, None, 0, None, None) }.map_err(|e| match map_err(&e) {
                Error::Failed(code) => Error::Failed(format!("module not loaded ({code})")),
                other => other,
            })?;
            let mut s = Session { device, cpu, pci_mutex: None, tjmax: 100.0, brand: super::super::common::cpu_brand() };
            match cpu {
                Cpu::AmdZen => {
                    let name = wide(PCI_MUTEX);
                    // Create it if no other monitor has; open it if this account may not create
                    // global objects. Without it reads go unguarded, as with a lone monitor.
                    let m = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
                        .or_else(|_| unsafe { OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000), false, PCWSTR(name.as_ptr())) });
                    s.pci_mutex = m.ok().map(Handle);
                }
                Cpu::IntelPackage => s.tjmax = intel_tjmax(s.execute1("ioctl_read_msr", MSR_IA32_TEMPERATURE_TARGET as u64)?),
            }
            Ok(s)
        }

        /// One module function with one input and one output value.
        fn execute1(&self, name: &str, input: u64) -> Result<u64, Error> {
            let mut buf = [0u8; FN_NAME_LENGTH + 8];
            let n = name.len().min(FN_NAME_LENGTH - 1);
            buf[..n].copy_from_slice(&name.as_bytes()[..n]);
            buf[FN_NAME_LENGTH..].copy_from_slice(&input.to_le_bytes());
            let mut out = 0u64;
            let mut got = 0u32;
            unsafe {
                DeviceIoControl(
                    self.device.0,
                    IOCTL_PIO_EXECUTE_FN,
                    Some(buf.as_ptr().cast()),
                    buf.len() as u32,
                    Some((&mut out as *mut u64).cast()),
                    8,
                    Some(&mut got),
                    None,
                )
            }
            .map_err(|e| match map_err(&e) {
                Error::Failed(code) => Error::Failed(format!("{name} failed ({code})")),
                other => other,
            })?;
            if got < 8 {
                return Err(Error::Failed(format!("{name} returned no value")));
            }
            Ok(out)
        }

        /// (label, °C); `Ok(None)` when another monitor held the PCI mutex for too long.
        pub fn read(&self) -> Result<Option<(String, f32)>, Error> {
            let reading = match self.cpu {
                Cpu::AmdZen => {
                    let locked = match &self.pci_mutex {
                        Some(m) => match unsafe { WaitForSingleObject(m.0, 10) }.0 {
                            0 | 0x80 => true, // WAIT_OBJECT_0, WAIT_ABANDONED: ours either way
                            _ => return Ok(None),
                        },
                        None => false,
                    };
                    let reg = self.execute1("ioctl_read_smn", SMN_THM_TCON_CUR_TMP as u64);
                    if locked {
                        if let Some(m) = &self.pci_mutex {
                            unsafe {
                                let _ = ReleaseMutex(m.0);
                            }
                        }
                    }
                    amd_reading(reg? as u32, &self.brand)
                }
                Cpu::IntelPackage => ("CPU Package".into(), intel_package(self.execute1("ioctl_read_msr", MSR_IA32_PACKAGE_THERM_STATUS as u64)?, self.tjmax)),
            };
            if plausible(reading.1) {
                Ok(Some(reading))
            } else {
                Err(Error::Failed(format!("implausible reading ({:.1} \u{b0}C)", reading.1)))
            }
        }
    }

    /// The collector's PawnIO state: opens the session when switched on, retries a failed open
    /// every 30 s and closes the device when switched off.
    pub struct Source {
        session: Option<Session>,
        last_try: Option<Instant>,
        cpu: Option<(String, f32)>,
        status: String,
    }

    impl Default for Source {
        fn default() -> Self {
            Source { session: None, last_try: None, cpu: None, status: "off".into() }
        }
    }

    impl Source {
        /// The CPU temperature and the status for the sources line; `None` status while off.
        pub fn sample(&mut self, on: bool) -> (Option<(String, f32)>, Option<String>) {
            if !on {
                *self = Source::default();
                return (None, None);
            }
            if self.session.is_none() && self.last_try.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
                self.last_try = Some(Instant::now());
                match Session::open() {
                    Ok(s) => self.session = Some(s),
                    Err(e) => self.status = e.status(),
                }
            }
            if let Some(s) = &self.session {
                match s.read() {
                    Ok(Some(r)) => {
                        self.status = r.0.clone();
                        self.cpu = Some(r);
                    }
                    Ok(None) => {} // PCI bus busy: keep the last reading
                    Err(e) => {
                        self.status = e.status();
                        self.cpu = None;
                        self.session = None;
                    }
                }
            }
            (self.cpu.clone(), Some(self.status.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_are_the_released_ones() {
        use sha2::{Digest, Sha256};
        let hex = |b: &[u8]| Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(AMD_FAMILY_17), "dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91");
        assert_eq!(hex(INTEL_MSR), "d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f");
    }

    #[test]
    fn classify_by_vendor_and_family() {
        // Family 17h: base F + extended 08h. Ryzen 7 5800X: family 19h (eax 0x00A20F12).
        assert_eq!(classify("AuthenticAMD", 0x0080_0F11, 0), Some(Cpu::AmdZen));
        assert_eq!(classify("AuthenticAMD", 0x00A2_0F12, 0), Some(Cpu::AmdZen));
        assert_eq!(classify("AuthenticAMD", 0x00B4_0F40, 0), Some(Cpu::AmdZen)); // 1Ah, Zen 5
        assert_eq!(classify("AuthenticAMD", 0x0060_0F12, 0), None); // 15h, Bulldozer
        assert_eq!(classify("AuthenticAMD", 0x00C0_0F00, 0), None); // 1Bh, not covered by the module
        assert_eq!(classify("GenuineIntel", 0x0009_06EA, 0x77), Some(Cpu::IntelPackage));
        assert_eq!(classify("GenuineIntel", 0x0001_067A, 0x07), None); // no package sensor
        assert_eq!(classify("HygonGenuine", 0x00900F01, 0x77), None);
    }

    #[test]
    fn amd_temperature_decoding() {
        let reg = |deg_eighths: u32, flags: u32| (deg_eighths << 21) | flags;
        assert_eq!(amd_tctl(reg(8 * 55 + 4, 0)), 55.5);
        assert_eq!(amd_tctl(reg(8 * 104, 0x8_0000)), 55.0); // RANGE_SEL
        assert_eq!(amd_tctl(reg(8 * 104, 0x3_0000)), 55.0); // TJ_SEL == 3
        assert_eq!(amd_tctl(reg(8 * 55, 0x1_0000)), 55.0); // TJ_SEL == 1: no shift
        assert_eq!(amd_reading(reg(8 * 60, 0), "AMD Ryzen 7 5800X 8-Core Processor"), ("CPU (Tctl/Tdie)".into(), 60.0));
        assert_eq!(amd_reading(reg(8 * 60, 0), "AMD Ryzen 7 1800X Eight-Core Processor"), ("CPU (Tdie)".into(), 40.0));
        assert_eq!(amd_tctl_offset("AMD Ryzen 7 2700X Eight-Core Processor"), 10.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen Threadripper 1950X 16-Core Processor"), 27.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen Threadripper 3970X 32-Core Processor"), 0.0);
    }

    #[test]
    fn intel_temperature_decoding() {
        let tjmax = intel_tjmax(100 << 16 | 0x0A00_0000); // TCC offset bits are ignored
        assert_eq!(tjmax, 100.0);
        assert_eq!(intel_tjmax(0), 100.0);
        assert_eq!(intel_tjmax(105 << 16), 105.0);
        assert_eq!(intel_package(0x8800_0000 | (38 << 16), 100.0), 62.0);
    }

    #[test]
    fn plausible_range() {
        assert!(plausible(45.0) && plausible(-5.0));
        assert!(!plausible(255.875) && !plausible(f32::NAN) && !plausible(-49.0));
    }
}
