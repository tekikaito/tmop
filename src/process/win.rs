//! Windows process enumeration (NtQuerySystemInformation) and actions.

use super::{affinity, Priority, ProcessRow, Totals};
use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Instant;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS};
use windows::Win32::Security::{GetTokenInformation, LookupAccountSidW, TokenUser, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Threading::{
    GetProcessAffinityMask, SetProcessAffinityMask, OpenProcess, OpenProcessToken, ProcessPowerThrottling, QueryFullProcessImageNameW, SetPriorityClass,
    SetProcessInformation, TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS,
    PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS, GetProcessTimes,
};

pub(crate) const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
const MAX_BUFFER: usize = 64 * 1024 * 1024; // safety limit

#[link(name = "ntdll")]
extern "system" {
    pub(crate) fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> NTSTATUS;
    fn NtSuspendProcess(process: HANDLE) -> NTSTATUS;
    fn NtResumeProcess(process: HANDLE) -> NTSTATUS;
}

/// Calls NtQuerySystemInformation, growing the buffer until it fits.
pub(crate) fn query_system_information(class: u32, initial: usize) -> Result<Vec<u8>, String> {
    let mut size = initial.max(4096);
    loop {
        let mut buf = vec![0u8; size];
        let mut needed = 0u32;
        let st = unsafe { NtQuerySystemInformation(class, buf.as_mut_ptr().cast(), size as u32, &mut needed) };
        if st.0 == STATUS_INFO_LENGTH_MISMATCH {
            size = (needed as usize).max(size * 2) + 64 * 1024;
            if size > MAX_BUFFER {
                return Err("SystemProcessInformation exceeded the 64 MiB safety limit".into());
            }
            continue;
        }
        if st.0 < 0 {
            return Err(format!("NtQuerySystemInformation({class}) failed: 0x{:08X}", st.0 as u32));
        }
        buf.truncate(needed.max(1) as usize);
        return Ok(buf);
    }
}

/// PIDs that own a visible, titled, non-tool top-level window, with whether any of
/// those windows is hung (IsHungAppWindow, Windows' "Not responding").
fn windowed_pids() -> HashMap<u32, bool> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW,
    };
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let map = &mut *(l.0 as *mut HashMap<u32, bool>);
        if IsWindowVisible(h).as_bool() && GetWindowTextLengthW(h) > 0 {
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 == 0 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                let hung = IsHungAppWindow(h).as_bool();
                *map.entry(pid).or_insert(false) |= hung;
            }
        }
        BOOL(1)
    }
    let mut map = HashMap::new();
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut map as *mut HashMap<u32, bool> as isize)) };
    map
}

/// x64 SYSTEM_THREAD_INFORMATION is 80 bytes; ThreadState at +68, WaitReason at +72.
const THREAD_INFO_SIZE: usize = 80;

/// True when every thread is waiting with the Suspended wait reason.
fn all_threads_suspended(buf: &[u8], first_thread: usize, n: usize) -> bool {
    if n == 0 {
        return false;
    }
    for i in 0..n {
        let o = first_thread + i * THREAD_INFO_SIZE;
        if o + THREAD_INFO_SIZE > buf.len() {
            return false;
        }
        let u = |off: usize| u32::from_ne_bytes([buf[o + off], buf[o + off + 1], buf[o + off + 2], buf[o + off + 3]]);
        if u(68) != 5 || u(72) != 5 {
            return false;
        }
    }
    true
}

/// x64 SYSTEM_PROCESS_INFORMATION (fixed part, 256 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name_length: u16,
    image_name_maximum_length: u16,
    image_name_buffer: *const u16,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

const _: () = assert!(std::mem::size_of::<SystemProcessInformation>() == 256);

#[derive(Clone, Copy)]
struct Prev {
    create_time: i64,
    cpu_time: u64,
    read: u64,
    write: u64,
    net: super::netio::NetBytes,
}

/// Facts about a process that do not change while it runs.
#[derive(Clone, Default)]
struct Identity {
    user: Option<String>,
    path: Option<String>,
    command_line: Option<String>,
    arch: &'static str,
    description: Option<String>,
}

pub struct Enumerator {
    prev: HashMap<u32, Prev>,
    last: Option<Instant>,
    identity_cache: HashMap<(u32, i64), Identity>,
    descriptions: HashMap<String, Option<String>>,
}

impl Default for Enumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Enumerator {
    pub fn new() -> Self {
        Self { prev: HashMap::new(), last: None, identity_cache: HashMap::new(), descriptions: HashMap::new() }
    }

    pub fn collect(&mut self, logical_cpus: usize, gpu_by_pid: &HashMap<u32, f32>) -> Result<(Vec<ProcessRow>, Totals), String> {
        let buf = query_system_information(SYSTEM_PROCESS_INFORMATION_CLASS, 1 << 20)?;
        let now = Instant::now();
        let elapsed_100ns = self.last.map(|t| now.duration_since(t).as_nanos() as f64 / 100.0).unwrap_or(0.0);
        let elapsed_s = elapsed_100ns / 1e7;
        let mut rows = Vec::with_capacity(512);
        let mut next_prev = HashMap::with_capacity(512);
        let mut totals = Totals { processes: 0, threads: 0, handles: 0 };
        let mut offset = 0usize;
        let mut guard = 0;
        let efficiency = super::efficiency_marks();
        let windowed = windowed_pids();
        let net_totals = super::netio::totals();
        loop {
            guard += 1;
            if guard > 100_000 || offset + std::mem::size_of::<SystemProcessInformation>() > buf.len() {
                break;
            }
            // SAFETY: bounds checked above; read_unaligned tolerates any alignment.
            let spi: SystemProcessInformation = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset).cast()) };
            let pid = spi.unique_process_id as u32;
            let name = if spi.image_name_buffer.is_null() || spi.image_name_length == 0 {
                if pid == 0 { "System Idle Process".to_string() } else { format!("Process {pid}") }
            } else {
                // The name buffer points into `buf`.
                let len = (spi.image_name_length / 2) as usize;
                let start = spi.image_name_buffer as usize;
                let base = buf.as_ptr() as usize;
                if start >= base && start + len * 2 <= base + buf.len() {
                    let s = unsafe { std::slice::from_raw_parts(spi.image_name_buffer, len) };
                    String::from_utf16_lossy(s)
                } else {
                    format!("Process {pid}")
                }
            };
            let cpu_time = (spi.user_time.max(0) + spi.kernel_time.max(0)) as u64;
            let read = (spi.read_transfer_count.max(0) + spi.other_transfer_count.max(0) / 2) as u64;
            let write = (spi.write_transfer_count.max(0) + spi.other_transfer_count.max(0) / 2) as u64;
            let net = net_totals.get(&pid).copied().unwrap_or_default();
            let (cpu_pct, read_bps, write_bps, (net_send_bps, net_recv_bps)) = match self.prev.get(&pid) {
                Some(p) if p.create_time == spi.create_time && elapsed_100ns > 0.0 && pid != 0 => (
                    ((cpu_time.saturating_sub(p.cpu_time)) as f64 / elapsed_100ns / logical_cpus.max(1) as f64 * 100.0) as f32,
                    read.saturating_sub(p.read) as f64 / elapsed_s.max(1e-3),
                    write.saturating_sub(p.write) as f64 / elapsed_s.max(1e-3),
                    super::netio::rates(p.net, net, elapsed_s.max(1e-3)),
                ),
                _ => (0.0, 0.0, 0.0, (0.0, 0.0)),
            };
            next_prev.insert(pid, Prev { create_time: spi.create_time, cpu_time, read, write, net });
            let threads_at = offset + std::mem::size_of::<SystemProcessInformation>();
            let suspended = pid > 4 && all_threads_suspended(&buf, threads_at, spi.number_of_threads as usize);
            let hung = windowed.get(&pid).copied().unwrap_or(false);
            totals.processes += 1;
            totals.threads += spi.number_of_threads as usize;
            totals.handles += spi.handle_count as usize;
            let key = (pid, spi.create_time);
            let descriptions = &mut self.descriptions;
            let id = self.identity_cache.entry(key).or_insert_with(|| resolve_identity(pid, descriptions)).clone();
            rows.push(ProcessRow {
                pid,
                parent_pid: spi.inherited_from_unique_process_id as u32,
                name,
                user: id.user,
                path: id.path,
                command_line: id.command_line,
                arch: id.arch,
                description: id.description,
                status: if suspended { super::Status::Suspended } else if hung { super::Status::NotResponding } else { super::Status::Running },
                read_bps,
                write_bps,
                net_send_bps,
                net_recv_bps,
                peak_working_set: spi.peak_working_set_size as u64,
                page_faults: spi.page_fault_count,
                threads: spi.number_of_threads,
                handles: spi.handle_count,
                session_id: spi.session_id,
                base_priority: spi.base_priority,
                priority: None,
                private_working_set: spi.working_set_private_size.max(0) as u64,
                working_set: spi.working_set_size as u64,
                commit: spi.pagefile_usage as u64,
                cpu_pct: cpu_pct.clamp(0.0, 100.0),
                cpu_time,
                io_bps: read_bps + write_bps,
                gpu_pct: gpu_by_pid.get(&pid).copied().unwrap_or(0.0),
                create_time: spi.create_time,
                efficiency: efficiency.contains(&key),
                has_window: windowed.contains_key(&pid),
            });
            if spi.next_entry_offset == 0 {
                break;
            }
            offset += spi.next_entry_offset as usize;
        }
        // Forget identities of exited processes.
        self.identity_cache.retain(|k, _| next_prev.get(&k.0).map(|p| p.create_time == k.1).unwrap_or(false));
        super::retain_efficiency(|k| next_prev.get(&k.0).map(|p| p.create_time == k.1).unwrap_or(false));
        super::netio::forget(&|pid| next_prev.contains_key(&pid));
        self.prev = next_prev;
        self.last = Some(now);
        Ok((rows, totals))
    }
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<OwnedHandle> {
    unsafe { OpenProcess(access, false, pid).map(OwnedHandle) }
}

/// What a stale row gets when the process it described is gone or its PID now names another one.
const ENDED: &str = "That process has already ended.";

/// The creation time of the process behind `h`, in the units of `ProcessRow::create_time`
/// (100 ns ticks since 1601, as `NtQuerySystemInformation` reports it). Needs
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
fn creation_time(h: &OwnedHandle) -> windows::core::Result<i64> {
    use windows::Win32::Foundation::FILETIME;
    let zero = FILETIME::default();
    let (mut created, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `h` is an open process handle with query access and all four out pointers are valid.
    unsafe { GetProcessTimes(h.0, &mut created, &mut exit, &mut kernel, &mut user)? };
    Ok(((created.dwHighDateTime as i64) << 32) | created.dwLowDateTime as i64)
}

/// Opens `pid` with `access` and checks, on that same handle, that it is still the process the row
/// described (`start` = its create_time), so there is no gap between the check and the action. A
/// `start` of 0 means the row never knew it (the idle process, a process whose times could not be
/// read): that keeps the earlier behaviour and acts on the PID unchecked. A PID that no longer
/// exists reads as ended too. The handle closes when dropped, on every path.
fn open_same(pid: u32, start: i64, access: PROCESS_ACCESS_RIGHTS) -> Result<OwnedHandle, String> {
    const ERROR_INVALID_PARAMETER: u32 = 0x8007_0057;
    let h = open(pid, access | PROCESS_QUERY_LIMITED_INFORMATION).map_err(|e| {
        if e.code().0 as u32 == ERROR_INVALID_PARAMETER {
            ENDED.to_string()
        } else {
            sys::err(e)
        }
    })?;
    if start != 0 && creation_time(&h).map_err(sys::err)? != start {
        return Err(ENDED.into());
    }
    Ok(h)
}

fn resolve_identity(pid: u32, descriptions: &mut HashMap<String, Option<String>>) -> Identity {
    if pid == 0 || pid == 4 {
        return Identity { user: Some("SYSTEM".into()), ..Default::default() };
    }
    let Ok(h) = open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else { return Identity::default() };
    let path = image_path(&h);
    let description = path.as_ref().and_then(|p| {
        descriptions
            .entry(p.to_lowercase())
            .or_insert_with(|| crate::pro::drivers::file_version(p).map(|v| v.description).filter(|d| !d.is_empty()))
            .clone()
    });
    Identity { user: token_user(&h), path, command_line: command_line(&h), arch: architecture(&h), description }
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> NTSTATUS;
}

/// ProcessCommandLineInformation (60): a UNICODE_STRING followed by its buffer.
fn command_line(h: &OwnedHandle) -> Option<String> {
    let mut buf = vec![0u64; 4096];
    let mut ret = 0u32;
    // SAFETY: the buffer is 32 KiB; the API writes at most len bytes.
    let st = unsafe { NtQueryInformationProcess(h.0, 60, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut ret) };
    if st.0 < 0 {
        return None;
    }
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), buf.len() * 8) };
    let len = u16::from_ne_bytes([bytes[0], bytes[1]]) as usize;
    let ptr = usize::from_ne_bytes(bytes[8..16].try_into().ok()?);
    let base = bytes.as_ptr() as usize;
    if len == 0 || ptr < base || ptr + len > base + bytes.len() {
        return None;
    }
    let off = ptr - base;
    let u: Vec<u16> = bytes[off..off + len].as_chunks::<2>().0.iter().map(|c| u16::from_ne_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&u))
}

fn architecture(h: &OwnedHandle) -> &'static str {
    use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE;
    use windows::Win32::System::Threading::IsWow64Process2;
    let mut process = IMAGE_FILE_MACHINE(0);
    let mut native = IMAGE_FILE_MACHINE(0);
    // SAFETY: plain output parameters.
    if unsafe { IsWow64Process2(h.0, &mut process, Some(&mut native)) }.is_err() {
        return "";
    }
    let m = if process.0 == 0 { native.0 } else { process.0 };
    match m {
        0x8664 => "x64",
        0x014c => "x86",
        0xAA64 => "ARM64",
        0x01c4 => "ARM",
        _ => "",
    }
}

fn image_path(h: &OwnedHandle) -> Option<String> {
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()? };
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn token_user(h: &OwnedHandle) -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(h.0, TOKEN_QUERY, &mut token).ok()?;
        let token = OwnedHandle(token);
        let mut needed = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 || needed > 4096 {
            return None;
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr().cast()), needed, &mut needed).ok()?;
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut name = [0u16; 256];
        let mut domain = [0u16; 256];
        let (mut nl, mut dl) = (name.len() as u32, domain.len() as u32);
        let mut use_ = SID_NAME_USE::default();
        LookupAccountSidW(
            PCWSTR::null(),
            tu.User.Sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut nl,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut dl,
            &mut use_,
        )
        .ok()?;
        Some(String::from_utf16_lossy(&name[..nl as usize]))
    }
}

/// The action primitives [`super::actions::run`] drives.
pub(super) mod sys {
    use super::*;

    fn priority_class(p: Priority) -> PROCESS_CREATION_FLAGS {
        match p {
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::High => HIGH_PRIORITY_CLASS,
            Priority::Realtime => REALTIME_PRIORITY_CLASS,
        }
    }

    pub(super) fn err(e: windows::core::Error) -> String {
        if e.code().0 as u32 == 0x8007_0005 {
            "Access denied".into()
        } else {
            e.message().to_string()
        }
    }

    fn nt(st: NTSTATUS, what: &str) -> Result<(), String> {
        if st.0 < 0 {
            Err(format!("{what} failed: 0x{:08X}", st.0 as u32))
        } else {
            Ok(())
        }
    }

    // Every action opens the process with `open_same`: the row creation time (`start`) is compared
    // with the live process on the handle the action then uses.
    pub fn terminate(pid: u32, start: i64) -> Result<(), String> {
        let h = open_same(pid, start, PROCESS_TERMINATE)?;
        unsafe { TerminateProcess(h.0, 1).map_err(err) }
    }

    pub fn suspend(pid: u32, start: i64) -> Result<(), String> {
        let h = open_same(pid, start, PROCESS_SUSPEND_RESUME)?;
        nt(unsafe { NtSuspendProcess(h.0) }, "NtSuspendProcess")
    }

    pub fn resume(pid: u32, start: i64) -> Result<(), String> {
        let h = open_same(pid, start, PROCESS_SUSPEND_RESUME)?;
        nt(unsafe { NtResumeProcess(h.0) }, "NtResumeProcess")
    }

    pub fn set_priority(pid: u32, start: i64, p: Priority) -> Result<(), String> {
        let h = open_same(pid, start, PROCESS_SET_INFORMATION)?;
        unsafe { SetPriorityClass(h.0, priority_class(p)).map_err(err) }
    }

    /// Efficiency mode = EcoQoS (execution-speed throttling) + idle priority, as
    /// Windows Task Manager does; turning it off hands throttling back to the system.
    pub fn set_efficiency(pid: u32, start: i64, on: bool) -> Result<(), String> {
        let h = open_same(pid, start, PROCESS_SET_INFORMATION)?;
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
            StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
        };
        unsafe {
            SetProcessInformation(
                h.0,
                ProcessPowerThrottling,
                &state as *const _ as *const c_void,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
            .map_err(err)?;
            SetPriorityClass(h.0, if on { IDLE_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS }).map_err(err)
        }
    }

    /// Processor groups of the machine: affinity masks are per group, see [`super::super::affinity`].
    pub fn processor_groups() -> usize {
        // SAFETY: no arguments.
        unsafe { windows::Win32::System::Threading::GetActiveProcessorGroupCount() as usize }
    }

    /// The processors of the system (group 0) and the ones `pid` may use now.
    pub fn get(pid: u32, start: i64) -> Result<(Vec<usize>, Vec<usize>), String> {
        let h = open_same(pid, start, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let (mut process, mut system) = (0usize, 0usize);
        // SAFETY: both out pointers are valid for the call.
        unsafe { GetProcessAffinityMask(h.0, &mut process, &mut system).map_err(err)? };
        Ok((affinity::mask_to_cpus(system as u64), affinity::mask_to_cpus(process as u64)))
    }

    /// Binds `pid` to `cpus` (numbers within group 0).
    pub fn set(pid: u32, start: i64, cpus: &[usize]) -> Result<(), String> {
        let mask = affinity::cpus_to_mask(cpus).ok_or("a processor outside the first 64 cannot be set")?;
        let h = open_same(pid, start, PROCESS_SET_INFORMATION)?;
        // SAFETY: a handle with PROCESS_SET_INFORMATION and a plain mask.
        unsafe { SetProcessAffinityMask(h.0, mask as usize).map_err(err) }
    }

    pub fn open_location(path: &str) -> Result<(), String> {
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{path}"))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The shell's Properties dialog of the file at `path` (`ShellExecuteExW`, verb `properties`).
    pub fn properties(path: &str) -> Result<(), String> {
        use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW};
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
        let verb: Vec<u16> = "properties".encode_utf16().chain(std::iter::once(0)).collect();
        let file: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOW.0,
            ..Default::default()
        };
        // SAFETY: `info` and both strings outlive the call; the dialog belongs to the shell.
        unsafe { ShellExecuteExW(&mut info).map_err(err) }
    }

    /// Writes a full memory dump of `pid` to a new file in `dir` (`MiniDumpWriteDump`); a failed
    /// dump leaves no file behind.
    pub fn create_dump(pid: u32, start: i64, name: &str, dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::System::Diagnostics::Debug::{MiniDumpWithFullMemory, MiniDumpWriteDump};
        use windows::Win32::System::Threading::PROCESS_QUERY_INFORMATION;
        const PROCESS_VM_READ: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x10);
        let h = open_same(pid, start, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
        // `create_new` fails if the name is taken meanwhile: try the next one.
        let (file, path) = (0u32..)
            .find_map(|n| {
                let p = dir.join(super::super::dump::file_name(name, n));
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                    Ok(f) => Some(Ok((f, p))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e.to_string())),
                }
            })
            .expect("an unbounded range")?;
        // A process that is still starting cannot be dumped yet (see `NOT_READY_CODES`): retry those
        // errors, briefly, starting the file over each time. Any other error is final at once.
        let first = Instant::now();
        let done = loop {
            let done = {
                // DbgHelp is single threaded: "you must synchronize all concurrent calls"
                // (MiniDumpWriteDump, Remarks). The UI writes one dump at a time; this makes it hold
                // for every caller, tests included.
                let _one = DBGHELP.lock().unwrap_or_else(|e| e.into_inner());
                // SAFETY: both handles are open for the call; no callbacks or extra streams.
                unsafe { MiniDumpWriteDump(h.0, pid, HANDLE(file.as_raw_handle()), MiniDumpWithFullMemory, None, None, None) }
            };
            match done {
                Err(ref e) if not_ready(e) && first.elapsed() + DUMP_RETRY_EVERY < DUMP_RETRY_FOR => {
                    std::thread::sleep(DUMP_RETRY_EVERY);
                    // Start the file over: the failed try may have written part of it.
                    use std::io::Seek;
                    if (&file).rewind().is_err() || file.set_len(0).is_err() {
                        break done;
                    }
                }
                _ => break done,
            }
        };
        match done {
            Ok(()) => Ok(path),
            Err(e) => {
                let msg = if not_ready(&e) { DUMP_NOT_READY.to_string() } else { err(e) };
                drop(file);
                let _ = std::fs::remove_file(&path);
                Err(msg)
            }
        }
    }

    /// Serialises `MiniDumpWriteDump` within TMOP (see `create_dump`).
    static DBGHELP: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `MiniDumpWriteDump` errors that mean "this process cannot be read *yet*", not "it cannot be
    /// dumped". Right after a process is created its loader data (the module list in its PEB) does
    /// not exist yet or is still being built, and the dump walks that list first, whatever the dump
    /// type. Microsoft documents the same for `EnumProcessModules`: "If the module list in the target
    /// process is corrupted or not yet initialized, or if the module list changes during the function
    /// call [...] EnumProcessModules may fail". Each code below was seen on this machine, not guessed:
    const NOT_READY_CODES: [u32; 2] = [
        // ERROR_PARTIAL_COPY: every dump (normal or full memory) of a child created suspended, where
        // EnumProcessModules fails with the same code; ~8% of dumps taken right after a spawn on a
        // loaded machine; the v0.17.0 Release run.
        0x8007_012B,
        // ERROR_BAD_LENGTH: ~1% of dumps taken right after a spawn (the list is half built); the
        // v0.16.0 Release run (#140).
        0x8007_0018,
    ];

    fn not_ready(e: &windows::core::Error) -> bool {
        NOT_READY_CODES.contains(&(e.code().0 as u32))
    }

    /// How long a dump keeps retrying [`NOT_READY_CODES`], counted from the first try (a slow full
    /// dump that fails late is not repeated). Children started under full CPU load were dumpable
    /// after at most ~250 ms here, so this leaves a wide margin and still answers quickly for a
    /// process that never gets ready (one created suspended).
    const DUMP_RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(2);
    const DUMP_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(50);

    /// What the user reads when [`NOT_READY_CODES`] outlast the retries.
    pub(super) const DUMP_NOT_READY: &str =
        "Windows could not read the program yet: it is still starting (or loading modules). Try the dump again in a moment.";
}

#[cfg(test)]
pub(super) mod tests {
    use super::sys;
    use super::*;
    use std::process::{Child, Command, Stdio};

    /// A child of this test (the only process these tests ever act on) and its creation time.
    fn child() -> (Child, i64) {
        let c = Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
        let h = open(c.id(), PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let start = creation_time(&h).unwrap();
        (c, start)
    }

    /// Like [`child`], but returns only once the child has printed its first line: it is running
    /// its own code, so the loader has finished starting it (a dump does not race its start-up).
    /// Its output is drained until it ends, so it never blocks on a full pipe.
    pub(in crate::process) fn started_child() -> (Child, i64) {
        use std::io::BufRead;
        let mut c = Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::piped()).spawn().unwrap();
        let out = c.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut tx = Some(tx);
            for line in std::io::BufReader::new(out).split(b'\n') {
                let Ok(line) = line else { break };
                if !line.trim_ascii().is_empty() {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        });
        if rx.recv_timeout(std::time::Duration::from_secs(30)).is_err() {
            let _ = c.kill();
            let _ = c.wait();
            panic!("the child printed nothing within 30 s");
        }
        let h = open(c.id(), PROCESS_QUERY_LIMITED_INFORMATION).unwrap();
        let start = creation_time(&h).unwrap();
        (c, start)
    }

    fn alive(c: &mut Child) -> bool {
        c.try_wait().unwrap().is_none()
    }

    #[test]
    fn creation_time_matches_the_enumerator() {
        let (mut c, start) = child();
        let rows = Enumerator::new().collect(4, &HashMap::new()).unwrap().0;
        let _ = c.kill();
        let _ = c.wait();
        assert_eq!(rows.iter().find(|r| r.pid == c.id()).map(|r| r.create_time), Some(start));
    }

    #[test]
    fn a_stale_identity_is_refused_by_every_action() {
        let (mut c, start) = child();
        let (pid, stale) = (c.id(), start + 1);
        let dir = std::env::temp_dir().join(format!("tmop-stale-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let refused = |r: Result<(), String>| assert_eq!(r.unwrap_err(), ENDED);
        refused(sys::suspend(pid, stale));
        refused(sys::resume(pid, stale));
        refused(sys::set_priority(pid, stale, Priority::Idle));
        refused(sys::set_efficiency(pid, stale, true));
        refused(sys::set(pid, stale, &[0]));
        assert_eq!(sys::get(pid, stale).unwrap_err(), ENDED);
        assert_eq!(sys::create_dump(pid, stale, "ping.exe", &dir).unwrap_err(), ENDED);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "a refused dump leaves no file");
        refused(sys::terminate(pid, stale));
        assert!(alive(&mut c), "nothing was done to the live child");
        let _ = c.kill();
        let _ = c.wait();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_exited_process_is_refused_whether_or_not_its_pid_is_still_held() {
        let (mut c, start) = child();
        let pid = c.id();
        c.kill().unwrap();
        c.wait().unwrap();
        // Our handle to the child is still open: the pid cannot be reused yet, a wrong identity is refused.
        assert_eq!(sys::terminate(pid, start + 1).unwrap_err(), ENDED);
        drop(c);
        // Handle gone: the pid is free (ended), already another program's (its start differs, or it
        // cannot be opened), or the dead process lingers because something else holds it open (the
        // identity still matches, and an action on a dead process fails): an error in every case.
        assert!(sys::terminate(pid, start).is_err());
        assert!(sys::suspend(pid, start).is_err());
    }

    #[test]
    fn the_right_identity_acts_on_a_live_child() {
        let (mut c, start) = child();
        let pid = c.id();
        sys::suspend(pid, start).unwrap();
        sys::resume(pid, start).unwrap();
        sys::set_priority(pid, start, Priority::BelowNormal).unwrap();
        sys::set_efficiency(pid, start, true).unwrap();
        sys::set_efficiency(pid, start, false).unwrap();
        let (_all, current) = sys::get(pid, start).unwrap();
        sys::set(pid, start, &current).unwrap();
        assert!(alive(&mut c));
        sys::terminate(pid, start).unwrap();
        assert!(c.wait().is_ok());
    }

    #[test]
    fn an_unknown_start_keeps_the_unchecked_behaviour() {
        let (mut c, _) = child();
        sys::terminate(c.id(), 0).unwrap();
        assert!(c.wait().is_ok());
    }

    #[test]
    fn a_dump_with_the_right_identity_works() {
        let (mut c, start) = started_child();
        let dir = std::env::temp_dir().join(format!("tmop-stale-dump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = sys::create_dump(c.id(), start, "ping.exe", &dir);
        let _ = c.kill();
        let _ = c.wait();
        assert!(std::fs::metadata(r.unwrap()).unwrap().len() > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The extreme of "still starting": a child created suspended has run no code at all, so its
    /// loader data does not exist. The dump must answer within the retry time, either with a dump
    /// or with the plain message, and never leave a file behind.
    #[test]
    fn a_dump_of_a_process_that_is_still_starting_fails_plainly_and_leaves_no_file() {
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x4;
        let mut c = Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::null()).creation_flags(CREATE_SUSPENDED).spawn().unwrap();
        let start = creation_time(&open(c.id(), PROCESS_QUERY_LIMITED_INFORMATION).unwrap()).unwrap();
        let dir = std::env::temp_dir().join(format!("tmop-starting-dump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Instant::now();
        let r = sys::create_dump(c.id(), start, "ping.exe", &dir);
        let took = t.elapsed();
        let _ = sys::resume(c.id(), start);
        let _ = c.kill();
        let _ = c.wait();
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(took < std::time::Duration::from_secs(10), "the dump gave up in time ({took:?})");
        match r {
            Ok(p) => assert_eq!(left, vec![p], "a dump that worked is the only file"),
            Err(e) => {
                assert_eq!(e, sys::DUMP_NOT_READY);
                assert!(left.is_empty(), "a failed dump leaves no file: {left:?}");
            }
        }
    }
}
