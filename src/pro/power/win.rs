//! Windows battery health from the battery driver: every battery device interface
//! (`GUID_DEVCLASS_BATTERY` through SetupAPI), opened and asked for its tag
//! (`IOCTL_BATTERY_QUERY_TAG`) and its `BatteryInformation` (`IOCTL_BATTERY_QUERY_INFORMATION`).
//! Runs inside [`super::read`] on the loader thread; the result is kept for a few minutes because
//! capacities and cycles change slowly.

use super::parse::{cell_health, Cell, CellHealth};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    GUID_DEVCLASS_BATTERY, HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::Power::{
    BatteryInformation, BATTERY_CAPACITY_RELATIVE, BATTERY_INFORMATION, BATTERY_QUERY_INFORMATION, BATTERY_TAG_INVALID, IOCTL_BATTERY_QUERY_INFORMATION, IOCTL_BATTERY_QUERY_TAG,
};
use windows::Win32::System::IO::DeviceIoControl;

/// How long a reading is reused.
const KEEP: Duration = Duration::from_secs(300);
/// More battery interfaces than any machine has; ends the enumeration on a broken driver.
const MAX_BATTERIES: u32 = 16;

static CACHE: Mutex<Option<(Instant, CellHealth)>> = Mutex::new(None);

/// The health of all batteries, read at most every [`KEEP`].
pub fn health() -> CellHealth {
    if let Ok(g) = CACHE.lock() {
        if let Some((at, h)) = *g {
            if at.elapsed() < KEEP {
                return h;
            }
        }
    }
    let h = cell_health(&cells());
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), h));
    }
    h
}

/// Destroys the device information set on every path.
struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW and is destroyed once, here.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// Closes the battery handle on every path.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once, here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// `BATTERY_INFORMATION` of every battery that answers; batteries that do not are left out.
fn cells() -> Vec<Cell> {
    let flags = DIGCF_PRESENT | DIGCF_DEVICEINTERFACE;
    // SAFETY: a class GUID that outlives the call and no enumerator or window.
    let Ok(set) = (unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_BATTERY), PCWSTR::null(), None, flags) }) else { return Vec::new() };
    let set = DevInfo(set);
    let mut out = Vec::new();
    for index in 0..MAX_BATTERIES {
        let mut iface = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        // SAFETY: `iface` is a writable SP_DEVICE_INTERFACE_DATA with its cbSize set.
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, None, &GUID_DEVCLASS_BATTERY, index, &mut iface) }.is_err() {
            break; // ERROR_NO_MORE_ITEMS (or a failure: nothing more to learn either way)
        }
        if let Some(cell) = interface_path(&set, &iface).and_then(|path| query(&path)) {
            out.push(cell);
        }
    }
    out
}

/// The device path of one interface, NUL-terminated.
fn interface_path(set: &DevInfo, iface: &SP_DEVICE_INTERFACE_DATA) -> Option<Vec<u16>> {
    let mut needed = 0u32;
    // SAFETY: a size query (no buffer); fails with ERROR_INSUFFICIENT_BUFFER and sets `needed`.
    let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(set.0, iface, None, 0, Some(&mut needed), None) };
    let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    let needed = needed as usize;
    if needed <= offset || needed > 64 * 1024 {
        return None;
    }
    // u64 storage keeps the buffer aligned for the struct's u32 header.
    let mut buf = vec![0u64; needed.div_ceil(8)];
    let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // SAFETY: `buf` holds at least `needed` bytes, aligned for the struct; cbSize is the size
    // of the fixed part, as the API requires, and the call writes at most `needed` bytes.
    let ok = unsafe {
        (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(set.0, iface, Some(detail), needed as u32, None, None)
    };
    ok.ok()?;
    // SAFETY: the path's UTF-16 units lie inside `buf`, from `offset` up to `needed` bytes.
    let units = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>().add(offset).cast::<u16>(), (needed - offset) / 2) };
    let len = units.iter().position(|u| *u == 0)?;
    let mut path = units[..len].to_vec();
    path.push(0);
    Some(path)
}

/// One battery's information, `None` when it cannot be opened or does not answer.
fn query(path: &[u16]) -> Option<Cell> {
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(PCWSTR(path.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)
    }
    .ok()?;
    let h = Handle(h);
    let wait: u32 = 0; // ms: answer at once, do not wait for a battery to be inserted
    let mut tag: u32 = BATTERY_TAG_INVALID;
    let mut got = 0u32;
    // SAFETY: the input is a u32 and the output a writable u32, with their sizes passed.
    unsafe {
        DeviceIoControl(h.0, IOCTL_BATTERY_QUERY_TAG, Some((&raw const wait).cast()), 4, Some((&raw mut tag).cast()), 4, Some(&mut got), None)
    }
    .ok()?;
    if tag == BATTERY_TAG_INVALID {
        return None;
    }
    let q = BATTERY_QUERY_INFORMATION { BatteryTag: tag, InformationLevel: BatteryInformation, AtRate: 0 };
    let mut info = BATTERY_INFORMATION::default();
    // SAFETY: the input is a BATTERY_QUERY_INFORMATION and the output a writable
    // BATTERY_INFORMATION, with their sizes passed.
    unsafe {
        DeviceIoControl(
            h.0,
            IOCTL_BATTERY_QUERY_INFORMATION,
            Some((&raw const q).cast()),
            std::mem::size_of::<BATTERY_QUERY_INFORMATION>() as u32,
            Some((&raw mut info).cast()),
            std::mem::size_of::<BATTERY_INFORMATION>() as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    if (got as usize) < std::mem::size_of::<BATTERY_INFORMATION>() {
        return None;
    }
    Some(Cell { relative: info.Capabilities & BATTERY_CAPACITY_RELATIVE != 0, designed: info.DesignedCapacity, full_charged: info.FullChargedCapacity, cycles: info.CycleCount })
}

#[cfg(test)]
mod tests {
    /// Runs the real enumeration: on a machine without a battery it finds none, on a laptop it
    /// must not fail either way. Only checks that nothing panics and the result is consistent.
    #[test]
    fn enumerates_without_panicking() {
        let cells = super::cells();
        let h = super::cell_health(&cells);
        if cells.is_empty() {
            assert_eq!(h, Default::default());
        }
    }
}
