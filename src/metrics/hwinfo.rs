//! Optional HWiNFO shared-memory source (`Global\HWiNFO_SENS_SM2`) for the CPU package temperature
//! and the GPUs' temperatures (GPU, hot spot, memory, memory junction).
//!
//! Needs "Shared Memory Support" enabled in HWiNFO. The parser is pure so it is unit-tested on a
//! synthetic buffer; element sizes come from the header so newer layouts with appended fields parse.

use super::GpuTempKind;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reading {
    /// The sensor's name as the user may have renamed it, else HWiNFO's.
    pub sensor: String,
    /// HWiNFO's own name of the sensor ("GPU [#0]: NVIDIA GeForce RTX 5070: "), whatever the user
    /// calls it.
    pub sensor_orig: String,
    pub label: String,
    /// HWiNFO's own label of the reading, whatever the user calls it.
    pub label_orig: String,
    pub unit: String,
    /// 1 = temperature.
    pub kind: u32,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotRunning,
    Parse(String),
}

const HEADER: usize = 44;
const KIND_TEMP: u32 = 1;

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn f64_at(b: &[u8], o: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// NUL-terminated string field; UTF-8 when valid, else Latin-1 (HWiNFO writes the degree sign as 0xB0).
fn cstr(b: &[u8], off: usize, len: usize) -> String {
    let Some(f) = b.get(off..off + len) else { return String::new() };
    let f = &f[..f.iter().position(|c| *c == 0).unwrap_or(f.len())];
    match std::str::from_utf8(f) {
        Ok(s) => s.to_string(),
        Err(_) => f.iter().map(|c| *c as char).collect(),
    }
}

/// Parses the `HWiNFO_SENSORS_SHARED_MEM2` image (packed, little-endian). Every element is bounds-checked.
pub fn parse(buf: &[u8]) -> Result<Vec<Reading>, Error> {
    let bad = |m: &str| Error::Parse(m.to_string());
    if buf.len() < HEADER || buf.get(0..4) != Some(b"HWiS") {
        return Err(bad("bad signature"));
    }
    let field = |o| u32_at(buf, o).ok_or_else(|| bad("short header"));
    let (s_off, s_size, s_n) = (field(20)? as usize, field(24)? as usize, field(28)? as usize);
    let (r_off, r_size, r_n) = (field(32)? as usize, field(36)? as usize, field(40)? as usize);
    if s_size < 136 + 128 || r_size < 316 {
        return Err(bad("element size too small"));
    }
    let end = |off: usize, size: usize, n: usize| size.checked_mul(n).and_then(|t| t.checked_add(off));
    if end(s_off, s_size, s_n).is_none_or(|e| e > buf.len()) || end(r_off, r_size, r_n).is_none_or(|e| e > buf.len()) {
        return Err(bad("truncated buffer"));
    }
    // (shown name, HWiNFO's name) of each sensor.
    let sensors: Vec<(String, String)> = (0..s_n)
        .map(|i| {
            let o = s_off + i * s_size;
            let (orig, user) = (cstr(buf, o + 8, 128), cstr(buf, o + 136, 128));
            (if user.is_empty() { orig.clone() } else { user }, orig)
        })
        .collect();
    let mut out = Vec::with_capacity(r_n);
    for i in 0..r_n {
        let o = r_off + i * r_size;
        let (Some(kind), Some(si), Some(value)) = (u32_at(buf, o), u32_at(buf, o + 4), f64_at(buf, o + 284)) else { continue };
        let label_orig = cstr(buf, o + 12, 128);
        let label = cstr(buf, o + 140, 128);
        let label = if label.is_empty() { label_orig.clone() } else { label };
        let (sensor, sensor_orig) = sensors.get(si as usize).cloned().unwrap_or_default();
        out.push(Reading { sensor, sensor_orig, label, label_orig, unit: cstr(buf, o + 268, 16), kind, value });
    }
    Ok(out)
}

/// HWiNFO's labels of GPU temperatures and what each measures. Matched on HWiNFO's own label (a
/// renamed reading still counts), whole and case ignored: "GPU VR VDDC Temperature" and other
/// voltage-regulator sensors are not the GPU's.
const GPU_LABELS: [(&str, GpuTempKind); 5] = [
    ("GPU Temperature", GpuTempKind::Gpu),
    ("GPU Hot Spot Temperature", GpuTempKind::HotSpot),
    ("GPU Hotspot Temperature", GpuTempKind::HotSpot),
    ("GPU Memory Junction Temperature", GpuTempKind::MemoryJunction),
    ("GPU Memory Temperature", GpuTempKind::Memory),
];

/// One GPU temperature from HWiNFO: HWiNFO's sensor name (which names the card), what it
/// measures and °C.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuReading {
    pub sensor: String,
    pub kind: GpuTempKind,
    pub c: f32,
}

/// The GPU temperatures among `readings`: temperatures in °C of a "GPU" sensor whose label is one
/// of [`GPU_LABELS`]; a reading of 0 or above 200 °C is no reading.
pub fn gpu_temps(readings: &[Reading]) -> Vec<GpuReading> {
    readings
        .iter()
        .filter(|r| r.kind == KIND_TEMP && is_celsius(&r.unit) && r.value > 0.0 && r.value <= 200.0)
        .filter(|r| r.sensor_orig.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("gpu")))
        .filter_map(|r| {
            let kind = GPU_LABELS.iter().find(|(l, _)| r.label_orig.trim().eq_ignore_ascii_case(l))?.1;
            Some(GpuReading { sensor: r.sensor_orig.clone(), kind, c: r.value as f32 })
        })
        .collect()
}

fn is_celsius(unit: &str) -> bool {
    let u = unit.trim();
    u.ends_with('C') && u.chars().count() <= 3
}

/// CPU temperature: package-like labels in priority order, then any `CPU*` temperature in °C.
pub fn cpu_temp(readings: &[Reading]) -> Option<(String, f32)> {
    let temps: Vec<(&str, f32)> = readings.iter().filter(|r| r.kind == KIND_TEMP && is_celsius(&r.unit) && r.value.is_finite()).map(|r| (r.label.as_str(), r.value as f32)).collect();
    super::common::pick_cpu_temp(&temps, &super::common::HWINFO_CPU_LABELS).map(|(l, c)| (l.to_string(), c))
}

#[cfg(windows)]
pub fn read() -> Result<Vec<Reading>, Error> {
    use crate::pro::winutil::wide;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Memory::{MapViewOfFile, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, MEMORY_BASIC_INFORMATION};
    use windows::Win32::System::Threading::{OpenMutexW, ReleaseMutex, WaitForSingleObject, SYNCHRONIZATION_ACCESS_RIGHTS};

    let name = wide("Global\\HWiNFO_SENS_SM2");
    let map = unsafe { windows::Win32::System::Memory::OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(name.as_ptr())) }.map_err(|_| Error::NotRunning)?;
    // Best effort: HWiNFO guards the section with a mutex while it rewrites it.
    let mname = wide("Global\\HWiNFO_SM2_MUTEX");
    let mutex: Option<HANDLE> = unsafe { OpenMutexW(SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000), false, PCWSTR(mname.as_ptr())) }.ok();
    let locked = mutex.is_some_and(|m| unsafe { WaitForSingleObject(m, 200) }.0 == 0);
    let view = unsafe { MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0) };
    let result = if view.Value.is_null() {
        Err(Error::Parse("cannot map section".into()))
    } else {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let got = unsafe { VirtualQuery(Some(view.Value), &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        let len = if got == 0 { 0 } else { mbi.RegionSize.min(16 << 20) };
        // SAFETY: the view is at least `len` bytes (region size) and read-only.
        let copy = unsafe { std::slice::from_raw_parts(view.Value as *const u8, len) }.to_vec();
        unsafe {
            let _ = UnmapViewOfFile(view);
        }
        parse(&copy)
    };
    if let Some(m) = mutex {
        unsafe {
            if locked {
                let _ = ReleaseMutex(m);
            }
            let _ = CloseHandle(m);
        }
    }
    unsafe {
        let _ = CloseHandle(map);
    }
    result
}

#[cfg(not(windows))]
pub fn read() -> Result<Vec<Reading>, Error> {
    Err(Error::NotRunning)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S_SIZE: usize = 264 + 8; // larger than the fixed layout on purpose
    const R_SIZE: usize = 316 + 8;

    fn put(b: &mut [u8], o: usize, s: &[u8]) {
        b[o..o + s.len()].copy_from_slice(s);
    }

    /// Each sensor is (HWiNFO's name, the user's name or ""); each reading's label is HWiNFO's,
    /// with "user label|" in front when the user renamed it.
    fn synth_named(readings: &[(u32, u32, &str, &[u8], f64)], sensors: &[(&str, &str)]) -> Vec<u8> {
        let mut b = synth(readings, &sensors.iter().map(|s| s.1).collect::<Vec<_>>());
        let r_off = HEADER + sensors.len() * S_SIZE;
        for (i, (orig, _)) in sensors.iter().enumerate() {
            put(&mut b, HEADER + i * S_SIZE + 8, orig.as_bytes());
        }
        for (i, (_, _, label, _, _)) in readings.iter().enumerate() {
            let o = r_off + i * R_SIZE;
            let (user, orig) = label.split_once('|').unwrap_or(("", label));
            b[o + 140..o + 268].fill(0);
            put(&mut b, o + 140, user.as_bytes());
            put(&mut b, o + 12, orig.as_bytes());
        }
        b
    }

    fn synth(readings: &[(u32, u32, &str, &[u8], f64)], sensors: &[&str]) -> Vec<u8> {
        let s_off = HEADER;
        let r_off = s_off + sensors.len() * S_SIZE;
        let mut b = vec![0u8; r_off + readings.len() * R_SIZE];
        put(&mut b, 0, b"HWiS");
        put(&mut b, 4, &2u32.to_le_bytes());
        put(&mut b, 20, &(s_off as u32).to_le_bytes());
        put(&mut b, 24, &(S_SIZE as u32).to_le_bytes());
        put(&mut b, 28, &(sensors.len() as u32).to_le_bytes());
        put(&mut b, 32, &(r_off as u32).to_le_bytes());
        put(&mut b, 36, &(R_SIZE as u32).to_le_bytes());
        put(&mut b, 40, &(readings.len() as u32).to_le_bytes());
        for (i, s) in sensors.iter().enumerate() {
            put(&mut b, s_off + i * S_SIZE + 136, s.as_bytes());
        }
        for (i, (kind, si, label, unit, v)) in readings.iter().enumerate() {
            let o = r_off + i * R_SIZE;
            put(&mut b, o, &kind.to_le_bytes());
            put(&mut b, o + 4, &si.to_le_bytes());
            put(&mut b, o + 140, label.as_bytes());
            put(&mut b, o + 268, unit);
            put(&mut b, o + 284, &v.to_le_bytes());
        }
        b
    }

    #[test]
    fn hwinfo_parse_synthetic() {
        let b = synth(
            &[(1, 0, "CPU Package", b"\xB0C", 61.5), (1, 0, "CPU (Tctl/Tdie)", "°C".as_bytes(), 63.0), (3, 0, "Core 0 Clock", b"MHz", 4500.0)],
            &["CPU [#0]: AMD Ryzen", "GPU"],
        );
        let r = parse(&b).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].sensor, "CPU [#0]: AMD Ryzen");
        assert_eq!(r[0].unit, "\u{b0}C");
        assert_eq!(r[2].value, 4500.0);
        assert_eq!(cpu_temp(&r), Some(("CPU Package".into(), 61.5)));
        let mut bad = b.clone();
        bad[0] = b'X';
        assert!(matches!(parse(&bad), Err(Error::Parse(_))));
        assert!(matches!(parse(&b[..b.len() - 10]), Err(Error::Parse(_))));
        assert!(matches!(parse(&b[..20]), Err(Error::Parse(_))));
    }

    #[test]
    fn gpu_temperatures_by_hwinfo_label() {
        let c = "\u{b0}C".as_bytes();
        let b = synth_named(
            &[
                (1, 0, "GPU Temperature", c, 48.5),
                (1, 0, "Hot|GPU Hot Spot Temperature", c, 61.0),
                (1, 0, "GPU Memory Junction Temperature", b"\xB0C", 70.0),
                (1, 0, "GPU VR VDDC Temperature", c, 55.0),
                (1, 0, "GPU Memory Temperature", c, 0.0),
                (3, 0, "GPU Temperature", b"MHz", 1800.0),
                (1, 1, "GPU Memory Temperature", c, 64.0),
                (1, 2, "GPU Temperature", c, 40.0),
                (1, 3, "CPU Package", c, 66.0),
            ],
            &[("GPU [#0]: NVIDIA GeForce RTX 5070: ", ""), ("GPU [#1]: AMD Radeon RX 7900 XTX: ", "My card"), ("Drive: Samsung SSD", "GPU-looking rename"), ("CPU [#0]: AMD Ryzen", "")],
        );
        let r = parse(&b).unwrap();
        assert_eq!((r[1].label.as_str(), r[1].label_orig.as_str()), ("Hot", "GPU Hot Spot Temperature"));
        assert_eq!((r[6].sensor.as_str(), r[6].sensor_orig.as_str()), ("My card", "GPU [#1]: AMD Radeon RX 7900 XTX: "));
        let temps = gpu_temps(&r);
        let got: Vec<(&str, GpuTempKind, f32)> = temps.iter().map(|g| (&g.sensor[..9], g.kind, g.c)).collect();
        assert_eq!(
            got,
            [
                ("GPU [#0]:", GpuTempKind::Gpu, 48.5),
                ("GPU [#0]:", GpuTempKind::HotSpot, 61.0),
                ("GPU [#0]:", GpuTempKind::MemoryJunction, 70.0),
                ("GPU [#1]:", GpuTempKind::Memory, 64.0),
            ],
            "renamed labels count by HWiNFO's; VR sensors, 0 °C, other units and non-GPU sensors do not"
        );
        assert_eq!(cpu_temp(&r), Some(("CPU Package".into(), 66.0)));
    }

    #[test]
    fn cpu_temp_priority_and_fallback() {
        let t = |label: &str, unit: &str, kind| Reading { label: label.into(), unit: unit.into(), kind, value: 50.0, ..Default::default() };
        assert_eq!(cpu_temp(&[t("CPU CCD1 (Tdie)", "\u{b0}C", 1)]).map(|c| c.0), Some("CPU CCD1 (Tdie)".into()));
        assert_eq!(cpu_temp(&[t("GPU Temperature", "\u{b0}C", 1)]), None);
        assert_eq!(cpu_temp(&[t("CPU Package", "W", 1)]), None);
        assert_eq!(cpu_temp(&[t("CPU Core 3", "\u{b0}C", 1), t("CPU Package", "\u{b0}C", 1)]).map(|c| c.0), Some("CPU Package".into()));
        assert_eq!(cpu_temp(&[t("CPU Core 3", "\u{b0}C", 1)]).map(|c| c.0), Some("CPU Core 3".into()));
        // Non-ASCII labels (byte 3 inside a multibyte char) must not panic.
        assert_eq!(cpu_temp(&[t("\u{422}\u{435}\u{43c}\u{43f}\u{435}\u{440}\u{430}\u{442}\u{443}\u{440}\u{430} GPU", "\u{b0}C", 1)]), None);
        assert_eq!(cpu_temp(&[t("\u{422}\u{435}\u{43c}", "\u{b0}C", 1), t("CPU Core 3", "\u{b0}C", 1)]).map(|c| c.0), Some("CPU Core 3".into()));
    }

    #[cfg(windows)]
    #[test]
    fn hwinfo_read_does_not_panic() {
        match read() {
            Ok(_) | Err(Error::NotRunning) | Err(Error::Parse(_)) => {}
        }
    }
}
