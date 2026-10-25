//! macOS process enumeration (libproc, `sysctl`, CoreGraphics window list) and actions (signals,
//! `setpriority`).
//!
//! `proc_pidinfo(PROC_PIDTASKALLINFO)`, `proc_pid_rusage` and `KERN_PROCARGS2` only answer for
//! processes of the same user unless TMOP runs as root; for the others the row keeps what every
//! user may read (`PROC_PIDT_SHORTBSDINFO`, `proc_pidpath`, `kinfo_proc`) and leaves CPU, memory
//! and threads at zero.

use super::darwin::{bundle_name, filetime, join_args, mach_to_100ns, parse_extern_proc, parse_procargs2, ExternProc, KINFO_PROC_SIZE};
use super::{Priority, ProcessRow, Status, Totals};
use std::collections::HashMap;
use std::ffi::c_int;
use std::mem::size_of;
use std::sync::OnceLock;
use std::time::Instant;

// The proc_info layouts libc declares, checked against <sys/proc_info.h> (PROC_PID*_SIZE).
const _: () = assert!(size_of::<libc::proc_bsdshortinfo>() == 64);
const _: () = assert!(size_of::<libc::proc_bsdinfo>() == 136);
const _: () = assert!(size_of::<libc::proc_taskinfo>() == 96);
const _: () = assert!(size_of::<libc::proc_taskallinfo>() == 232);
const _: () = assert!(size_of::<libc::rusage_info_v4>() == 296);
const _: () = assert!(size_of::<libc::proc_fdinfo>() == 8);

/// `p_flag` bit of a process running under Rosetta.
const P_TRANSLATED: i32 = 0x0002_0000;

/// Plain C data the kernel fills in.
///
/// # Safety
/// All-zero bytes must be a valid value of the type.
unsafe trait PlainData: Sized {}
unsafe impl PlainData for libc::proc_bsdshortinfo {}
unsafe impl PlainData for libc::proc_taskallinfo {}
unsafe impl PlainData for libc::rusage_info_v4 {}

/// `proc_pidinfo` of one flavor; None unless the kernel filled the whole struct.
fn pidinfo<T: PlainData>(pid: i32, flavor: c_int) -> Option<T> {
    // SAFETY: T is valid when zeroed (PlainData); the kernel writes at most size_of::<T>() bytes.
    let mut v: T = unsafe { std::mem::zeroed() };
    let n = unsafe { libc::proc_pidinfo(pid, flavor, 0, (&mut v as *mut T).cast(), size_of::<T>() as c_int) };
    (n > 0 && n as usize == size_of::<T>()).then_some(v)
}

/// `proc_pid_rusage(RUSAGE_INFO_V4)`: footprint, peak footprint and disk bytes (same user only).
fn rusage(pid: i32) -> Option<libc::rusage_info_v4> {
    // SAFETY: zeroed plain data; the kernel writes one rusage_info_v4 (the flavor says which).
    let mut ri: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, (&mut ri as *mut libc::rusage_info_v4).cast()) };
    (rc == 0).then_some(ri)
}

/// The head of `kinfo_proc` (start time, nice, flags, state), which any user may read.
fn kinfo(pid: i32) -> Option<ExternProc> {
    let mut buf = [0u64; KINFO_PROC_SIZE / 8];
    let mut len = KINFO_PROC_SIZE;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: `buf` is valid for `len` bytes; sysctl writes at most `len` and updates it.
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    // SAFETY: plain bytes of the initialised buffer.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), KINFO_PROC_SIZE) };
    (rc == 0 && len == KINFO_PROC_SIZE).then(|| parse_extern_proc(bytes, pid)).flatten()
}

#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

extern "C" {
    // libc declares it deprecated (in favour of the mach2 crate); the C function is stable.
    fn mach_timebase_info(info: *mut Timebase) -> c_int;
}

/// The mach timebase (numer, denom), read once.
fn timebase() -> (u32, u32) {
    static TB: OnceLock<(u32, u32)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut tb = Timebase { numer: 0, denom: 0 };
        // SAFETY: writes the two fields of `tb`.
        let ok = unsafe { mach_timebase_info(&mut tb) } == 0 && tb.numer > 0 && tb.denom > 0;
        if ok {
            (tb.numer, tb.denom)
        } else {
            (1, 1)
        }
    })
}

/// An integer `sysctlbyname` value.
fn sysctl_int(name: &std::ffi::CStr) -> Option<i64> {
    let mut v = 0i64;
    let mut len = size_of::<i64>();
    // SAFETY: `v` is valid for `len` bytes; an int key writes 4 of them (read below).
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), (&mut v as *mut i64).cast(), &mut len, std::ptr::null_mut(), 0) };
    match (rc, len) {
        (0, 4) => Some(i32::from_ne_bytes(v.to_ne_bytes()[..4].try_into().ok()?) as i64),
        (0, 8) => Some(v),
        _ => None,
    }
}

/// The Mac has Apple Silicon (also when TMOP itself runs under Rosetta).
fn arm_mac() -> bool {
    static ARM: OnceLock<bool> = OnceLock::new();
    *ARM.get_or_init(|| sysctl_int(c"hw.optional.arm64") == Some(1))
}

/// "ARM64", or "x64" for Intel Macs and Rosetta; empty when unknown.
fn architecture(flags: Option<i32>) -> &'static str {
    match (arm_mac(), flags) {
        (false, _) => "x64",
        (true, Some(f)) if f & P_TRANSLATED != 0 => "x64",
        (true, Some(_)) => "ARM64",
        (true, None) => "",
    }
}

fn list_pids() -> Result<Vec<i32>, String> {
    // SAFETY: a null buffer asks for the count only.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Err(format!("proc_listallpids failed: {}", std::io::Error::last_os_error()));
    }
    let mut pids = vec![0i32; n as usize + 64];
    // SAFETY: the buffer is valid for its byte length; the call returns the number of pids written.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as c_int) };
    if got < 0 {
        return Err(format!("proc_listallpids failed: {}", std::io::Error::last_os_error()));
    }
    pids.truncate(got as usize);
    pids.retain(|p| *p >= 0);
    Ok(pids)
}

/// Path of the executable (`proc_pidpath`, readable for every process).
fn image_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for its length; the call returns the bytes written.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..(n as usize).min(buf.len())]).into_owned())
}

/// The command line from `KERN_PROCARGS2` (same user or root); `buf` is reused.
fn command_line(pid: i32, buf: &mut Vec<u8>) -> Option<String> {
    static ARGMAX: OnceLock<usize> = OnceLock::new();
    let max = *ARGMAX.get_or_init(|| sysctl_int(c"kern.argmax").map(|v| v.clamp(4096, 4 << 20) as usize).unwrap_or(1 << 20));
    buf.resize(max, 0);
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buf` is valid for `len` bytes; sysctl writes at most that and updates `len`.
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 {
        return None;
    }
    join_args(&parse_procargs2(&buf[..len.min(buf.len())])?)
}

/// The account name of `uid`, else the number.
fn user_name(uid: u32) -> String {
    let mut buf = vec![0u8; 16 * 1024];
    // SAFETY: an all-zero passwd is valid; getpwuid_r writes into `pw` and `buf` (valid for its
    // length) and sets `res` to `&pw` on success.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() || pw.pw_name.is_null() {
        return uid.to_string();
    }
    // SAFETY: pw_name points into `buf` and is NUL-terminated.
    unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned()
}

/// Open file descriptors (`PROC_PIDLISTFDS`, same user or root); `nfiles` is the size of the
/// descriptor table, `buf` is reused.
fn fd_count(pid: i32, nfiles: u32, buf: &mut Vec<u64>) -> Option<u32> {
    let want = (nfiles as usize).clamp(16, 1 << 20) + 16;
    if buf.len() < want {
        buf.resize(want, 0);
    }
    let bytes = want * size_of::<libc::proc_fdinfo>();
    // SAFETY: `buf` holds `want` 8-byte proc_fdinfo slots; the kernel writes at most `bytes`.
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), bytes as c_int) };
    (n >= 0).then(|| (n as usize / size_of::<libc::proc_fdinfo>()) as u32)
}

/// A C name array up to its NUL.
fn c_name(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Which processes own a window, from the WindowServer list (`CGWindowListCopyWindowInfo`).
/// Raw CoreGraphics/CoreFoundation calls: the few needed here do not justify a crate.
mod cg {
    use std::collections::HashSet;
    use std::ffi::c_void;

    type CFRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFRef, idx: isize) -> CFRef;
        fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
        fn CFGetTypeID(cf: CFRef) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CFRef, the_type: isize, value: *mut c_void) -> u8;
        fn CFRelease(cf: CFRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    #[allow(non_upper_case_globals)]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFRef;
        static kCGWindowOwnerPID: CFRef;
        static kCGWindowLayer: CFRef;
        static kCGWindowAlpha: CFRef;
    }

    const LIST_ALL: u32 = 0;
    const EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
    const NUMBER_SINT64: isize = 4;
    const NUMBER_FLOAT64: isize = 6;

    /// A number of `dict` (borrowed, Get rule) converted to `T` through `the_type`.
    unsafe fn number<T: Default>(dict: CFRef, key: CFRef, the_type: isize) -> Option<T> {
        let v = CFDictionaryGetValue(dict, key);
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out = T::default();
        // A lossy conversion still writes the value; only presence matters here.
        CFNumberGetValue(v, the_type, (&mut out as *mut T).cast());
        Some(out)
    }

    /// Pids owning a visible normal window (layer 0, not transparent), on any Space and also
    /// when minimised. Empty without a WindowServer session (ssh, CI without a login).
    pub fn owners() -> HashSet<u32> {
        let mut out = HashSet::new();
        // SAFETY: the Copy rule makes the array ours, released below; dictionary values are
        // borrowed from it and only read while it lives. Every value is type-checked first.
        unsafe {
            let list = CGWindowListCopyWindowInfo(LIST_ALL | EXCLUDE_DESKTOP_ELEMENTS, 0);
            if list.is_null() {
                return out;
            }
            for i in 0..CFArrayGetCount(list) {
                let w = CFArrayGetValueAtIndex(list, i);
                if w.is_null() || CFGetTypeID(w) != CFDictionaryGetTypeID() {
                    continue;
                }
                let layer = number::<i64>(w, kCGWindowLayer, NUMBER_SINT64);
                let alpha = number::<f64>(w, kCGWindowAlpha, NUMBER_FLOAT64).unwrap_or(1.0);
                if let (Some(0), Some(pid)) = (layer, number::<i64>(w, kCGWindowOwnerPID, NUMBER_SINT64)) {
                    if alpha > 0.0 && pid > 0 {
                        out.insert(pid as u32);
                    }
                }
            }
            CFRelease(list);
        }
        out
    }
}

#[derive(Clone, Copy)]
struct Prev {
    create_time: i64,
    cpu_time: u64,
    read: u64,
    write: u64,
}

/// Facts about a process that do not change while it runs.
#[derive(Clone, Default)]
struct Identity {
    path: Option<String>,
    command_line: Option<String>,
    arch: &'static str,
    description: Option<String>,
}

pub struct Enumerator {
    prev: HashMap<u32, Prev>,
    last: Option<Instant>,
    identities: HashMap<(u32, i64), Identity>,
    users: HashMap<u32, String>,
    fd_buf: Vec<u64>,
    args_buf: Vec<u8>,
}

impl Default for Enumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Enumerator {
    pub fn new() -> Self {
        Self { prev: HashMap::new(), last: None, identities: HashMap::new(), users: HashMap::new(), fd_buf: Vec::new(), args_buf: Vec::new() }
    }

    pub fn collect(&mut self, logical_cpus: usize, gpu_by_pid: &HashMap<u32, f32>) -> Result<(Vec<ProcessRow>, Totals), String> {
        let pids = list_pids()?;
        let now = Instant::now();
        let elapsed_100ns = self.last.map(|t| now.duration_since(t).as_nanos() as f64 / 100.0).unwrap_or(0.0);
        let elapsed_s = elapsed_100ns / 1e7;
        let (numer, denom) = timebase();
        let efficiency = super::efficiency_marks();
        let windowed = cg::owners();
        let mut rows = Vec::with_capacity(pids.len());
        let mut next_prev = HashMap::with_capacity(pids.len());
        let mut totals = Totals::default();
        for pid in pids {
            // Gone since the listing, or a zombie (not shown, like exited processes elsewhere).
            let Some(short) = pidinfo::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO) else { continue };
            if short.pbsi_status == libc::SZOMB {
                continue;
            }
            let upid = pid as u32;
            let all = pidinfo::<libc::proc_taskallinfo>(pid, libc::PROC_PIDTASKALLINFO);
            let kp = if all.is_none() { kinfo(pid) } else { None };
            let (start, nice) = match (&all, &kp) {
                (Some(a), _) => ((a.pbsd.pbi_start_tvsec, a.pbsd.pbi_start_tvusec), a.pbsd.pbi_nice),
                (None, Some(k)) => (k.start, k.nice),
                (None, None) => ((0, 0), 0),
            };
            let create_time = if start.0 > 0 { filetime(start.0, start.1) } else { 0 };
            let ri = all.as_ref().and_then(|_| rusage(pid));
            let cpu_time = all.as_ref().map(|a| mach_to_100ns(a.ptinfo.pti_total_user.saturating_add(a.ptinfo.pti_total_system), numer, denom)).unwrap_or(0);
            let (read, write) = ri.as_ref().map(|r| (r.ri_diskio_bytesread, r.ri_diskio_byteswritten)).unwrap_or((0, 0));
            let (cpu_pct, read_bps, write_bps) = match self.prev.get(&upid) {
                Some(p) if p.create_time == create_time && elapsed_100ns > 0.0 => (
                    (cpu_time.saturating_sub(p.cpu_time) as f64 / elapsed_100ns / logical_cpus.max(1) as f64 * 100.0) as f32,
                    read.saturating_sub(p.read) as f64 / elapsed_s.max(1e-3),
                    write.saturating_sub(p.write) as f64 / elapsed_s.max(1e-3),
                ),
                _ => (0.0, 0.0, 0.0),
            };
            next_prev.insert(upid, Prev { create_time, cpu_time, read, write });
            let handles = all.as_ref().and_then(|a| fd_count(pid, a.pbsd.pbi_nfiles, &mut self.fd_buf)).unwrap_or(0);
            let threads = all.as_ref().map(|a| a.ptinfo.pti_threadnum.max(0) as u32).unwrap_or(0);
            totals.processes += 1;
            totals.threads += threads as usize;
            totals.handles += handles as usize;
            let key = (upid, create_time);
            let args_buf = &mut self.args_buf;
            let id = self
                .identities
                .entry(key)
                .or_insert_with(|| {
                    let path = image_path(pid);
                    let flags = kp.or_else(|| kinfo(pid)).map(|k| k.flags);
                    Identity { description: path.as_deref().and_then(bundle_name), command_line: command_line(pid, args_buf), arch: architecture(flags), path }
                })
                .clone();
            let user = self.users.entry(short.pbsi_uid).or_insert_with(|| user_name(short.pbsi_uid)).clone();
            let name = id
                .path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .or_else(|| all.as_ref().map(|a| c_name(&a.pbsd.pbi_name)).filter(|n| !n.is_empty()))
                .unwrap_or_else(|| c_name(&short.pbsi_comm));
            let resident = all.as_ref().map(|a| a.ptinfo.pti_resident_size).unwrap_or(0);
            let footprint = ri.as_ref().map(|r| r.ri_phys_footprint).unwrap_or(resident);
            rows.push(ProcessRow {
                pid: upid,
                parent_pid: short.pbsi_ppid,
                name,
                user: Some(user),
                path: id.path,
                command_line: id.command_line,
                arch: id.arch,
                description: id.description,
                status: if short.pbsi_status == libc::SSTOP { Status::Suspended } else { Status::Running },
                read_bps,
                write_bps,
                peak_working_set: ri.as_ref().map(|r| r.ri_lifetime_max_phys_footprint).unwrap_or(0).max(footprint),
                page_faults: all.as_ref().map(|a| a.ptinfo.pti_faults.max(0) as u32).unwrap_or(0),
                threads,
                handles,
                session_id: 0,
                base_priority: nice,
                priority: None,
                private_working_set: footprint,
                working_set: resident,
                commit: footprint,
                cpu_pct: cpu_pct.clamp(0.0, 100.0),
                cpu_time,
                io_bps: read_bps + write_bps,
                gpu_pct: gpu_by_pid.get(&upid).copied().unwrap_or(0.0),
                create_time,
                efficiency: efficiency.contains(&key),
                has_window: windowed.contains(&upid),
            });
        }
        self.identities.retain(|k, _| next_prev.get(&k.0).is_some_and(|p| p.create_time == k.1));
        super::retain_efficiency(|k| next_prev.get(&k.0).is_some_and(|p| p.create_time == k.1));
        self.prev = next_prev;
        self.last = Some(now);
        Ok((rows, totals))
    }
}

/// The action primitives [`super::actions::run`] drives.
pub(super) mod sys {
    use super::*;
    use std::time::Duration;

    /// How long a process may take to honour SIGTERM before it gets SIGKILL.
    const GRACE: Duration = Duration::from_secs(3);

    fn errno_text() -> String {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EPERM | libc::EACCES) => "Access denied".into(),
            Some(libc::ESRCH) => "No such process".into(),
            _ => e.to_string(),
        }
    }

    /// A real process id: 0 and negative values would address process groups, and pid 1 is
    /// launchd, whose death takes the whole system down.
    fn target(pid: u32) -> Result<i32, String> {
        let p = i32::try_from(pid).ok().filter(|p| *p > 0).ok_or_else(|| format!("invalid process id {pid}"))?;
        if p == 1 {
            return Err("refusing to signal launchd".into());
        }
        Ok(p)
    }

    /// `pid` as a real process id that still names the process started at `start` (the row's
    /// create_time). A row without a start time (0) cannot be checked against PID reuse, so it is
    /// refused rather than signalled blind.
    fn same(pid: u32, start: i64) -> Result<i32, String> {
        let p = target(pid)?;
        if start == 0 {
            return Err("the process start time is unknown, so a reused PID cannot be ruled out".into());
        }
        let k = kinfo(p).ok_or("the process has exited")?;
        if filetime(k.start.0, k.start.1) != start {
            return Err("the process has exited (its PID now belongs to another process)".into());
        }
        Ok(p)
    }

    fn signal(pid: u32, sig: c_int) -> Result<(), String> {
        let pid = target(pid)?;
        // SAFETY: kill has no memory preconditions; `pid` names a single process.
        if unsafe { libc::kill(pid, sig) } == 0 {
            Ok(())
        } else {
            Err(errno_text())
        }
    }

    /// The same process (start time) still lives and has not exited.
    fn alive(pid: i32, start: (u64, u64)) -> bool {
        kinfo(pid).is_some_and(|k| k.start == start && k.stat as u32 != libc::SZOMB)
    }

    /// SIGTERM (with SIGCONT for a stopped process, so it can act on it); a process that has not
    /// ended after [`GRACE`] gets SIGKILL from a background thread, unless its pid now names
    /// another process.
    pub fn terminate(pid: u32, start: i64) -> Result<(), String> {
        let p = same(pid, start)?;
        let before = kinfo(p);
        signal(pid, libc::SIGTERM)?;
        let Some(before) = before else { return Ok(()) };
        if before.stat as u32 == libc::SSTOP {
            let _ = signal(pid, libc::SIGCONT);
        }
        std::thread::spawn(move || {
            let step = Duration::from_millis(100);
            let mut waited = Duration::ZERO;
            while waited < GRACE {
                std::thread::sleep(step);
                waited += step;
                if !alive(p, before.start) {
                    return;
                }
            }
            let _ = signal(pid, libc::SIGKILL);
        });
        Ok(())
    }

    pub fn suspend(pid: u32, start: i64) -> Result<(), String> {
        same(pid, start)?;
        signal(pid, libc::SIGSTOP)
    }

    pub fn resume(pid: u32, start: i64) -> Result<(), String> {
        same(pid, start)?;
        signal(pid, libc::SIGCONT)
    }

    fn set(which: c_int, pid: u32, value: c_int) -> Result<(), String> {
        let pid = target(pid)?;
        // SAFETY: setpriority has no memory preconditions.
        if unsafe { libc::setpriority(which, pid as libc::id_t, value) } == 0 {
            Ok(())
        } else {
            Err(errno_text())
        }
    }

    /// The nice value of the class; raising priority (a lower nice) needs root.
    pub fn set_priority(pid: u32, start: i64, p: Priority) -> Result<(), String> {
        same(pid, start)?;
        set(libc::PRIO_PROCESS, pid, p.nice())
    }

    /// Background priority (`PRIO_DARWIN_BG`, as `taskpolicy -b`): low CPU and I/O priority and
    /// throttled network until turned off.
    pub fn set_efficiency(pid: u32, start: i64, on: bool) -> Result<(), String> {
        same(pid, start)?;
        set(libc::PRIO_DARWIN_PROCESS, pid, if on { libc::PRIO_DARWIN_BG } else { 0 })
    }

    /// Selects the image in Finder (`open -R`).
    pub fn open_location(path: &str) -> Result<(), String> {
        crate::platform::reveal(std::path::Path::new(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn row_of(e: &mut Enumerator, pid: u32) -> Option<ProcessRow> {
        e.collect(4, &HashMap::new()).unwrap().0.into_iter().find(|r| r.pid == pid)
    }

    /// Polls until `f` holds for the row of `pid` (2 s at most).
    fn wait_for(e: &mut Enumerator, pid: u32, f: impl Fn(&ProcessRow) -> bool) -> ProcessRow {
        for _ in 0..40 {
            if let Some(r) = row_of(e, pid).filter(|r| f(r)) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("pid {pid}: {:?}", row_of(e, pid));
    }

    #[test]
    fn finds_itself() {
        let me = std::process::id();
        let mut e = Enumerator::new();
        let (rows, totals) = e.collect(4, &HashMap::new()).unwrap();
        assert!(rows.len() > 10 && totals.processes == rows.len() && totals.threads > 0, "{totals:?}");
        assert!(rows.iter().any(|r| r.pid == 1 && r.name == "launchd"), "launchd");
        // Some CPU time to measure.
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(50) {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let r = row_of(&mut e, me).expect("own row");
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(std::path::Path::new(r.path.as_deref().unwrap()).canonicalize().unwrap(), exe);
        assert_eq!(r.name, exe.file_name().unwrap().to_string_lossy());
        if let Ok(user) = std::env::var("USER") {
            assert_eq!(r.user.as_deref(), Some(user.as_str()));
        }
        assert!(r.command_line.as_deref().unwrap().contains(&*exe.file_name().unwrap().to_string_lossy()), "{:?}", r.command_line);
        assert!(r.threads >= 1 && r.handles >= 3 && r.private_working_set > 0 && r.working_set > 0, "{r:?}");
        assert!(r.cpu_time > 0 && r.parent_pid > 0 && !r.arch.is_empty(), "{r:?}");
        assert_eq!(r.status, Status::Running);
        // Started within the last day, as a FILETIME.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        let now_ft = filetime(now.as_secs(), now.subsec_micros() as u64);
        assert!(r.create_time <= now_ft && r.create_time > now_ft - 864_000_000_000, "{} vs {now_ft}", r.create_time);
    }

    #[test]
    fn kinfo_matches_proc_pidinfo() {
        // Checks the hand-made kinfo_proc offsets against libc's proc_bsdinfo.
        let me = std::process::id() as i32;
        let k = kinfo(me).expect("kinfo_proc");
        let a = pidinfo::<libc::proc_taskallinfo>(me, libc::PROC_PIDTASKALLINFO).expect("taskallinfo");
        assert_eq!(k.start, (a.pbsd.pbi_start_tvsec, a.pbsd.pbi_start_tvusec));
        assert_eq!(k.nice, a.pbsd.pbi_nice);
        assert_eq!(k.stat as u32, libc::SRUN);
        assert_eq!(a.pbsd.pbi_pid, me as u32);
        assert!(kinfo(i32::MAX).is_none());
    }

    #[test]
    fn controls_a_child() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().expect("spawn sleep");
        let pid = child.id();
        let mut e = Enumerator::new();
        let r = wait_for(&mut e, pid, |_| true);
        assert_eq!(r.parent_pid, std::process::id());
        assert_eq!(r.name, "sleep");
        assert_eq!(r.command_line.as_deref(), Some("sleep 30"));
        assert!(super::super::tree_of(&e.collect(4, &HashMap::new()).unwrap().0, std::process::id()).contains(&pid));

        let start = r.create_time;
        assert!(start > 0);
        // A wrong start (a reused pid) is refused before any signal.
        assert!(sys::suspend(pid, start + 1).unwrap_err().contains("exited"));
        sys::suspend(pid, start).unwrap();
        wait_for(&mut e, pid, |r| r.status == Status::Suspended);
        sys::resume(pid, start).unwrap();
        wait_for(&mut e, pid, |r| r.status == Status::Running);

        sys::set_priority(pid, start, Priority::BelowNormal).unwrap();
        let r = wait_for(&mut e, pid, |r| r.base_priority == 10);
        assert_eq!(Priority::from_row(&r), Some(Priority::BelowNormal));
        sys::set_efficiency(pid, start, true).unwrap();
        sys::set_efficiency(pid, start, false).unwrap();

        sys::terminate(pid, start).unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert!(row_of(&mut e, pid).is_none());
    }

    #[test]
    fn a_stopped_child_still_ends() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().expect("spawn sleep");
        let pid = child.id();
        let mut e = Enumerator::new();
        let start = wait_for(&mut e, pid, |_| true).create_time;
        sys::suspend(pid, start).unwrap();
        let r = wait_for(&mut e, pid, |r| r.status == Status::Suspended);
        sys::terminate(pid, r.create_time).unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn refuses_group_ids() {
        for pid in [0, u32::MAX, i32::MAX as u32 + 1] {
            assert!(sys::terminate(pid, 0).unwrap_err().starts_with("invalid process id"));
            assert!(sys::suspend(pid, 0).is_err() && sys::set_priority(pid, 0, Priority::Idle).is_err());
        }
        // launchd is never signalled, whatever the caller's rights.
        let start = kinfo(1).map_or(1, |k| filetime(k.start.0, k.start.1));
        assert!(sys::terminate(1, start).unwrap_err().contains("launchd"));
        assert!(sys::suspend(1, start).unwrap_err().contains("launchd"));
    }

    #[test]
    fn refuses_an_unknown_start() {
        let mut child = Command::new("sleep").arg("30").stdin(Stdio::null()).spawn().expect("spawn sleep");
        let pid = child.id();
        assert!(sys::terminate(pid, 0).unwrap_err().contains("start time is unknown"));
        assert!(sys::set_priority(pid, 0, Priority::BelowNormal).is_err());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn window_owners_do_not_include_tests() {
        // A test binary has no window; the call must also cope without a WindowServer.
        assert!(!cg::owners().contains(&std::process::id()));
    }
}
