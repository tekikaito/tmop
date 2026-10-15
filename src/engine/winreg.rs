//! HKCU registry writes, elevated shell launches, detached process spawn, and named kernel objects
//! (single-instance mutex, manual-reset event) plus an SDDL-based pipe security descriptor.
//!
//! Only the current user's hive is written. Nothing here runs with elevated rights.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, BOOL, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, HLOCAL,
    HWND, LPARAM, WAIT_OBJECT_0, WIN32_ERROR, WPARAM,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_EXPAND_SZ, REG_ROUTINE_FLAGS, REG_SZ, REG_VALUE_TYPE,
    RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenEventW, OpenProcessToken, SetEvent, WaitForSingleObject, EVENT_ALL_ACCESS,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, WM_SETTINGCHANGE};

use crate::pro::winutil::{from_wide, wide};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `SE_ERR_ACCESSDENIED`: what ShellExecuteW returns when the UAC prompt is declined.
const SE_ERR_ACCESSDENIED: isize = 5;
const ENVIRONMENT_KEY: &str = "Environment";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn reg_error(what: &str, st: WIN32_ERROR) -> String {
    format!("{what} failed with error {}", st.0)
}

/// Reads a string value under HKCU. `None` when the value does not exist.
fn read_user_string(subkey: &str, name: &str) -> Result<Option<String>, String> {
    let key = wide(subkey);
    let value = wide(name);
    let flags = REG_ROUTINE_FLAGS(RRF_RT_REG_SZ.0 | RRF_RT_REG_EXPAND_SZ.0 | RRF_NOEXPAND.0);
    let mut size = 0u32;
    // SAFETY: a null data pointer only asks for the size in bytes.
    let st = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), flags, None, None, Some(&mut size))
    };
    if st == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if st != ERROR_SUCCESS {
        return Err(reg_error("RegGetValueW", st));
    }
    let mut buf = vec![0u16; size as usize / 2 + 1];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `bytes` bytes, as passed in.
    let st = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut bytes),
        )
    };
    if st == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if st != ERROR_SUCCESS {
        return Err(reg_error("RegGetValueW", st));
    }
    Ok(Some(from_wide(&buf)))
}

/// Writes a string value under HKCU with the given registry type.
fn write_user_string(subkey: &str, name: &str, value: &str, ty: REG_VALUE_TYPE) -> Result<(), String> {
    let key = wide(subkey);
    let name_w = wide(name);
    let data = reg_string_bytes(value);
    // SAFETY: `data` holds `data.len()` bytes of UTF-16 including the terminator.
    let st = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            ty.0,
            Some(data.as_ptr() as *const c_void),
            data.len() as u32,
        )
    };
    if st != ERROR_SUCCESS {
        return Err(reg_error("RegSetKeyValueW", st));
    }
    Ok(())
}

/// UTF-16LE bytes of `s` with a NUL terminator, the layout of REG_SZ and REG_EXPAND_SZ data.
fn reg_string_bytes(s: &str) -> Vec<u8> {
    wide(s).iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Reads the user PATH (`HKCU\Environment\Path`) unexpanded. Empty when the value is absent.
pub fn read_user_path() -> Result<String, String> {
    Ok(read_user_string(ENVIRONMENT_KEY, "Path")?.unwrap_or_default())
}

/// Replaces the user PATH with `value` (REG_EXPAND_SZ) and tells running programs about it.
pub fn write_user_path(value: &str) -> Result<(), String> {
    write_user_string(ENVIRONMENT_KEY, "Path", value, REG_EXPAND_SZ)?;
    broadcast_environment_change();
    Ok(())
}

/// Sends WM_SETTINGCHANGE "Environment" to all top-level windows. Best effort, bounded to 2 s.
fn broadcast_environment_change() {
    let env = wide("Environment");
    let mut result = 0usize;
    // SAFETY: `env` outlives the call and `result` is a valid out pointer.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(env.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            2000,
            Some(&mut result),
        );
    }
}

/// Reads a value of the per-user Run key. `None` when it does not exist.
pub fn run_key_get(name: &str) -> Result<Option<String>, String> {
    read_user_string(RUN_KEY, name)
}

/// Sets a value of the per-user Run key to `command` (REG_SZ).
pub fn run_key_set(name: &str, command: &str) -> Result<(), String> {
    write_user_string(RUN_KEY, name, command, REG_SZ)
}

/// Removes a value of the per-user Run key. A missing value is not an error.
pub fn run_key_remove(name: &str) -> Result<(), String> {
    let key = wide(RUN_KEY);
    let value = wide(name);
    // SAFETY: both strings are NUL-terminated and alive for the call.
    let st = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr())) };
    if st == ERROR_SUCCESS || st == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(reg_error("RegDeleteKeyValueW", st))
    }
}

/// Runs `exe` with `args` through the UAC prompt (`runas`). Returns once the process is started.
pub fn run_elevated(exe: &str, args: &str) -> Result<(), String> {
    let verb = wide("runas");
    let exe_w = wide(exe);
    let args_w = wide(args);
    // SAFETY: all strings are NUL-terminated and alive for the call.
    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe_w.as_ptr()),
            PCWSTR(args_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        return Ok(());
    }
    // A declined UAC prompt shows up as ERROR_CANCELLED or as access denied with that last error.
    // SAFETY: GetLastError has no preconditions.
    let last = unsafe { GetLastError() };
    if code == 1223 || (code == SE_ERR_ACCESSDENIED && last == ERROR_CANCELLED) {
        return Err("cancelled by the user".into());
    }
    Err(format!("could not start {exe} elevated (ShellExecute code {code})"))
}

/// Starts `exe` as a detached process with no console and null stdio. Returns its pid.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> std::io::Result<u32> {
    let child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()?;
    Ok(child.id())
}

/// An owned kernel handle, closed on drop.
struct Kernel(HANDLE);

// SAFETY: kernel object handles may be used and closed from any thread.
unsafe impl Send for Kernel {}
// SAFETY: as above; the wrapped operations are thread-safe.
unsafe impl Sync for Kernel {}

impl Kernel {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a Create/Open call and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A named mutex used as a single-instance lock. Holding the handle is the lock: the object
/// lives while any handle to it is open, so dropping the value releases the name.
pub struct NamedMutex {
    _handle: Kernel,
}

impl NamedMutex {
    /// Creates `name`. Returns `None` when another handle already holds it.
    pub fn try_acquire(name: &str) -> Result<Option<NamedMutex>, String> {
        let w = wide(name);
        // SAFETY: `w` is NUL-terminated and alive for the call.
        let handle = unsafe { CreateMutexW(None, BOOL(0), PCWSTR(w.as_ptr())) }.map_err(|e| format!("CreateMutexW failed: {e}"))?;
        let handle = Kernel(handle);
        // SAFETY: GetLastError has no preconditions; it is read right after the create call.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Ok(None);
        }
        Ok(Some(NamedMutex { _handle: handle }))
    }
}

/// A named manual-reset event, used to signal the relay.
pub struct NamedEvent(Kernel);

impl NamedEvent {
    /// Creates a manual-reset event in the unset state, or opens it when it already exists.
    pub fn create(name: &str) -> Result<NamedEvent, String> {
        let w = wide(name);
        // SAFETY: `w` is NUL-terminated and alive for the call.
        let handle = unsafe { CreateEventW(None, BOOL(1), BOOL(0), PCWSTR(w.as_ptr())) }.map_err(|e| format!("CreateEventW failed: {e}"))?;
        Ok(NamedEvent(Kernel(handle)))
    }

    /// Opens an existing event. `None` when it does not exist or cannot be accessed.
    pub fn open(name: &str) -> Option<NamedEvent> {
        let w = wide(name);
        // SAFETY: `w` is NUL-terminated and alive for the call.
        unsafe { OpenEventW(EVENT_ALL_ACCESS, BOOL(0), PCWSTR(w.as_ptr())) }.ok().map(|h| NamedEvent(Kernel(h)))
    }

    /// Sets the event.
    pub fn set(&self) {
        // SAFETY: the handle is open for the lifetime of `self`.
        let _ = unsafe { SetEvent(self.0.raw()) };
    }

    /// True when the event is set right now.
    pub fn is_set(&self) -> bool {
        self.wait(Duration::ZERO)
    }

    /// Waits up to `timeout` for the event. True when it was set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: the handle is open for the lifetime of `self`.
        unsafe { WaitForSingleObject(self.0.raw(), ms) == WAIT_OBJECT_0 }
    }
}

/// SDDL DACL that grants full access to the current user and SYSTEM only.
pub fn current_user_sddl_dacl() -> Result<String, String> {
    Ok(format!("D:P(A;;GA;;;{})(A;;GA;;;SY)", current_user_sid()?))
}

fn current_user_sid() -> Result<String, String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle from GetCurrentProcess needs no closing; `token` receives a real handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(|e| format!("OpenProcessToken failed: {e}"))?;
    let token = Kernel(token);

    let mut needed = 0u32;
    // SAFETY: a null buffer only asks for the required size.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    // u64 storage keeps the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds `needed` bytes at an 8-byte aligned address.
    unsafe { GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr() as *mut c_void), needed, &mut needed) }
        .map_err(|e| format!("GetTokenInformation failed: {e}"))?;
    // SAFETY: the buffer now holds a TOKEN_USER; the SID it points to lives in `buf`.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };

    let mut out = windows::core::PWSTR::null();
    // SAFETY: `user.User.Sid` is valid while `buf` lives; the output string is freed below.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut out) }.map_err(|e| format!("ConvertSidToStringSidW failed: {e}"))?;
    let text = take_local_wide(out);
    Ok(text)
}

/// Copies a NUL-terminated string allocated by LocalAlloc and frees it.
fn take_local_wide(p: windows::core::PWSTR) -> String {
    // SAFETY: the API returned a NUL-terminated buffer; it is read up to the terminator and freed once.
    unsafe {
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len));
        LocalFree(HLOCAL(p.0 as *mut c_void));
        text
    }
}

/// Security attributes built from an SDDL string. Owns the descriptor and frees it on drop.
pub struct SecurityAttributes {
    // Boxed so the pointer handed out by `as_ptr` stays valid when `self` moves.
    attrs: Box<SECURITY_ATTRIBUTES>,
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable heap memory owned by this value.
unsafe impl Send for SecurityAttributes {}

impl SecurityAttributes {
    /// Pointer for `create_with_security_attributes_raw`. Valid while `self` lives.
    pub fn as_ptr(&self) -> *mut SECURITY_ATTRIBUTES {
        &*self.attrs as *const SECURITY_ATTRIBUTES as *mut SECURITY_ATTRIBUTES
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            LocalFree(HLOCAL(self.descriptor.0));
        }
    }
}

/// Builds security attributes from an SDDL string such as the one from [`current_user_sddl_dacl`].
pub fn security_attributes_from_sddl(sddl: &str) -> Result<SecurityAttributes, String> {
    let w = wide(sddl);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `w` is NUL-terminated; the descriptor is released by SecurityAttributes::drop.
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(w.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }
        .map_err(|e| format!("invalid SDDL {sddl}: {e}"))?;
    let attrs = Box::new(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: BOOL(0),
    });
    Ok(SecurityAttributes { attrs, descriptor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_user_path_succeeds() {
        assert!(read_user_path().is_ok());
    }

    #[test]
    fn run_key_get_missing_value_is_ok() {
        assert!(run_key_get("TMOP engine relay").is_ok());
    }

    #[test]
    fn reg_string_bytes_is_utf16_with_terminator() {
        assert_eq!(reg_string_bytes("ab"), vec![b'a', 0, b'b', 0, 0, 0]);
        assert_eq!(reg_string_bytes(""), vec![0, 0]);
    }

    #[test]
    fn named_mutex_is_exclusive_until_dropped() {
        let name = format!(r"Local\tmop_winreg_test_mutex_{}", std::process::id());
        let first = NamedMutex::try_acquire(&name).expect("create");
        assert!(first.is_some());
        assert!(NamedMutex::try_acquire(&name).expect("second").is_none());
        drop(first);
        assert!(NamedMutex::try_acquire(&name).expect("third").is_some());
    }

    #[test]
    fn named_event_set_and_wait() {
        let name = format!(r"Local\tmop_winreg_test_event_{}", std::process::id());
        let event = NamedEvent::create(&name).expect("create");
        assert!(!event.is_set());
        assert!(!event.wait(Duration::from_millis(1)));
        let opened = NamedEvent::open(&name).expect("open");
        opened.set();
        assert!(event.is_set());
        assert!(event.wait(Duration::from_millis(10)));
    }

    #[test]
    fn current_user_dacl_builds_descriptor() {
        let sddl = current_user_sddl_dacl().expect("sid");
        assert!(sddl.starts_with("D:P(A;;GA;;;S-1-5-"), "{sddl}");
        assert!(sddl.ends_with("(A;;GA;;;SY)"));
        assert!(security_attributes_from_sddl(&sddl).is_ok());
    }

    #[test]
    fn invalid_sddl_is_error() {
        assert!(security_attributes_from_sddl("not an sddl").is_err());
    }
}
