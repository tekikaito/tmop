//! Process table model, enumeration and actions.
//!
//! Enumeration uses `NtQuerySystemInformation(SystemProcessInformation)`. Actions:
//! terminate (`TerminateProcess`), suspend/resume (`NtSuspendProcess`/`NtResumeProcess`),
//! priority (`SetPriorityClass`) and efficiency mode (EcoQoS via `SetProcessInformation`).

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{actions, query_processor_performance, Enumerator};

use std::cmp::Ordering;

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
    pub base_priority: i32,
    /// Private working set (bytes) - what Windows Task Manager shows as "Memory".
    pub private_working_set: u64,
    pub working_set: u64,
    /// Commit charge (private bytes).
    pub commit: u64,
    pub cpu_pct: f32,
    /// Total CPU time in 100 ns units.
    pub cpu_time: u64,
    /// Read+write transfer rate (bytes/s); includes non-disk I/O.
    pub io_bps: f64,
    pub gpu_pct: f32,
    /// Creation FILETIME (100 ns since 1601), used to detect PID reuse.
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

/// Status values: Running, Suspended, Not responding (Stopped/Exited are not shown).
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
}
