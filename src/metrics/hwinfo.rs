//! Optional HWiNFO shared-memory source (`Global\HWiNFO_SENS_SM2`) for the CPU package temperature.
//!
//! Needs "Shared Memory Support" enabled in HWiNFO. The parser is pure so it is unit-tested on a
//! synthetic buffer; element sizes come from the header so newer layouts with appended fields parse.

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub sensor: String,
    pub label: String,
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
    let sensors: Vec<String> = (0..s_n)
        .map(|i| {
            let o = s_off + i * s_size;
            let user = cstr(buf, o + 136, 128);
            if user.is_empty() { cstr(buf, o + 8, 128) } else { user }
        })
        .collect();
    let mut out = Vec::with_capacity(r_n);
    for i in 0..r_n {
        let o = r_off + i * r_size;
        let (Some(kind), Some(si), Some(value)) = (u32_at(buf, o), u32_at(buf, o + 4), f64_at(buf, o + 284)) else { continue };
        let label = cstr(buf, o + 140, 128);
        let label = if label.is_empty() { cstr(buf, o + 12, 128) } else { label };
        out.push(Reading { sensor: sensors.get(si as usize).cloned().unwrap_or_default(), label, unit: cstr(buf, o + 268, 16), kind, value });
    }
    Ok(out)
}

fn is_celsius(unit: &str) -> bool {
    let u = unit.trim();
    u.ends_with('C') && u.chars().count() <= 3
}

/// CPU temperature: package-like labels in priority order, then any `CPU*` temperature in °C.
pub fn cpu_temp(readings: &[Reading]) -> Option<(String, f32)> {
    let temps = || readings.iter().filter(|r| r.kind == KIND_TEMP && is_celsius(&r.unit) && r.value.is_finite());
    const PRIORITY: [&str; 5] = ["CPU Package", "CPU (Tctl/Tdie)", "CPU Tctl/Tdie", "CPU Die (average)", "CPU CCD1 (Tdie)"];
    let pick = PRIORITY
        .iter()
        .find_map(|l| temps().find(|r| r.label.eq_ignore_ascii_case(l)))
        .or_else(|| temps().find(|r| r.label.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("cpu"))))?;
    Some((pick.label.clone(), pick.value as f32))
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
    fn cpu_temp_priority_and_fallback() {
        let t = |label: &str, unit: &str, kind| Reading { sensor: String::new(), label: label.into(), unit: unit.into(), kind, value: 50.0 };
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
