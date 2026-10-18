//! Opening TMOP in place of Windows Task Manager.
//!
//! Windows starts the program named in the `Debugger` value of
//! `HKLM\...\Image File Execution Options\taskmgr.exe` instead of Task Manager, with the
//! original command line appended (`"TMOP.exe" C:\Windows\system32\taskmgr.exe /7`). That
//! covers Ctrl+Shift+Esc, the taskbar menu and `taskmgr.exe` alike.
//!
//! * The registry value is the only record of the setting; nothing is kept in the settings file.
//! * Writing it needs administrator rights, so the app starts an elevated copy of itself with
//!   `--set-taskmgr-replacement=on|off`, which makes the change and exits without a window.
//! * TMOP only removes values that point at a TMOP.exe; another program's value is left alone.
//! * A Task Manager launch is handed to an already running main window over a named pipe, the
//!   way Task Manager brings its own window back instead of opening a second one.
//! * [`open_real`] starts the real Task Manager past the replacement, as a debuggee that is
//!   detached at once (Windows skips the `Debugger` value for processes started under a debugger).
//!   It does so from an elevated copy, because a non-elevated Task Manager restarts itself
//!   elevated, and that restart would open TMOP again.

use std::path::{Path, PathBuf};

/// Registry key (under HKLM, 64-bit view) that holds the `Debugger` value.
pub const IFEO_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
/// Hidden switch for the elevated helper: `on` registers this exe, `off` restores Task Manager.
pub const HELPER_ARG: &str = "--set-taskmgr-replacement=";
/// Restores Windows Task Manager from the command line (asks for administrator permission).
pub const RESTORE_ARG: &str = "--restore-taskmgr";
/// Hidden switch: start the real Task Manager (used elevated when it needs elevation).
pub const OPEN_REAL_ARG: &str = "--open-taskmgr";
/// Exit code of the helper when the value belongs to another program and was left alone.
pub const EXIT_NOT_OURS: i32 = 0x2000_0001;

/// What the `Debugger` value currently says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// No value: Windows opens its own Task Manager.
    Default,
    /// Task Manager launches open this TMOP.exe.
    Ours,
    /// Task Manager launches open another TMOP.exe (a moved or second copy). When it no longer
    /// exists, Ctrl+Shift+Esc opens nothing at all.
    OtherTmop { path: PathBuf, exists: bool },
    /// Another program replaces Task Manager; the raw value.
    Foreign(String),
}

impl State {
    /// True when the value points at some TMOP.exe, so TMOP may remove it.
    pub fn is_tmop(&self) -> bool {
        matches!(self, State::Ours | State::OtherTmop { .. })
    }

    /// True when the value points at a TMOP.exe that is gone.
    pub fn is_broken(&self) -> bool {
        matches!(self, State::OtherTmop { exists: false, .. })
    }
}

/// The `Debugger` value for `exe`: the path, always quoted so spaces survive.
pub fn debugger_value(exe: &Path) -> String {
    format!("\"{}\"", plain(exe))
}

/// The program path in a `Debugger` value (quoted, or unquoted up to `.exe`).
pub fn debugger_path(value: &str) -> PathBuf {
    let v = value.trim();
    if let Some(rest) = v.strip_prefix('"') {
        return PathBuf::from(rest.split('"').next().unwrap_or(rest));
    }
    match v.to_ascii_lowercase().find(".exe") {
        Some(i) => PathBuf::from(&v[..i + 4]),
        None => PathBuf::from(v.split_whitespace().next().unwrap_or(v)),
    }
}

/// Classifies a `Debugger` value against the running exe (as [`this_exe`] gives it).
/// `resolve` returns a path's full long form when it exists, so `C:\Users\ADMINI~1\...` and
/// `C:\Users\Administrator\...` count as the same file.
pub fn classify(value: Option<&str>, current: &Path, resolve: impl Fn(&Path) -> Option<PathBuf>) -> State {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return State::Default;
    };
    let path = debugger_path(value);
    let resolved = resolve(&path);
    if same_path(&path, current) || resolved.as_deref().map(|r| same_path(r, current)).unwrap_or(false) {
        State::Ours
    } else if is_tmop_exe(&path) {
        State::OtherTmop { path, exists: resolved.is_some() }
    } else {
        State::Foreign(value.to_string())
    }
}

fn is_tmop_exe(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).map(|n| n.eq_ignore_ascii_case("TMOP.exe")).unwrap_or(false)
}

/// Path text without the `\\?\` prefix `current_exe` can return.
fn plain(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| plain(p).replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

fn under(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| plain(p).replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && p.len() > d.len() && p.starts_with(&d) && p.as_bytes()[d.len()] == b'\\'
}

/// Why `exe` is a poor place to register, if it is: these copies tend to be deleted or unplugged.
pub fn risky_location(exe: &Path, downloads: Option<&Path>, temp: &Path, removable: bool) -> Option<&'static str> {
    if downloads.map(|d| under(exe, d)).unwrap_or(false) {
        Some("your Downloads folder")
    } else if under(exe, temp) {
        Some("a temporary folder")
    } else if removable {
        Some("a removable drive")
    } else {
        None
    }
}

/// A launch in place of Task Manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskmgrLaunch {
    /// Page id to open, when the switches ask for one.
    pub page: Option<&'static str>,
}

/// Recognises a launch through the `Debugger` value: the first argument is Task Manager's path.
/// Of its switches only `/startup` (used by msconfig) names a page; the numeric ones only say
/// how it was started.
pub fn taskmgr_launch(args: &[String]) -> Option<TaskmgrLaunch> {
    let first = Path::new(args.first()?.trim_matches('"'));
    let stem = first.file_stem()?.to_str()?;
    let ext_ok = first.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(true);
    if !stem.eq_ignore_ascii_case("taskmgr") || !ext_ok {
        return None;
    }
    let startup = args[1..].iter().any(|a| a.eq_ignore_ascii_case("/startup"));
    Some(TaskmgrLaunch { page: startup.then_some("startup") })
}

/// Handles the helper and restore switches. Returns the exit code when one was given.
pub fn run_switches(args: &[String]) -> Option<i32> {
    if let Some(v) = args.iter().find_map(|a| a.strip_prefix(HELPER_ARG)) {
        return Some(match v {
            "on" => code(apply(true)),
            "off" => code(apply(false)),
            _ => 1,
        });
    }
    if args.iter().any(|a| a == RESTORE_ARG) {
        let r = set(false);
        match &r {
            Ok(()) => message("Windows Task Manager is restored.", false),
            Err(e) => message(&format!("Windows Task Manager was not restored: {e}"), true),
        }
        return Some(if r.is_ok() { 0 } else { 1 });
    }
    if args.iter().any(|a| a == OPEN_REAL_ARG) {
        return Some(match open_real() {
            Ok(()) => 0,
            Err(e) => {
                message(&e, true);
                1
            }
        });
    }
    None
}

/// Why the helper could not make the change.
#[derive(Debug)]
pub enum ApplyError {
    NotOurs(String),
    Os(i32),
}

fn code(r: Result<(), ApplyError>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(ApplyError::NotOurs(_)) => EXIT_NOT_OURS,
        Err(ApplyError::Os(c)) => c,
    }
}

/// Text for a helper exit code.
pub fn describe_exit(code: i32) -> String {
    match code {
        0 => "done".into(),
        EXIT_NOT_OURS => "another program replaces Task Manager, so TMOP left it alone".into(),
        c => std::io::Error::from_raw_os_error(c).to_string(),
    }
}

/// The running exe in its full long form (no 8.3 short names), as it is registered.
pub fn this_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    std::fs::canonicalize(&exe).map(|p| PathBuf::from(plain(&p))).unwrap_or(exe)
}

/// Current state, read from the registry.
pub fn state() -> State {
    classify(win::read_value().as_deref(), &this_exe(), |p| std::fs::canonicalize(p).ok())
}

/// Makes the change in this process (needs administrator rights).
pub fn apply(on: bool) -> Result<(), ApplyError> {
    if on {
        return win::write_value(&debugger_value(&this_exe())).map_err(ApplyError::Os);
    }
    match state() {
        State::Foreign(v) => Err(ApplyError::NotOurs(v)),
        // With no value left this only removes an empty key that may remain.
        State::Default | State::Ours | State::OtherTmop { .. } => win::delete_value().map_err(ApplyError::Os),
    }
}

/// Turns the replacement on (this exe) or off, elevating through the helper when needed.
/// Blocks until the helper has finished; call it off the UI thread.
pub fn set(on: bool) -> Result<(), String> {
    if win::is_elevated() {
        return apply(on).map_err(|e| describe_exit(code(Err(e))));
    }
    let arg = format!("{HELPER_ARG}{}", if on { "on" } else { "off" });
    match win::run_elevated(&arg, true)? {
        0 => Ok(()),
        c => Err(describe_exit(c)),
    }
}

/// Why registering this exe is a bad idea, if it is.
pub fn location_warning() -> Option<String> {
    let exe = this_exe();
    let downloads = win::downloads_dir();
    let why = risky_location(&exe, downloads.as_deref(), &std::env::temp_dir(), win::on_removable_drive(&exe))?;
    Some(format!("This copy of TMOP is in {why}. If it is deleted, moved or unplugged, Ctrl+Shift+Esc opens nothing until Windows Task Manager is restored."))
}

/// Starts the real Windows Task Manager even while TMOP replaces it.
pub fn open_real() -> Result<(), String> {
    win::open_real()
}

/// Hands a Task Manager launch to a running TMOP window. Returns false when none answers.
/// `page` is a page id, or empty to keep that window's page.
pub fn hand_off(page: &str) -> bool {
    win::hand_off(page)
}

/// Requests from later Task Manager launches, filled by a background listener.
#[derive(Clone, Default)]
pub struct Inbox(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl Inbox {
    /// The most recent request since the last call (a page id, or empty for "just focus").
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.take())
    }

    #[cfg(windows)]
    fn put(&self, page: String) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(page);
        }
    }
}

/// Starts listening for Task Manager launches. `wake` is called after each request.
pub fn listen(wake: impl Fn() + Send + 'static) -> Option<Inbox> {
    let inbox = Inbox::default();
    win::listen(inbox.clone(), Box::new(wake)).then_some(inbox)
}

fn message(text: &str, error: bool) {
    win::message(text, error)
}

#[cfg(windows)]
mod win {
    use super::Inbox;
    use crate::pro::winutil::{wide, Key, View};
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_CONNECTED, ERROR_SUCCESS, HANDLE, HWND};
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Diagnostics::Debug::{DebugActiveProcessStop, DebugSetProcessKillOnExit};
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{CreateProcessW, GetCurrentProcessId, GetExitCodeProcess, WaitForSingleObject, DEBUG_ONLY_THIS_PROCESS, INFINITE, PROCESS_INFORMATION, STARTUPINFOW};
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, IsUserAnAdmin, SHGetKnownFolderPath, ShellExecuteExW, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, SW_HIDE};

    pub fn read_value() -> Option<String> {
        Key::open(HKEY_LOCAL_MACHINE, super::IFEO_KEY, View::Wow64_64)?.string("Debugger")
    }

    pub fn write_value(value: &str) -> Result<(), i32> {
        let path = wide(super::IFEO_KEY);
        let mut h = HKEY::default();
        // SAFETY: valid NUL-terminated path; `h` receives the handle and is closed below.
        let st = unsafe {
            RegCreateKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), 0, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, None, &mut h, None)
        };
        if st != ERROR_SUCCESS {
            return Err(st.0 as i32);
        }
        let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: `h` is open; `data` is a NUL-terminated UTF-16 string.
        let st = unsafe { RegSetValueExW(h, w!("Debugger"), 0, REG_SZ, Some(&data)) };
        // SAFETY: opened above.
        unsafe {
            let _ = RegCloseKey(h);
        }
        if st == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(st.0 as i32)
        }
    }

    /// Removes the `Debugger` value, and the key too when nothing else is left in it (the key
    /// may hold other options, which are kept).
    pub fn delete_value() -> Result<(), i32> {
        let path = wide(super::IFEO_KEY);
        let mut h = HKEY::default();
        // SAFETY: as in `write_value`.
        let st = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), 0, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, &mut h) };
        if st == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if st != ERROR_SUCCESS {
            return Err(st.0 as i32);
        }
        // SAFETY: `h` is open.
        let st = unsafe { RegDeleteValueW(h, w!("Debugger")) };
        let (mut subkeys, mut values) = (1u32, 1u32);
        // SAFETY: `h` is open; only the two counts are written.
        let info = unsafe { RegQueryInfoKeyW(h, PWSTR::null(), None, None, Some(&mut subkeys), None, None, Some(&mut values), None, None, None, None) };
        // SAFETY: opened above.
        unsafe {
            let _ = RegCloseKey(h);
        }
        if st != ERROR_SUCCESS && st != ERROR_FILE_NOT_FOUND {
            return Err(st.0 as i32);
        }
        if info == ERROR_SUCCESS && subkeys == 0 && values == 0 {
            // SAFETY: valid NUL-terminated path. A failure only leaves an empty key behind.
            unsafe {
                let _ = RegDeleteKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), KEY_WOW64_64KEY.0, 0);
            }
        }
        Ok(())
    }

    pub fn is_elevated() -> bool {
        // SAFETY: no arguments.
        unsafe { IsUserAnAdmin().as_bool() }
    }

    /// Starts this exe elevated with `args`. With `wait`, returns its exit code.
    pub fn run_elevated(args: &str, wait: bool) -> Result<i32, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let file = wide(&super::plain(&exe));
        let params = wide(args);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | if wait { SEE_MASK_NOCLOSEPROCESS } else { 0 },
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` and the strings it points to outlive the call.
        if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() { "administrator permission was not given".into() } else { e.message() });
        }
        if !wait || info.hProcess.is_invalid() {
            return Ok(0);
        }
        let mut exit = 1u32;
        // SAFETY: SEE_MASK_NOCLOSEPROCESS gave us the process handle; it is closed here.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut exit);
            let _ = CloseHandle(info.hProcess);
        }
        Ok(exit as i32)
    }

    pub fn downloads_dir() -> Option<PathBuf> {
        // SAFETY: the returned string is copied and then freed with CoTaskMemFree.
        unsafe {
            let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            s.map(PathBuf::from)
        }
    }

    pub fn on_removable_drive(exe: &Path) -> bool {
        let s = super::plain(exe);
        let Some(root) = s.get(..3).filter(|r| r.ends_with(":\\")) else { return false };
        let w = wide(root);
        // SAFETY: valid NUL-terminated root path. 2 is DRIVE_REMOVABLE.
        unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == 2 }
    }

    pub fn open_real() -> Result<(), String> {
        // Started without admin rights, Task Manager restarts itself elevated, and that restart
        // goes through the replacement again. So it is always started from an elevated copy.
        if !is_elevated() {
            return run_elevated(super::OPEN_REAL_ARG, false).map(|_| ());
        }
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let app = wide(&root.join(r"System32\Taskmgr.exe").display().to_string());
        let mut cmd = wide("taskmgr.exe");
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: valid strings and out structs; the handles are closed below.
        let r = unsafe { CreateProcessW(PCWSTR(app.as_ptr()), PWSTR(cmd.as_mut_ptr()), None, None, false, DEBUG_ONLY_THIS_PROCESS, None, PCWSTR::null(), &si, &mut pi) };
        match r {
            Ok(()) => {
                // Detach straight away; Task Manager then runs as usual.
                // SAFETY: this thread created the debuggee; the handles came from CreateProcessW.
                unsafe {
                    let _ = DebugSetProcessKillOnExit(false);
                    let _ = DebugActiveProcessStop(pi.dwProcessId);
                    let _ = CloseHandle(pi.hThread);
                    let _ = CloseHandle(pi.hProcess);
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not start Windows Task Manager: {}", e.message())),
        }
    }

    fn pipe_name() -> String {
        let mut session = 0u32;
        // SAFETY: writes the session id of this process.
        unsafe {
            let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
        }
        format!(r"\\.\pipe\tmop-taskmgr-{session}")
    }

    pub fn hand_off(page: &str) -> bool {
        let name = pipe_name();
        for _ in 0..10 {
            match std::fs::OpenOptions::new().write(true).open(&name) {
                Ok(mut f) => {
                    let mut pid = 0u32;
                    // Our keypress gave this process the right to take the foreground; pass it on.
                    // SAFETY: `f` is an open pipe handle.
                    unsafe {
                        if GetNamedPipeServerProcessId(HANDLE(f.as_raw_handle()), &mut pid).is_ok() {
                            let _ = AllowSetForegroundWindow(pid);
                        }
                    }
                    return f.write_all(page.as_bytes()).is_ok();
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
                // Busy between two listener instances; try again shortly.
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        false
    }

    pub fn listen(inbox: Inbox, wake: Box<dyn Fn() + Send>) -> bool {
        let name = wide(&pipe_name());
        std::thread::Builder::new()
            .name("tmop-taskmgr-pipe".into())
            .spawn(move || loop {
                // SAFETY: valid NUL-terminated name; the handle is owned by `File` below.
                let h = unsafe { CreateNamedPipeW(PCWSTR(name.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, None) };
                if h.is_invalid() {
                    return;
                }
                // SAFETY: `h` is a valid pipe handle; a client that connected first is fine too.
                let connected = unsafe { ConnectNamedPipe(h, None) };
                // SAFETY: `h` is ours and closed when the File drops.
                let f = unsafe { std::fs::File::from_raw_handle(h.0) };
                if connected.is_ok() || connected.as_ref().err().map(|e| e.code() == ERROR_PIPE_CONNECTED.to_hresult()).unwrap_or(false) {
                    let mut page = String::new();
                    if f.take(256).read_to_string(&mut page).is_ok() {
                        inbox.put(page.trim().to_string());
                        wake();
                    }
                }
            })
            .is_ok()
    }

    pub fn message(text: &str, error: bool) {
        let t = wide(text);
        let style = MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION };
        // SAFETY: valid NUL-terminated strings.
        unsafe {
            MessageBoxW(HWND::default(), PCWSTR(t.as_ptr()), w!("TMOP"), style);
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::Inbox;
    use std::path::{Path, PathBuf};

    pub fn read_value() -> Option<String> {
        None
    }
    pub fn write_value(_: &str) -> Result<(), i32> {
        Err(1)
    }
    pub fn delete_value() -> Result<(), i32> {
        Ok(())
    }
    pub fn is_elevated() -> bool {
        false
    }
    pub fn run_elevated(_: &str, _: bool) -> Result<i32, String> {
        Err("only available on Windows".into())
    }
    pub fn downloads_dir() -> Option<PathBuf> {
        None
    }
    pub fn on_removable_drive(_: &Path) -> bool {
        false
    }
    pub fn open_real() -> Result<(), String> {
        Err("only available on Windows".into())
    }
    pub fn hand_off(_: &str) -> bool {
        false
    }
    pub fn listen(_: Inbox, _: Box<dyn Fn() + Send>) -> bool {
        false
    }
    pub fn message(text: &str, _: bool) {
        eprintln!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognises_taskmgr_launches() {
        assert_eq!(taskmgr_launch(&args(&[r"C:\WINDOWS\system32\taskmgr.exe", "/7"])), Some(TaskmgrLaunch { page: None }));
        assert_eq!(taskmgr_launch(&args(&[r"C:\Windows\System32\Taskmgr.exe", "/0", "/startup"])), Some(TaskmgrLaunch { page: Some("startup") }));
        assert_eq!(taskmgr_launch(&args(&["\"C:\\Windows\\System32\\taskmgr.exe\""])), Some(TaskmgrLaunch { page: None }));
        assert_eq!(taskmgr_launch(&args(&["taskmgr"])), Some(TaskmgrLaunch { page: None }));
        assert_eq!(taskmgr_launch(&args(&["--page=processes"])), None);
        assert_eq!(taskmgr_launch(&args(&[r"C:\tools\taskmgr.txt"])), None);
        assert_eq!(taskmgr_launch(&[]), None);
    }

    #[test]
    fn startup_page_maps_to_startup_apps() {
        assert_eq!(crate::ui::Page::from_id("startup"), crate::ui::Page::StartupApps);
    }

    #[test]
    fn debugger_value_round_trips_paths_with_spaces() {
        let exe = Path::new(r"C:\Program Files\My Tools\TMOP.exe");
        let v = debugger_value(exe);
        assert_eq!(v, r#""C:\Program Files\My Tools\TMOP.exe""#);
        assert_eq!(debugger_path(&v), exe);
        assert_eq!(debugger_value(Path::new(r"\\?\C:\x\TMOP.exe")), r#""C:\x\TMOP.exe""#);
        assert_eq!(debugger_path(r"C:\Program Files\Proc\procexp64.exe /e"), Path::new(r"C:\Program Files\Proc\procexp64.exe"));
    }

    #[test]
    fn classifies_values() {
        let cur = Path::new(r"C:\Users\me\Desktop\TMOP.exe");
        let none = |_: &Path| None;
        let found = |p: &Path| Some(p.to_path_buf());
        assert_eq!(classify(None, cur, none), State::Default);
        assert_eq!(classify(Some("  "), cur, none), State::Default);
        assert_eq!(classify(Some(r#""c:\users\ME\desktop\tmop.exe""#), cur, none), State::Ours);
        // A renamed copy is still ours when it is the running exe.
        assert_eq!(classify(Some(r#""C:\t\tmop-0.2.exe""#), Path::new(r"C:\t\tmop-0.2.exe"), none), State::Ours);
        // A short (8.3) spelling of the running exe is ours too.
        let long = |_: &Path| Some(PathBuf::from(r"C:\Users\me\Desktop\TMOP.exe"));
        assert_eq!(classify(Some(r#""C:\Users\me\DESKTO~1\TMOP.exe""#), cur, long), State::Ours);
        assert_eq!(classify(Some(r#""C:\Old\TMOP.exe""#), cur, none), State::OtherTmop { path: r"C:\Old\TMOP.exe".into(), exists: false });
        assert!(classify(Some(r#""C:\Old\TMOP.exe""#), cur, none).is_broken());
        assert!(!classify(Some(r#""C:\Old\TMOP.exe""#), cur, found).is_broken());
        let foreign = classify(Some(r#""C:\Sys\procexp64.exe""#), cur, none);
        assert_eq!(foreign, State::Foreign(r#""C:\Sys\procexp64.exe""#.into()));
        assert!(!foreign.is_tmop());
    }

    #[test]
    fn flags_risky_locations() {
        let temp = Path::new(r"C:\Users\me\AppData\Local\Temp");
        let dl = Path::new(r"C:\Users\me\Downloads");
        assert_eq!(risky_location(Path::new(r"C:\Users\me\Downloads\TMOP.exe"), Some(dl), temp, false), Some("your Downloads folder"));
        assert_eq!(risky_location(Path::new(r"C:\Users\me\AppData\Local\Temp\x\TMOP.exe"), Some(dl), temp, false), Some("a temporary folder"));
        assert_eq!(risky_location(Path::new(r"E:\TMOP.exe"), Some(dl), temp, true), Some("a removable drive"));
        assert_eq!(risky_location(Path::new(r"C:\Users\me\Desktop\TMOP.exe"), Some(dl), temp, false), None);
        // A sibling folder that merely shares the prefix is not inside it.
        assert_eq!(risky_location(Path::new(r"C:\Users\me\Downloads2\TMOP.exe"), Some(dl), temp, false), None);
    }

    #[test]
    fn helper_exit_codes_have_text() {
        assert_eq!(describe_exit(0), "done");
        assert!(describe_exit(EXIT_NOT_OURS).contains("another program"));
        assert!(!describe_exit(5).is_empty());
    }
}
