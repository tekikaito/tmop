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
//!   way Task Manager brings its own window back instead of opening a second one. Tray mode
//!   uses the same pipe for a second start and `--exit` (see [`crate::tray`]). The test
//!   override `TMOP_TASKMGR_PIPE` names another pipe, so a test copy never reaches a TMOP the
//!   user is running.
//! * The pipe's name is predictable, so both ends check each other. The listener creates the
//!   first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE` (when the name is taken it does not
//!   listen at all), keeps an instance open at all times, and gives the pipe a descriptor
//!   ([`pipe_sddl`]) that lets in only this user and SYSTEM, with a medium label so an
//!   elevated TMOP still hears starts that are not elevated. A start sends nothing, and grants
//!   no foreground right, to a server process of another user ([`plan`]).
//! * [`open_real`] starts the real Task Manager past the replacement, as a debuggee that is
//!   detached at once (Windows skips the `Debugger` value for processes started under a debugger).
//!   It does so from an elevated copy, because a non-elevated Task Manager restarts itself
//!   elevated, and that restart would open TMOP again.
//! * The registered exe must not carry the "Run this program as an administrator"
//!   compatibility setting (`RUNASADMIN` under `AppCompatFlags\Layers`): Windows then refuses
//!   the start from the taskbar with "The requested operation requires elevation", and
//!   Ctrl+Shift+Esc opens nothing. Turning the replacement on removes that setting, and
//!   [`admin_flag`] finds it when it is added later.

use std::path::{Path, PathBuf};

/// Registry key (under HKLM, 64-bit view) that holds the `Debugger` value.
pub const IFEO_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
/// Hidden switch for the elevated helper: `on` registers this exe, `off` restores Task Manager.
pub const HELPER_ARG: &str = "--set-taskmgr-replacement=";
/// Restores Windows Task Manager from the command line (asks for administrator permission).
pub const RESTORE_ARG: &str = "--restore-taskmgr";
/// Hidden switch: start the real Task Manager (used elevated when it needs elevation).
pub const OPEN_REAL_ARG: &str = "--open-taskmgr";
/// Hidden switch for the elevated helper: removes the all-users "run as administrator"
/// compatibility setting of the registered exe.
pub const CLEAR_ADMIN_FLAG_ARG: &str = "--clear-runasadmin";
/// Compatibility settings per exe (under HKCU for one user, under HKLM for all users).
pub const LAYERS_KEY: &str = r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";
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
    if is_this_exe(&path, current, &resolve) {
        State::Ours
    } else if is_tmop_exe(&path) {
        let exists = resolve(&path).is_some();
        State::OtherTmop { path, exists }
    } else {
        State::Foreign(value.to_string())
    }
}

fn is_tmop_exe(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).map(|n| n.eq_ignore_ascii_case("TMOP.exe")).unwrap_or(false)
}

/// Path text without the `\\?\` prefix `current_exe` can return.
pub fn plain(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// Whether two Windows paths name the same file, ignoring case, slash style and a trailing slash.
pub fn same_path(a: &Path, b: &Path) -> bool {
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

/// Where a "Run this program as an administrator" compatibility setting is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminFlag {
    /// For the current user only (HKCU); removing it needs no administrator rights.
    User,
    /// For all users (HKLM), set under "Change settings for all users".
    AllUsers,
}

/// True when a `Layers` value (`~ RUNASADMIN HIGHDPIAWARE`) asks to run as administrator.
pub fn layers_run_as_admin(value: &str) -> bool {
    value.split_whitespace().any(|t| t.eq_ignore_ascii_case("RUNASADMIN"))
}

/// A `Layers` value without `RUNASADMIN`, other settings kept; `None` when nothing is left and
/// the value should be deleted.
pub fn layers_without_admin(value: &str) -> Option<String> {
    let rest: Vec<&str> = value.split_whitespace().filter(|t| !t.eq_ignore_ascii_case("RUNASADMIN")).collect();
    (!rest.iter().all(|t| *t == "~")).then(|| rest.join(" "))
}

/// The exe Task Manager launches open, when it is a TMOP.exe that exists.
fn registered_tmop() -> Option<PathBuf> {
    match state() {
        State::Ours => Some(this_exe()),
        State::OtherTmop { path, exists: true } => Some(path),
        _ => None,
    }
}

/// The "run as administrator" compatibility setting of the registered TMOP.exe, if it has one
/// (per-user first). Only looked for while a TMOP.exe is registered, since only then it matters.
pub fn admin_flag() -> Option<AdminFlag> {
    win::admin_flags(&registered_tmop()?).first().copied()
}

/// True when this exe is set to run as administrator (for this user or all users).
pub fn this_exe_runs_as_admin() -> bool {
    !win::admin_flags(&this_exe()).is_empty()
}

/// Removes the "run as administrator" setting of the registered TMOP.exe: for this user in
/// this process, for all users through the elevated helper when that one is set too.
/// Blocks while the helper runs; call it off the UI thread.
pub fn clear_admin_flag() -> Result<(), String> {
    let Some(exe) = registered_tmop() else { return Ok(()) };
    let flags = win::admin_flags(&exe);
    if flags.contains(&AdminFlag::User) {
        win::clear_admin_flag(&exe, AdminFlag::User).map_err(describe_exit)?;
    }
    if flags.contains(&AdminFlag::AllUsers) {
        if crate::platform::is_elevated() {
            win::clear_admin_flag(&exe, AdminFlag::AllUsers).map_err(describe_exit)?;
        } else {
            match win::run_elevated(CLEAR_ADMIN_FLAG_ARG, true)? {
                0 => {}
                c => return Err(describe_exit(c)),
            }
        }
    }
    Ok(())
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
    if args.iter().any(|a| a == CLEAR_ADMIN_FLAG_ARG) {
        // The elevated helper; the per-user setting was already removed by the caller.
        return Some(match registered_tmop() {
            Some(exe) => win::clear_admin_flag(&exe, AdminFlag::AllUsers).err().unwrap_or(0),
            None => 0,
        });
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

/// Shown when the setting is not in place after the change, or is gone again later.
pub const BLOCKED_TEXT: &str = "Windows or your security software blocked or undid this change. Registering a program in place of Task Manager looks like malware behaviour to some security products. Allow the change in your security software (it may list a blocked registry change under Image File Execution Options\\taskmgr.exe) and try again, or leave this setting off.";

/// Checks the registry state read back after a change. Some security software lets the write
/// succeed and reverts it a moment later, or blocks it with an access error.
pub fn verify(on: bool, after: &State) -> Result<(), String> {
    let ok = if on { *after == State::Ours } else { !after.is_tmop() };
    if ok {
        Ok(())
    } else {
        Err(BLOCKED_TEXT.to_string())
    }
}

/// True when TMOP turned the replacement on in this session but the value is gone now.
pub fn reverted(turned_on: bool, now: &State) -> bool {
    turned_on && *now == State::Default
}

/// Text for a helper exit code.
pub fn describe_exit(code: i32) -> String {
    match code {
        0 => "done".into(),
        5 => BLOCKED_TEXT.into(),
        EXIT_NOT_OURS => "another program replaces Task Manager, so TMOP left it alone".into(),
        c => std::io::Error::from_raw_os_error(c).to_string(),
    }
}

/// The running exe in its full long form (no 8.3 short names), as it is registered (the
/// `Debugger` value, the Run entry). In a Scoop install that is the `current` spelling (see
/// [`stable_exe`]).
pub fn this_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let canonical = std::fs::canonicalize(&exe).map(|p| PathBuf::from(plain(&p))).unwrap_or(exe);
    stable_exe(&canonical, crate::update::is_scoop_install(), |p| std::fs::canonicalize(p).ok())
}

/// The path to register for `canonical` (the running exe with junctions resolved). Scoop keeps
/// each version in `...\apps\<name>\<version>\` and points the junction `...\apps\<name>\current`
/// at the newest one; resolving the junction would register the version folder, and after
/// `scoop update` the old version would go on starting. So when `scoop` (this is a Scoop
/// install) and the sibling `current` resolves to the exe's folder, the `current` spelling is
/// returned. `resolve` gives a path with junctions resolved, when it exists.
pub fn stable_exe(canonical: &Path, scoop: bool, resolve: impl Fn(&Path) -> Option<PathBuf>) -> PathBuf {
    let stable = || {
        let (dir, file) = (canonical.parent()?, canonical.file_name()?);
        let current = dir.parent()?.join("current");
        let points_here = !same_path(dir, &current) && resolve(&current).is_some_and(|r| same_path(&r, dir));
        points_here.then(|| current.join(file))
    };
    scoop.then(stable).flatten().unwrap_or_else(|| canonical.to_path_buf())
}

/// Whether a registered `path` is the running exe `current`: the same spelling, or both resolve
/// to the same file (`resolve` gives the full long form with junctions resolved), so an 8.3
/// short name and Scoop's `current` and version spellings count as this copy.
pub fn is_this_exe(path: &Path, current: &Path, resolve: impl Fn(&Path) -> Option<PathBuf>) -> bool {
    if same_path(path, current) {
        return true;
    }
    let Some(resolved) = resolve(path) else { return false };
    same_path(&resolved, current) || resolve(current).is_some_and(|c| same_path(&resolved, &c))
}

/// Current state, read from the registry.
pub fn state() -> State {
    classify(win::read_value().as_deref(), &this_exe(), |p| std::fs::canonicalize(p).ok())
}

/// Makes the change in this process (needs administrator rights).
pub fn apply(on: bool) -> Result<(), ApplyError> {
    if on {
        win::write_value(&debugger_value(&this_exe())).map_err(ApplyError::Os)?;
        // Set to run as administrator, TMOP cannot be started from the taskbar or Ctrl+Shift+Esc
        // at all. This process is elevated, so the all-users setting is removed here; [`set`]
        // removes the per-user one in the user's own process.
        return win::clear_admin_flag(&this_exe(), AdminFlag::AllUsers).map_err(ApplyError::Os);
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
    if on {
        win::clear_admin_flag(&this_exe(), AdminFlag::User).map_err(describe_exit)?;
    }
    if crate::platform::is_elevated() {
        apply(on).map_err(|e| describe_exit(code(Err(e))))?;
    } else {
        let arg = format!("{HELPER_ARG}{}", if on { "on" } else { "off" });
        match win::run_elevated(&arg, true)? {
            0 => {}
            c => return Err(describe_exit(c)),
        }
    }
    // Read the value back: a write can succeed and be undone at once by security software.
    verify(on, &state())?;
    if on {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        verify(on, &state())?;
    }
    Ok(())
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

/// Who answers on the hand-off pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A process of another user, or one whose user TMOP cannot tell: nothing is sent to it,
    /// and it gets no right to take the foreground.
    Foreign,
    /// A process of this user; `elevated` as far as Windows says.
    Ours { elevated: Option<bool> },
}

/// What a start does with the TMOP that answers on the pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Hand the request over.
    Send,
    /// Leave it alone and start normally.
    Skip,
    /// Ask it to exit, and start in its place.
    AskToExit,
}

/// The rule: only a TMOP of this user gets the request. With `prefer_elevated` (a start that
/// opens the window), an elevated start does not hand over to a TMOP that is not elevated: the
/// user asked for administrator rights, so the old copy exits and this one takes its place.
pub fn plan(peer: Peer, self_elevated: bool, prefer_elevated: bool) -> Plan {
    match peer {
        Peer::Foreign => Plan::Skip,
        Peer::Ours { elevated: Some(false) } if self_elevated && prefer_elevated => Plan::AskToExit,
        Peer::Ours { .. } => Plan::Send,
    }
}

/// What [`hand_off_start`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandOff {
    /// The running TMOP has the request; this start ends.
    Delivered,
    /// No TMOP of this user listens: start normally.
    NoServer,
    /// This start is elevated and the running TMOP is not. It was asked to exit; this start
    /// takes its place as a `--replacing` start does.
    Replacing,
}

/// Hands a Task Manager launch (or `--exit`) to a running TMOP window. Returns false when none
/// of this user answers. `page` is a page id, or empty to keep that window's page.
pub fn hand_off(page: &str) -> bool {
    win::hand_off(page, false) == HandOff::Delivered
}

/// Hands a second start in tray mode to the running TMOP (see [`plan`] for `prefer_elevated`).
pub fn hand_off_start(message: &str, prefer_elevated: bool) -> HandOff {
    win::hand_off(message, prefer_elevated)
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

/// Starts listening for Task Manager launches and later starts. `wake` is called after each
/// request. Only one TMOP listens: `None` when another process has the pipe already (this
/// TMOP is then not the primary copy). With `claim_for` (a `--replacing` start, whose
/// predecessor is still exiting), the pipe is claimed in the background for up to that long,
/// and the inbox is returned at once.
pub fn listen(wake: impl Fn() + Send + 'static, claim_for: std::time::Duration) -> Option<Inbox> {
    let inbox = Inbox::default();
    win::listen(inbox.clone(), Box::new(wake), claim_for).then_some(inbox)
}

/// The pipe's security descriptor: owned by `user_sid`, full access for that user and SYSTEM
/// only, and a medium integrity label, so that an elevated TMOP still hears the same user's
/// starts that are not elevated (by default its pipe would have an administrators-only DACL
/// and a high label).
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("O:{user_sid}D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)S:(ML;;NW;;;ME)")
}

fn message(text: &str, error: bool) {
    win::message(text, error)
}

#[cfg(windows)]
mod win {
    use super::{AdminFlag, Inbox};
    use crate::pro::winutil::{long_path, wide, Key, Value, View};
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_SUCCESS, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows::Win32::System::Diagnostics::Debug::{DebugActiveProcessStop, DebugSetProcessKillOnExit};
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE,
        REG_SAM_FLAGS, REG_SZ,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{CreateProcessW, GetCurrentProcessId, GetExitCodeProcess, WaitForSingleObject, DEBUG_ONLY_THIS_PROCESS, INFINITE, PROCESS_INFORMATION, STARTUPINFOW};
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, ShellExecuteExW, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, SW_HIDE};

    pub fn read_value() -> Option<String> {
        Key::open(HKEY_LOCAL_MACHINE, super::IFEO_KEY, View::Wow64_64)?.string("Debugger")
    }

    pub fn write_value(value: &str) -> Result<(), i32> {
        let path = wide(super::IFEO_KEY);
        let mut h = HKEY::default();
        // SAFETY: valid NUL-terminated path; `h` receives the handle and is closed below.
        let st = unsafe {
            RegCreateKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, None, &mut h, None)
        };
        if st != ERROR_SUCCESS {
            return Err(st.0 as i32);
        }
        let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: `h` is open; `data` is a NUL-terminated UTF-16 string.
        let st = unsafe { RegSetValueExW(h, w!("Debugger"), None, REG_SZ, Some(&data)) };
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
        let st = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), None, KEY_SET_VALUE | KEY_QUERY_VALUE | KEY_WOW64_64KEY, &mut h) };
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
        let info = unsafe { RegQueryInfoKeyW(h, None, None, None, Some(&mut subkeys), None, None, Some(&mut values), None, None, None, None) };
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
                let _ = RegDeleteKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), KEY_WOW64_64KEY.0, None);
            }
        }
        Ok(())
    }


    /// The `Layers` keys of `scope`. HKLM is read in both registry views: a 32-bit program
    /// writing there lands in the 32-bit one.
    fn layers_keys(scope: AdminFlag) -> &'static [(HKEY, View)] {
        match scope {
            AdminFlag::User => &[(HKEY_CURRENT_USER, View::Default)],
            AdminFlag::AllUsers => &[(HKEY_LOCAL_MACHINE, View::Wow64_64), (HKEY_LOCAL_MACHINE, View::Wow64_32)],
        }
    }

    /// The values of a `Layers` key that name `exe` (in any spelling) and ask to run it as
    /// administrator: (value name, data).
    fn admin_values(root: HKEY, view: View, exe: &Path) -> Vec<(String, String)> {
        let Some(key) = Key::open(root, super::LAYERS_KEY, view) else { return Vec::new() };
        let target = long_path(exe).unwrap_or_else(|| exe.to_path_buf());
        key.values()
            .into_iter()
            .filter_map(|(name, value)| match value {
                Value::Str(s) if super::layers_run_as_admin(&s) => Some((name, s)),
                _ => None,
            })
            .filter(|(name, _)| {
                let p = Path::new(name);
                super::same_path(p, exe) || super::same_path(p, &target) || long_path(p).is_some_and(|l| super::same_path(&l, &target))
            })
            .collect()
    }

    /// Where `exe` carries the "run as administrator" setting, per-user first.
    pub fn admin_flags(exe: &Path) -> Vec<AdminFlag> {
        [AdminFlag::User, AdminFlag::AllUsers]
            .into_iter()
            .filter(|&scope| layers_keys(scope).iter().any(|&(root, view)| !admin_values(root, view, exe).is_empty()))
            .collect()
    }

    /// Removes `RUNASADMIN` from the settings of `exe` in `scope` and keeps the others. All
    /// users (HKLM) needs administrator rights.
    pub fn clear_admin_flag(exe: &Path, scope: AdminFlag) -> Result<(), i32> {
        let path = wide(super::LAYERS_KEY);
        for &(root, view) in layers_keys(scope) {
            let found = admin_values(root, view, exe);
            if found.is_empty() {
                continue;
            }
            let sam = KEY_SET_VALUE
                | match view {
                    View::Wow64_64 => KEY_WOW64_64KEY,
                    View::Wow64_32 => KEY_WOW64_32KEY,
                    View::Default => REG_SAM_FLAGS(0),
                };
            let mut h = HKEY::default();
            // SAFETY: valid NUL-terminated path; `h` is closed below.
            let st = unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, sam, &mut h) };
            if st != ERROR_SUCCESS {
                return Err(st.0 as i32);
            }
            let mut result = ERROR_SUCCESS;
            for (name, value) in found {
                let n = wide(&name);
                let st = match super::layers_without_admin(&value) {
                    Some(rest) => {
                        let data: Vec<u8> = wide(&rest).iter().flat_map(|c| c.to_le_bytes()).collect();
                        // SAFETY: `h` is open; `data` is a NUL-terminated UTF-16 string.
                        unsafe { RegSetValueExW(h, PCWSTR(n.as_ptr()), None, REG_SZ, Some(&data)) }
                    }
                    // SAFETY: `h` is open; valid NUL-terminated name.
                    None => unsafe { RegDeleteValueW(h, PCWSTR(n.as_ptr())) },
                };
                if st != ERROR_SUCCESS {
                    result = st;
                }
            }
            // SAFETY: opened above.
            unsafe {
                let _ = RegCloseKey(h);
            }
            if result != ERROR_SUCCESS {
                return Err(result.0 as i32);
            }
        }
        Ok(())
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
        if !crate::platform::is_elevated() {
            return run_elevated(super::OPEN_REAL_ARG, false).map(|_| ());
        }
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let app = wide(&root.join(r"System32\Taskmgr.exe").display().to_string());
        let mut cmd = wide("taskmgr.exe");
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: valid strings and out structs; the handles are closed below.
        let r = unsafe { CreateProcessW(PCWSTR(app.as_ptr()), Some(PWSTR(cmd.as_mut_ptr())), None, None, false, DEBUG_ONLY_THIS_PROCESS, None, PCWSTR::null(), &si, &mut pi) };
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
        if let Some(name) = std::env::var_os("TMOP_TASKMGR_PIPE").and_then(|n| n.into_string().ok()).filter(|n| !n.is_empty()) {
            return format!(r"\\.\pipe\{name}");
        }
        let mut session = 0u32;
        // SAFETY: writes the session id of this process.
        unsafe {
            let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
        }
        format!(r"\\.\pipe\tmop-taskmgr-{session}")
    }

    pub fn hand_off(message: &str, prefer_elevated: bool) -> super::HandOff {
        hand_off_to(&pipe_name(), message, prefer_elevated)
    }

    /// Opens the pipe `name` and, when [`super::plan`] says so, sends `message`.
    pub(super) fn hand_off_to(name: &str, message: &str, prefer_elevated: bool) -> super::HandOff {
        use super::{HandOff, Plan};
        let Some(mut f) = open_pipe(name) else { return HandOff::NoServer };
        let (pid, peer) = server_of(&f);
        let elevated = crate::engine::winreg::current_process_elevated().unwrap_or_else(crate::platform::is_elevated);
        let plan = super::plan(peer, elevated, prefer_elevated);
        let text = match plan {
            Plan::Skip => return HandOff::NoServer,
            Plan::Send => message,
            Plan::AskToExit => crate::tray::EXIT_REQUEST,
        };
        if plan == Plan::Send {
            if let Some(pid) = pid {
                // Our keypress gave this process the right to take the foreground; pass it on,
                // to a TMOP of this user only.
                // SAFETY: plain call with a process id.
                unsafe {
                    let _ = AllowSetForegroundWindow(pid);
                }
            }
        }
        match (f.write_all(text.as_bytes()).is_ok(), plan) {
            (true, Plan::AskToExit) => HandOff::Replacing,
            (true, _) => HandOff::Delivered,
            (false, _) => HandOff::NoServer,
        }
    }

    /// The pipe `name` opened for writing; `None` when nobody listens (or it stays busy).
    fn open_pipe(name: &str) -> Option<std::fs::File> {
        for _ in 0..10 {
            match std::fs::OpenOptions::new().write(true).open(name) {
                Ok(f) => return Some(f),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
                // Busy between two listener instances; try again shortly. Access denied (a
                // pipe of another user) ends the same way, after half a second.
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        None
    }

    /// The process that serves `pipe`, and whether it is this user's. A server whose process
    /// cannot be read (an elevated TMOP seen from a start that is not) is identified by the
    /// pipe's owner, which a TMOP sets to its user (see [`super::pipe_sddl`]).
    fn server_of(pipe: &std::fs::File) -> (Option<u32>, super::Peer) {
        use super::Peer;
        let handle = HANDLE(pipe.as_raw_handle());
        let mut pid = 0u32;
        // SAFETY: `handle` is an open pipe handle.
        let pid = unsafe { GetNamedPipeServerProcessId(handle, &mut pid) }.ok().map(|()| pid);
        let Ok(me) = crate::engine::winreg::current_user_sid() else { return (pid, Peer::Foreign) };
        if let Some(Ok((sid, elevated))) = pid.map(crate::engine::winreg::process_user) {
            return (pid, if sid == me { Peer::Ours { elevated } } else { Peer::Foreign });
        }
        let ours = pipe_owner(handle).is_some_and(|owner| owner == me);
        (pid, if ours { Peer::Ours { elevated: None } } else { Peer::Foreign })
    }

    /// The owner SID of a pipe (or any kernel object) handle.
    pub(super) fn pipe_owner(handle: HANDLE) -> Option<String> {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `handle` is open; `sd` receives a LocalAlloc'ed descriptor that `owner` points into.
        let st = unsafe { GetSecurityInfo(handle, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut sd)) };
        if st != ERROR_SUCCESS {
            return None;
        }
        // SAFETY: `owner` points into `sd`, which is freed once, after the copy.
        unsafe {
            let sid = crate::engine::winreg::sid_string(owner).ok();
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            sid
        }
    }

    /// The pipe's security attributes for this user (see [`super::pipe_sddl`]).
    fn pipe_security() -> Result<crate::engine::winreg::SecurityAttributes, String> {
        crate::engine::winreg::security_attributes_from_sddl(&super::pipe_sddl(&crate::engine::winreg::current_user_sid()?))
    }

    /// Creates an instance of the pipe `name`. `first`: the first one, which fails when the name
    /// exists already (another TMOP, or anything else that took it).
    fn create_instance(name: &[u16], security: &crate::engine::winreg::SecurityAttributes, first: bool) -> Option<HANDLE> {
        let mode = if first { PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE } else { PIPE_ACCESS_INBOUND };
        // SAFETY: valid NUL-terminated name; the attributes outlive the call.
        let h = unsafe { CreateNamedPipeW(PCWSTR(name.as_ptr()), mode, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, Some(security.as_ptr())) };
        (!h.is_invalid()).then_some(h)
    }

    pub fn listen(inbox: Inbox, wake: Box<dyn Fn() + Send>, claim_for: std::time::Duration) -> bool {
        listen_on(&pipe_name(), inbox, wake, claim_for)
    }

    pub(super) fn listen_on(name: &str, inbox: Inbox, wake: Box<dyn Fn() + Send>, claim_for: std::time::Duration) -> bool {
        let Ok(security) = pipe_security() else { return false };
        let name = wide(name);
        let first = create_instance(&name, &security, true);
        if first.is_none() && claim_for.is_zero() {
            return false;
        }
        struct Raw(HANDLE);
        // SAFETY: a pipe handle may be used from any thread.
        unsafe impl Send for Raw {}
        let first = first.map(Raw);
        std::thread::Builder::new()
            .name("tmop-taskmgr-pipe".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + claim_for;
                let mut current = match first {
                    Some(Raw(h)) => h,
                    None => loop {
                        // The TMOP this one replaces still has the pipe; it is exiting.
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if let Some(h) = create_instance(&name, &security, true) {
                            break h;
                        }
                        if std::time::Instant::now() >= deadline {
                            return;
                        }
                    },
                };
                loop {
                    // SAFETY: `current` is a valid pipe handle. A client may have connected before
                    // (ERROR_PIPE_CONNECTED), or even written and closed already (ERROR_NO_DATA):
                    // what it wrote is still there to read.
                    let connected = unsafe { ConnectNamedPipe(current, None) };
                    // The next instance exists before this one closes, so the name never lapses
                    // and no other process can take it in between.
                    let next = create_instance(&name, &security, false);
                    // SAFETY: `current` is ours and closed when the File drops.
                    let f = unsafe { std::fs::File::from_raw_handle(current.0) };
                    let early = |e: &windows::core::Error| e.code() == ERROR_PIPE_CONNECTED.to_hresult() || e.code() == ERROR_NO_DATA.to_hresult();
                    if connected.as_ref().err().is_none_or(early) {
                        let mut page = String::new();
                        if f.take(256).read_to_string(&mut page).is_ok() {
                            inbox.put(page.trim().to_string());
                            wake();
                        }
                    }
                    match next {
                        Some(h) => current = h,
                        None => return,
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
            MessageBoxW(None, PCWSTR(t.as_ptr()), w!("TMOP"), style);
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::{AdminFlag, Inbox};
    use std::path::{Path, PathBuf};

    pub fn admin_flags(_: &Path) -> Vec<AdminFlag> {
        Vec::new()
    }
    pub fn clear_admin_flag(_: &Path, _: AdminFlag) -> Result<(), i32> {
        Ok(())
    }

    pub fn read_value() -> Option<String> {
        None
    }
    pub fn write_value(_: &str) -> Result<(), i32> {
        Err(1)
    }
    pub fn delete_value() -> Result<(), i32> {
        Ok(())
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
    pub fn hand_off(_: &str, _: bool) -> super::HandOff {
        super::HandOff::NoServer
    }
    pub fn listen(_: Inbox, _: Box<dyn Fn() + Send>, _: std::time::Duration) -> bool {
        false
    }
    pub fn message(text: &str, _: bool) {
        eprintln!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Task Manager paths and `Debugger` values are Win32 paths; elsewhere `\` is not a separator.
    #[cfg(windows)]
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
    fn reads_and_strips_run_as_admin_layers() {
        assert!(layers_run_as_admin("~ RUNASADMIN"));
        assert!(layers_run_as_admin("~ HIGHDPIAWARE runasadmin"));
        assert!(layers_run_as_admin("RUNASADMIN"));
        assert!(!layers_run_as_admin("~ HIGHDPIAWARE WIN8RTM"));
        assert!(!layers_run_as_admin(""));
        assert_eq!(layers_without_admin("~ RUNASADMIN"), None);
        assert_eq!(layers_without_admin("RUNASADMIN"), None);
        assert_eq!(layers_without_admin("~ RUNASADMIN HIGHDPIAWARE"), Some("~ HIGHDPIAWARE".into()));
        assert_eq!(layers_without_admin("~ WIN7RTM  RUNASADMIN"), Some("~ WIN7RTM".into()));
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

    #[cfg(windows)]
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

    /// Scoop: the version folder that `current` points at is registered as `current`, so the
    /// registration follows `scoop update`. Anything else stays as it is.
    #[cfg(windows)]
    #[test]
    fn scoop_installs_register_the_current_junction() {
        let version = Path::new(r"C:\Users\me\scoop\apps\tmop\0.11.2\TMOP.exe");
        let current = PathBuf::from(r"C:\Users\me\scoop\apps\tmop\current\TMOP.exe");
        let to = |target: &'static str| move |p: &Path| plain(p).to_lowercase().ends_with(r"\apps\tmop\current").then(|| PathBuf::from(format!(r"\\?\C:\Users\me\scoop\apps\tmop\{target}")));
        assert_eq!(stable_exe(version, true, to("0.11.2")), current);
        // `current` points at another version (this one is an older copy still on disk).
        assert_eq!(stable_exe(version, true, to("0.12.0")), version);
        // Not a Scoop install, or no `current` next to it.
        assert_eq!(stable_exe(version, false, to("0.11.2")), version);
        assert_eq!(stable_exe(version, true, |_: &Path| None), version);
        // Already the `current` spelling (its junction was not resolved).
        assert_eq!(stable_exe(&current, true, to("0.11.2")), current);
        let portable = Path::new(r"D:\Tools\TMOP.exe");
        assert_eq!(stable_exe(portable, true, |p: &Path| Some(p.to_path_buf())), portable);
        // Both spellings are this copy for the Task Manager registration.
        let junction = |p: &Path| Some(PathBuf::from(plain(p).to_lowercase().replace(r"\current\", r"\0.11.2\")));
        assert_eq!(classify(Some(&debugger_value(version)), &current, junction), State::Ours);
        assert_eq!(classify(Some(&debugger_value(&current)), version, junction), State::Ours);
        let old = r#""C:\Users\me\scoop\apps\tmop\0.11.1\TMOP.exe""#;
        assert!(matches!(classify(Some(old), &current, junction), State::OtherTmop { exists: true, .. }));
    }

    /// The same with a real directory junction (`mklink /J` needs no administrator rights).
    #[cfg(windows)]
    #[test]
    fn stable_exe_follows_a_real_junction() {
        let root = std::env::temp_dir().join(format!("tmop-scoop-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        // The temp folder can be an 8.3 spelling; compare long forms.
        let root = PathBuf::from(plain(&std::fs::canonicalize(&root).unwrap()));
        let version = root.join(r"apps\tmop\0.11.2");
        std::fs::create_dir_all(&version).unwrap();
        std::fs::write(version.join("TMOP.exe"), b"").unwrap();
        let current = root.join(r"apps\tmop\current");
        let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&current).arg(&version).output().map(|o| o.status.success()).unwrap_or(false);
        let resolve = |p: &Path| std::fs::canonicalize(p).ok();
        let canonical = resolve(&current.join("TMOP.exe")).map(|p| PathBuf::from(plain(&p)));
        let stable = canonical.as_deref().map(|c| stable_exe(c, true, resolve));
        let same = canonical.as_deref().zip(stable.as_deref()).is_some_and(|(c, s)| is_this_exe(c, s, resolve));
        // Cleaned up before the checks, so a failure leaves nothing behind.
        let _ = std::fs::remove_dir(&current);
        let _ = std::fs::remove_dir_all(&root);
        assert!(made, "mklink /J failed");
        assert!(same_path(canonical.as_deref().unwrap(), &version.join("TMOP.exe")), "{canonical:?}");
        assert!(same_path(stable.as_deref().unwrap(), &current.join("TMOP.exe")), "{stable:?}");
        assert!(same);
    }

    /// Nothing goes to a process of another user; an elevated start replaces a TMOP that is not
    /// elevated only when it opens the window.
    #[test]
    fn hand_off_plan() {
        let ours = |elevated| Peer::Ours { elevated };
        for (self_elevated, prefer) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(plan(Peer::Foreign, self_elevated, prefer), Plan::Skip);
            assert_eq!(plan(ours(Some(true)), self_elevated, prefer), Plan::Send);
            // Unknown elevation: the pipe's owner said it is ours, its token could not be read
            // (an elevated TMOP seen from a start that is not).
            assert_eq!(plan(ours(None), self_elevated, prefer), Plan::Send);
        }
        assert_eq!(plan(ours(Some(false)), false, true), Plan::Send);
        assert_eq!(plan(ours(Some(false)), true, false), Plan::Send, "--tray and --exit never replace");
        assert_eq!(plan(ours(Some(false)), true, true), Plan::AskToExit);
    }

    #[test]
    fn pipe_descriptor_text() {
        let sddl = pipe_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(sddl, "O:S-1-5-21-1-2-3-1001D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)S:(ML;;NW;;;ME)");
    }

    #[cfg(windows)]
    mod pipe {
        use super::super::win::{hand_off_to, listen_on};
        use super::super::*;
        use std::time::{Duration, Instant};

        fn unique(tag: &str) -> String {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            format!(r"\\.\pipe\tmop-test-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
        }

        fn wait_for(inbox: &Inbox) -> Option<String> {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if let Some(m) = inbox.take() {
                    return Some(m);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        }

        /// A TMOP of this user hears later starts, again and again; with no listener nobody does.
        #[test]
        fn hands_over_to_a_listener_of_this_user() {
            let name = unique("ok");
            assert_eq!(hand_off_to(&name, "x", false), HandOff::NoServer);
            let inbox = Inbox::default();
            assert!(listen_on(&name, inbox.clone(), Box::new(|| {}), Duration::ZERO));
            for page in ["processes", "", "performance"] {
                assert_eq!(hand_off_to(&name, page, true), HandOff::Delivered);
                assert_eq!(wait_for(&inbox).as_deref(), Some(page));
            }
        }

        /// One listener per name: a second TMOP (or anything that took the name first) does not
        /// get it, so it is not the primary copy.
        #[test]
        fn the_first_instance_is_exclusive() {
            let name = unique("first");
            assert!(listen_on(&name, Inbox::default(), Box::new(|| {}), Duration::ZERO));
            assert!(!listen_on(&name, Inbox::default(), Box::new(|| {}), Duration::ZERO));

            // A squatter with a plain pipe of that name.
            use windows::core::PCWSTR;
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
            use windows::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES};
            let squat = unique("squat");
            let w = crate::pro::winutil::wide(&squat);
            // SAFETY: valid name; the handle is closed below.
            let h = unsafe { CreateNamedPipeW(PCWSTR(w.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, None) };
            assert!(!h.is_invalid());
            assert!(!listen_on(&squat, Inbox::default(), Box::new(|| {}), Duration::ZERO));
            // SAFETY: created above.
            unsafe {
                let _ = CloseHandle(h);
            }
        }

        /// A `--replacing` start gets the pipe once the TMOP before it lets go.
        #[test]
        fn a_replacing_start_claims_the_pipe_once_it_is_free() {
            let name = unique("claim");
            use windows::core::PCWSTR;
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
            use windows::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES};
            let w = crate::pro::winutil::wide(&name);
            // SAFETY: valid name; the handle is closed below.
            let old = unsafe { CreateNamedPipeW(PCWSTR(w.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, None) };
            assert!(!old.is_invalid());
            let inbox = Inbox::default();
            assert!(listen_on(&name, inbox.clone(), Box::new(|| {}), Duration::from_secs(3)));
            std::thread::sleep(Duration::from_millis(300));
            // SAFETY: created above; the old TMOP exits.
            unsafe {
                let _ = CloseHandle(old);
            }
            let until = Instant::now() + Duration::from_secs(3);
            let mut result = HandOff::NoServer;
            while Instant::now() < until && result != HandOff::Delivered {
                result = hand_off_to(&name, "summary", false);
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(result, HandOff::Delivered);
            assert_eq!(wait_for(&inbox).as_deref(), Some("summary"));
        }

        /// The pipe Windows made carries the descriptor: this user owns it, only this user and
        /// SYSTEM have access, and its label is medium (so starts that are not elevated reach
        /// an elevated TMOP). What it means across elevation can only be seen elevated.
        #[test]
        fn the_pipe_has_the_descriptor() {
            use crate::engine::winreg::{current_user_sid, security_attributes_from_sddl, take_local_wide};
            use windows::core::{PCWSTR, PWSTR};
            use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HLOCAL};
            use windows::Win32::Security::Authorization::{ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT};
            use windows::Win32::Security::{DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
            use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
            use windows::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES};
            let sid = current_user_sid().unwrap();
            let attrs = security_attributes_from_sddl(&pipe_sddl(&sid)).unwrap();
            let w = crate::pro::winutil::wide(&unique("sd"));
            // SAFETY: valid name and attributes; the handle is closed below.
            let h = unsafe { CreateNamedPipeW(PCWSTR(w.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, 0, 512, 0, Some(attrs.as_ptr())) };
            assert!(!h.is_invalid());
            let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
            let mut sd = PSECURITY_DESCRIPTOR::default();
            let mut owner = PSID::default();
            // SAFETY: `h` is open; `sd` is freed below, `owner` points into it.
            let st = unsafe { GetSecurityInfo(h, SE_KERNEL_OBJECT, info, Some(&mut owner), None, None, None, Some(&mut sd)) };
            assert_eq!(st, ERROR_SUCCESS);
            // SAFETY: `owner` points into `sd`, which is still alive.
            let owner = unsafe { crate::engine::winreg::sid_string(owner) }.unwrap();
            let mut text = PWSTR::null();
            // SAFETY: `sd` is valid; `text` is freed by take_local_wide.
            unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, info & !OWNER_SECURITY_INFORMATION, &mut text, None) }.unwrap();
            let text = take_local_wide(text);
            // SAFETY: allocated by GetSecurityInfo; the pipe was created above.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(sd.0)));
                let _ = CloseHandle(h);
            }
            assert_eq!(owner, sid);
            // SDDL may print a well-known alias for the user (LA for the built-in Administrator).
            let aces: Vec<&str> = text.strip_prefix("D:P").unwrap_or_else(|| panic!("{text}")).split("S:").next().unwrap().split(')').filter(|a| !a.is_empty()).collect();
            assert_eq!(aces.len(), 2, "{text}");
            assert!(aces.iter().all(|a| a.starts_with("(A;;FA;;;")), "{text}");
            assert!(aces.iter().any(|a| a.ends_with(";;;SY")), "{text}");
            assert!(text.contains("(ML;;NW;;;ME)"), "{text}");
        }

        /// When the server's process cannot be read (an elevated TMOP seen from a start that is
        /// not), the pipe's owner tells: a client's write handle can read it, and it is this user.
        #[test]
        fn a_client_reads_the_pipe_owner() {
            use std::os::windows::io::AsRawHandle;
            let name = unique("owner");
            assert!(listen_on(&name, Inbox::default(), Box::new(|| {}), Duration::ZERO));
            let f = std::fs::OpenOptions::new().write(true).open(&name).unwrap();
            let owner = super::super::win::pipe_owner(windows::Win32::Foundation::HANDLE(f.as_raw_handle()));
            assert_eq!(owner, crate::engine::winreg::current_user_sid().ok());
        }

        /// The listener's own process is this user's, and its elevation is known.
        #[test]
        fn this_process_is_ours() {
            let (sid, elevated) = crate::engine::winreg::process_user(std::process::id()).unwrap();
            assert_eq!(sid, crate::engine::winreg::current_user_sid().unwrap());
            assert_eq!(elevated, crate::engine::winreg::current_process_elevated());
            assert!(elevated.is_some());
            // The System process cannot be read (or is not this user's).
            assert!(crate::engine::winreg::process_user(4).map(|(s, _)| s != sid).unwrap_or(true));
        }
    }

    #[test]
    fn detects_blocked_or_reverted_changes() {
        let other = State::OtherTmop { path: r"C:\Old\TMOP.exe".into(), exists: true };
        assert!(verify(true, &State::Ours).is_ok());
        assert_eq!(verify(true, &State::Default).unwrap_err(), BLOCKED_TEXT);
        assert!(verify(true, &other).is_err());
        assert!(verify(true, &State::Foreign("x.exe".into())).is_err());
        assert!(verify(false, &State::Default).is_ok());
        assert!(verify(false, &State::Foreign("x.exe".into())).is_ok());
        assert!(verify(false, &State::Ours).is_err());
        assert!(verify(false, &other).is_err());
        assert!(reverted(true, &State::Default));
        assert!(!reverted(true, &State::Ours));
        assert!(!reverted(false, &State::Default));
        // Access denied from the helper gets the same plain explanation.
        assert_eq!(describe_exit(5), BLOCKED_TEXT);
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
