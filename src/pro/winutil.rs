//! Small Win32 helpers shared by the inventory back ends (registry access, strings).

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_BINARY, REG_DWORD,
    REG_EXPAND_SZ, REG_QWORD, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Registry view to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Default,
    Wow64_64,
    Wow64_32,
}

/// An open registry key, closed on drop.
pub struct Key(HKEY);

impl Key {
    pub fn open(root: HKEY, path: &str, view: View) -> Option<Key> {
        let w = wide(path);
        let mut sam: REG_SAM_FLAGS = KEY_READ;
        match view {
            View::Default => {}
            View::Wow64_64 => sam |= KEY_WOW64_64KEY,
            View::Wow64_32 => sam |= KEY_WOW64_32KEY,
        }
        let mut h = HKEY::default();
        // SAFETY: valid NUL-terminated path; `h` receives the handle.
        let st = unsafe { RegOpenKeyExW(root, PCWSTR(w.as_ptr()), 0, sam, &mut h) };
        (st == ERROR_SUCCESS).then_some(Key(h))
    }

    pub fn subkey(&self, name: &str) -> Option<Key> {
        let w = wide(name);
        let mut h = HKEY::default();
        // SAFETY: as above, relative to an open key.
        let st = unsafe { RegOpenKeyExW(self.0, PCWSTR(w.as_ptr()), 0, KEY_READ, &mut h) };
        (st == ERROR_SUCCESS).then_some(Key(h))
    }

    /// Names of all subkeys.
    pub fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            // SAFETY: buffer and length describe valid writable memory.
            let st = unsafe { RegEnumKeyExW(self.0, i, windows::core::PWSTR(buf.as_mut_ptr()), &mut len, None, windows::core::PWSTR::null(), None, None) };
            if st != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            i += 1;
        }
        out
    }

    /// All values as (name, decoded value). Binary values are returned as raw bytes.
    pub fn values(&self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let mut i = 0u32;
        loop {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let mut ty = 0u32;
            let mut data = vec![0u8; 4096];
            let mut data_len = data.len() as u32;
            // SAFETY: all buffers are valid for the lengths passed.
            let st = unsafe {
                RegEnumValueW(self.0, i, windows::core::PWSTR(name.as_mut_ptr()), &mut name_len, None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut data_len))
            };
            if st != ERROR_SUCCESS {
                break;
            }
            data.truncate(data_len as usize);
            out.push((String::from_utf16_lossy(&name[..name_len as usize]), decode(REG_VALUE_TYPE(ty), data)));
            i += 1;
        }
        out
    }

    pub fn value(&self, name: &str) -> Option<Value> {
        let w = wide(name);
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        // SAFETY: first call only queries the size.
        let st = unsafe { RegQueryValueExW(self.0, PCWSTR(w.as_ptr()), None, Some(&mut ty), None, Some(&mut len)) };
        if st != ERROR_SUCCESS || len > 1 << 20 {
            return None;
        }
        let mut data = vec![0u8; len as usize + 2];
        let mut len2 = len;
        // SAFETY: buffer is at least `len` bytes.
        let st = unsafe { RegQueryValueExW(self.0, PCWSTR(w.as_ptr()), None, Some(&mut ty), Some(data.as_mut_ptr()), Some(&mut len2)) };
        if st != ERROR_SUCCESS {
            return None;
        }
        data.truncate(len2 as usize);
        Some(decode(ty, data))
    }

    pub fn string(&self, name: &str) -> Option<String> {
        match self.value(name)? {
            Value::Str(s) => Some(s).filter(|s| !s.is_empty()),
            Value::U32(v) => Some(v.to_string()),
            Value::U64(v) => Some(v.to_string()),
            Value::Bytes(_) => None,
        }
    }

    pub fn u32(&self, name: &str) -> Option<u32> {
        match self.value(name)? {
            Value::U32(v) => Some(v),
            Value::U64(v) => Some(v as u32),
            Value::Str(s) => s.trim().parse().ok(),
            Value::Bytes(_) => None,
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by RegOpenKeyExW.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    U32(u32),
    U64(u64),
    Bytes(Vec<u8>),
}

fn decode(ty: REG_VALUE_TYPE, data: Vec<u8>) -> Value {
    match ty {
        REG_SZ | REG_EXPAND_SZ => {
            let u: Vec<u16> = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let s = from_wide(&u);
            if ty == REG_EXPAND_SZ {
                Value::Str(expand_env(&s))
            } else {
                Value::Str(s)
            }
        }
        REG_DWORD if data.len() >= 4 => Value::U32(u32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        REG_QWORD if data.len() >= 8 => Value::U64(u64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]))),
        REG_BINARY => Value::Bytes(data),
        _ => Value::Bytes(data),
    }
}

/// True when the process runs elevated (administrator token).
pub fn is_elevated() -> bool {
    // SAFETY: no arguments.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// Path of a known folder (`FOLDERID_*`).
pub fn known_folder(id: &windows::core::GUID) -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: the returned string is copied and then freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        s.map(std::path::PathBuf::from)
    }
}

/// Expands 8.3 short names (`GetLongPathNameW`); `None` when the path does not exist.
pub fn long_path(p: &std::path::Path) -> Option<std::path::PathBuf> {
    use windows::Win32::Storage::FileSystem::GetLongPathNameW;
    let w = wide(&p.to_string_lossy());
    let mut buf = vec![0u16; 1024];
    // SAFETY: NUL-terminated input, output buffer sized as passed.
    let n = unsafe { GetLongPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    (n > 0 && n < buf.len()).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

pub use super::expand_env;

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

    #[test]
    fn reads_current_version() {
        let k = Key::open(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", View::Default).expect("open CurrentVersion");
        assert!(k.string("CurrentBuild").is_some());
        assert!(!k.subkeys().is_empty() || !k.values().is_empty());
    }
}
