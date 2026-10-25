//! Process table model, enumeration and actions.
//!
//! The model, the action dispatcher ([`actions::run`]) and the per-OS predicates live here. One
//! backend per target supplies the [`Enumerator`] and the action primitives (`sys::{terminate,
//! suspend, resume, set_priority, set_efficiency, open_location}`): `win.rs`
//! (`NtQuerySystemInformation`, `TerminateProcess`, `NtSuspendProcess`/`NtResumeProcess`,
//! `SetPriorityClass`, EcoQoS via `SetProcessInformation`) and `stub.rs` for targets without a
//! backend yet. A Linux or macOS backend plugs in beside `win.rs` with the same API.

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as imp;
#[cfg(not(windows))]
#[path = "stub.rs"]
mod imp;

pub use imp::Enumerator;
/// Moved to the metrics backend; kept here for old callers.
#[cfg(windows)]
pub use crate::metrics::query_processor_performance;

pub use crate::platform::caps::ProcCaps;
use crate::platform::{Os, OS};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex, OnceLock};

/// What the process backend of this build can do (columns, menu items).
pub fn caps() -> ProcCaps {
    crate::platform::Caps::get().proc
}

/// Process, thread and handle counts of one enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub processes: usize,
    pub threads: usize,
    pub handles: usize,
}

/// Processes this app put into efficiency mode, keyed by (pid, creation time).
static EFFICIENCY: LazyLock<Mutex<HashSet<(u32, i64)>>> = LazyLock::new(Default::default);

/// The (pid, creation time) of the processes this app put into efficiency mode; an
/// [`Enumerator`] sets [`ProcessRow::efficiency`] from it.
pub fn efficiency_marks() -> HashSet<(u32, i64)> {
    EFFICIENCY.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Forgets the efficiency marks of processes that are gone (`alive` is false).
pub fn retain_efficiency(alive: impl Fn(&(u32, i64)) -> bool) {
    if let Ok(mut g) = EFFICIENCY.lock() {
        g.retain(|k| alive(k));
    }
}

fn mark_efficiency(key: (u32, i64), on: bool) {
    if let Ok(mut g) = EFFICIENCY.lock() {
        if on {
            g.insert(key);
        } else {
            g.remove(&key);
        }
    }
}

/// What efficiency mode is called here: EcoQoS on Windows, `SCHED_IDLE` on Linux,
/// `PRIO_DARWIN_BG` on macOS.
pub const fn efficiency_word() -> &'static str {
    match OS {
        Os::Linux => "Idle priority",
        Os::MacOs => "Background priority",
        _ => "Efficiency mode",
    }
}

/// What [`ProcessRow::handles`] counts: "handles", or "open files" (file descriptors).
pub const fn handles_word() -> &'static str {
    match OS {
        Os::Windows => "handles",
        _ => "open files",
    }
}

/// The filter tab of the operating system's own processes: "Windows", else "System".
pub const fn os_group() -> &'static str {
    match OS {
        Os::Windows => "Windows",
        _ => "System",
    }
}

/// [`os_group`] in capitals, for row badges.
pub const fn os_badge() -> &'static str {
    match OS {
        Os::Windows => "WINDOWS",
        _ => "SYSTEM",
    }
}

/// Accounts of the Unix base system and common daemons (besides `root`, `_*` and `systemd-*`).
const UNIX_SYSTEM_ACCOUNTS: [&str; 18] =
    ["daemon", "bin", "sys", "nobody", "messagebus", "dbus", "polkitd", "avahi", "colord", "rtkit", "syslog", "kernoops", "whoopsie", "gdm", "sddm", "lightdm", "chrony", "uuidd"];

/// Runs as a built-in system account: SYSTEM, LOCAL SERVICE or NETWORK SERVICE on Windows (with
/// or without the domain); root and the daemon accounts on Linux and macOS.
pub fn is_system_account(user: Option<&str>) -> bool {
    system_account(OS, user)
}

fn system_account(os: Os, user: Option<&str>) -> bool {
    let Some(user) = user else { return false };
    match os {
        Os::Windows => {
            let name = user.rsplit('\\').next().unwrap_or(user).trim();
            ["SYSTEM", "LOCAL SERVICE", "NETWORK SERVICE"].iter().any(|a| name.eq_ignore_ascii_case(a))
        }
        _ => {
            let name = user.trim();
            name == "root" || name.starts_with('_') || name.starts_with("systemd-") || UNIX_SYSTEM_ACCOUNTS.contains(&name)
        }
    }
}

/// Folders of the operating system's own programs: `%SystemRoot%` (lower case) on Windows.
fn os_roots(os: Os) -> Vec<String> {
    match os {
        Os::Windows => vec![std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).trim_end_matches('\\').to_lowercase()],
        Os::Linux => ["/usr/lib/systemd", "/lib/systemd", "/usr/libexec", "/usr/sbin", "/sbin"].map(String::from).to_vec(),
        Os::MacOs => ["/System", "/usr/libexec", "/usr/sbin", "/sbin"].map(String::from).to_vec(),
        Os::Other => Vec::new(),
    }
}

/// Whether `path` lies under the folder `root`: case-insensitive below `\` on Windows (`root` in
/// lower case), exact below `/` elsewhere.
fn path_under(os: Os, path: &str, root: &str) -> bool {
    let (p, r) = (path.as_bytes(), root.as_bytes());
    if p.len() <= r.len() {
        return false;
    }
    match os {
        Os::Windows => p[..r.len()].eq_ignore_ascii_case(r) && p[r.len()] == b'\\',
        _ => &p[..r.len()] == r && p[r.len()] == b'/',
    }
}

/// A process of the operating system itself (the [`os_group`] tab): its image lies under the OS
/// folders; without a path also the Linux kernel threads (`kthreadd`, pid 2, and its children)
/// and macOS `kernel_task` (pid 0).
pub fn is_os_process(r: &ProcessRow) -> bool {
    static ROOTS: OnceLock<Vec<String>> = OnceLock::new();
    os_process(OS, r, ROOTS.get_or_init(|| os_roots(OS)))
}

fn os_process(os: Os, r: &ProcessRow, roots: &[String]) -> bool {
    match &r.path {
        Some(p) => roots.iter().any(|root| path_under(os, p, root)),
        None => match os {
            Os::Linux => r.pid == 2 || r.parent_pid == 2,
            Os::MacOs => r.pid == 0,
            _ => false,
        },
    }
}

/// A background service (the "Services" tab): a windowless process of a system account; off
/// Windows it must also have been started by init or launchd (parent pid 1).
pub fn is_service(r: &ProcessRow) -> bool {
    service(OS, r)
}

fn service(os: Os, r: &ProcessRow) -> bool {
    !r.has_window && system_account(os, r.user.as_deref()) && (os == Os::Windows || r.parent_pid == 1)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub user: Option<String>,
    pub path: Option<String>,
    pub threads: u32,
    pub handles: u32,
    pub session_id: u32,
    /// The OS's own scheduling number: the NT base priority on Windows, the nice value on Linux
    /// and macOS ([`Priority::from_row`] names it).
    pub base_priority: i32,
    /// A priority class the backend already knows; when None, [`Priority::from_row`] derives
    /// it from `base_priority`.
    pub priority: Option<Priority>,
    /// Private working set (bytes) - what Windows Task Manager shows as "Memory".
    pub private_working_set: u64,
    pub working_set: u64,
    /// Commit charge (private bytes).
    pub commit: u64,
    pub cpu_pct: f32,
    /// Total CPU time in 100 ns units (backends convert clock ticks or Mach time).
    pub cpu_time: u64,
    /// Read+write transfer rate (bytes/s); includes non-disk I/O.
    pub io_bps: f64,
    pub gpu_pct: f32,
    /// Creation FILETIME (100 ns since 1601), used to detect PID reuse. Other backends give a
    /// start stamp in the same unit.
    pub create_time: i64,
    /// Efficiency mode was turned on for this process by this app.
    pub efficiency: bool,
    /// Owns a visible top-level window ("Apps" group).
    pub has_window: bool,
    pub status: Status,
    /// Read / write transfer rates (bytes/s), including non-disk I/O.
    pub read_bps: f64,
    pub write_bps: f64,
    pub peak_working_set: u64,
    pub page_faults: u32,
    pub command_line: Option<String>,
    /// "x64", "x86", "ARM64" or empty when unknown.
    pub arch: &'static str,
    /// FileDescription of the image.
    pub description: Option<String>,
}

/// Status values: Running, Suspended, Not responding (Stopped/Exited are not shown; Unix
/// backends report a zombie as Running and a stopped process as Suspended).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    #[default]
    Running,
    Suspended,
    NotResponding,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Running => "Running",
            Status::Suspended => "Suspended",
            Status::NotResponding => "Not responding",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    Name,
    Pid,
    Status,
    User,
    Cpu,
    Memory,
    DiskRead,
    DiskWrite,
    Threads,
    Gpu,
    PeakMemory,
    CpuTime,
    PageFaults,
    Handles,
    Priority,
    Architecture,
    CommandLine,
    Disk,
    Session,
    ParentPid,
    Commit,
    Path,
    Description,
}

impl Column {
    /// Display order; the visible-by-default columns come first.
    pub const ALL: [Column; 23] = [
        Column::Name,
        Column::Pid,
        Column::Status,
        Column::User,
        Column::Cpu,
        Column::Memory,
        Column::DiskRead,
        Column::DiskWrite,
        Column::Threads,
        Column::Gpu,
        Column::PeakMemory,
        Column::CpuTime,
        Column::PageFaults,
        Column::Handles,
        Column::Priority,
        Column::Architecture,
        Column::CommandLine,
        Column::Disk,
        Column::Session,
        Column::ParentPid,
        Column::Commit,
        Column::Path,
        Column::Description,
    ];

    /// Stable id used in `processTableHiddenColumns` / `processTableColumnWidths`.
    pub fn id(self) -> &'static str {
        match self {
            Column::Name => "name",
            Column::Pid => "pid",
            Column::Status => "status",
            Column::User => "user",
            Column::Cpu => "cpu",
            Column::Memory => "memory",
            Column::DiskRead => "diskRead",
            Column::DiskWrite => "diskWrite",
            Column::PeakMemory => "peakMemory",
            Column::PageFaults => "pageFaults",
            Column::Architecture => "architecture",
            Column::CommandLine => "commandLine",
            Column::Description => "description",
            Column::Disk => "disk",
            Column::Gpu => "gpu",
            Column::Threads => "threads",
            Column::Handles => "handles",
            Column::Session => "session",
            Column::Priority => "basePriority",
            Column::ParentPid => "parentPid",
            Column::CpuTime => "cpuTime",
            Column::Commit => "commitSize",
            Column::Path => "imagePath",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Pid => "PID",
            Column::Status => "Status",
            Column::User => "User name",
            Column::Cpu => "CPU",
            Column::Memory => "Memory",
            Column::DiskRead => "Disk read",
            Column::DiskWrite => "Disk write",
            Column::PeakMemory => "Peak memory",
            Column::PageFaults => "Page faults",
            Column::Architecture => "Architecture",
            Column::CommandLine => "Command",
            Column::Description => "Description",
            Column::Disk => "Disk",
            Column::Gpu => "GPU",
            Column::Threads => "Threads",
            Column::Handles => "Handles",
            Column::Session => "Session",
            Column::Priority => "Priority",
            Column::ParentPid => "Parent PID",
            Column::CpuTime => "CPU time",
            Column::Commit => "Commit size",
            Column::Path => "Image path",
        }
    }

    pub fn default_width(self) -> f32 {
        match self {
            Column::Name => 160.0,
            Column::Path | Column::CommandLine => 300.0,
            Column::Description => 220.0,
            Column::User => 82.0,
            Column::Pid => 52.0,
            Column::Status | Column::Architecture => 70.0,
            Column::Cpu | Column::Gpu | Column::Threads | Column::Handles | Column::Priority | Column::Session => 56.0,
            Column::Memory | Column::DiskRead | Column::DiskWrite | Column::Disk => 76.0,
            _ => 84.0,
        }
    }

    /// Hidden unless the user enables them.
    pub fn hidden_by_default(self) -> bool {
        matches!(self, Column::Disk | Column::Session | Column::ParentPid | Column::Commit | Column::Path | Column::Description)
    }

    pub fn numeric(self) -> bool {
        !matches!(self, Column::Name | Column::User | Column::Path | Column::Status | Column::Architecture | Column::CommandLine | Column::Description)
    }
}

pub fn compare(a: &ProcessRow, b: &ProcessRow, col: Column) -> Ordering {
    let f = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    match col {
        Column::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)),
        Column::Pid => a.pid.cmp(&b.pid),
        Column::Status => a.status.cmp(&b.status),
        Column::User => a.user.cmp(&b.user),
        Column::DiskRead => f(a.read_bps, b.read_bps),
        Column::DiskWrite => f(a.write_bps, b.write_bps),
        Column::PeakMemory => a.peak_working_set.cmp(&b.peak_working_set),
        Column::PageFaults => a.page_faults.cmp(&b.page_faults),
        Column::Architecture => a.arch.cmp(b.arch),
        Column::CommandLine => a.command_line.cmp(&b.command_line),
        Column::Description => a.description.cmp(&b.description),
        Column::Cpu => f(a.cpu_pct as f64, b.cpu_pct as f64),
        Column::Memory => a.private_working_set.cmp(&b.private_working_set),
        Column::Disk => f(a.io_bps, b.io_bps),
        Column::Gpu => f(a.gpu_pct as f64, b.gpu_pct as f64),
        Column::Threads => a.threads.cmp(&b.threads),
        Column::Handles => a.handles.cmp(&b.handles),
        Column::Session => a.session_id.cmp(&b.session_id),
        Column::Priority => a.base_priority.cmp(&b.base_priority),
        Column::ParentPid => a.parent_pid.cmp(&b.parent_pid),
        Column::CpuTime => a.cpu_time.cmp(&b.cpu_time),
        Column::Commit => a.commit.cmp(&b.commit),
        Column::Path => a.path.cmp(&b.path),
    }
}

/// `pid` and all descendants (children first), using parent links validated by
/// creation time (a child must have been created after its parent).
pub fn tree_of(rows: &[ProcessRow], pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    fn walk(rows: &[ProcessRow], pid: u32, out: &mut Vec<u32>, depth: usize) {
        if depth > 64 || out.contains(&pid) {
            return;
        }
        let Some(parent) = rows.iter().find(|r| r.pid == pid) else { return };
        for child in rows.iter().filter(|r| r.parent_pid == pid && r.pid != pid && r.create_time >= parent.create_time) {
            walk(rows, child.pid, out, depth + 1);
        }
        out.push(pid);
    }
    walk(rows, pid, &mut out, 0);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    pub const ALL: [Priority; 6] =
        [Priority::Realtime, Priority::High, Priority::AboveNormal, Priority::Normal, Priority::BelowNormal, Priority::Idle];
    pub fn label(self) -> &'static str {
        match self {
            Priority::Idle => "Low",
            Priority::BelowNormal => "Below normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above normal",
            Priority::High => "High",
            Priority::Realtime => "Realtime",
        }
    }

    /// The nice value a Unix backend sets for this class. Realtime has none; -20 comes closest.
    pub const fn nice(self) -> i32 {
        match self {
            Priority::Idle => 19,
            Priority::BelowNormal => 10,
            Priority::Normal => 0,
            Priority::AboveNormal => -5,
            Priority::High => -10,
            Priority::Realtime => -20,
        }
    }

    /// The class of `r` ([`ProcessRow::base_priority`]), when it is exactly one of the classes.
    pub fn from_row(r: &ProcessRow) -> Option<Priority> {
        r.priority.or_else(|| Self::from_value(OS, r.base_priority))
    }

    fn from_value(os: Os, v: i32) -> Option<Priority> {
        match os {
            Os::Windows => match v {
                4 => Some(Priority::Idle),
                6 => Some(Priority::BelowNormal),
                8 => Some(Priority::Normal),
                10 => Some(Priority::AboveNormal),
                13 => Some(Priority::High),
                24 => Some(Priority::Realtime),
                _ => None,
            },
            // A nice of -20 is not Realtime.
            _ => Priority::ALL.into_iter().filter(|p| *p != Priority::Realtime).find(|p| p.nice() == v),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    EndTask,
    EndTree,
    Suspend,
    Resume,
    SetPriority(Priority),
    Efficiency(bool),
    OpenFileLocation,
}

impl Action {
    /// Whether a backend with `caps` can do this.
    pub fn supported(self, caps: &ProcCaps) -> bool {
        match self {
            Action::Suspend | Action::Resume => caps.suspend,
            Action::Efficiency(_) => caps.efficiency,
            Action::SetPriority(Priority::Realtime) => caps.realtime,
            _ => true,
        }
    }
}

impl Column {
    /// Whether a backend with `caps` fills this column (the others are not offered).
    pub fn supported(self, caps: &ProcCaps) -> bool {
        match self {
            Column::Handles => caps.handles,
            Column::Session => caps.session,
            Column::Architecture => caps.arch,
            Column::Gpu => caps.gpu,
            _ => true,
        }
    }
}

pub mod actions {
    use super::{caps, imp, mark_efficiency, tree_of, Action, Priority, ProcCaps, ProcessRow};

    /// The primitives [`run`] drives: the backend's `sys` functions, or a fake in tests.
    pub(crate) trait Sys {
        fn terminate(&self, pid: u32) -> Result<(), String>;
        fn suspend(&self, pid: u32) -> Result<(), String>;
        fn resume(&self, pid: u32) -> Result<(), String>;
        fn set_priority(&self, pid: u32, p: Priority) -> Result<(), String>;
        fn set_efficiency(&self, pid: u32, on: bool) -> Result<(), String>;
        fn open_location(&self, path: &str) -> Result<(), String>;
    }

    struct Native;

    impl Sys for Native {
        fn terminate(&self, pid: u32) -> Result<(), String> {
            imp::sys::terminate(pid)
        }
        fn suspend(&self, pid: u32) -> Result<(), String> {
            imp::sys::suspend(pid)
        }
        fn resume(&self, pid: u32) -> Result<(), String> {
            imp::sys::resume(pid)
        }
        fn set_priority(&self, pid: u32, p: Priority) -> Result<(), String> {
            imp::sys::set_priority(pid, p)
        }
        fn set_efficiency(&self, pid: u32, on: bool) -> Result<(), String> {
            imp::sys::set_efficiency(pid, on)
        }
        fn open_location(&self, path: &str) -> Result<(), String> {
            imp::sys::open_location(path)
        }
    }

    /// Runs `action` against `row`; `rows` is needed for process trees.
    pub fn run(action: Action, row: &ProcessRow, rows: &[ProcessRow]) -> Result<String, String> {
        run_with(&Native, &caps(), action, row, rows)
    }

    pub(crate) fn run_with(sys: &impl Sys, caps: &ProcCaps, action: Action, row: &ProcessRow, rows: &[ProcessRow]) -> Result<String, String> {
        if !action.supported(caps) {
            return Err(format!("not available on {}", crate::platform::os_name()));
        }
        let end = |pid: u32| if pid == std::process::id() { Err("refusing to terminate TMOP itself".to_string()) } else { sys.terminate(pid) };
        match action {
            Action::EndTask => end(row.pid).map(|_| format!("Ended {} ({})", row.name, row.pid)),
            Action::EndTree => {
                let pids = tree_of(rows, row.pid);
                let failed: Vec<String> = pids.iter().filter_map(|p| end(*p).err().map(|e| format!("{p}: {e}"))).collect();
                if failed.is_empty() {
                    Ok(format!("Ended {} process(es)", pids.len()))
                } else {
                    Err(failed.join("; "))
                }
            }
            Action::Suspend => sys.suspend(row.pid).map(|_| format!("Suspended {}", row.name)),
            Action::Resume => sys.resume(row.pid).map(|_| format!("Resumed {}", row.name)),
            Action::SetPriority(p) => sys.set_priority(row.pid, p).map(|_| format!("{} priority: {}", row.name, p.label())),
            Action::Efficiency(on) => {
                sys.set_efficiency(row.pid, on)?;
                mark_efficiency((row.pid, row.create_time), on);
                Ok(format!("{} {} for {}", super::efficiency_word(), if on { "on" } else { "off" }, row.name))
            }
            Action::OpenFileLocation => match &row.path {
                Some(p) => sys.open_location(p).map(|_| String::new()),
                None => Err("Image path unavailable".into()),
            },
        }
    }
}

pub fn format_bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn format_rate(bps: f64) -> String {
    format!("{}/s", format_bytes(bps))
}

/// `%llu:%02llu:%02llu` style CPU time.
pub fn format_cpu_time(t100ns: u64) -> String {
    let s = t100ns / 10_000_000;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, ct: i64) -> ProcessRow {
        ProcessRow { pid, parent_pid: ppid, create_time: ct, name: format!("p{pid}"), ..Default::default() }
    }

    #[test]
    fn tree_walk_respects_creation_order() {
        let rows = vec![row(1, 0, 10), row(2, 1, 20), row(3, 2, 30), row(4, 1, 5) /* reused pid, older */];
        assert_eq!(tree_of(&rows, 1), vec![3, 2, 1]);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1536.0), "1.5 KB");
        assert_eq!(format_cpu_time(36_610 * 10_000_000), "10:10:10");
    }

    /// Records the primitives it is asked for; pids in `fail` are refused.
    #[derive(Default)]
    struct FakeSys {
        calls: std::cell::RefCell<Vec<String>>,
        fail: Vec<u32>,
    }

    impl FakeSys {
        fn call(&self, what: String, pid: u32) -> Result<(), String> {
            self.calls.borrow_mut().push(what);
            if self.fail.contains(&pid) {
                Err("Access denied".into())
            } else {
                Ok(())
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl actions::Sys for FakeSys {
        fn terminate(&self, pid: u32) -> Result<(), String> {
            self.call(format!("terminate {pid}"), pid)
        }
        fn suspend(&self, pid: u32) -> Result<(), String> {
            self.call(format!("suspend {pid}"), pid)
        }
        fn resume(&self, pid: u32) -> Result<(), String> {
            self.call(format!("resume {pid}"), pid)
        }
        fn set_priority(&self, pid: u32, p: Priority) -> Result<(), String> {
            self.call(format!("priority {pid} {}", p.label()), pid)
        }
        fn set_efficiency(&self, pid: u32, on: bool) -> Result<(), String> {
            self.call(format!("efficiency {pid} {on}"), pid)
        }
        fn open_location(&self, path: &str) -> Result<(), String> {
            self.call(format!("open {path}"), 0)
        }
    }

    const ALL_CAPS: ProcCaps = crate::platform::Caps::WINDOWS.proc;

    #[test]
    fn dispatcher_drives_the_backend() {
        use actions::run_with;
        let me = std::process::id();
        // A tree with TMOP inside: everything else ends, children first; TMOP and a refused pid fail.
        let rows = vec![row(900_001, 0, 10), row(900_002, 900_001, 20), row(me, 900_001, 30), row(900_003, 900_002, 40)];
        let sys = FakeSys { fail: vec![900_003], ..Default::default() };
        let err = run_with(&sys, &ALL_CAPS, Action::EndTree, &rows[0], &rows).unwrap_err();
        assert_eq!(sys.calls(), ["terminate 900003", "terminate 900002", "terminate 900001"]);
        assert!(err.contains("900003: Access denied") && err.contains(&format!("{me}: refusing")), "{err}");
        let sys = FakeSys::default();
        assert_eq!(run_with(&sys, &ALL_CAPS, Action::EndTask, &rows[1], &rows).unwrap(), "Ended p900002 (900002)");
        assert!(run_with(&sys, &ALL_CAPS, Action::EndTask, &rows[2], &rows).is_err());
        assert_eq!(run_with(&sys, &ALL_CAPS, Action::SetPriority(Priority::High), &rows[1], &rows).unwrap(), "p900002 priority: High");
        assert_eq!(run_with(&sys, &ALL_CAPS, Action::Suspend, &rows[1], &rows).unwrap(), "Suspended p900002");
        assert_eq!(run_with(&sys, &ALL_CAPS, Action::OpenFileLocation, &rows[1], &rows).unwrap_err(), "Image path unavailable");
        let with_path = ProcessRow { path: Some("/x/y".into()), ..rows[1].clone() };
        assert!(run_with(&sys, &ALL_CAPS, Action::OpenFileLocation, &with_path, &rows).is_ok());
        assert_eq!(sys.calls(), ["terminate 900002", "priority 900002 High", "suspend 900002", "open /x/y"]);

        // Efficiency mode is remembered per (pid, creation time) until turned off.
        let key = (rows[1].pid, rows[1].create_time);
        assert!(run_with(&sys, &ALL_CAPS, Action::Efficiency(true), &rows[1], &rows).unwrap().ends_with(" on for p900002"));
        assert!(efficiency_marks().contains(&key));
        run_with(&sys, &ALL_CAPS, Action::Efficiency(false), &rows[1], &rows).unwrap();
        assert!(!efficiency_marks().contains(&key));
        // A failed call marks nothing.
        let refusing = FakeSys { fail: vec![900_002], ..Default::default() };
        assert!(run_with(&refusing, &ALL_CAPS, Action::Efficiency(true), &rows[1], &rows).is_err());
        assert!(!efficiency_marks().contains(&key));

        // What the backend cannot do is refused without asking it.
        let none = crate::platform::Caps::OTHER.proc;
        let sys = FakeSys::default();
        for a in [Action::Suspend, Action::Resume, Action::Efficiency(true), Action::SetPriority(Priority::Realtime)] {
            assert!(run_with(&sys, &none, a, &rows[1], &rows).unwrap_err().starts_with("not available on"));
        }
        assert!(sys.calls().is_empty());
        assert!(Action::SetPriority(Priority::Idle).supported(&none) && Action::EndTree.supported(&none));
    }

    #[test]
    fn priority_classes_per_os() {
        assert_eq!(Priority::from_value(Os::Windows, 8), Some(Priority::Normal));
        assert_eq!(Priority::from_value(Os::Windows, 24), Some(Priority::Realtime));
        assert_eq!(Priority::from_value(Os::Windows, 7), None);
        assert_eq!(Priority::from_value(Os::Linux, 19), Some(Priority::Idle));
        assert_eq!(Priority::from_value(Os::MacOs, -10), Some(Priority::High));
        assert_eq!(Priority::from_value(Os::Linux, -20), None);
        assert_eq!(Priority::from_value(Os::Linux, 8), None);
        for p in Priority::ALL.into_iter().filter(|p| *p != Priority::Realtime) {
            assert_eq!(Priority::from_value(Os::Linux, p.nice()), Some(p));
        }
    }

    #[test]
    fn system_accounts_per_os() {
        let w = |u| system_account(Os::Windows, Some(u));
        assert!(w("SYSTEM") && w(r"NT AUTHORITY\Local Service") && w("network service"));
        assert!(!w("alex") && !w("root") && !system_account(Os::Windows, None));
        let l = |u| system_account(Os::Linux, Some(u));
        assert!(l("root") && l("systemd-resolve") && l("messagebus") && l("_windowserver"));
        assert!(!l("alex") && !l("SYSTEM") && !l("rooted"));
    }

    #[test]
    fn os_processes_per_os() {
        let at = |path: Option<&str>| ProcessRow { pid: 50, parent_pid: 1, path: path.map(String::from), ..Default::default() };
        let win = [r"c:\windows".to_string()];
        assert!(os_process(Os::Windows, &at(Some(r"C:\Windows\System32\svchost.exe")), &win));
        assert!(!os_process(Os::Windows, &at(Some(r"C:\Windows2\x.exe")), &win) && !os_process(Os::Windows, &at(Some(r"C:\Program Files\x.exe")), &win));
        assert!(!os_process(Os::Windows, &at(None), &win));
        let linux = os_roots(Os::Linux);
        assert!(os_process(Os::Linux, &at(Some("/usr/lib/systemd/systemd-journald")), &linux));
        assert!(!os_process(Os::Linux, &at(Some("/usr/bin/firefox")), &linux) && !os_process(Os::Linux, &at(Some("/usr/sbinx/y")), &linux));
        assert!(!os_process(Os::Linux, &at(Some("/USR/SBIN/sshd")), &linux));
        let kworker = ProcessRow { pid: 80, parent_pid: 2, ..Default::default() };
        assert!(os_process(Os::Linux, &kworker, &linux) && !os_process(Os::Linux, &at(None), &linux));
        let mac = os_roots(Os::MacOs);
        assert!(os_process(Os::MacOs, &at(Some("/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder")), &mac));
        assert!(os_process(Os::MacOs, &ProcessRow::default(), &mac) && !os_process(Os::MacOs, &at(Some("/Applications/X.app/x")), &mac));
    }

    #[test]
    fn services_per_os() {
        let svc = ProcessRow { pid: 700, parent_pid: 600, user: Some("SYSTEM".into()), ..Default::default() };
        assert!(service(Os::Windows, &svc));
        assert!(!service(Os::Windows, &ProcessRow { has_window: true, ..svc.clone() }));
        let daemon = ProcessRow { pid: 700, parent_pid: 1, user: Some("root".into()), ..Default::default() };
        assert!(service(Os::Linux, &daemon) && service(Os::MacOs, &daemon));
        assert!(!service(Os::Linux, &ProcessRow { parent_pid: 650, ..daemon.clone() }));
        assert!(!service(Os::Linux, &ProcessRow { user: Some("alex".into()), ..daemon.clone() }));
    }

    #[test]
    fn columns_follow_caps() {
        let none = crate::platform::Caps::OTHER.proc;
        assert!(Column::ALL.iter().all(|c| c.supported(&ALL_CAPS)));
        assert!(!Column::Handles.supported(&none) && !Column::Gpu.supported(&none) && Column::Cpu.supported(&none));
    }
}
