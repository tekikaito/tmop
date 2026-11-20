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
//! column) and is stopped when that ends or TMOP exits. A session left behind by a TMOP that
//! crashed is stopped and replaced when the next one starts.

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
    /// The session could not be started (the message says why).
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
            State::Failed(e) => Some(format!("The network counters could not start: {e}")),
        }
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

#[cfg(windows)]
pub use etw::{forget, set_wanted, state, totals, SESSION_NAME};

#[cfg(not(windows))]
pub fn set_wanted(_on: bool) {}

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
    use super::{count, parse, NetBytes, State};
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::thread::JoinHandle;
    use windows::core::{GUID, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Diagnostics::Etw::{
        CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP,
        EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
    };

    /// The name of TMOP's session (`logman query -ets` lists it while it runs).
    pub const SESSION_NAME: &str = "TMOP Network";
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
    static SESSION: Mutex<Option<Session>> = Mutex::new(None);
    static STATE: Mutex<Option<State>> = Mutex::new(None);

    struct Session {
        control: CONTROLTRACE_HANDLE,
        trace: PROCESSTRACE_HANDLE,
        consumer: Option<JoinHandle<()>>,
    }

    pub fn state() -> State {
        STATE.lock().ok().and_then(|s| s.clone()).unwrap_or(State::Off)
    }

    fn set_state(s: State) {
        if let Ok(mut g) = STATE.lock() {
            *g = Some(s);
        }
    }

    /// Starts the session (`on`) or stops it. Cheap when nothing changes, so it may be called
    /// every frame; after a failure it tries again only once it was turned off in between.
    pub fn set_wanted(on: bool) {
        let Ok(mut session) = SESSION.lock() else { return };
        match (on, session.is_some()) {
            (true, false) => {
                if matches!(state(), State::Off) {
                    let next = if !crate::platform::is_elevated() {
                        State::NeedsAdmin
                    } else {
                        match start() {
                            Ok(s) => {
                                *session = Some(s);
                                State::Running
                            }
                            Err(e) => State::Failed(e),
                        }
                    };
                    set_state(next);
                }
            }
            (false, true) => {
                if let Some(s) = session.take() {
                    stop(s);
                }
                set_state(State::Off);
            }
            (false, false) => set_state(State::Off),
            (true, true) => {}
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

    fn start() -> Result<Session, String> {
        let name = wide(SESSION_NAME);
        let mut props = Properties::new();
        let mut control = CONTROLTRACE_HANDLE::default();
        // SAFETY: `props` and `name` outlive the calls; the handles are TMOP's own.
        unsafe {
            let mut r = StartTraceW(&mut control, PCWSTR(name.as_ptr()), props.ptr());
            if r == ERROR_ALREADY_EXISTS {
                // Left behind by a TMOP that did not exit cleanly: stop it and start anew.
                let mut old = Properties::new();
                let _ = ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(name.as_ptr()), old.ptr(), EVENT_TRACE_CONTROL_STOP);
                props = Properties::new();
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
            Ok(Session { control, trace, consumer: Some(consumer) })
        }
    }

    fn stop(mut s: Session) {
        let mut props = Properties::new();
        // SAFETY: the handles are those `start` returned. Stopping the session ends ProcessTrace.
        unsafe {
            let _ = ControlTraceW(s.control, PCWSTR::null(), props.ptr(), EVENT_TRACE_CONTROL_STOP);
            let _ = CloseTrace(s.trace);
        }
        if let Some(t) = s.consumer.take() {
            let _ = t.join();
        }
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

        /// Elevated: the session starts, counts this process's traffic to another machine and
        /// stops again; loopback traffic is not counted. Not elevated: no session, no figures.
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
            // Give the provider a moment to come up.
            std::thread::sleep(Duration::from_millis(1500));
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
            set_wanted(false);
            assert_eq!(state(), State::Off);
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

    #[cfg(not(windows))]
    #[test]
    fn other_systems_have_no_counters() {
        set_wanted(true);
        assert_eq!(state(), State::Unsupported);
        assert!(totals().is_empty());
    }
}
