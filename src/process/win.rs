//! Windows process enumeration (NtQuerySystemInformation) and actions.

use super::{Action, Priority, ProcessRow};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::time::Instant;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS};
use windows::Win32::Security::{GetTokenInformation, LookupAccountSidW, TokenUser, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, ProcessPowerThrottling, QueryFullProcessImageNameW, SetPriorityClass,
    SetProcessInformation, TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS,
    HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_ACCESS_RIGHTS, PROCESS_CREATION_FLAGS,
    PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, REALTIME_PRIORITY_CLASS,
};

pub(crate) const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
pub(crate) const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;
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
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
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

/// Raw SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION array (processor group 0).
pub fn query_processor_performance(size: usize) -> Result<Vec<u8>, String> {
    query_system_information(SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS, size)
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

/// Processes this app put into efficiency mode, keyed by (pid, creation time).
static EFFICIENCY: std::sync::LazyLock<std::sync::Mutex<HashSet<(u32, i64)>>> =
    std::sync::LazyLock::new(Default::default);

impl Default for Enumerator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Totals {
    pub processes: usize,
    pub threads: usize,
    pub handles: usize,
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
        let efficiency = EFFICIENCY.lock().map(|g| g.clone()).unwrap_or_default();
        let windowed = windowed_pids();
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
            let (cpu_pct, read_bps, write_bps) = match self.prev.get(&pid) {
                Some(p) if p.create_time == spi.create_time && elapsed_100ns > 0.0 && pid != 0 => (
                    ((cpu_time.saturating_sub(p.cpu_time)) as f64 / elapsed_100ns / logical_cpus.max(1) as f64 * 100.0) as f32,
                    read.saturating_sub(p.read) as f64 / elapsed_s.max(1e-3),
                    write.saturating_sub(p.write) as f64 / elapsed_s.max(1e-3),
                ),
                _ => (0.0, 0.0, 0.0),
            };
            next_prev.insert(pid, Prev { create_time: spi.create_time, cpu_time, read, write });
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
                peak_working_set: spi.peak_working_set_size as u64,
                page_faults: spi.page_fault_count,
                threads: spi.number_of_threads,
                handles: spi.handle_count,
                session_id: spi.session_id,
                base_priority: spi.base_priority,
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
        if let Ok(mut g) = EFFICIENCY.lock() {
            g.retain(|k| next_prev.get(&k.0).map(|p| p.create_time == k.1).unwrap_or(false));
        }
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
            PWSTR(name.as_mut_ptr()),
            &mut nl,
            PWSTR(domain.as_mut_ptr()),
            &mut dl,
            &mut use_,
        )
        .ok()?;
        Some(String::from_utf16_lossy(&name[..nl as usize]))
    }
}

pub mod actions {
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

    fn err(e: windows::core::Error) -> String {
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

    pub fn terminate(pid: u32) -> Result<(), String> {
        if pid == std::process::id() {
            return Err("refusing to terminate TMOP itself".into());
        }
        let h = open(pid, PROCESS_TERMINATE).map_err(err)?;
        unsafe { TerminateProcess(h.0, 1).map_err(err) }
    }

    pub fn suspend(pid: u32) -> Result<(), String> {
        let h = open(pid, PROCESS_SUSPEND_RESUME).map_err(err)?;
        nt(unsafe { NtSuspendProcess(h.0) }, "NtSuspendProcess")
    }

    pub fn resume(pid: u32) -> Result<(), String> {
        let h = open(pid, PROCESS_SUSPEND_RESUME).map_err(err)?;
        nt(unsafe { NtResumeProcess(h.0) }, "NtResumeProcess")
    }

    pub fn set_priority(pid: u32, p: Priority) -> Result<(), String> {
        let h = open(pid, PROCESS_SET_INFORMATION).map_err(err)?;
        unsafe { SetPriorityClass(h.0, priority_class(p)).map_err(err) }
    }

    /// Efficiency mode = EcoQoS (execution-speed throttling) + idle priority, as
    /// Windows Task Manager does; turning it off hands throttling back to the system.
    pub fn set_efficiency(pid: u32, on: bool) -> Result<(), String> {
        let h = open(pid, PROCESS_SET_INFORMATION).map_err(err)?;
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

    pub fn open_file_location(path: &str) -> Result<(), String> {
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{path}"))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Runs `action` against `row`; `rows` is needed for process trees.
    pub fn run(action: Action, row: &ProcessRow, rows: &[ProcessRow]) -> Result<String, String> {
        match action {
            Action::EndTask => terminate(row.pid).map(|_| format!("Ended {} ({})", row.name, row.pid)),
            Action::EndTree => {
                let pids = super::super::tree_of(rows, row.pid);
                let mut failed = Vec::new();
                for p in &pids {
                    if let Err(e) = terminate(*p) {
                        failed.push(format!("{p}: {e}"));
                    }
                }
                if failed.is_empty() {
                    Ok(format!("Ended {} process(es)", pids.len()))
                } else {
                    Err(failed.join("; "))
                }
            }
            Action::Suspend => suspend(row.pid).map(|_| format!("Suspended {}", row.name)),
            Action::Resume => resume(row.pid).map(|_| format!("Resumed {}", row.name)),
            Action::SetPriority(p) => set_priority(row.pid, p).map(|_| format!("{} priority: {}", row.name, p.label())),
            Action::Efficiency(on) => {
                set_efficiency(row.pid, on)?;
                let key = (row.pid, row.create_time);
                if let Ok(mut g) = EFFICIENCY.lock() {
                    if on {
                        g.insert(key);
                    } else {
                        g.remove(&key);
                    }
                }
                Ok(format!("Efficiency mode {} for {}", if on { "on" } else { "off" }, row.name))
            }
            Action::OpenFileLocation => match &row.path {
                Some(p) => open_file_location(p).map(|_| String::new()),
                None => Err("Image path unavailable".into()),
            },
        }
    }
}
