//! The speed of the installed memory, from the SMBIOS (DMI) table's Memory Device structures
//! (type 17).
//!
//! | | Source |
//! |---|---|
//! | Windows | `GetSystemFirmwareTable('RSMB')` |
//! | Linux | `/sys/firmware/dmi/tables/DMI`, which only root may read on most systems: unreadable means unknown, TMOP never asks for more rights |
//! | macOS | none (Apple silicon has no SMBIOS) |
//!
//! Where the table can't be read, or says nothing, there is no speed and the pages leave the row out.

/// The speed the installed memory runs at, in MT/s (SMBIOS 3.1.1+ defines the field as MT/s: DDR4-3200 is 3200). Read once.
pub fn memory_speed_mhz() -> Option<u32> {
    static SPEED: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *SPEED.get_or_init(|| parse_memory_speed(&read_table()?))
}

/// The row the pages show for [`memory_speed_mhz`].
pub fn format_speed(mhz: u32) -> String {
    format!("{mhz} MT/s")
}

/// The raw structure table, without an entry point or header.
#[cfg(windows)]
fn read_table() -> Option<Vec<u8>> {
    use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None) } as usize;
    let mut buf = vec![0u8; size];
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(&mut buf)) } as usize;
    if got == 0 || got > buf.len() {
        return None;
    }
    buf.truncate(got);
    raw_smbios_table(&buf).map(<[u8]>::to_vec)
}

#[cfg(target_os = "linux")]
fn read_table() -> Option<Vec<u8>> {
    std::fs::read("/sys/firmware/dmi/tables/DMI").ok()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn read_table() -> Option<Vec<u8>> {
    None
}

/// The table inside Windows' `RawSMBIOSData`: four bytes (calling method, SMBIOS major and
/// minor version, DMI revision), the table length as a little-endian u32, then the table.
pub fn raw_smbios_table(raw: &[u8]) -> Option<&[u8]> {
    let len = u32::from_le_bytes(raw.get(4..8)?.try_into().ok()?) as usize;
    raw.get(8..)?.get(..len)
}

/// The speed of the installed modules in an SMBIOS structure table, in MT/s: the slowest one,
/// as modules of different speeds run at the lowest. `None` when no populated Memory Device
/// reports a speed.
pub fn parse_memory_speed(table: &[u8]) -> Option<u32> {
    structures(table).filter(|s| s.first() == Some(&17)).filter_map(device_speed).min()
}

/// The formatted areas of the structures in `table`, up to the end-of-table structure (type
/// 127) or the first one that doesn't fit.
fn structures(table: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = table;
    std::iter::from_fn(move || {
        let len = *rest.get(1)? as usize;
        if len < 4 || rest[0] == 127 {
            return None;
        }
        let formatted = rest.get(..len)?;
        // The strings follow, each NUL-terminated, and one more NUL ends them (two when there
        // are none).
        let strings = &rest[len..];
        let end = strings.windows(2).position(|w| w == [0, 0])?;
        rest = &strings[end + 2..];
        Some(formatted)
    })
}

/// The speed of one Memory Device (type 17) structure, `None` for an empty socket or an
/// unknown speed. The configured speed (SMBIOS 2.7) is what the module runs at; older tables
/// only have the module's own speed.
fn device_speed(s: &[u8]) -> Option<u32> {
    let word = |at: usize| s.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    // Speeds of 65535 and more are in the extended fields (SMBIOS 3.3), bit 31 reserved.
    let speed = |at: usize, extended: usize| match word(at)? {
        0 => None,
        0xFFFF => s.get(extended..extended + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x7FFF_FFFF).filter(|v| *v != 0),
        v => Some(v as u32),
    };
    // Size 0: no module in the socket.
    if word(0x0C)? == 0 {
        return None;
    }
    speed(0x20, 0x58).or_else(|| speed(0x15, 0x54))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structure of `len` formatted bytes with `fields` (offset, bytes) set, and its strings.
    fn structure(kind: u8, len: u8, fields: &[(usize, &[u8])], strings: &[&str]) -> Vec<u8> {
        let mut s = vec![0u8; len as usize];
        s[0] = kind;
        s[1] = len;
        for (at, bytes) in fields {
            s[*at..*at + bytes.len()].copy_from_slice(bytes);
        }
        for text in strings {
            s.extend_from_slice(text.as_bytes());
            s.push(0);
        }
        s.push(0);
        if strings.is_empty() {
            s.push(0);
        }
        s
    }

    /// A Memory Device of SMBIOS 2.8 (40 bytes): size in MB, speed and configured speed.
    fn device(size: u16, speed: u16, configured: u16) -> Vec<u8> {
        structure(17, 0x28, &[(0x0C, &size.to_le_bytes()), (0x15, &speed.to_le_bytes()), (0x20, &configured.to_le_bytes())], &["DIMM_A1", "BANK 0", "Kingston"])
    }

    const END: [u8; 6] = [127, 4, 0xFF, 0xFE, 0, 0];

    fn table(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut t = structure(0, 0x18, &[], &["American Megatrends", "1.20", "03/14/2024"]);
        // A structure without strings ends in two NULs.
        t.extend(structure(16, 0x17, &[(0x0D, &[4, 0])], &[]));
        for p in parts {
            t.extend_from_slice(p);
        }
        t.extend_from_slice(&END);
        t
    }

    #[test]
    fn configured_speed_wins_over_module_speed() {
        // Two DDR4-3600 modules running at 3200, and two empty sockets.
        let t = table(&[device(16384, 3600, 3200), device(0, 0, 0), device(16384, 3600, 3200), device(0, 0, 0)]);
        assert_eq!(parse_memory_speed(&t), Some(3200));
    }

    #[test]
    fn slowest_module_counts() {
        let t = table(&[device(8192, 3200, 2666), device(8192, 2400, 2400)]);
        assert_eq!(parse_memory_speed(&t), Some(2400));
    }

    #[test]
    fn falls_back_to_module_speed() {
        // Configured speed unknown (0).
        assert_eq!(parse_memory_speed(&table(&[device(8192, 2133, 0)])), Some(2133));
        // SMBIOS 2.3 to 2.6: the structure ends before the configured speed.
        let old = structure(17, 0x1B, &[(0x0C, &4096u16.to_le_bytes()), (0x15, &1600u16.to_le_bytes())], &["DIMM 1"]);
        assert_eq!(parse_memory_speed(&table(&[old])), Some(1600));
        // SMBIOS 2.1: no speed at all.
        let older = structure(17, 0x15, &[(0x0C, &512u16.to_le_bytes())], &["DIMM 1"]);
        assert_eq!(parse_memory_speed(&table(&[older])), None);
    }

    #[test]
    fn extended_speeds() {
        let ext = |speed: u16, configured: u16, ext_speed: u32, ext_configured: u32| {
            structure(
                17,
                0x5C,
                &[(0x0C, &0x7FFFu16.to_le_bytes()), (0x15, &speed.to_le_bytes()), (0x20, &configured.to_le_bytes()), (0x54, &ext_speed.to_le_bytes()), (0x58, &ext_configured.to_le_bytes())],
                &["DIMM 1"],
            )
        };
        assert_eq!(parse_memory_speed(&table(&[ext(0xFFFF, 0xFFFF, 80_000, 70_000)])), Some(70_000));
        // Bit 31 is reserved.
        assert_eq!(parse_memory_speed(&table(&[ext(0xFFFF, 0xFFFF, 80_000, 0x8000_0000 | 70_000)])), Some(70_000));
        // An extended field of 0 is unknown: the module's own speed is used instead.
        assert_eq!(parse_memory_speed(&table(&[ext(0xFFFF, 0xFFFF, 80_000, 0)])), Some(80_000));
        assert_eq!(parse_memory_speed(&table(&[ext(5600, 0xFFFF, 0, 0)])), Some(5600));
        // 0xFFFF in a structure too short for the extended fields is unknown.
        assert_eq!(parse_memory_speed(&table(&[device(8192, 0xFFFF, 0xFFFF)])), None);
        // Extended fields are ignored unless the 16-bit field says so.
        assert_eq!(parse_memory_speed(&table(&[ext(4800, 4400, 1, 2)])), Some(4400));
    }

    #[test]
    fn nothing_known_is_none() {
        assert_eq!(parse_memory_speed(&[]), None);
        assert_eq!(parse_memory_speed(&table(&[])), None);
        // Only empty sockets, and a populated one of unknown speed.
        assert_eq!(parse_memory_speed(&table(&[device(0, 3200, 3200), device(8192, 0, 0)])), None);
    }

    #[test]
    fn stops_at_the_end_of_table_and_at_damage() {
        // Structures after type 127 are not read.
        let mut t = table(&[device(8192, 3200, 3200)]);
        t.extend(device(8192, 1333, 1333));
        assert_eq!(parse_memory_speed(&t), Some(3200));
        // A table cut anywhere never panics and never reads past what is complete.
        let whole = table(&[device(8192, 3200, 3000), device(8192, 2400, 2133)]);
        for cut in 0..whole.len() {
            let got = parse_memory_speed(&whole[..cut]);
            assert!(matches!(got, None | Some(3000) | Some(2133)), "cut at {cut}: {got:?}");
        }
        // A length below the 4-byte header ends the walk.
        assert_eq!(parse_memory_speed(&[17, 2, 0, 0, 0, 0]), None);
    }

    #[test]
    fn windows_header_is_stripped() {
        let t = table(&[device(8192, 3200, 3200)]);
        let mut raw = vec![0, 3, 3, 0];
        raw.extend_from_slice(&(t.len() as u32).to_le_bytes());
        raw.extend_from_slice(&t);
        assert_eq!(raw_smbios_table(&raw), Some(&t[..]));
        // Trailing bytes after the stated length are not part of the table.
        raw.push(0xAA);
        assert_eq!(raw_smbios_table(&raw), Some(&t[..]));
        // A length beyond the buffer, or no header.
        raw[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(raw_smbios_table(&raw), None);
        assert_eq!(raw_smbios_table(&[0, 3, 3]), None);
    }

    #[test]
    fn speed_text() {
        assert_eq!(format_speed(3200), "3200 MT/s");
    }

    /// What this machine reports; a speed, when there is one, is plausible.
    #[test]
    fn live_memory_speed() {
        if let Some(mhz) = memory_speed_mhz() {
            assert!((100..200_000).contains(&mhz), "{mhz}");
        }
    }
}
