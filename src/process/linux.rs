//! Linux process enumeration from `/proc` and the actions: signals through a pidfd checked
//! against the start time, nice for every thread, `SCHED_IDLE` for efficiency mode, and the
//! `pkexec` retry. Window owners come from the X11 client list, else (Wayland, no X server) from
//! a `.desktop` heuristic. The parsers live in `procfs.rs`.

use super::procfs::{self, DesktopApp, Elevated, Stat};
use super::{Action, Priority, ProcessRow, Status, Totals, ACCESS_DENIED};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Open files are counted on every this many enumerations (and when a process first shows up).
const HEAVY_EVERY: u64 = 5;
/// How long the `.desktop` index is used before it is read again.
const DESKTOP_TTL: Duration = Duration::from_secs(120);

fn clk_tck() -> u64 {
    static TCK: OnceLock<u64> = OnceLock::new();
    // SAFETY: sysconf has no preconditions.
    *TCK.get_or_init(|| u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok().filter(|t| *t > 0).unwrap_or(100))
}

fn boot_secs() -> u64 {
    static BOOT: OnceLock<u64> = OnceLock::new();
    *BOOT.get_or_init(|| std::fs::read_to_string("/proc/stat").ok().and_then(|s| procfs::parse_btime(&s)).unwrap_or(0))
}

fn proc_file(pid: u32, name: &str) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/{name}"))
}

fn read(pid: u32, name: &str) -> Option<String> {
    std::fs::read_to_string(proc_file(pid, name)).ok()
}

fn read_stat(pid: u32) -> Option<Stat> {
    read(pid, "stat").as_deref().and_then(procfs::parse_stat)
}

/// The start of a process as [`ProcessRow::create_time`].
fn stamp(st: &Stat) -> i64 {
    procfs::create_stamp(boot_secs(), st.start_ticks, clk_tck())
}

/// The [`ProcessRow::create_time`] of whatever process has `pid` now.
pub(crate) fn start_of(pid: u32) -> Option<i64> {
    read_stat(pid).map(|s| stamp(&s))
}

/// The thread ids of `pid`.
fn tids(pid: u32) -> Vec<u32> {
    let mut ids: Vec<u32> = std::fs::read_dir(proc_file(pid, "task")).map(|d| d.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect()).unwrap_or_default();
    if ids.is_empty() {
        ids.push(pid);
    }
    ids
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The account name of `uid` (`getpwuid_r`), else the number.
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

/// The architecture of the image of `pid` from its ELF header.
fn image_arch(pid: u32) -> &'static str {
    use std::io::Read;
    let mut header = [0u8; 20];
    match std::fs::File::open(proc_file(pid, "exe")).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => procfs::elf_arch(&header),
        Err(_) => "",
    }
}

/// Facts about a process that do not change while it runs.
#[derive(Clone, Default)]
struct Identity {
    name: String,
    path: Option<String>,
    command_line: Option<String>,
    arch: &'static str,
    /// The key into the `.desktop` index.
    app: String,
}

fn identity(pid: u32, st: &Stat, archs: &mut HashMap<String, &'static str>) -> Identity {
    if st.kernel_thread() {
        return Identity { name: st.comm.clone(), app: st.comm.to_lowercase(), ..Default::default() };
    }
    // Another user's image link is unreadable without root: argv[0] when it is a path.
    let exe = std::fs::read_link(proc_file(pid, "exe")).ok().map(|p| procfs::clean_exe(&p.to_string_lossy()).to_string());
    let args = std::fs::read(proc_file(pid, "cmdline")).map(|raw| procfs::cmdline_args(&raw)).unwrap_or_default();
    let argv0 = args.first().map(String::as_str);
    let arch = exe.as_ref().map_or("", |p| *archs.entry(p.clone()).or_insert_with(|| image_arch(pid)));
    let path = exe.clone().or_else(|| argv0.filter(|a| a.starts_with('/') && !a.contains(' ')).map(String::from));
    Identity {
        name: procfs::display_name(&st.comm, exe.as_deref(), &args),
        app: procfs::app_key(path.as_deref(), &st.comm),
        path,
        command_line: procfs::join_args(&args),
        arch,
    }
}

fn count_files(pid: u32) -> u32 {
    std::fs::read_dir(proc_file(pid, "fd")).map(|d| d.count() as u32).unwrap_or(0)
}

/// The `.desktop` folders, highest priority first: `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`, then the
/// Flatpak and Snap exports.
fn desktop_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let home = env("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share"))).into_iter().collect();
    let system = env("XDG_DATA_DIRS").map(|v| v.to_string_lossy().into_owned()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| d.starts_with('/')).map(PathBuf::from));
    dirs.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
    dirs.extend(["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop"].map(PathBuf::from));
    let mut seen = HashSet::new();
    dirs.into_iter().map(|d| d.join("applications")).filter(|d| seen.insert(d.clone())).collect()
}

fn load_desktop() -> HashMap<String, DesktopApp> {
    let mut entries = Vec::new();
    for dir in desktop_dirs() {
        let Ok(list) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = list.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect();
        files.sort();
        entries.extend(files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).filter_map(|s| procfs::parse_desktop(&s)));
    }
    procfs::index_desktop(entries)
}

#[derive(Clone, Copy)]
struct Prev {
    start: u64,
    cpu_time: u64,
    read: u64,
    write: u64,
}

pub struct Enumerator {
    prev: HashMap<u32, Prev>,
    last: Option<Instant>,
    /// Keyed by (pid, start ticks).
    identities: HashMap<(u32, u64), Identity>,
    files: HashMap<(u32, u64), u32>,
    users: HashMap<u32, String>,
    archs: HashMap<String, &'static str>,
    desktop: HashMap<String, DesktopApp>,
    desktop_at: Option<Instant>,
    x11: x11::Windows,
    collections: u64,
}

impl Default for Enumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Enumerator {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            last: None,
            identities: HashMap::new(),
            files: HashMap::new(),
            users: HashMap::new(),
            archs: HashMap::new(),
            desktop: HashMap::new(),
            desktop_at: None,
            x11: x11::Windows::default(),
            collections: 0,
        }
    }

    pub fn collect(&mut self, logical_cpus: usize, gpu_by_pid: &HashMap<u32, f32>) -> Result<(Vec<ProcessRow>, Totals), String> {
        let dir = std::fs::read_dir("/proc").map_err(|e| format!("cannot list /proc: {e}"))?;
        let now = Instant::now();
        let elapsed_100ns = self.last.map(|t| now.duration_since(t).as_nanos() as f64 / 100.0).unwrap_or(0.0);
        let elapsed_s = elapsed_100ns / 1e7;
        let heavy = self.collections.is_multiple_of(HEAVY_EVERY);
        self.collections += 1;
        if self.desktop_at.is_none_or(|t| now.duration_since(t) >= DESKTOP_TTL) {
            self.desktop = load_desktop();
            self.desktop_at = Some(now);
        }
        let tck = clk_tck();
        let efficiency = super::efficiency_marks();
        let mut rows = Vec::with_capacity(512);
        let mut apps: Vec<(u32, u32, Option<u32>, Option<String>)> = Vec::with_capacity(512);
        let mut next_prev = HashMap::with_capacity(512);
        let mut totals = Totals::default();
        for entry in dir.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            // Gone since the listing.
            let Some(st) = read_stat(pid) else { continue };
            let status = read(pid, "status").map(|s| procfs::parse_status(&s)).unwrap_or_default();
            let uid = if st.kernel_thread() { Some(0) } else { status.euid };
            let key = (pid, st.start_ticks);
            let archs = &mut self.archs;
            let id = self.identities.entry(key).or_insert_with(|| identity(pid, &st, archs)).clone();
            let cpu_time = procfs::ticks_to_100ns(st.cpu_ticks, tck);
            // Another user's I/O counters are unreadable without root.
            let (read_b, write_b) = read(pid, "io").and_then(|s| procfs::parse_io(&s)).unwrap_or((0, 0));
            let (cpu_pct, read_bps, write_bps) = match self.prev.get(&pid) {
                Some(p) if p.start == st.start_ticks && elapsed_100ns > 0.0 => (
                    (cpu_time.saturating_sub(p.cpu_time) as f64 / elapsed_100ns / logical_cpus.max(1) as f64 * 100.0) as f32,
                    read_b.saturating_sub(p.read) as f64 / elapsed_s.max(1e-3),
                    write_b.saturating_sub(p.write) as f64 / elapsed_s.max(1e-3),
                ),
                _ => (0.0, 0.0, 0.0),
            };
            next_prev.insert(pid, Prev { start: st.start_ticks, cpu_time, read: read_b, write: write_b });
            let files = match self.files.get(&key) {
                Some(n) if !heavy => *n,
                _ => {
                    let n = count_files(pid);
                    self.files.insert(key, n);
                    n
                }
            };
            totals.processes += 1;
            totals.threads += st.threads as usize;
            totals.handles += files as usize;
            let user = uid.map(|u| self.users.entry(u).or_insert_with(|| user_name(u)).clone());
            let app = self.desktop.get(&id.app);
            apps.push((pid, st.ppid, uid, app.filter(|a| a.windowed && !st.kernel_thread()).map(|_| id.app.clone())));
            let create_time = stamp(&st);
            rows.push(ProcessRow {
                pid,
                parent_pid: st.ppid,
                name: id.name,
                user,
                path: id.path,
                threads: st.threads,
                handles: files,
                session_id: st.session,
                base_priority: st.nice,
                private_working_set: status.rss_anon,
                working_set: status.rss,
                commit: status.rss_anon + status.swap,
                cpu_pct: cpu_pct.clamp(0.0, 100.0),
                cpu_time,
                io_bps: read_bps + write_bps,
                gpu_pct: gpu_by_pid.get(&pid).copied().unwrap_or(0.0),
                create_time,
                efficiency: efficiency.contains(&(pid, create_time)),
                has_window: false,
                // Stopped and traced processes are suspended; zombies and idle kernel threads run.
                status: if matches!(st.state, 'T' | 't') { Status::Suspended } else { Status::Running },
                read_bps,
                write_bps,
                peak_working_set: status.peak_rss,
                page_faults: st.faults.min(u32::MAX as u64) as u32,
                command_line: id.command_line,
                arch: id.arch,
                description: app.map(|a| a.name.clone()),
            });
        }
        let windowed = self.window_owners(&apps);
        for r in &mut rows {
            r.has_window = windowed.contains(&r.pid);
        }
        // Forget what belongs to exited processes.
        let alive = |pid: u32, start: u64| next_prev.get(&pid).is_some_and(|p: &Prev| p.start == start);
        self.identities.retain(|k, _| alive(k.0, k.1));
        self.files.retain(|k, _| alive(k.0, k.1));
        let starts: HashMap<u32, i64> = rows.iter().map(|r| (r.pid, r.create_time)).collect();
        super::retain_efficiency(|k| starts.get(&k.0) == Some(&k.1));
        self.prev = next_prev;
        self.last = Some(now);
        Ok((rows, totals))
    }

    /// The pids that own a window: the X11 client list on X11; on Wayland (or without an X
    /// server) the `.desktop` heuristic plus the XWayland clients.
    fn window_owners(&mut self, apps: &[(u32, u32, Option<u32>, Option<String>)]) -> HashSet<u32> {
        // Only this user's processes: a pid from the X server can belong to another pid namespace
        // (Flatpak sandboxes, an X server in a container such as WSLg's) and name a stranger.
        let me = euid();
        let mine: HashSet<u32> = apps.iter().filter(|a| a.2 == Some(me)).map(|a| a.0).collect();
        let x11 = self.x11.client_pids().map(|p| p.into_iter().filter(|p| mine.contains(p)).collect::<HashSet<u32>>());
        match x11 {
            Some(pids) if !crate::platform::is_wayland() => pids,
            x11 => {
                let procs: Vec<(u32, u32, Option<u32>, Option<&str>)> = apps.iter().map(|a| (a.0, a.1, a.2, a.3.as_deref())).collect();
                let mut pids = procfs::heuristic_windows(&procs, me);
                pids.extend(x11.unwrap_or_default());
                pids
            }
        }
    }
}

/// The X11 client list through x11rb (pure Rust protocol, no libX11).
mod x11 {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
    use x11rb::connection::{Connection, RequestConnection as _};
    use x11rb::protocol::res::{self, ClientIdMask, ClientIdSpec, ConnectionExt as _};
    use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, GetPropertyReply, MapState, Window};
    use x11rb::rust_connection::RustConnection;

    /// How long to wait before connecting again after a failure.
    const RETRY: Duration = Duration::from_secs(30);
    const ATOMS: [&[u8]; 7] =
        [b"_NET_CLIENT_LIST", b"_NET_WM_PID", b"_NET_WM_WINDOW_TYPE", b"_NET_WM_WINDOW_TYPE_DOCK", b"_NET_WM_WINDOW_TYPE_DESKTOP", b"_NET_WM_STATE", b"_NET_WM_STATE_SKIP_TASKBAR"];

    struct Session {
        conn: RustConnection,
        root: Window,
        atoms: [Atom; 7],
        /// The X-Resource extension is there.
        res: bool,
    }

    #[derive(Default)]
    pub struct Windows {
        /// The display to ask; `None` for `$DISPLAY`.
        display: Option<String>,
        session: Option<Session>,
        retry_at: Option<Instant>,
    }

    impl Windows {
        #[cfg(test)]
        pub fn on(display: &str) -> Self {
            Self { display: Some(display.into()), ..Default::default() }
        }

        /// The pids owning a managed top-level window (`_NET_CLIENT_LIST`, `_NET_WM_PID`) that is
        /// not a dock, a desktop or kept off the taskbar; `None` without an X server. The pids are
        /// as the X server sees them (its pid namespace).
        pub fn client_pids(&mut self) -> Option<HashSet<u32>> {
            if self.session.is_none() {
                let no_display = self.display.is_none() && std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty());
                if no_display || self.retry_at.is_some_and(|t| Instant::now() < t) {
                    return None;
                }
                self.session = Session::connect(self.display.as_deref());
            }
            let pids = self.session.as_ref().and_then(Session::client_pids);
            if pids.is_none() {
                self.session = None;
                self.retry_at = Some(Instant::now() + RETRY);
            }
            pids
        }
    }

    impl Session {
        fn connect(display: Option<&str>) -> Option<Session> {
            let (conn, screen) = x11rb::connect(display).ok()?;
            let root = conn.setup().roots.get(screen)?.root;
            let cookies = ATOMS.iter().map(|n| conn.intern_atom(false, n)).collect::<Result<Vec<_>, _>>().ok()?;
            let mut atoms = [0; 7];
            for (slot, c) in atoms.iter_mut().zip(cookies) {
                *slot = c.reply().ok()?.atom;
            }
            let res = conn.extension_information(res::X11_EXTENSION_NAME).ok().flatten().is_some();
            Some(Session { conn, root, atoms, res })
        }

        fn client_pids(&self) -> Option<HashSet<u32>> {
            let [client_list, wm_pid, wtype, dock, desktop, state, skip] = self.atoms;
            let c = &self.conn;
            let list = c.get_property(false, self.root, client_list, AtomEnum::WINDOW, 0, u32::MAX / 4).ok()?.reply().ok()?;
            let windows: Vec<Window> = match list.value32() {
                Some(v) if list.type_ != u32::from(AtomEnum::NONE) => v.collect(),
                // No EWMH client list (a rootless XWayland such as WSLg's, a bare WM): the mapped,
                // managed top-level windows instead.
                _ => self.mapped_top_levels()?,
            };
            // All requests first, then the replies: one round trip.
            let mut cookies = Vec::with_capacity(windows.len());
            for w in windows {
                cookies.push((
                    w,
                    (
                        c.get_property(false, w, wm_pid, AtomEnum::CARDINAL, 0, 1).ok()?,
                        c.get_property(false, w, wtype, AtomEnum::ATOM, 0, 16).ok()?,
                        c.get_property(false, w, state, AtomEnum::ATOM, 0, 32).ok()?,
                    ),
                ));
            }
            let has = |r: &GetPropertyReply, a: Atom| r.value32().is_some_and(|mut v| v.any(|x| x == a));
            let mut pids = HashSet::new();
            let mut unnamed = Vec::new();
            for (w, (p, t, s)) in cookies {
                // A window closed meanwhile answers with an error: skip it.
                let (Ok(p), Ok(t), Ok(s)) = (p.reply(), t.reply(), s.reply()) else { continue };
                if has(&t, dock) || has(&t, desktop) || has(&s, skip) {
                    continue;
                }
                match p.value32().and_then(|mut v| v.next()).filter(|p| *p > 0) {
                    Some(pid) => {
                        pids.insert(pid);
                    }
                    None => unnamed.push(w),
                }
            }
            // Clients that do not set _NET_WM_PID (Xt programs, some toolkits): the X server
            // knows the pid of a local client (X-Resource).
            if self.res && !unnamed.is_empty() {
                let cookies: Vec<_> = unnamed.iter().filter_map(|w| c.res_query_client_ids(&[ClientIdSpec { client: *w, mask: ClientIdMask::LOCAL_CLIENT_PID }]).ok()).collect();
                for cookie in cookies {
                    let Ok(reply) = cookie.reply() else { continue };
                    pids.extend(reply.ids.iter().filter(|id| id.spec.mask.contains(ClientIdMask::LOCAL_CLIENT_PID)).filter_map(|id| id.value.first().copied()).filter(|p| *p > 0));
                }
            }
            Some(pids)
        }

        /// The root's children that are viewable and not override-redirect (menus, tooltips).
        fn mapped_top_levels(&self) -> Option<Vec<Window>> {
            let c = &self.conn;
            let children = c.query_tree(self.root).ok()?.reply().ok()?.children;            let cookies = children.iter().map(|w| c.get_window_attributes(*w).map(|k| (*w, k))).collect::<Result<Vec<_>, _>>().ok()?;
            Some(
                cookies
                    .into_iter()
                    .filter_map(|(w, k)| k.reply().ok().filter(|a| a.map_state == MapState::VIEWABLE && !a.override_redirect).map(|_| w))
                    .collect(),
            )
        }
    }
}

/// An open pidfd (closed on drop).
struct Pidfd(libc::c_int);

impl Drop for Pidfd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this value.
        unsafe { libc::close(self.0) };
    }
}

/// End task sends SIGTERM; a process still there this long after gets SIGKILL, so it ends as
/// surely as with TerminateProcess on Windows.
const KILL_AFTER_MS: i32 = 5000;

fn os_err(e: std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => format!("{ACCESS_DENIED} (needs root)"),
        Some(libc::ESRCH) => "the process has exited".into(),
        _ => e.to_string(),
    }
}

/// The process `pid` if it is still the one started at `start`, with a pidfd pinning it (`None`
/// on kernels before 5.3).
fn target(pid: u32, start: i64) -> Result<Option<Pidfd>, String> {
    if pid <= 1 {
        return Err("refusing to signal init".into());
    }
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    let fd = if fd >= 0 {
        Some(Pidfd(fd as libc::c_int))
    } else {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ENOSYS) {
            return Err(os_err(e));
        }
        None
    };
    // Checked after opening: a signal through the pidfd reaches the process it was opened for or
    // nothing, so a match here means the right process is hit.
    let st = read_stat(pid).ok_or("the process has exited")?;
    if stamp(&st) != start {
        return Err("the process has exited (its PID now belongs to another process)".into());
    }
    if st.kernel_thread() {
        return Err("kernel threads cannot be signalled".into());
    }
    Ok(fd)
}

fn send(fd: &Option<Pidfd>, pid: u32, sig: libc::c_int) -> Result<(), String> {
    let rc = match fd {
        // SAFETY: a valid pidfd, a signal number, no siginfo and no flags.
        Some(fd) => unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd.0, sig, std::ptr::null::<libc::siginfo_t>(), 0) },
        // SAFETY: kill has no memory preconditions.
        None => libc::c_long::from(unsafe { libc::kill(pid as libc::pid_t, sig) }),
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(os_err(std::io::Error::last_os_error()))
    }
}

/// SIGKILL for a process that ignored SIGTERM for [`KILL_AFTER_MS`].
fn escalate(fd: Option<Pidfd>, pid: u32, start: i64) {
    match &fd {
        Some(f) => {
            let mut p = libc::pollfd { fd: f.0, events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd. A pidfd turns readable when its process exits.
            if unsafe { libc::poll(&mut p, 1, KILL_AFTER_MS) } == 0 {
                let _ = send(&fd, pid, libc::SIGKILL);
            }
        }
        None => {
            std::thread::sleep(Duration::from_millis(KILL_AFTER_MS as u64));
            if start_of(pid) == Some(start) {
                let _ = send(&fd, pid, libc::SIGKILL);
            }
        }
    }
}

/// Runs `f` for every thread of `pid` (nice and scheduling policy are per thread on Linux);
/// threads that ended meanwhile are skipped.
fn each_thread(pid: u32, f: impl Fn(u32) -> libc::c_int) -> Result<(), String> {
    for tid in tids(pid) {
        if f(tid) != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(os_err(e));
            }
        }
    }
    Ok(())
}

fn renice(pid: u32, nice: i32) -> Result<(), String> {
    // SAFETY: setpriority has no memory preconditions.
    each_thread(pid, |tid| unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) })
}

fn set_policy(pid: u32, policy: libc::c_int) -> Result<(), String> {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a valid sched_param for the call.
    each_thread(pid, |tid| unsafe { libc::sched_setscheduler(tid as libc::pid_t, policy, &param) })
}

/// The action primitives [`super::actions::run`] drives.
pub(super) mod sys {
    use super::*;

    pub fn terminate(pid: u32, start: i64) -> Result<(), String> {
        let fd = target(pid, start)?;
        send(&fd, pid, libc::SIGTERM)?;
        // A stopped process acts on SIGTERM only once it runs again.
        let _ = send(&fd, pid, libc::SIGCONT);
        let _ = std::thread::Builder::new().name("tmop-end-task".into()).spawn(move || escalate(fd, pid, start));
        Ok(())
    }

    pub fn suspend(pid: u32, start: i64) -> Result<(), String> {
        send(&target(pid, start)?, pid, libc::SIGSTOP)
    }

    pub fn resume(pid: u32, start: i64) -> Result<(), String> {
        send(&target(pid, start)?, pid, libc::SIGCONT)
    }

    /// The nice value of the class for every thread; a lower one than now needs root
    /// (`CAP_SYS_NICE`). Realtime is not offered (`caps.realtime`).
    pub fn set_priority(pid: u32, start: i64, p: Priority) -> Result<(), String> {
        target(pid, start)?;
        renice(pid, p.nice())
    }

    /// Efficiency mode = `SCHED_IDLE` for every thread (it runs only when nothing else wants
    /// the CPU; the nice value stays and counts again afterwards). Off = `SCHED_OTHER`, which the
    /// kernel allows without root only within `RLIMIT_NICE` (usually not at all).
    pub fn set_efficiency(pid: u32, start: i64, on: bool) -> Result<(), String> {
        target(pid, start)?;
        set_policy(pid, if on { libc::SCHED_IDLE } else { libc::SCHED_OTHER })
    }

    /// Selects the image in the file manager.
    pub fn open_location(path: &str) -> Result<(), String> {
        crate::platform::reveal(Path::new(path))
    }
}

/// The `pkexec` retry behind [`super::elevated`].
pub(super) mod elevated {
    use super::*;

    /// Only programs from the system folders run as root, never one found through `$PATH`.
    const DIRS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

    fn find(program: &str) -> Option<String> {
        DIRS.iter().map(|d| format!("{d}/{program}")).find(|p| Path::new(p).is_file())
    }

    fn tools() -> Option<&'static procfs::Tools> {
        static TOOLS: OnceLock<Option<procfs::Tools>> = OnceLock::new();
        TOOLS.get_or_init(|| Some(procfs::Tools { pkexec: find("pkexec")?, kill: find("kill")?, renice: find("renice")?, chrt: find("chrt") })).as_ref()
    }

    /// What the retry runs for `action`, if it has one.
    fn op(action: Action) -> Option<Elevated> {
        match action {
            Action::EndTask | Action::EndTree => Some(Elevated::Signal("TERM")),
            Action::Suspend => Some(Elevated::Signal("STOP")),
            Action::Resume => Some(Elevated::Signal("CONT")),
            Action::SetPriority(p) if p != Priority::Realtime => Some(Elevated::Renice(p.nice())),
            Action::Efficiency(false) => Some(Elevated::NormalPolicy),
            _ => None,
        }
    }

    /// A refused action with an elevated form, when `pkexec` (and the program the form runs) is
    /// installed and TMOP is not root.
    pub fn offered(action: Action, err: &str) -> bool {
        err.contains(ACCESS_DENIED) && !crate::platform::is_elevated() && op(action).zip(tools()).is_some_and(|(op, t)| procfs::elevated_argv(t, op, &[]).is_some())
    }

    /// Runs the one `kill` or `renice` the action needs through `pkexec` (it asks for the
    /// password). Only processes that are still the ones in the table are named; the authorization
    /// prompt itself can take a while, which leaves a small window for pid reuse.
    pub fn run(action: Action, row: &ProcessRow, rows: &[ProcessRow]) -> Result<String, String> {
        let op = op(action).ok_or_else(|| format!("{action:?} has no administrator retry"))?;
        let t = tools().ok_or("pkexec is not installed")?;
        let me = std::process::id();
        let targets: Vec<(u32, i64)> = match action {
            Action::EndTree => super::super::tree_of(rows, row.pid).into_iter().filter(|p| *p != me).map(|p| (p, rows.iter().find(|r| r.pid == p).map_or(row.create_time, |r| r.create_time))).collect(),
            _ if row.pid == me => return Err("refusing to act on TMOP itself".into()),
            _ => vec![(row.pid, row.create_time)],
        };
        let live: Vec<u32> = targets.into_iter().filter(|(p, s)| *p > 1 && start_of(*p) == Some(*s)).map(|t| t.0).collect();
        if live.is_empty() {
            return Err("the process has exited".into());
        }
        // renice acts on one thread per id; chrt --all-tasks and kill take the process.
        let ids: Vec<u32> = match op {
            Elevated::Renice(_) => live.iter().flat_map(|p| tids(*p)).collect(),
            _ => live.clone(),
        };
        let argv = procfs::elevated_argv(t, op, &ids).ok_or("chrt is not installed")?;
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("could not start pkexec: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(procfs::pkexec_error(out.status.code(), stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")));
        }
        Ok(match action {
            Action::EndTree => format!("Ended {} process(es) as administrator", live.len()),
            Action::EndTask => format!("Ended {} ({}) as administrator", row.name, row.pid),
            Action::Suspend => format!("Suspended {} as administrator", row.name),
            Action::Resume => format!("Resumed {} as administrator", row.name),
            Action::SetPriority(p) => format!("{} priority: {} (as administrator)", row.name, p.label()),
            _ => {
                super::super::mark_efficiency((row.pid, row.create_time), false);
                format!("{} off for {} (as administrator)", super::super::efficiency_word(), row.name)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    fn state(pid: u32) -> char {
        read_stat(pid).map_or('?', |s| s.state)
    }

    /// Polls until `f` holds (the kernel acts on signals asynchronously).
    fn until(what: &str, f: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn spawn(cmd: &str, args: &[&str]) -> (Child, u32, i64) {
        let child = Command::new(cmd).args(args).spawn().expect("spawn");
        let pid = child.id();
        until("the child to start", || start_of(pid).is_some());
        (child, pid, start_of(pid).unwrap())
    }

    fn row_of(e: &mut Enumerator, pid: u32) -> ProcessRow {
        let (rows, totals) = e.collect(2, &HashMap::new()).expect("collect");
        assert!(totals.processes == rows.len() && totals.threads >= rows.len());
        rows.into_iter().find(|r| r.pid == pid).expect("pid enumerated")
    }

    #[test]
    fn enumerates_itself() {
        let mut e = Enumerator::new();
        let me = std::process::id();
        let first = row_of(&mut e, me);
        // Burn some CPU so the second sample has a delta.
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(200) {
            x = x.wrapping_mul(31).wrapping_add(7);
        }
        assert!(x != 1);
        let r = row_of(&mut e, me);
        let exe = std::env::current_exe().unwrap();
        assert_eq!(r.path.as_deref().map(Path::new), Some(exe.as_path()));
        let id = Command::new("id").arg("-un").output().unwrap();
        assert_eq!(r.user.as_deref(), Some(String::from_utf8_lossy(&id.stdout).trim()));
        assert_eq!(r.parent_pid, std::os::unix::process::parent_id());
        assert_eq!(r.create_time, first.create_time);
        assert!(r.cpu_time > first.cpu_time && r.cpu_pct > 0.0, "{} -> {} ({}%)", first.cpu_time, r.cpu_time, r.cpu_pct);
        assert!(r.threads >= 1 && r.working_set > 0 && r.private_working_set > 0 && r.handles >= 3);
        assert_eq!(r.arch, if cfg!(target_arch = "x86_64") { "x64" } else if cfg!(target_arch = "aarch64") { "ARM64" } else { r.arch });
        assert_eq!(r.status, Status::Running);
        assert!(r.command_line.as_deref().is_some_and(|c| !c.is_empty()));
        // The start converts to a FILETIME in the last day (the Processes page shows it).
        let now_ft = procfs::FILETIME_UNIX + std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 * 10_000_000;
        assert!(r.create_time <= now_ft + 600_000_000 && r.create_time > now_ft - 864_000_000_000, "{} vs {now_ft}", r.create_time);
        // pid 1 is there, and kernel threads (where visible) carry no image.
        let (rows, _) = e.collect(2, &HashMap::new()).unwrap();
        assert!(rows.iter().any(|r| r.pid == 1));
        if rows.iter().any(|r| r.pid == 2 && r.name == "kthreadd") {
            assert!(rows.iter().filter(|r| r.parent_pid == 2).all(|r| r.path.is_none() && r.user.as_deref() == Some("root")));
        }
    }

    #[test]
    fn suspend_resume_and_end_a_child() {
        let (mut child, pid, start) = spawn("sleep", &["30"]);
        let mut e = Enumerator::new();
        // A wrong start (a reused pid) is refused before any signal.
        assert!(sys::suspend(pid, start + 1).unwrap_err().contains("exited"));
        sys::suspend(pid, start).unwrap();
        until("the stop", || state(pid) == 'T');
        assert_eq!(row_of(&mut e, pid).status, Status::Suspended);
        sys::resume(pid, start).unwrap();
        until("the continue", || state(pid) != 'T');
        assert_eq!(row_of(&mut e, pid).status, Status::Running);

        sys::set_priority(pid, start, Priority::BelowNormal).unwrap();
        assert_eq!(read_stat(pid).unwrap().nice, 10);
        assert_eq!(Priority::from_row(&row_of(&mut e, pid)), Some(Priority::BelowNormal));
        sys::set_efficiency(pid, start, true).unwrap();
        // SAFETY: sched_getscheduler has no memory preconditions.
        assert_eq!(unsafe { libc::sched_getscheduler(pid as libc::pid_t) }, libc::SCHED_IDLE);
        assert_eq!(read_stat(pid).unwrap().nice, 10);
        // Leaving SCHED_IDLE needs root unless RLIMIT_NICE allows it: then the retry is offered.
        let policy = match sys::set_efficiency(pid, start, false) {
            Ok(()) => libc::SCHED_OTHER,
            Err(err) => {
                assert!(err.contains(ACCESS_DENIED), "{err}");
                libc::SCHED_IDLE
            }
        };
        assert_eq!(unsafe { libc::sched_getscheduler(pid as libc::pid_t) }, policy);

        // End task works on a stopped process too.
        sys::suspend(pid, start).unwrap();
        until("the stop", || state(pid) == 'T');
        sys::terminate(pid, start).unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(sys::terminate(pid, start).is_err());
        assert!(sys::terminate(1, start_of(1).unwrap()).unwrap_err().contains("init"));
    }

    #[test]
    fn end_task_kills_what_ignores_sigterm() {
        let (mut child, pid, start) = spawn("sh", &["-c", "trap '' TERM; while :; do sleep 0.1; done"]);
        // Let the shell install its trap.
        std::thread::sleep(Duration::from_millis(300));
        let t = Instant::now();
        sys::terminate(pid, start).unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(t.elapsed() >= Duration::from_millis(KILL_AFTER_MS as u64 - 200));
    }

    #[test]
    fn retry_is_offered_only_for_refusals() {
        assert!(!elevated::offered(Action::EndTask, "the process has exited"));
        assert!(!elevated::offered(Action::OpenFileLocation, ACCESS_DENIED));
        assert!(!elevated::offered(Action::Efficiency(true), ACCESS_DENIED));
        let pkexec = ["/usr/bin/pkexec", "/bin/pkexec", "/usr/sbin/pkexec", "/sbin/pkexec"].iter().any(|p| Path::new(p).is_file());
        let expected = pkexec && !crate::platform::is_elevated();
        assert_eq!(elevated::offered(Action::SetPriority(Priority::High), &format!("{ACCESS_DENIED} (needs root)")), expected);
        assert_eq!(elevated::offered(Action::EndTree, &format!("4242: {ACCESS_DENIED} (needs root)")), expected);
    }

    /// On the X server named by `TMOP_TEST_X11_DISPLAY` (e.g. an `Xvfb :99`; a local server, in
    /// this pid namespace) with `xmessage` at hand, the window it opens is found. Skipped
    /// otherwise.
    #[test]
    fn x11_finds_a_window() {
        let Some(display) = std::env::var("TMOP_TEST_X11_DISPLAY").ok().filter(|d| !d.is_empty()) else { return };
        let xmessage = std::env::var("TMOP_TEST_XMESSAGE").unwrap_or_else(|_| "/usr/bin/xmessage".into());
        let mut x = x11::Windows::on(&display);
        let before = x.client_pids().expect("the test X server answers");
        let (mut child, pid, start) = spawn(&xmessage, &["-display", &display, "tmop x11 test"]);
        let end = Instant::now() + Duration::from_secs(10);
        let mut found = false;
        while !found && Instant::now() < end {
            found = x.client_pids().is_some_and(|p| p.contains(&pid));
            std::thread::sleep(Duration::from_millis(100));
        }
        sys::terminate(pid, start).unwrap();
        let _ = child.wait();
        // Without a window manager there is no client list: the mapped window and X-Resource.
        assert!(found && !before.contains(&pid), "xmessage {pid} not found");

        // Play window manager: a client list with a normal window and a dock, each with a pid.
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass};
        use x11rb::wrapper::ConnectionExt as _;
        let (c, screen) = x11rb::connect(Some(&display)).unwrap();
        let root = c.setup().roots[screen].root;
        let atom = |n: &[u8]| c.intern_atom(false, n).unwrap().reply().unwrap().atom;
        let (list, wm_pid, wtype, dock) = (atom(b"_NET_CLIENT_LIST"), atom(b"_NET_WM_PID"), atom(b"_NET_WM_WINDOW_TYPE"), atom(b"_NET_WM_WINDOW_TYPE_DOCK"));
        let mut ids = Vec::new();
        for fake_pid in [4_242_001u32, 4_242_002] {
            let w = c.generate_id().unwrap();
            c.create_window(0, w, root, 0, 0, 10, 10, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new()).unwrap();
            c.change_property32(PropMode::REPLACE, w, wm_pid, AtomEnum::CARDINAL, &[fake_pid]).unwrap();
            ids.push(w);
        }
        c.change_property32(PropMode::REPLACE, ids[1], wtype, AtomEnum::ATOM, &[dock]).unwrap();
        c.change_property32(PropMode::REPLACE, root, list, AtomEnum::WINDOW, &ids).unwrap();
        c.sync().unwrap();
        let pids = x.client_pids().unwrap();
        c.delete_property(root, list).unwrap();
        c.sync().unwrap();
        assert!(pids.contains(&4_242_001) && !pids.contains(&4_242_002), "{pids:?}");
    }

    #[test]
    fn window_owners_without_x_are_heuristic() {
        // Whatever the session, the call answers (an X server may or may not exist here).
        let mut e = Enumerator::new();
        let (rows, _) = e.collect(1, &HashMap::new()).unwrap();
        assert!(rows.iter().filter(|r| r.has_window).all(|r| r.pid > 1));
    }
}
