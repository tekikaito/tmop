//! Windows Disk Cleanup handlers (`HKLM\...\Explorer\VolumeCaches`) for Disk Space "Clean up".
//!
//! The COM handlers (`IEmptyVolumeCache`/`IEmptyVolumeCache2`) run in a HELPER PROCESS, this exe started with
//! `--volume-caches=query:<drive>` (one JSON line per handler: id, names, size, needs_admin/error, ms) or
//! `--volume-caches=purge:<drive>:<id1,id2,...>`. A crashing or hanging third-party handler cannot take the app
//! down, and the parent kills the helper after a timeout. Purge only ever touches ids that were passed AND are
//! registered ([`select_purge`]); `DownloadsFolder` (the user's files) is never listed and never purged.

use super::cleanup::Category;
use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::time::{Duration, Instant};

/// Helper switch; the value is `query:<drive>` or `purge:<drive>:<ids>`.
pub const ARG: &str = "--volume-caches=";

/// Handlers that are never listed, queried or purged (they hold the user's own files).
const EXCLUDED: [&str; 1] = ["DownloadsFolder"];

/// Checked by default: Temporary Files, Thumbnail Cache, Delivery Optimization Files, D3D Shader Cache and
/// Windows Error Reporting. Everything else (Recycle Bin, Update Cleanup, ...) is opt-in.
const PRESELECTED: [&str; 5] = ["Temporary Files", "Thumbnail Cache", "Delivery Optimization Files", "D3D Shader Cache", "Windows Error Reporting Files"];

const QUERY_TIMEOUT: Duration = Duration::from_secs(180);
const PURGE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// One handler as the helper reports it. `size` is what Windows would free (bytes).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Handler {
    /// Registry key name (stable, English); the id passed to `purge:`.
    pub id: String,
    /// Localised display name from Windows.
    pub name: String,
    pub description: String,
    pub size: u64,
    pub needs_admin: bool,
    pub error: Option<String>,
    pub ms: u64,
}

// ---------------------------------------------------------------- pure rules

pub fn is_excluded(id: &str) -> bool {
    EXCLUDED.iter().any(|e| e.eq_ignore_ascii_case(id))
}

pub fn preselected(id: &str) -> bool {
    PRESELECTED.iter().any(|p| p.eq_ignore_ascii_case(id))
}

/// The handlers to purge: exactly `requested` ∩ `enumerated` (case-insensitive, enumerated spelling and
/// order), never `DownloadsFolder`, no duplicates. Unknown ids are ignored.
pub fn select_purge(requested: &[String], enumerated: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in enumerated {
        if !is_excluded(id) && requested.iter().any(|r| r.eq_ignore_ascii_case(id)) && !out.iter().any(|o| o.eq_ignore_ascii_case(id)) {
            out.push(id.clone());
        }
    }
    out
}

/// What the helper was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    Query { drive: String },
    Purge { drive: String, ids: Vec<String> },
}

/// `X`, `X:` or `X:\` -> `X:\`.
fn parse_drive(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches('\\');
    let b = t.as_bytes();
    (b.len() <= 2 && !b.is_empty() && b[0].is_ascii_alphabetic() && (b.len() == 1 || b[1] == b':')).then(|| format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}

/// Parses the value of [`ARG`]: `query:C` or `purge:C:id1,id2`.
pub fn parse_cmd(v: &str) -> Option<Cmd> {
    let (mode, rest) = v.split_once(':')?;
    match mode {
        "query" => Some(Cmd::Query { drive: parse_drive(rest)? }),
        "purge" => {
            // `C:ids` (the drive letter, its colon, then the ids)
            let (drive, ids) = rest.split_once(':').map(|(d, i)| (d, i.trim_start_matches(['\\', ':'])))?;
            let ids: Vec<String> = ids.split(',').map(str::trim).filter(|i| !i.is_empty()).map(str::to_string).collect();
            Some(Cmd::Purge { drive: parse_drive(drive)?, ids })
        }
        _ => None,
    }
}

/// The helper's output: one JSON object per line (anything else is ignored).
pub fn parse_lines(text: &str) -> Vec<Handler> {
    text.lines().filter_map(|l| serde_json::from_str::<Handler>(l.trim()).ok()).filter(|h| !h.id.is_empty()).collect()
}

/// Handlers worth showing: something to free, or one that needs administrator (when not elevated, listed
/// greyed). Failed handlers and `DownloadsFolder` never. Biggest first, admin-only ones last.
pub fn listed(all: &[Handler], elevated: bool) -> Vec<Handler> {
    let mut v: Vec<Handler> = all.iter().filter(|h| !is_excluded(&h.id) && h.error.is_none() && if h.needs_admin { !elevated } else { h.size > 0 }).cloned().collect();
    v.sort_by_key(|h| (h.needs_admin, std::cmp::Reverse(h.size)));
    v
}

/// TMOP categories a (working) Windows handler replaces; the handler wins and the category is hidden.
pub fn covers(id: &str) -> &'static [Category] {
    match id.to_ascii_lowercase().as_str() {
        "temporary files" => &[Category::UserTemp, Category::WindowsTemp],
        "thumbnail cache" => &[Category::ThumbnailCache],
        "delivery optimization files" => &[Category::DeliveryOptimization],
        "recycle bin" => &[Category::RecycleBin],
        "windows error reporting files" => &[Category::WerReports],
        _ => &[],
    }
}

/// Categories to hide: those covered by a handler that answered (a failed one covers nothing, so nothing is lost).
pub fn hidden_categories(all: &[Handler]) -> Vec<Category> {
    all.iter().filter(|h| h.error.is_none() && !is_excluded(&h.id)).flat_map(|h| covers(&h.id).iter().copied()).collect()
}

/// Drive the handlers are queried for: the system drive.
pub fn system_drive() -> String {
    std::env::var("SystemDrive").ok().and_then(|d| parse_drive(&d)).unwrap_or_else(|| "C:\\".into())
}

// ---------------------------------------------------------------- parent side: run the helper

/// Handlers of `drive`, or why the helper failed (crash, timeout, cannot start).
pub fn query(drive: &str) -> Result<Vec<Handler>, String> {
    let (lines, failure) = run_helper(&format!("query:{}", drive.trim_end_matches('\\')), QUERY_TIMEOUT);
    match failure {
        Some(why) => Err(why),
        None => Ok(lines),
    }
}

/// Purges exactly `ids` on `drive`. Returns what the helper reported per handler (`size` = bytes before,
/// `error` None = purged) and the helper's failure, if it died or timed out before finishing.
pub fn purge(drive: &str, ids: &[String]) -> (Vec<Handler>, Option<String>) {
    let ids: Vec<&str> = ids.iter().map(String::as_str).filter(|i| !is_excluded(i) && !i.contains(',')).collect();
    if ids.is_empty() {
        return (Vec::new(), None);
    }
    run_helper(&format!("purge:{}:{}", drive.trim_end_matches('\\'), ids.join(",")), PURGE_TIMEOUT)
}

/// Runs this exe with `--volume-caches=<what>`, collects its JSON lines and kills it after `limit`.
fn run_helper(what: &str, limit: Duration) -> (Vec<Handler>, Option<String>) {
    if cfg!(not(windows)) {
        return (Vec::new(), Some("Windows Disk Cleanup is only available on Windows".into()));
    }
    use std::process::{Command, Stdio};
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return (Vec::new(), Some(format!("cannot find TMOP.exe: {e}"))),
    };
    let mut cmd = Command::new(exe);
    cmd.arg(format!("{ARG}{what}")).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (Vec::new(), Some(format!("cannot start the Windows Disk Cleanup helper: {e}"))),
    };
    let Some(out) = child.stdout.take() else { return (Vec::new(), Some("helper has no output".into())) };
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + limit;
    let mut text = String::new();
    let mut failure = None;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(l) => {
                text.push_str(&l);
                text.push('\n');
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                failure = Some(format!("Windows Disk Cleanup did not answer within {} s and was stopped", limit.as_secs()));
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    match child.wait() {
        Ok(s) if failure.is_none() && !s.success() => failure = Some(format!("Windows Disk Cleanup helper failed (exit {})", s.code().map(|c| format!("{:#x}", c as u32)).unwrap_or_else(|| "?".into()))),
        Err(e) if failure.is_none() => failure = Some(format!("Windows Disk Cleanup helper: {e}")),
        _ => {}
    }
    (parse_lines(&text), failure)
}

// ---------------------------------------------------------------- helper side

/// `--volume-caches=...`: runs the helper and returns its exit code; None when the switch is absent.
pub fn run_switches(args: &[String]) -> Option<i32> {
    let v = args.iter().find_map(|a| a.strip_prefix(ARG))?;
    Some(match parse_cmd(v) {
        Some(c) => helper_main(c),
        None => {
            eprintln!("usage: TMOP.exe {ARG}query:<drive> | {ARG}purge:<drive>:<id1,id2,...>");
            2
        }
    })
}

#[cfg(windows)]
fn emit(h: &Handler) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if let Ok(s) = serde_json::to_string(h) {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}

#[cfg(not(windows))]
fn helper_main(_: Cmd) -> i32 {
    eprintln!("Windows Disk Cleanup is only available on Windows");
    2
}

#[cfg(windows)]
fn helper_main(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Query { drive } => {
            for reg in com::enumerate() {
                match com::run_one(reg.clone(), &drive, false, com::QUERY_EACH) {
                    Some(h) => emit(&h),
                    None => emit(&Handler { id: reg.id, name: reg.name, error: Some("timed out".into()), ..Default::default() }),
                }
            }
            0
        }
        Cmd::Purge { drive, ids } => {
            let regs = com::enumerate();
            let names: Vec<String> = regs.iter().map(|r| r.id.clone()).collect();
            for id in select_purge(&ids, &names) {
                let Some(reg) = regs.iter().find(|r| r.id == id).cloned() else { continue };
                match com::run_one(reg.clone(), &drive, true, com::PURGE_EACH) {
                    Some(h) => emit(&h),
                    None => {
                        // A purge that hangs is not retried and nothing else is purged after it.
                        emit(&Handler { id: reg.id, name: reg.name, error: Some("timed out".into()), ..Default::default() });
                        return 4;
                    }
                }
            }
            0
        }
    }
}

#[cfg(windows)]
mod com {
    use super::{is_excluded, Handler};
    use crate::pro::winutil::{Key, View};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use windows::core::{Interface, GUID, HRESULT, HSTRING, IUnknown_Vtbl, PCWSTR, PWSTR};
    use windows::Win32::System::Com::{CLSIDFromString, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::Win32::UI::LegacyWindowsEnvironmentFeatures::{
        IEmptyVolumeCache, IEmptyVolumeCache2, IEmptyVolumeCacheCallBack, IEmptyVolumeCacheCallBack_Vtbl, EMPTY_VOLUME_CACHE_FLAGS,
    };
    use windows::Win32::UI::Shell::SHLoadIndirectString;

    pub const ROOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\VolumeCaches";
    pub const QUERY_EACH: Duration = Duration::from_secs(30);
    pub const PURGE_EACH: Duration = Duration::from_secs(10 * 60);
    const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;
    /// HRESULT_FROM_WIN32(ERROR_ELEVATION_REQUIRED)
    const E_ELEVATION: i32 = 0x8007_02E4_u32 as i32;

    /// A registered handler: key name and what the registry says about it.
    #[derive(Clone)]
    pub struct Reg {
        pub id: String,
        pub clsid: String,
        pub name: String,
        pub description: String,
    }

    /// `@%SystemRoot%\system32\x.dll,-123` -> the localised string; plain strings pass through.
    fn resolve_indirect(s: &str) -> String {
        if !s.starts_with('@') {
            return s.to_string();
        }
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated input, output buffer sized as passed.
        match unsafe { SHLoadIndirectString(&HSTRING::from(crate::pro::expand_env(s)), &mut buf, None) } {
            Ok(()) => crate::pro::winutil::from_wide(&buf),
            Err(_) => String::new(),
        }
    }

    /// Every registered handler except the excluded ones (registry read only).
    pub fn enumerate() -> Vec<Reg> {
        let Some(root) = Key::open(HKEY_LOCAL_MACHINE, ROOT, View::Default) else { return Vec::new() };
        root.subkeys()
            .into_iter()
            .filter(|id| !is_excluded(id))
            .filter_map(|id| {
                let k = root.subkey(&id)?;
                Some(Reg {
                    clsid: k.string("").unwrap_or_default(),
                    name: k.string("Display").map(|s| resolve_indirect(&s)).unwrap_or_default(),
                    description: k.string("Description").map(|s| resolve_indirect(&s)).unwrap_or_default(),
                    id,
                })
            })
            .collect()
    }

    // The progress callback. Several handlers (D3D Shader Cache, User file versions, Delivery Optimization)
    // use it unconditionally: NULL crashes them or returns E_POINTER. Hand-written COM object (a one-method-pair
    // vtable) because `#[implement]` needs windows-core as a direct dependency.
    #[repr(C)]
    struct Callback {
        vtbl: *const IEmptyVolumeCacheCallBack_Vtbl,
        refs: AtomicU32,
    }

    static CALLBACK_VTBL: IEmptyVolumeCacheCallBack_Vtbl = IEmptyVolumeCacheCallBack_Vtbl {
        base__: IUnknown_Vtbl { QueryInterface: cb_query, AddRef: cb_add_ref, Release: cb_release },
        ScanProgress: cb_scan,
        PurgeProgress: cb_purge,
    };

    unsafe extern "system" fn cb_query(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
        if iid.is_null() || out.is_null() {
            return HRESULT(0x8000_4003_u32 as i32); // E_POINTER
        }
        let iid = &*iid;
        if *iid == IEmptyVolumeCacheCallBack::IID || *iid == windows::core::IUnknown::IID {
            cb_add_ref(this);
            *out = this;
            HRESULT(0)
        } else {
            *out = std::ptr::null_mut();
            HRESULT(0x8000_4002_u32 as i32) // E_NOINTERFACE
        }
    }

    unsafe extern "system" fn cb_add_ref(this: *mut c_void) -> u32 {
        (*(this as *const Callback)).refs.fetch_add(1, Ordering::AcqRel) + 1
    }

    unsafe extern "system" fn cb_release(this: *mut c_void) -> u32 {
        let left = (*(this as *const Callback)).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut Callback));
        }
        left
    }

    /// S_OK: keep going.
    unsafe extern "system" fn cb_scan(_: *mut c_void, _used: u64, _flags: u32, _status: PCWSTR) -> HRESULT {
        HRESULT(0)
    }

    unsafe extern "system" fn cb_purge(_: *mut c_void, _freed: u64, _to_free: u64, _flags: u32, _status: PCWSTR) -> HRESULT {
        HRESULT(0)
    }

    fn new_callback() -> IEmptyVolumeCacheCallBack {
        let raw = Box::into_raw(Box::new(Callback { vtbl: &CALLBACK_VTBL, refs: AtomicU32::new(1) }));
        // SAFETY: `raw` is a valid COM object with a matching vtable; the interface takes over the one reference.
        unsafe { IEmptyVolumeCacheCallBack::from_raw(raw as *mut c_void) }
    }

    struct Fail {
        code: i32,
        what: &'static str,
    }

    fn at(what: &'static str) -> impl Fn(windows::core::Error) -> Fail {
        move |e| Fail { code: e.code().0, what }
    }

    /// Takes ownership of a CoTaskMem string out-parameter.
    unsafe fn take(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }

    /// Initialize, GetSpaceUsed and (only with `purge`, and only when something is reclaimable) Purge for one
    /// handler. Fills `h.size`, and the names Windows reports when the registry has none.
    unsafe fn probe_com(reg: &Reg, drive: &str, purge: bool, h: &mut Handler) -> Result<(), Fail> {
        if reg.clsid.is_empty() {
            return Err(Fail { code: 0, what: "no handler class registered" });
        }
        let guid: GUID = CLSIDFromString(&HSTRING::from(reg.clsid.as_str())).map_err(at("CLSID"))?;
        let cache: IEmptyVolumeCache = CoCreateInstance(&guid, None, CLSCTX_INPROC_SERVER).map_err(at("CoCreateInstance"))?;
        let key = Key::open(HKEY_LOCAL_MACHINE, &format!(r"{ROOT}\{}", reg.id), View::Default).ok_or(Fail { code: 0, what: "cannot open the handler key" })?;
        let vol = HSTRING::from(drive);
        let (mut disp, mut desc) = (PWSTR::null(), PWSTR::null());
        let mut flags = EMPTY_VOLUME_CACHE_FLAGS(0);
        let init = if let Ok(c2) = cache.cast::<IEmptyVolumeCache2>() {
            let mut btn = PWSTR::null();
            let r = c2.InitializeEx(key.raw(), &vol, &HSTRING::from(reg.id.as_str()), &mut disp, &mut desc, &mut btn, &mut flags);
            take(btn);
            r
        } else {
            cache.Initialize(key.raw(), &vol, &mut disp, &mut desc, &mut flags)
        };
        let (disp, desc) = (take(disp), take(desc));
        init.map_err(at("Initialize"))?;
        if h.name.is_empty() {
            h.name = resolve_indirect(&disp);
        }
        if h.description.is_empty() {
            h.description = resolve_indirect(&desc);
        }
        let cb = new_callback();
        let mut used = 0u64;
        cache.GetSpaceUsed(&mut used, &cb).map_err(at("GetSpaceUsed"))?;
        h.size = used;
        if purge && used > 0 {
            let r = cache.Purge(used, &cb);
            let _ = cache.Deactivate();
            r.map_err(at("Purge"))?;
        } else {
            let _ = cache.Deactivate();
        }
        Ok(())
    }

    fn probe(reg: &Reg, drive: &str, purge: bool) -> Handler {
        let t0 = Instant::now();
        let mut h = Handler { id: reg.id.clone(), name: reg.name.clone(), description: reg.description.clone(), ..Default::default() };
        // Purge is only reachable for registered, non-excluded ids (checked again here).
        let r = if purge && is_excluded(&reg.id) {
            Err(Fail { code: 0, what: "refused" })
        } else {
            // SAFETY: COM calls on this thread, which initialised an STA; all pointers come from the calls themselves.
            unsafe { probe_com(reg, drive, purge, &mut h) }
        };
        if let Err(f) = r {
            if f.code == E_ACCESSDENIED || f.code == E_ELEVATION {
                h.needs_admin = true;
                if purge {
                    h.error = Some("needs administrator".into());
                }
            } else if f.code == 0 {
                h.error = Some(f.what.to_string());
            } else {
                h.error = Some(format!("{} failed ({:#010x})", f.what, f.code as u32));
            }
        }
        if h.name.is_empty() {
            h.name = reg.id.clone();
        }
        h.ms = t0.elapsed().as_millis() as u64;
        h
    }

    /// One handler on its own STA thread; None when it does not finish within `limit` (the thread is abandoned).
    pub fn run_one(reg: Reg, drive: &str, purge: bool, limit: Duration) -> Option<Handler> {
        let (tx, rx) = std::sync::mpsc::channel();
        let drive = drive.to_string();
        std::thread::Builder::new()
            .name("tmop-volcache".into())
            .spawn(move || {
                // SAFETY: balanced with CoUninitialize below when it succeeded.
                let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
                let _ = tx.send(probe(&reg, &drive, purge));
                if com {
                    // SAFETY: matches the CoInitializeEx above; every COM object of `probe` is gone.
                    unsafe { CoUninitialize() };
                }
            })
            .ok()?;
        rx.recv_timeout(limit).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn purge_selection_is_requested_and_enumerated_only() {
        let enumerated = s(&["Temporary Files", "Thumbnail Cache", "Recycle Bin", "D3D Shader Cache"]);
        assert_eq!(select_purge(&s(&["Thumbnail Cache", "Temporary Files"]), &enumerated), s(&["Temporary Files", "Thumbnail Cache"]));
        // Unknown ids are ignored, never invented; nothing requested -> nothing purged.
        assert_eq!(select_purge(&s(&["Nope", "../../x", "Recycle Bin"]), &enumerated), s(&["Recycle Bin"]));
        assert!(select_purge(&[], &enumerated).is_empty());
        assert!(select_purge(&s(&["Temporary Files"]), &[]).is_empty());
        // Case-insensitive, no duplicates, enumerated spelling.
        assert_eq!(select_purge(&s(&["thumbnail cache", "THUMBNAIL CACHE"]), &enumerated), s(&["Thumbnail Cache"]));
        // DownloadsFolder is refused even if requested and even if (wrongly) enumerated.
        let with_dl = s(&["Temporary Files", "DownloadsFolder"]);
        assert_eq!(select_purge(&s(&["downloadsfolder", "Temporary Files"]), &with_dl), s(&["Temporary Files"]));
        // Never more than requested.
        for req in [s(&["Recycle Bin"]), s(&["Recycle Bin", "Thumbnail Cache", "x"])] {
            let got = select_purge(&req, &enumerated);
            assert!(got.iter().all(|g| req.iter().any(|r| r.eq_ignore_ascii_case(g)) && enumerated.contains(g)));
        }
    }

    #[test]
    fn command_line_parsing() {
        assert_eq!(parse_cmd("query:C"), Some(Cmd::Query { drive: r"C:\".into() }));
        assert_eq!(parse_cmd(r"query:d:\"), Some(Cmd::Query { drive: r"D:\".into() }));
        assert_eq!(parse_cmd("purge:C:Temporary Files,Thumbnail Cache"), Some(Cmd::Purge { drive: r"C:\".into(), ids: s(&["Temporary Files", "Thumbnail Cache"]) }));
        assert_eq!(parse_cmd("purge:C:"), Some(Cmd::Purge { drive: r"C:\".into(), ids: vec![] }));
        for bad in ["", "query", "query:", "query:CC", "query:1", "query:\\\\srv\\share", "purge", "purge:Temporary Files", "wipe:C", "purge:::x"] {
            assert_eq!(parse_cmd(bad), None, "{bad}");
        }
    }

    #[test]
    fn listed_hides_empty_failed_and_excluded() {
        let h = |id: &str, size: u64, admin: bool, err: Option<&str>| Handler { id: id.into(), size, needs_admin: admin, error: err.map(str::to_string), ..Default::default() };
        let all = vec![
            h("Small", 5, false, None),
            h("Empty", 0, false, None),
            h("DownloadsFolder", 99, false, None),
            h("Broken", 0, false, Some("x")),
            h("Update Cleanup", 0, true, None),
            h("Big", 500, false, None),
        ];
        let ids = |v: Vec<Handler>| v.into_iter().map(|h| h.id).collect::<Vec<_>>();
        assert_eq!(ids(listed(&all, false)), vec!["Big", "Small", "Update Cleanup"]);
        assert_eq!(ids(listed(&all, true)), vec!["Big", "Small"]);
    }

    #[test]
    fn preselection_and_overlaps() {
        for id in ["Temporary Files", "Thumbnail Cache", "Delivery Optimization Files", "D3D Shader Cache", "Windows Error Reporting Files"] {
            assert!(preselected(id), "{id}");
        }
        for id in ["Recycle Bin", "Update Cleanup", "Internet Cache Files", "DownloadsFolder", "Previous Installations"] {
            assert!(!preselected(id), "{id}");
        }
        let ok = |id: &str| Handler { id: id.into(), ..Default::default() };
        let hidden = hidden_categories(&[ok("Temporary Files"), ok("Recycle Bin"), ok("D3D Shader Cache")]);
        assert!(hidden.contains(&Category::UserTemp) && hidden.contains(&Category::WindowsTemp) && hidden.contains(&Category::RecycleBin));
        assert!(!hidden.contains(&Category::ThumbnailCache) && !hidden.contains(&Category::BrowserCaches));
        // A handler that failed covers nothing: TMOP keeps its own category.
        let bad = Handler { id: "Thumbnail Cache".into(), error: Some("boom".into()), ..Default::default() };
        assert!(hidden_categories(&[bad]).is_empty());
        // TMOP keeps browser caches, Windows Update downloads, crash dumps, old Downloads, developer artifacts.
        let every: Vec<&str> = ["Temporary Files", "Thumbnail Cache", "Delivery Optimization Files", "Recycle Bin", "Windows Error Reporting Files"].to_vec();
        let all: Vec<Handler> = every.into_iter().map(ok).collect();
        let hidden = hidden_categories(&all);
        for kept in [Category::BrowserCaches, Category::UpdateDownloads, Category::CrashDumps, Category::OldDownloads, Category::DevArtifacts] {
            assert!(!hidden.contains(&kept), "{kept:?}");
        }
    }

    #[test]
    fn helper_lines_round_trip() {
        let h = Handler { id: "Temporary Files".into(), name: "Tempor\u{e4}re Dateien".into(), description: "d".into(), size: 7, needs_admin: false, error: None, ms: 3 };
        let line = serde_json::to_string(&h).unwrap();
        assert!(!line.contains('\n'));
        let text = format!("noise\n{line}\n{{\"id\":\"\"}}\n{{\"id\":\"X\",\"size\":1}}\n");
        let got = parse_lines(&text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], h);
        assert_eq!(got[1].id, "X");
        assert_eq!(system_drive().len(), 3);
    }

    #[cfg(windows)]
    #[test]
    fn registry_enumeration_never_lists_downloads_folder() {
        let regs = com::enumerate();
        assert!(!regs.is_empty());
        assert!(regs.iter().all(|r| !is_excluded(&r.id)));
    }

    /// Query mode only (GetSpaceUsed): never purges.
    #[cfg(windows)]
    #[test]
    fn query_mode_in_process_reports_sizes() {
        let drive = system_drive();
        let regs = com::enumerate();
        let mut ok = 0;
        for reg in regs.into_iter().take(40) {
            let h = com::run_one(reg.clone(), &drive, false, com::QUERY_EACH).expect("handler answered in time");
            assert_eq!(h.id, reg.id);
            assert!(!h.name.is_empty());
            if h.error.is_none() && !h.needs_admin {
                ok += 1;
            }
        }
        assert!(ok > 0, "at least one handler must answer");
    }
}
