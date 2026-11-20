//! Network use per process: the bytes each process sent and received over the network, loopback
//! traffic left out.
//!
//! On Windows they are counted from the TCP and UDP send and receive events of the
//! Microsoft-Windows-Kernel-Network ETW provider, in a real-time session of TMOP's own
//! ([`SESSION_NAME`]). Resource Monitor reads the same events. Enabling that provider needs an
//! administrator token: members of Performance Log Users may start a session, but the provider
//! refuses them, and per-connection TCP statistics (`SetPerTcpConnectionEStats`) need
//! elevation as well. So without elevation [`state`] is [`State::NeedsAdmin`] and no figure is
//! shown, rather than a guess.
//!
//! Other systems are [`State::Unsupported`]. Linux has no unprivileged per-process counters:
//! `sock_diag` reports bytes only for live TCP sockets, nothing for UDP, and maps sockets to
//! processes only for the user's own processes. macOS keeps them in the private
//! NetworkStatistics framework.
//!
//! The session runs while [`set_wanted`] asks for it (the Processes page shows the Network
//! column) and is stopped when that ends or TMOP exits. There is one session per machine, so
//! one TMOP at a time owns it: the one that holds a named mutex. Another TMOP that wants the
//! column waits ([`State::Elsewhere`]) and takes over once the owner lets go. A session left
//! behind by a TMOP that crashed has no owner (the mutex went with the process), so it is
//! stopped and replaced. The rules live in [`Driver`], which tests drive without ETW.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The bytes a process sent and received since TMOP began counting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetBytes {
    pub sent: u64,
    pub received: u64,
}

/// Whether the counters run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// This system has no source of per-process network figures.
    Unsupported,
    /// Windows, but TMOP does not run as administrator.
    NeedsAdmin,
    /// Not asked for (the column is hidden).
    Off,
    Running,
    /// Another TMOP runs the session; this one takes over once that one lets it go.
    Elsewhere,
    /// The session ended while it was still wanted; it is started again shortly.
    Stopped,
    /// The session could not be started (the message says why); it is tried again later.
    Failed(String),
}

impl State {
    /// Why a cell has no figure, for its hover text; `None` while the counters run.
    pub fn reason(&self) -> Option<String> {
        match self {
            State::Running => None,
            State::Unsupported => Some("This system reports no network use per process".into()),
            State::NeedsAdmin => Some("Network use per process needs TMOP running as administrator".into()),
            State::Off => Some("Starting the network counters".into()),
            State::Elsewhere => Some("Network use per process is being measured by another TMOP window".into()),
            State::Stopped => Some("The network counters stopped; TMOP starts them again shortly".into()),
            State::Failed(e) => Some(format!("The network counters could not start: {e}")),
        }
    }
}

/// Network events the session dropped (its `EventsLost`) and real-time buffers it dropped
/// because TMOP did not read them fast enough (`RealTimeBuffersLost`), since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lost {
    pub events: u64,
    pub buffers: u64,
}

impl Lost {
    /// Hover text while some were lost.
    pub fn note(self) -> Option<String> {
        (self.events > 0 || self.buffers > 0).then(|| format!("Events lost: {}, buffers lost: {} \u{2014} the figures may be a little low", self.events, self.buffers))
    }
}

/// One send or receive that a Kernel-Network event reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub pid: u32,
    pub bytes: u32,
    pub sent: bool,
    /// Both ends on this machine (loopback): not network use.
    pub loopback: bool,
}

/// Reads the payload of Kernel-Network event `id`. The send and receive events of TCP and UDP,
/// IPv4 and IPv6, all start with `PID: u32, size: u32, daddr, saddr` (4 or 16 bytes each);
/// other events are `None`.
pub fn parse(id: u16, data: &[u8]) -> Option<Transfer> {
    let (sent, v6) = match id {
        // TcpIp Send / UdpIp Send, IPv4.
        10 | 42 => (true, false),
        // TcpIp Recv / UdpIp Recv, IPv4.
        11 | 43 => (false, false),
        26 | 58 => (true, true),
        27 | 59 => (false, true),
        _ => return None,
    };
    let u32_at = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (pid, bytes) = (u32_at(0)?, u32_at(4)?);
    let len = if v6 { 16 } else { 4 };
    let addr = |at: usize| -> Option<IpAddr> {
        let b = data.get(at..at + len)?;
        Some(if v6 {
            let mut a = [0u8; 16];
            a.copy_from_slice(b);
            IpAddr::V6(Ipv6Addr::from(a))
        } else {
            IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
        })
    };
    let (daddr, saddr) = (addr(8)?, addr(8 + len)?);
    Some(Transfer { pid, bytes, sent, loopback: is_loopback(daddr) && is_loopback(saddr) })
}

/// 127.0.0.0/8, ::1 and IPv4-mapped loopback.
fn is_loopback(a: IpAddr) -> bool {
    match a {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// Adds `t` to `counters` (loopback transfers are left out).
pub fn count(counters: &mut HashMap<u32, NetBytes>, t: Transfer) {
    if t.loopback {
        return;
    }
    let c = counters.entry(t.pid).or_default();
    if t.sent {
        c.sent += u64::from(t.bytes);
    } else {
        c.received += u64::from(t.bytes);
    }
}

/// Bytes per second from the counters `prev` and `now`, `secs` apart: (sent, received). Counters
/// that went down (a process that took over a pid) give 0.
pub fn rates(prev: NetBytes, now: NetBytes, secs: f64) -> (f64, f64) {
    if secs <= 0.0 {
        return (0.0, 0.0);
    }
    (now.sent.saturating_sub(prev.sent) as f64 / secs, now.received.saturating_sub(prev.received) as f64 / secs)
}

/// Why a session did not start.
#[cfg(any(windows, test))]
#[derive(Debug)]
pub enum StartError {
    /// Another live TMOP owns the session.
    Owned,
    Failed(String),
}

/// What [`Driver`] runs the session with: ETW on Windows, a fake in tests.
#[cfg(any(windows, test))]
pub trait Backend {
    /// A running session of this process's own: dropping it without [`Backend::stop`] is a bug.
    type Session;
    fn elevated(&self) -> bool;
    /// Takes ownership and starts the session; a leftover without an owner is replaced.
    fn start(&mut self) -> Result<Self::Session, StartError>;
    /// Whether events still arrive (the consumer has not ended).
    fn running(&self, s: &Self::Session) -> bool;
    /// Stops `s` and lets ownership go.
    fn stop(&mut self, s: Self::Session);
    fn lost(&self, s: &Self::Session) -> Lost;
}

/// How often a TMOP waiting for another one's session looks whether it is free yet.
#[cfg(any(windows, test))]
const OWNER_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// The first wait before starting again after a failure; it doubles up to [`MAX_BACKOFF`].
#[cfg(any(windows, test))]
const FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(any(windows, test))]
const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(300);
/// A session that ran this long before it ended resets the backoff.
#[cfg(any(windows, test))]
const STABLE: std::time::Duration = std::time::Duration::from_secs(60);
/// How often a running session's losses are read.
#[cfg(any(windows, test))]
const LOST_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Starts and stops the session as the column comes and goes, and decides what the column
/// shows meanwhile.
#[cfg(any(windows, test))]
pub struct Driver<B: Backend> {
    backend: B,
    session: Option<B::Session>,
    state: State,
    /// No attempt to start before this.
    retry_at: Option<std::time::Instant>,
    backoff: std::time::Duration,
    started: Option<std::time::Instant>,
    lost: Lost,
    lost_at: Option<std::time::Instant>,
}

#[cfg(any(windows, test))]
impl<B: Backend> Driver<B> {
    pub fn new(backend: B) -> Self {
        Self { backend, session: None, state: State::Off, retry_at: None, backoff: FIRST_BACKOFF, started: None, lost: Lost::default(), lost_at: None }
    }

    pub fn state(&self) -> State {
        self.state.clone()
    }

    pub fn lost(&self) -> Lost {
        self.lost
    }

    /// Brings the session in line with `wanted`. Cheap when nothing changes, so it may run every
    /// frame.
    pub fn tick(&mut self, wanted: bool, now: std::time::Instant) {
        if !wanted {
            if let Some(s) = self.session.take() {
                self.backend.stop(s);
            }
            self.state = State::Off;
            self.retry_at = None;
            self.backoff = FIRST_BACKOFF;
            return;
        }
        if let Some(s) = &self.session {
            if self.backend.running(s) {
                if self.lost_at.is_none_or(|t| now >= t) {
                    self.lost = self.backend.lost(s);
                    self.lost_at = Some(now + LOST_POLL);
                }
                return;
            }
            // Stopped from outside (logman, a crash of the consumer): no figures until it runs again.
            if let Some(s) = self.session.take() {
                self.backend.stop(s);
            }
            if self.started.is_some_and(|t| now.saturating_duration_since(t) >= STABLE) {
                self.backoff = FIRST_BACKOFF;
            }
            self.state = State::Stopped;
            self.wait(now);
            return;
        }
        if self.state == State::NeedsAdmin || self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        if !self.backend.elevated() {
            self.state = State::NeedsAdmin;
            return;
        }
        match self.backend.start() {
            Ok(s) => {
                self.session = Some(s);
                self.state = State::Running;
                self.started = Some(now);
                self.retry_at = None;
                self.lost = Lost::default();
                self.lost_at = None;
            }
            Err(StartError::Owned) => {
                self.state = State::Elsewhere;
                self.retry_at = Some(now + OWNER_POLL);
            }
            Err(StartError::Failed(e)) => {
                self.state = State::Failed(e);
                self.wait(now);
            }
        }
    }

    /// Puts the next attempt off by the backoff, and doubles that.
    fn wait(&mut self, now: std::time::Instant) {
        self.retry_at = Some(now + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(windows)]
pub use etw::{events_lost, forget, set_wanted, state, totals, SESSION_NAME};

#[cfg(not(windows))]
pub fn set_wanted(_on: bool) {}

#[cfg(not(windows))]
pub fn events_lost() -> Lost {
    Lost::default()
}

#[cfg(not(windows))]
pub fn state() -> State {
    State::Unsupported
}

#[cfg(not(windows))]
pub fn totals() -> HashMap<u32, NetBytes> {
    HashMap::new()
}

#[cfg(not(windows))]
pub fn forget(_alive: &dyn Fn(u32) -> bool) {}

#[cfg(windows)]
mod etw {
    use super::{count, parse, Backend, Driver, Lost, NetBytes, StartError, State};
    use crate::engine::winreg::NamedMutex;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::thread::JoinHandle;
    use std::time::Instant;
    use windows::core::{GUID, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_QUERY,
        EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
        WNODE_FLAG_TRACED_GUID,
    };

    /// The name of TMOP's session (`logman query -ets` lists it while it runs).
    pub const SESSION_NAME: &str = "TMOP Network";
    /// Held by the TMOP that owns the session. `Global\`: the session is one per machine, so
    /// TMOPs in other logon sessions must see it too.
    const OWNER_MUTEX: &str = r"Global\TMOP Network session owner";
    /// Buffer size in KB. 64 KB holds several hundred events; ETW's default depends on memory.
    const BUFFER_KB: u32 = 64;
    /// Buffers in use while traffic is light: ETW wants at least two per processor.
    fn min_buffers() -> u32 {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get()) as u32;
        (2 * cpus).max(8)
    }
    /// Room for bursts on top of [`min_buffers`]: 128 × 64 KB = 8 MB, held only while a burst
    /// outruns TMOP's reading, so a busy second at gigabit speed is not dropped.
    const EXTRA_BUFFERS: u32 = 128;
    /// Microsoft-Windows-Kernel-Network.
    const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
    /// `KERNEL_NETWORK_KEYWORD_IPV4 | KERNEL_NETWORK_KEYWORD_IPV6`.
    const KEYWORDS: u64 = 0x10 | 0x20;
    /// `TRACE_LEVEL_INFORMATION`.
    const LEVEL: u8 = 4;
    /// `INVALID_PROCESSTRACE_HANDLE` (64-bit builds).
    const INVALID_HANDLE: u64 = u64::MAX;

    /// The counters, by pid, since the first session started (they go on across sessions).
    static COUNTERS: LazyLock<Mutex<HashMap<u32, NetBytes>>> = LazyLock::new(Default::default);
    static DRIVER: LazyLock<Mutex<Driver<Etw>>> = LazyLock::new(|| Mutex::new(Driver::new(Etw)));

    /// TMOP's session, owned while `owner` is held.
    pub(super) struct Session {
        trace: PROCESSTRACE_HANDLE,
        consumer: Option<JoinHandle<()>>,
        owner: NamedMutex,
    }

    pub(super) struct Etw;

    impl Backend for Etw {
        type Session = Session;

        fn elevated(&self) -> bool {
            crate::platform::is_elevated()
        }

        fn start(&mut self) -> Result<Session, StartError> {
            start()
        }

        fn running(&self, s: &Session) -> bool {
            s.consumer.as_ref().is_some_and(|t| !t.is_finished())
        }

        fn stop(&mut self, s: Session) {
            stop(s);
        }

        fn lost(&self, _s: &Session) -> Lost {
            query().unwrap_or_default()
        }
    }

    pub fn state() -> State {
        DRIVER.lock().map(|d| d.state()).unwrap_or(State::Off)
    }

    /// Network events lost since the session started.
    pub fn events_lost() -> Lost {
        DRIVER.lock().map(|d| d.lost()).unwrap_or_default()
    }

    /// Starts the session (`on`) or stops it; see [`Driver::tick`]. Cheap when nothing changes,
    /// so it may be called every frame.
    pub fn set_wanted(on: bool) {
        if let Ok(mut d) = DRIVER.lock() {
            d.tick(on, Instant::now());
        }
    }

    /// The counters, by pid.
    pub fn totals() -> HashMap<u32, NetBytes> {
        COUNTERS.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Forgets the counters of the processes for which `alive(pid)` does not hold.
    pub fn forget(alive: &dyn Fn(u32) -> bool) {
        if let Ok(mut c) = COUNTERS.lock() {
            c.retain(|pid, _| alive(*pid));
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `EVENT_TRACE_PROPERTIES` followed by room for the session name, as `StartTraceW` and
    /// `ControlTraceW` want it.
    struct Properties(Vec<u64>);

    impl Properties {
        fn new() -> Self {
            let name_bytes = (SESSION_NAME.len() + 1) * 2;
            let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name_bytes;
            let mut buf = vec![0u64; size.div_ceil(8)];
            let p = buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            // SAFETY: `buf` is zeroed, 8-aligned and at least as large as the struct.
            unsafe {
                (*p).Wnode.BufferSize = (buf.len() * 8) as u32;
                (*p).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
                // QueryPerformanceCounter time stamps.
                (*p).Wnode.ClientContext = 1;
                (*p).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
                // Hand events over at least once a second.
                (*p).FlushTimer = 1;
                (*p).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            }
            Self(buf)
        }

        /// For `StartTraceW`: [`Properties::new`] with TMOP's buffer sizes.
        fn for_start() -> Self {
            let mut p = Self::new();
            let min = min_buffers();
            // SAFETY: `p` holds a whole EVENT_TRACE_PROPERTIES.
            unsafe {
                (*p.ptr()).BufferSize = BUFFER_KB;
                (*p.ptr()).MinimumBuffers = min;
                (*p.ptr()).MaximumBuffers = min + EXTRA_BUFFERS;
            }
            p
        }

        fn get(&mut self) -> &EVENT_TRACE_PROPERTIES {
            // SAFETY: as in `for_start`.
            unsafe { &*self.ptr() }
        }

        fn ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
            self.0.as_mut_ptr().cast()
        }
    }

    fn check(what: &str, e: WIN32_ERROR) -> Result<(), String> {
        match e {
            ERROR_SUCCESS => Ok(()),
            ERROR_ACCESS_DENIED => Err(format!("{what}: access denied (TMOP must run as administrator)")),
            e => Err(format!("{what} failed (error {})", e.0)),
        }
    }

    /// Stops the session called [`SESSION_NAME`], by name: only while this process owns it.
    fn stop_by_name(_owner: &NamedMutex) {
        let name = wide(SESSION_NAME);
        let mut p = Properties::new();
        // SAFETY: `p` and `name` outlive the call.
        let _ = unsafe { ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), p.ptr(), EVENT_TRACE_CONTROL_STOP) };
    }

    /// The session's losses so far.
    fn query() -> Option<Lost> {
        let name = wide(SESSION_NAME);
        let mut p = Properties::new();
        // SAFETY: `p` and `name` outlive the call.
        let r = unsafe { ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), p.ptr(), EVENT_TRACE_CONTROL_QUERY) };
        let p = p.get();
        (r == ERROR_SUCCESS).then(|| Lost { events: u64::from(p.EventsLost), buffers: u64::from(p.RealTimeBuffersLost) })
    }

    pub(super) fn start() -> Result<Session, StartError> {
        let owner = match NamedMutex::try_acquire(OWNER_MUTEX) {
            Ok(Some(m)) => m,
            Ok(None) => return Err(StartError::Owned),
            Err(e) => return Err(StartError::Failed(e)),
        };
        start_owned(owner).map_err(StartError::Failed)
    }

    fn start_owned(owner: NamedMutex) -> Result<Session, String> {
        let name = wide(SESSION_NAME);
        let mut props = Properties::for_start();
        let mut control = CONTROLTRACE_HANDLE::default();
        // SAFETY: `props` and `name` outlive the calls; the handles are TMOP's own.
        unsafe {
            let mut r = StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.ptr());
            if r == ERROR_ALREADY_EXISTS {
                // No live TMOP owns it (this one holds the mutex): left behind by a TMOP that
                // did not exit cleanly. Stop it and start anew.
                stop_by_name(&owner);
                props = Properties::for_start();
                r = StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.ptr());
            }
            check("Starting the trace session", r)?;
            let stop_now = |control| {
                let mut p = Properties::new();
                let _ = ControlTraceW(control, PCWSTR::null(), p.ptr(), EVENT_TRACE_CONTROL_STOP);
            };
            if let Err(e) = check("Enabling Kernel-Network", EnableTraceEx2(control, &KERNEL_NETWORK, EVENT_CONTROL_CODE_ENABLE_PROVIDER.0, LEVEL, KEYWORDS, 0, 0, None)) {
                stop_now(control);
                return Err(e);
            }
            let mut logger = name.clone();
            let mut logfile: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            logfile.LoggerName = PWSTR(logger.as_mut_ptr());
            logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            logfile.Anonymous2.EventRecordCallback = Some(on_event);
            let trace = OpenTraceW(&mut logfile);
            if trace.Value == INVALID_HANDLE {
                stop_now(control);
                return Err("Opening the trace session failed".into());
            }
            // ProcessTrace delivers the events on this thread until the session stops.
            let consumer = std::thread::Builder::new()
                .name("tmop-netio".into())
                .spawn(move || {
                    let _ = ProcessTrace(&[trace], None, None);
                })
                .map_err(|e| {
                    let _ = CloseTrace(trace);
                    stop_now(control);
                    e.to_string()
                })?;
            Ok(Session { trace, consumer: Some(consumer), owner })
        }
    }

    fn stop(mut s: Session) {
        // By name rather than by its control handle: if the session was stopped from outside,
        // its logger id may since belong to someone else's session. The name is TMOP's, and no
        // other TMOP starts a session while this one holds the mutex. Stopping the session
        // ends ProcessTrace.
        stop_by_name(&s.owner);
        // SAFETY: the handle is the one `start` opened.
        unsafe {
            let _ = CloseTrace(s.trace);
        }
        if let Some(t) = s.consumer.take() {
            let _ = t.join();
        }
        // Dropping `s` lets the mutex go, only once the session is gone.
    }

    /// Called by ProcessTrace for every event.
    unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
        // SAFETY: ETW passes a valid record whose UserData holds UserDataLength bytes.
        let Some(r) = (unsafe { rec.as_ref() }) else { return };
        if r.EventHeader.ProviderId != KERNEL_NETWORK || r.UserData.is_null() {
            return;
        }
        let data = unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) };
        if let Some(t) = parse(r.EventHeader.EventDescriptor.Id, data) {
            if let Ok(mut c) = COUNTERS.lock() {
                count(&mut c, t);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::{Read, Write};
        use std::time::{Duration, Instant};

        /// Elevated: the session starts, a second TMOP's start leaves it alone, it counts this
        /// process's traffic to another machine and stops again; loopback traffic is not
        /// counted. Not elevated: no session, no figures. (Run elevated on a machine where a
        /// TMOP shows the Network column, the session is that TMOP's and this test fails.)
        #[test]
        fn counts_this_process_when_elevated() {
            if !crate::platform::is_elevated() {
                // GitHub's Windows runners run elevated: there this test must not shrink to the
                // short case.
                assert!(std::env::var_os("GITHUB_ACTIONS").is_none(), "expected an elevated CI runner");
                set_wanted(true);
                assert_eq!(state(), State::NeedsAdmin);
                set_wanted(false);
                assert_eq!(state(), State::Off);
                return;
            }
            set_wanted(true);
            assert_eq!(state(), State::Running);
            // A second TMOP (the same start, as another process makes it) finds the session owned
            // and leaves it running: the counting below needs it.
            assert!(matches!(start(), Err(StartError::Owned)), "a second start while the owner is alive");
            // Give the provider a moment to come up.
            std::thread::sleep(Duration::from_millis(1500));
            set_wanted(true);
            assert_eq!(state(), State::Running, "the session survived the second start");
            let me = std::process::id();
            let before = totals().get(&me).copied().unwrap_or_default();
            // Loopback: 200 kB each way, which must not count.
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let mut a = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
            let (mut b, _) = l.accept().unwrap();
            a.write_all(&vec![1u8; 200_000]).unwrap();
            let mut buf = vec![0u8; 200_000];
            b.read_exact(&mut buf).unwrap();
            // The network: an HTTP request whose answer is several kB.
            let mut got = 0usize;
            if let Ok(mut s) = std::net::TcpStream::connect("example.com:80") {
                s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let _ = s.write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n");
                let mut v = Vec::new();
                let _ = s.read_to_end(&mut v);
                got = v.len();
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut now = before;
            while Instant::now() < deadline {
                now = totals().get(&me).copied().unwrap_or_default();
                if got == 0 || now.received.saturating_sub(before.received) >= got as u64 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            let lost = query();
            set_wanted(false);
            assert_eq!(state(), State::Off);
            assert!(query().is_none(), "the session is gone");
            assert!(matches!(NamedMutex::try_acquire(OWNER_MUTEX), Ok(Some(_))), "and so is the owner's mutex");
            eprintln!("network: lost {lost:?}");
            let (sent, received) = (now.sent.saturating_sub(before.sent), now.received.saturating_sub(before.received));
            eprintln!("network: answer {got} bytes; counted sent {sent}, received {received}");
            assert!(sent < 200_000 && received < 200_000, "loopback is not counted: sent {sent}, received {received}");
            if got > 0 {
                assert!(received >= got as u64, "the answer ({got} bytes) is counted: {received}");
                assert!(received < got as u64 * 2, "and nothing much else: {received}");
                assert!(sent > 0, "the request is counted");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(pid: u32, size: u32, daddr: &[u8], saddr: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&pid.to_le_bytes());
        v.extend_from_slice(&size.to_le_bytes());
        v.extend_from_slice(daddr);
        v.extend_from_slice(saddr);
        // dport, sport, and what follows them.
        v.extend_from_slice(&[0u8; 24]);
        v
    }

    #[test]
    fn parses_send_and_receive_events() {
        let lan = [192, 168, 1, 20];
        let web = [93, 184, 215, 14];
        let t = parse(10, &payload(4242, 1460, &web, &lan)).unwrap();
        assert_eq!(t, Transfer { pid: 4242, bytes: 1460, sent: true, loopback: false });
        assert!(!parse(43, &payload(7, 512, &lan, &web)).unwrap().sent, "UDP receive");
        let v6 = Ipv6Addr::new(0x2a00, 0x1450, 0, 0, 0, 0, 0, 0x200e).octets();
        let mine = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1).octets();
        assert_eq!(parse(27, &payload(9, 100, &v6, &mine)), Some(Transfer { pid: 9, bytes: 100, sent: false, loopback: false }));
        assert!(parse(58, &payload(9, 100, &v6, &mine)).unwrap().sent, "UDP send over IPv6");
        // Connect, disconnect and retransmit carry no data of their own.
        for id in [12, 13, 14, 15, 18, 28] {
            assert_eq!(parse(id, &payload(1, 1, &web, &lan)), None, "event {id}");
        }
        assert_eq!(parse(10, &[1, 0, 0, 0, 2, 0]), None, "cut short");
        assert_eq!(parse(26, &payload(1, 1, &web, &lan)[..32]), None, "an IPv6 event too short for its addresses");
    }

    #[test]
    fn loopback_is_not_network_use() {
        let lo = [127, 0, 0, 1];
        assert!(parse(10, &payload(1, 10, &lo, &lo)).unwrap().loopback);
        let lo6 = Ipv6Addr::LOCALHOST.octets();
        assert!(parse(26, &payload(1, 10, &lo6, &lo6)).unwrap().loopback);
        let mapped = Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped().octets();
        assert!(parse(59, &payload(1, 10, &mapped, &lo6)).unwrap().loopback);
        assert!(!parse(10, &payload(1, 10, &[10, 0, 0, 2], &lo)).unwrap().loopback, "one end elsewhere");
        let mut c = HashMap::new();
        count(&mut c, parse(10, &payload(5, 1000, &lo, &lo)).unwrap());
        count(&mut c, parse(10, &payload(5, 300, &[10, 0, 0, 2], &[10, 0, 0, 1])).unwrap());
        count(&mut c, parse(11, &payload(5, 700, &[10, 0, 0, 2], &[10, 0, 0, 1])).unwrap());
        assert_eq!(c[&5], NetBytes { sent: 300, received: 700 });
    }

    #[test]
    fn rates_per_second() {
        let a = NetBytes { sent: 1000, received: 5000 };
        let b = NetBytes { sent: 3000, received: 5000 };
        assert_eq!(rates(a, b, 2.0), (1000.0, 0.0));
        assert_eq!(rates(b, a, 2.0), (0.0, 0.0), "a counter that went down");
        assert_eq!(rates(a, b, 0.0), (0.0, 0.0));
    }

    /// The machine the fake backends share: who holds the owner mutex, whose session runs.
    #[derive(Default)]
    struct Machine {
        elevated: bool,
        owner: Option<u32>,
        session: Option<u32>,
        starts: u32,
        fail: Option<String>,
    }

    /// One TMOP process's backend.
    struct Fake {
        pid: u32,
        m: std::rc::Rc<std::cell::RefCell<Machine>>,
    }

    impl Backend for Fake {
        type Session = u32;
        fn elevated(&self) -> bool {
            self.m.borrow().elevated
        }
        fn start(&mut self) -> Result<u32, StartError> {
            let mut m = self.m.borrow_mut();
            if m.owner.is_some_and(|o| o != self.pid) {
                return Err(StartError::Owned);
            }
            m.starts += 1;
            if let Some(e) = m.fail.clone() {
                return Err(StartError::Failed(e));
            }
            // A leftover without an owner is replaced.
            m.owner = Some(self.pid);
            m.session = Some(self.pid);
            Ok(self.pid)
        }
        fn running(&self, s: &u32) -> bool {
            self.m.borrow().session == Some(*s)
        }
        fn stop(&mut self, s: u32) {
            let mut m = self.m.borrow_mut();
            assert_eq!(m.owner, Some(s), "stops only a session it owns");
            if m.session == Some(s) {
                m.session = None;
            }
            m.owner = None;
        }
        fn lost(&self, _s: &u32) -> Lost {
            Lost { events: 3, buffers: 1 }
        }
    }

    fn machine() -> std::rc::Rc<std::cell::RefCell<Machine>> {
        std::rc::Rc::new(std::cell::RefCell::new(Machine { elevated: true, ..Default::default() }))
    }

    fn tmop(pid: u32, m: &std::rc::Rc<std::cell::RefCell<Machine>>) -> Driver<Fake> {
        Driver::new(Fake { pid, m: m.clone() })
    }

    fn secs(s: u64) -> std::time::Duration {
        std::time::Duration::from_secs(s)
    }

    #[test]
    fn a_second_tmop_waits_for_the_owner_and_then_takes_over() {
        let m = machine();
        let t0 = std::time::Instant::now();
        let (mut a, mut b) = (tmop(1, &m), tmop(2, &m));
        a.tick(true, t0);
        assert_eq!(a.state(), State::Running);
        b.tick(true, t0);
        assert_eq!(b.state(), State::Elsewhere);
        assert!(b.state().reason().unwrap().contains("another TMOP window"));
        assert_eq!(m.borrow().session, Some(1), "the owner's session runs on");
        // Not every frame: only every OWNER_POLL.
        b.tick(true, t0 + secs(1));
        assert_eq!(m.borrow().starts, 1);
        a.tick(true, t0 + secs(1));
        assert_eq!(a.state(), State::Running, "the owner is not disturbed");
        assert_eq!(a.lost(), Lost { events: 3, buffers: 1 }, "read while it runs");
        // The owner hides the column: the waiting one takes over at its next look.
        a.tick(false, t0 + secs(1));
        assert_eq!((a.state(), m.borrow().session), (State::Off, None));
        b.tick(true, t0 + secs(2));
        assert_eq!(b.state(), State::Running);
        assert_eq!(m.borrow().session, Some(2));
        // And the first one, shown the column again, now waits in turn.
        a.tick(true, t0 + secs(3));
        assert_eq!(a.state(), State::Elsewhere);
    }

    #[test]
    fn a_crashed_owners_session_is_replaced() {
        let m = machine();
        let t0 = std::time::Instant::now();
        let mut a = tmop(1, &m);
        a.tick(true, t0);
        let mut b = tmop(2, &m);
        b.tick(true, t0);
        assert_eq!(b.state(), State::Elsewhere);
        // A crashes: its mutex goes with it, its session stays behind.
        drop(a);
        m.borrow_mut().owner = None;
        b.tick(true, t0 + secs(2));
        assert_eq!(b.state(), State::Running);
        assert_eq!(m.borrow().session, Some(2));
    }

    #[test]
    fn a_session_that_ends_shows_no_figures_and_restarts_with_backoff() {
        let m = machine();
        let t0 = std::time::Instant::now();
        let mut a = tmop(1, &m);
        a.tick(true, t0);
        // Stopped from outside.
        m.borrow_mut().session = None;
        a.tick(true, t0 + secs(1));
        assert_eq!(a.state(), State::Stopped);
        assert!(a.state().reason().is_some(), "cells show no figure");
        assert_eq!(m.borrow().owner, None, "ownership let go");
        for s in [1, 2, 5] {
            a.tick(true, t0 + secs(s));
        }
        assert_eq!((a.state(), m.borrow().starts), (State::Stopped, 1), "no tight loop");
        a.tick(true, t0 + secs(6));
        assert_eq!((a.state(), m.borrow().starts), (State::Running, 2));
        // Ending again soon after: the wait doubles.
        m.borrow_mut().session = None;
        a.tick(true, t0 + secs(7));
        a.tick(true, t0 + secs(16));
        assert_eq!(a.state(), State::Stopped);
        a.tick(true, t0 + secs(17));
        assert_eq!(a.state(), State::Running);
        // After a long run the wait is short again.
        m.borrow_mut().session = None;
        a.tick(true, t0 + secs(100));
        a.tick(true, t0 + secs(105));
        assert_eq!(a.state(), State::Running);
    }

    #[test]
    fn a_failed_start_is_retried_later_and_at_once_after_off() {
        let m = machine();
        m.borrow_mut().fail = Some("no".into());
        let t0 = std::time::Instant::now();
        let mut a = tmop(1, &m);
        a.tick(true, t0);
        assert_eq!(a.state(), State::Failed("no".into()));
        a.tick(true, t0 + secs(4));
        assert_eq!(m.borrow().starts, 1);
        a.tick(true, t0 + secs(5));
        assert_eq!(m.borrow().starts, 2);
        a.tick(true, t0 + secs(14));
        assert_eq!(m.borrow().starts, 2, "the wait doubled");
        m.borrow_mut().fail = None;
        a.tick(false, t0 + secs(14));
        a.tick(true, t0 + secs(14));
        assert_eq!(a.state(), State::Running);
    }

    #[test]
    fn without_elevation_nothing_starts() {
        let m = machine();
        m.borrow_mut().elevated = false;
        let t0 = std::time::Instant::now();
        let mut a = tmop(1, &m);
        for s in 0..400 {
            a.tick(true, t0 + secs(s));
        }
        assert_eq!((a.state(), m.borrow().starts), (State::NeedsAdmin, 0));
        a.tick(false, t0);
        assert_eq!(a.state(), State::Off);
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_have_no_counters() {
        set_wanted(true);
        assert_eq!(state(), State::Unsupported);
        assert!(totals().is_empty());
    }
}
