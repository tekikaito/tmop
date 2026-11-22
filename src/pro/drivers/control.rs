//! Starting, stopping and restarting services from the Services page.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | Start | `StartServiceW` | `systemctl start` | `launchctl kickstart` |
//! | Stop | `ControlService(SERVICE_CONTROL_STOP)`, running dependents first | `systemctl stop` | `launchctl kill SIGTERM` (not for KeepAlive jobs) |
//! | Restart | Stop, then Start (and the dependents it stopped) | `systemctl restart` | `launchctl kickstart -k` |
//! | needs | administrator | root for system units | root for daemons |
//!
//! Everything that decides what is offered, what the confirmation says, which command line runs
//! and how long to wait is a pure function here ([`offer`], [`confirm_text`],
//! [`systemctl_args`], [`launchctl_args`], [`wait_for`], [`after_note`]); [`run`] is the thin
//! shell that acts on the real system. TMOP never asks for more rights: an action that needs them
//! is greyed out with the reason.
#![cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]

use super::Service;
use crate::platform::Os;
use std::time::Duration;

/// What can be done to a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Start, Action::Stop, Action::Restart];

    pub fn label(self) -> &'static str {
        match self {
            Action::Start => "Start",
            Action::Stop => "Stop",
            Action::Restart => "Restart",
        }
    }

    /// The lower-case verb: "start", "stop", "restart".
    pub fn verb(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
        }
    }

    /// What the page says while it runs: "Starting" ...
    pub fn progress(self) -> &'static str {
        match self {
            Action::Start => "Starting",
            Action::Stop => "Stopping",
            Action::Restart => "Restarting",
        }
    }

    /// Stop and Restart are confirmed first; Start is not.
    pub fn needs_confirm(self) -> bool {
        self != Action::Start
    }
}

/// How TMOP can control a service (filled in by the reader, not shown).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Control {
    /// Nothing TMOP can do here (drivers, modules, kexts, unsupported systems): no buttons.
    #[default]
    None,
    /// Listed, but not controlled by TMOP, for this reason.
    Fixed(&'static str),
    /// Windows: a Win32 service and its `dwControlsAccepted` bits.
    Scm { accepted: u32 },
    /// Linux: a systemd unit (the full name, `cron.service`); `user` for `systemctl --user`.
    /// `can_start` / `can_stop` are systemd's `CanStart` / `CanStop` properties.
    Systemd { unit: String, user: bool, can_start: bool, can_stop: bool },
    /// macOS: a launchd job with a job file; `daemon` for the system domain, `keep_alive` when
    /// launchd brings it back as soon as it exits.
    Launchd { label: String, daemon: bool, keep_alive: bool },
}

/// Whether this build can control services at all (else the buttons are not shown).
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux", target_os = "macos"));

/// Whether an action is shown, and why it is greyed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Not shown: the platform or the row has no such action.
    Hidden,
    Greyed(&'static str),
    Offered,
}

/// `SERVICE_ACCEPT_STOP` in `dwControlsAccepted`.
pub const ACCEPT_STOP: u32 = 0x1;

const PENDING: &str = "Wait until it has finished starting or stopping";
const NOT_RUNNING: &str = "Not running";

/// What TMOP may do with `svc`. `privileged`: TMOP runs as administrator (Windows) or root.
pub fn offer(svc: &Service, action: Action, privileged: bool) -> Offer {
    let verdict = match &svc.control {
        Control::None => return Offer::Hidden,
        Control::Fixed(why) => Err(*why),
        Control::Scm { accepted } if privileged => scm_offer(svc.state, &svc.start, *accepted, action),
        Control::Scm { .. } => Err("Needs administrator"),
        Control::Systemd { user: false, .. } if !privileged => Err("Needs root"),
        Control::Systemd { can_start, can_stop, .. } => systemd_offer(svc.state, &svc.start, *can_start, *can_stop, action),
        Control::Launchd { daemon: true, .. } if !privileged => Err("Needs root"),
        Control::Launchd { keep_alive, .. } => launchd_offer(svc.state, *keep_alive, action),
    };
    match verdict {
        Ok(()) => Offer::Offered,
        Err(why) => Offer::Greyed(why),
    }
}

/// Windows: by SCM state, start type and the controls the service accepts right now.
fn scm_offer(state: &str, start: &str, accepted: u32, action: Action) -> Result<(), &'static str> {
    let stops = accepted & ACCEPT_STOP != 0;
    let disabled = start == "Disabled";
    match (action, state) {
        (_, "Starting" | "Stopping" | "Continuing" | "Pausing") => Err(PENDING),
        (Action::Start, "Stopped") if disabled => Err("Its start type is Disabled"),
        (Action::Start, "Stopped") => Ok(()),
        (Action::Start, _) => Err("Already running"),
        (_, "Stopped") => Err(NOT_RUNNING),
        (_, "Running" | "Paused") if !stops => Err("The service does not accept Stop"),
        (Action::Restart, _) if disabled => Err("Its start type is Disabled: it could not start again"),
        (_, "Running" | "Paused") => Ok(()),
        _ => Err("The system reports an unknown state"),
    }
}

/// Linux: by unit state, masking and systemd's CanStart / CanStop.
fn systemd_offer(state: &str, start: &str, can_start: bool, can_stop: bool, action: Action) -> Result<(), &'static str> {
    let active = matches!(state, "Running" | "Exited" | "Active");
    let masked = start == "Masked";
    match action {
        _ if matches!(state, "Starting" | "Stopping" | "Reloading") => Err(PENDING),
        Action::Start if active => Err("Already active"),
        Action::Start if state == "Unknown" => Err("systemd reports an unknown state"),
        Action::Start if masked => Err("Masked: systemd refuses to start it"),
        Action::Start if !can_start => Err("The unit refuses to be started by hand"),
        Action::Start => Ok(()),
        _ if !active => Err("Not active"),
        _ if !can_stop => Err("The unit refuses to be stopped by hand"),
        Action::Restart if masked => Err("Masked: systemd refuses to start it"),
        Action::Restart if !can_start => Err("The unit refuses to be started by hand"),
        _ => Ok(()),
    }
}

/// macOS: by launchd state. Only loaded jobs can be kickstarted; TMOP never loads or unloads one.
fn launchd_offer(state: &str, keep_alive: bool, action: Action) -> Result<(), &'static str> {
    match (action, state) {
        (_, "Not loaded") => Err("Not loaded in launchd; TMOP does not load jobs"),
        (_, "Unknown") => Err("launchd does not show its state to TMOP"),
        (Action::Start, "Running") => Err("Already running"),
        (Action::Start, _) => Ok(()),
        (_, s) if s != "Running" => Err(NOT_RUNNING),
        (Action::Stop, _) if keep_alive => Err("launchd starts it again at once (KeepAlive); TMOP does not unload jobs"),
        _ => Ok(()),
    }
}

/// The toolbar button for the selected service (None: nothing is selected).
pub fn toolbar_offer(svc: Option<&Service>, action: Action, privileged: bool, busy: bool) -> Offer {
    let Some(svc) = svc else { return Offer::Greyed("Select a service first") };
    match offer(svc, action, privileged) {
        Offer::Offered if busy => Offer::Greyed("Another service action is running"),
        o => o,
    }
}

/// Title, paragraph and button label of the Stop / Restart confirmation. `dependents` are the
/// running services the system says stop with it (display names; Windows only).
pub fn confirm_text(action: Action, svc: &Service, dependents: &[String], os: Os) -> (String, String, String) {
    let title_name = if svc.display.is_empty() || os != Os::Windows { &svc.name } else { &svc.display };
    let title = format!("{} **{}**?", action.label(), title_name);
    let mut body = match action {
        Action::Restart => format!("{} is stopped and started again.", svc.name),
        _ => format!("{} is stopped. Programs that use it lose it until it is started again.", svc.name),
    };
    match os {
        Os::Windows if !dependents.is_empty() => {
            let n = dependents.len();
            let them = if n == 1 { "This running service depends on it and stops" } else { "These running services depend on it and stop" };
            body.push_str(&format!(" {them} first: {}.", dependents.join(", ")));
            if action == Action::Restart {
                body.push_str(if n == 1 { " It is started again afterwards." } else { " They are started again afterwards." });
            }
        }
        Os::Linux => body.push_str(" systemd also stops the units that require this one."),
        Os::MacOs if action == Action::Stop => body.push_str(" launchd sends it SIGTERM. launchd may start it again when something asks for it."),
        Os::MacOs => body.push_str(" launchd kills the running process and starts it again."),
        _ => {}
    }
    (title, body, format!("{} service", action.label()))
}

// ---- Linux and macOS: command lines ----

/// `systemctl` arguments for `action` on `unit`. `--no-ask-password`: never a polkit prompt.
pub fn systemctl_args(action: Action, unit: &str, user: bool) -> Vec<String> {
    let mut args = Vec::new();
    if user {
        args.push("--user".to_string());
    }
    args.extend([action.verb().to_string(), "--no-ask-password".into(), "--".into(), unit.to_string()]);
    args
}

/// `launchctl` arguments for `action` on the job `label`, in the system domain (daemons) or the
/// user's GUI login domain `gui/<uid>` (agents; the domain `launchctl list` shows a user).
///
/// - Start: `kickstart`, which runs a loaded job now, whatever its launch conditions.
/// - Restart: `kickstart -k`, which kills the running instance first.
/// - Stop: `kill SIGTERM`, the signal launchd itself sends to stop a job. The job stays loaded,
///   so it can be started again (and launchd may start an on-demand job when asked for it). It is
///   not offered for KeepAlive jobs, which launchd starts again at once. `bootout` is never used:
///   it unloads the job, and only a `bootstrap` with the plist path would bring it back.
pub fn launchctl_args(action: Action, label: &str, daemon: bool, uid: u32) -> Vec<String> {
    let target = if daemon { format!("system/{label}") } else { format!("gui/{uid}/{label}") };
    let mut args: Vec<String> = match action {
        Action::Start => vec!["kickstart".into()],
        Action::Restart => vec!["kickstart".into(), "-k".into()],
        Action::Stop => vec!["kill".into(), "SIGTERM".into()],
    };
    args.push(target);
    args
}

/// The error of a command that ran and failed: everything it wrote to stderr (else stdout).
pub fn command_error(cmd: &str, code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let text = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let how = code.map(|c| format!("exit code {c}")).unwrap_or_else(|| "a signal".into());
    if lines.is_empty() {
        format!("{cmd} failed with {how}")
    } else {
        format!("{}\n({cmd}, {how})", lines.join("\n"))
    }
}

// ---- Windows: waiting for a pending state ----

/// The parts of `SERVICE_STATUS_PROCESS` the wait looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScmStatus {
    /// `dwCurrentState`: 1 stopped, 2 start pending, 3 stop pending, 4 running ...
    pub state: u32,
    pub checkpoint: u32,
    pub wait_hint_ms: u32,
    pub exit_code: u32,
    pub specific_exit: u32,
}

pub const STOPPED: u32 = 1;
pub const START_PENDING: u32 = 2;
pub const STOP_PENDING: u32 = 3;
pub const RUNNING: u32 = 4;

/// Longest wait for one service, however much its wait hint asks for.
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// The longest a service may go without advancing its checkpoint, whatever its hint.
const MAX_STALL: Duration = Duration::from_secs(30);
const MIN_STALL: Duration = Duration::from_secs(2);
/// A state that is neither the target nor pending counts as the end only after this long (a
/// service may report its old state for a moment right after the request).
const GRACE: Duration = Duration::from_secs(1);

/// How the wait ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The target state was reached.
    Reached,
    /// Still pending when the time ran out.
    Pending { state: u32, waited: Duration },
    /// It settled in another state (a start that ended stopped).
    Ended(ScmStatus),
}

/// Time between two status queries: a tenth of the wait hint, between 100 ms and 1 s (as in
/// Microsoft's sample).
pub fn poll_interval(wait_hint_ms: u32) -> Duration {
    (Duration::from_millis(wait_hint_ms as u64) / 10).clamp(Duration::from_millis(100), Duration::from_secs(1))
}

/// Waits until `query` reports `target`, while the state is `pending` and the checkpoint keeps
/// advancing within the wait hint (capped), at most [`MAX_WAIT`]. `elapsed` is the time since the
/// wait began and `sleep` waits; both are injected so the logic is tested without a clock.
pub fn wait_for(target: u32, pending: u32, query: &mut dyn FnMut() -> Result<ScmStatus, String>, elapsed: &mut dyn FnMut() -> Duration, sleep: &mut dyn FnMut(Duration)) -> Result<Settled, String> {
    let mut last_checkpoint = None;
    let mut progress_at = elapsed();
    loop {
        let s = query()?;
        let now = elapsed();
        if s.state == target {
            return Ok(Settled::Reached);
        }
        if s.state != pending && now >= GRACE {
            return Ok(Settled::Ended(s));
        }
        if last_checkpoint != Some(s.checkpoint) {
            last_checkpoint = Some(s.checkpoint);
            progress_at = now;
        }
        let stall = Duration::from_millis(s.wait_hint_ms as u64).clamp(MIN_STALL, MAX_STALL);
        if now >= MAX_WAIT || now.saturating_sub(progress_at) > stall {
            return Ok(Settled::Pending { state: s.state, waited: now });
        }
        sleep(poll_interval(s.wait_hint_ms));
    }
}

/// The sentence for a wait that ran out of time, or the error of one that ended elsewhere.
/// `exit_message` turns a Win32 exit code into the system's text.
pub fn settled_text(name: &str, settled: Settled, exit_message: &dyn Fn(u32) -> String) -> Result<Option<String>, String> {
    match settled {
        Settled::Reached => Ok(None),
        Settled::Pending { state, waited } => {
            let doing = if state == START_PENDING { "starting" } else if state == STOP_PENDING { "stopping" } else { super::state_name(state) };
            Ok(Some(format!("{name} is still {} after {} s", doing.to_lowercase(), waited.as_secs())))
        }
        Settled::Ended(s) => {
            let mut e = format!("{name} is {} instead", super::state_name(s.state).to_lowercase());
            // 1066 ERROR_SERVICE_SPECIFIC_ERROR: the service's own code is the one that matters.
            match (s.exit_code, s.specific_exit) {
                (0, _) => {}
                (1066, code) => e.push_str(&format!(" (service-specific error {code})")),
                (code, _) => e.push_str(&format!(": {} (error {code})", exit_message(code))),
            }
            Err(e)
        }
    }
}

// ---- After the action ----

/// What an action that the system accepted reports: a sentence when it did not settle in time or
/// left something undone, else None.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Done {
    pub note: Option<String>,
}

/// The page subtitle after an action, from the list read after it.
pub fn after_note(name: &str, action: Action, done: &Done, list: &[Service]) -> String {
    let now = list.iter().find(|s| s.name == name);
    let state = match now {
        Some(s) => format!("{name} now reads as {}", s.state.to_lowercase()),
        None => format!("{name} is no longer listed"),
    };
    match &done.note {
        Some(note) => format!("{}: {note}; {state}", action.label()),
        None => state,
    }
}

// ---- The shell around the system ----

/// Runs `action` on `svc` and waits for it to settle (bounded). The caller reads the list again
/// afterwards and shows what the system says.
pub fn run(svc: &Service, action: Action) -> Result<Done, String> {
    if let Offer::Greyed(why) = offer(svc, action, crate::platform::is_elevated()) {
        return Err(why.to_string());
    }
    run_os(svc, action)
}

/// The running services that stop with `svc` (display names), for the confirmation. Empty where
/// the system does not report them this way (Linux, macOS).
pub fn dependents(svc: &Service) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    if let Control::Scm { .. } = svc.control {
        return scm::dependents(&svc.name).map(|d| d.into_iter().map(|(name, display)| if display.is_empty() { name } else { display }).collect());
    }
    let _ = svc;
    Ok(Vec::new())
}

#[cfg(windows)]
fn run_os(svc: &Service, action: Action) -> Result<Done, String> {
    match action {
        Action::Start => scm::start(&svc.name).map(|note| Done { note }),
        Action::Stop => scm::stop(&svc.name).map(|(_, note)| Done { note }),
        Action::Restart => scm::restart(&svc.name),
    }
}

#[cfg(target_os = "linux")]
fn run_os(svc: &Service, action: Action) -> Result<Done, String> {
    let Control::Systemd { unit, user, .. } = &svc.control else { return Err("TMOP cannot control this entry".into()) };
    let args = systemctl_args(action, unit, *user);
    run_command("systemctl", &args, Duration::from_secs(100))
}

#[cfg(target_os = "macos")]
fn run_os(svc: &Service, action: Action) -> Result<Done, String> {
    let Control::Launchd { label, daemon, .. } = &svc.control else { return Err("TMOP cannot control this entry".into()) };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    run_command("launchctl", &launchctl_args(action, label, *daemon, uid), Duration::from_secs(30))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn run_os(_svc: &Service, _action: Action) -> Result<Done, String> {
    Err(format!("services cannot be controlled on {}", crate::platform::os_name()))
}

/// Runs a control command; a timeout is not an error (the system may still be working on it).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_command(cmd: &str, args: &[String], timeout: Duration) -> Result<Done, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::platform::run_capture_any(cmd, &args, timeout) {
        Ok((Some(0), _, _)) => Ok(Done::default()),
        Ok((code, out, err)) => Err(command_error(cmd, code, &out, &err)),
        Err(e) if e.starts_with(&format!("{cmd} timed out")) => Ok(Done { note: Some(format!("{cmd} did not finish within {} s; the system may still be working on it", timeout.as_secs())) }),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
mod scm {
    //! The Service Control Manager calls. Every handle is closed by [`H`] on every path.
    use super::*;
    use crate::pro::winutil::wide;
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::System::Services::{
        CloseServiceHandle, ControlService, EnumDependentServicesW, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, StartServiceW, ENUM_SERVICE_STATUSW, SC_HANDLE, SC_STATUS_PROCESS_INFO,
        SERVICE_ACTIVE, SERVICE_CONTROL_STOP, SERVICE_STATUS, SERVICE_STATUS_PROCESS,
    };

    const SC_MANAGER_CONNECT: u32 = 0x1;
    const SERVICE_QUERY_STATUS: u32 = 0x4;
    const SERVICE_ENUMERATE_DEPENDENTS: u32 = 0x8;
    const SERVICE_START: u32 = 0x10;
    const SERVICE_STOP: u32 = 0x20;
    const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;
    const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;
    const ERROR_MORE_DATA: u32 = 234;

    struct H(SC_HANDLE);
    impl Drop for H {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenSCManagerW / OpenServiceW and is closed once, here.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    fn is(e: &windows::core::Error, code: u32) -> bool {
        e.code() == HRESULT::from_win32(code)
    }

    fn manager() -> Result<H, String> {
        // SAFETY: the local SCM and its default database (null names); connect right only.
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.map(H).map_err(|e| format!("Could not open the Service Control Manager: {}", e.message()))
    }

    fn open(scm: &H, name: &str, access: u32) -> Result<H, String> {
        let w = wide(name);
        // SAFETY: `w` is a NUL-terminated name that outlives the call; `scm` is open.
        unsafe { OpenServiceW(scm.0, PCWSTR(w.as_ptr()), access) }.map(H).map_err(|e| format!("Could not open {name}: {}", e.message()))
    }

    fn status(h: &H) -> Result<ScmStatus, String> {
        let mut st = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        // SAFETY: the buffer is exactly one SERVICE_STATUS_PROCESS, which SC_STATUS_PROCESS_INFO
        // writes; it lives on this stack frame for the call.
        let buf = unsafe { std::slice::from_raw_parts_mut(&mut st as *mut SERVICE_STATUS_PROCESS as *mut u8, std::mem::size_of::<SERVICE_STATUS_PROCESS>()) };
        // SAFETY: `h` is open with SERVICE_QUERY_STATUS; `buf` and `needed` are valid.
        unsafe { QueryServiceStatusEx(h.0, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed) }.map_err(|e| format!("QueryServiceStatusEx: {}", e.message()))?;
        Ok(ScmStatus { state: st.dwCurrentState.0, checkpoint: st.dwCheckPoint, wait_hint_ms: st.dwWaitHint, exit_code: st.dwWin32ExitCode, specific_exit: st.dwServiceSpecificExitCode })
    }

    fn wait(h: &H, name: &str, target: u32, pending: u32) -> Result<Option<String>, String> {
        let t0 = std::time::Instant::now();
        let settled = wait_for(target, pending, &mut || status(h), &mut || t0.elapsed(), &mut std::thread::sleep)?;
        settled_text(name, settled, &|code| windows::core::Error::from(HRESULT::from_win32(code)).message())
    }

    /// (name, display name) of the running services that depend on `name`, in the order they are
    /// to be stopped (`EnumDependentServicesW` returns them in reverse start order).
    pub fn dependents(name: &str) -> Result<Vec<(String, String)>, String> {
        let scm = manager()?;
        let h = open(&scm, name, SERVICE_ENUMERATE_DEPENDENTS)?;
        dependents_of(&h)
    }

    fn dependents_of(h: &H) -> Result<Vec<(String, String)>, String> {
        let (mut needed, mut count) = (0u32, 0u32);
        // SAFETY: a size query with no buffer; it fails with ERROR_MORE_DATA when there are any.
        match unsafe { EnumDependentServicesW(h.0, SERVICE_ACTIVE, None, 0, &mut needed, &mut count) } {
            Ok(()) => return Ok(Vec::new()),
            Err(e) if !is(&e, ERROR_MORE_DATA) || needed == 0 || needed > 1 << 20 => return Err(format!("Could not list the dependent services: {}", e.message())),
            Err(_) => {}
        }
        // u64 elements keep the buffer aligned for ENUM_SERVICE_STATUSW.
        let mut buf = vec![0u64; needed as usize / 8 + 1];
        let items = buf.as_mut_ptr() as *mut ENUM_SERVICE_STATUSW;
        // SAFETY: the buffer holds `needed` bytes, as the size query asked.
        unsafe { EnumDependentServicesW(h.0, SERVICE_ACTIVE, Some(items), (buf.len() * 8) as u32, &mut needed, &mut count) }.map_err(|e| format!("EnumDependentServices: {}", e.message()))?;
        Ok((0..count as usize)
            .map(|i| {
                // SAFETY: `count` entries were written at the start of the buffer; their strings point
                // into the same buffer, which is alive here.
                let it = unsafe { &*items.add(i) };
                unsafe { (it.lpServiceName.to_string().unwrap_or_default(), it.lpDisplayName.to_string().unwrap_or_default()) }
            })
            .collect())
    }

    /// Sends Stop to an open service; a service that is already stopped is fine.
    fn send_stop(h: &H, name: &str) -> Result<(), String> {
        let mut st = SERVICE_STATUS::default();
        // SAFETY: `h` is open with SERVICE_STOP; `st` receives the latest status.
        match unsafe { ControlService(h.0, SERVICE_CONTROL_STOP, &mut st) } {
            Ok(()) => Ok(()),
            Err(e) if is(&e, ERROR_SERVICE_NOT_ACTIVE) => Ok(()),
            Err(e) => Err(format!("Could not stop {name}: {}", e.message())),
        }
    }

    fn send_start(h: &H, name: &str) -> Result<(), String> {
        // SAFETY: `h` is open with SERVICE_START; no arguments are passed to the service.
        match unsafe { StartServiceW(h.0, None) } {
            Ok(()) => Ok(()),
            Err(e) if is(&e, ERROR_SERVICE_ALREADY_RUNNING) => Ok(()),
            Err(e) => Err(format!("Could not start {name}: {}", e.message())),
        }
    }

    pub fn start(name: &str) -> Result<Option<String>, String> {
        let scm = manager()?;
        let h = open(&scm, name, SERVICE_START | SERVICE_QUERY_STATUS)?;
        send_start(&h, name)?;
        wait(&h, name, RUNNING, START_PENDING)
    }

    /// Stops the running dependents (in the order the system gives), then `name`. Returns the
    /// dependents it stopped and a note when something did not settle in time.
    pub fn stop(name: &str) -> Result<(Vec<String>, Option<String>), String> {
        let scm = manager()?;
        let h = open(&scm, name, SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS)?;
        let mut stopped = Vec::new();
        for (dep, _) in dependents_of(&h)? {
            let d = open(&scm, &dep, SERVICE_STOP | SERVICE_QUERY_STATUS)?;
            send_stop(&d, &dep)?;
            stopped.push(dep.clone());
            if let Some(note) = wait(&d, &dep, STOPPED, STOP_PENDING)? {
                return Ok((stopped, Some(format!("{note}; {name} was not stopped"))));
            }
        }
        send_stop(&h, name)?;
        Ok((stopped, wait(&h, name, STOPPED, STOP_PENDING)?))
    }

    /// Stop (with the dependents), Start, then the dependents again in start order.
    pub fn restart(name: &str) -> Result<super::Done, String> {
        let (deps, note) = stop(name)?;
        if note.is_some() {
            return Ok(super::Done { note: note.map(|n| format!("{n}; it was not started again")) });
        }
        let mut notes: Vec<String> = start(name)?.into_iter().collect();
        if notes.is_empty() {
            let scm = manager()?;
            for dep in deps.iter().rev() {
                let r = open(&scm, dep, SERVICE_START | SERVICE_QUERY_STATUS).and_then(|d| {
                    send_start(&d, dep)?;
                    wait(&d, dep, RUNNING, START_PENDING)
                });
                match r {
                    Ok(None) => {}
                    Ok(Some(n)) => notes.push(n),
                    Err(e) => notes.push(format!("{dep} did not start again: {e}")),
                }
            }
        }
        Ok(super::Done { note: (!notes.is_empty()).then(|| notes.join("; ")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn svc(state: &'static str, start: &str, control: Control) -> Service {
        Service { name: "svc".into(), display: "The service".into(), state, running: state == "Running", start: start.into(), control, ..Default::default() }
    }

    fn offers(s: &Service, privileged: bool) -> Vec<Offer> {
        Action::ALL.iter().map(|a| offer(s, *a, privileged)).collect()
    }

    const STOPPABLE: Control = Control::Scm { accepted: ACCEPT_STOP };

    #[test]
    fn windows_offers_follow_state_and_accepted_controls() {
        use Offer::*;
        assert_eq!(offers(&svc("Running", "Automatic", STOPPABLE), true), [Greyed("Already running"), Offered, Offered]);
        assert_eq!(offers(&svc("Stopped", "Manual", Control::Scm { accepted: 0 }), true), [Offered, Greyed(NOT_RUNNING), Greyed(NOT_RUNNING)]);
        assert_eq!(offers(&svc("Paused", "Manual", STOPPABLE), true), [Greyed("Already running"), Offered, Offered]);
        // A running service that does not accept Stop is never offered Stop (nor Restart).
        let fixed = offers(&svc("Running", "Automatic", Control::Scm { accepted: 0 }), true);
        assert_eq!(fixed[1..], [Greyed("The service does not accept Stop"), Greyed("The service does not accept Stop")]);
        // Pending states wait; a disabled one does not start (and is not restarted).
        assert!(offers(&svc("Starting", "Manual", STOPPABLE), true).iter().all(|o| *o == Greyed(PENDING)));
        assert!(offers(&svc("Stopping", "Manual", STOPPABLE), true).iter().all(|o| *o == Greyed(PENDING)));
        assert_eq!(offer(&svc("Stopped", "Disabled", STOPPABLE), Action::Start, true), Greyed("Its start type is Disabled"));
        assert_eq!(offers(&svc("Running", "Disabled", STOPPABLE), true)[1..], [Offered, Greyed("Its start type is Disabled: it could not start again")]);
        // Not elevated: everything greyed with one reason.
        assert!(offers(&svc("Running", "Automatic", STOPPABLE), false).iter().all(|o| *o == Greyed("Needs administrator")));
    }

    fn unit(can_start: bool, can_stop: bool, user: bool) -> Control {
        Control::Systemd { unit: "svc.service".into(), user, can_start, can_stop }
    }

    #[test]
    fn systemd_offers() {
        use Offer::*;
        assert_eq!(offers(&svc("Running", "Enabled", unit(true, true, false)), true), [Greyed("Already active"), Offered, Offered]);
        assert_eq!(offers(&svc("Exited", "Enabled", unit(true, true, false)), true), [Greyed("Already active"), Offered, Offered]);
        assert_eq!(offers(&svc("Stopped", "Disabled", unit(true, true, false)), true), [Offered, Greyed("Not active"), Greyed("Not active")]);
        assert_eq!(offers(&svc("Failed", "Enabled", unit(true, true, false)), true)[0], Offered);
        assert_eq!(offer(&svc("Stopped", "Masked", unit(false, true, false)), Action::Start, true), Greyed("Masked: systemd refuses to start it"));
        assert_eq!(offer(&svc("Stopped", "Static", unit(false, true, false)), Action::Start, true), Greyed("The unit refuses to be started by hand"));
        assert_eq!(offers(&svc("Running", "Static", unit(true, false, false)), true)[1..], [Greyed("The unit refuses to be stopped by hand"), Greyed("The unit refuses to be stopped by hand")]);
        assert_eq!(offer(&svc("Running", "Static", unit(false, true, false)), Action::Restart, true), Greyed("The unit refuses to be started by hand"));
        assert!(offers(&svc("Reloading", "Enabled", unit(true, true, false)), true).iter().all(|o| *o == Greyed(PENDING)));
        // System units need root; user units do not.
        assert!(offers(&svc("Running", "Enabled", unit(true, true, false)), false).iter().all(|o| *o == Greyed("Needs root")));
        assert_eq!(offers(&svc("Running", "Enabled", unit(true, true, true)), false), [Greyed("Already active"), Offered, Offered]);
    }

    fn job(daemon: bool, keep_alive: bool) -> Control {
        Control::Launchd { label: "com.example.job".into(), daemon, keep_alive }
    }

    #[test]
    fn launchd_offers() {
        use Offer::*;
        assert_eq!(offers(&svc("Running", "On demand", job(false, false)), false), [Greyed("Already running"), Offered, Offered]);
        assert_eq!(offers(&svc("Loaded", "On demand", job(false, false)), false), [Offered, Greyed(NOT_RUNNING), Greyed(NOT_RUNNING)]);
        assert_eq!(offers(&svc("Failed", "On demand", job(false, false)), false)[0], Offered);
        // KeepAlive: Stop would not stop it, so it is not offered; Restart is.
        let ka = offers(&svc("Running", "At load", job(false, true)), false);
        assert_eq!(ka[1..], [Greyed("launchd starts it again at once (KeepAlive); TMOP does not unload jobs"), Offered]);
        assert!(offers(&svc("Not loaded", "On demand", job(false, false)), false).iter().all(|o| *o == Greyed("Not loaded in launchd; TMOP does not load jobs")));
        assert!(offers(&svc("Running", "At load", job(true, false)), false).iter().all(|o| *o == Greyed("Needs root")));
        assert_eq!(offers(&svc("Running", "At load", job(true, false)), true)[1], Offered);
        assert!(offers(&svc("Loaded", "", Control::Fixed("No job file")), true).iter().all(|o| *o == Greyed("No job file")));
    }

    #[test]
    fn nothing_is_offered_without_a_control_and_the_toolbar_needs_a_selection() {
        assert!(offers(&svc("Running", "Automatic", Control::None), true).iter().all(|o| *o == Offer::Hidden));
        assert_eq!(toolbar_offer(None, Action::Stop, true, false), Offer::Greyed("Select a service first"));
        let s = svc("Running", "Automatic", STOPPABLE);
        assert_eq!(toolbar_offer(Some(&s), Action::Stop, true, false), Offer::Offered);
        assert_eq!(toolbar_offer(Some(&s), Action::Stop, true, true), Offer::Greyed("Another service action is running"));
        assert_eq!(toolbar_offer(Some(&s), Action::Start, true, true), Offer::Greyed("Already running"));
        assert!(!Action::Start.needs_confirm() && Action::Stop.needs_confirm() && Action::Restart.needs_confirm());
    }

    #[test]
    fn confirmations_name_the_service_and_its_dependents() {
        let s = Service { name: "Spooler".into(), display: "Print Spooler".into(), ..Default::default() };
        let (title, body, button) = confirm_text(Action::Stop, &s, &[], Os::Windows);
        assert_eq!((title.as_str(), button.as_str()), ("Stop **Print Spooler**?", "Stop service"));
        assert_eq!(body, "Spooler is stopped. Programs that use it lose it until it is started again.");
        let (_, body, _) = confirm_text(Action::Stop, &s, &["Fax".into()], Os::Windows);
        assert!(body.ends_with("This running service depends on it and stops first: Fax."), "{body}");
        let (title, body, button) = confirm_text(Action::Restart, &s, &["Fax".into(), "Other".into()], Os::Windows);
        assert_eq!((title.as_str(), button.as_str()), ("Restart **Print Spooler**?", "Restart service"));
        assert_eq!(body, "Spooler is stopped and started again. These running services depend on it and stop first: Fax, Other. They are started again afterwards.");
        let cron = Service { name: "cron".into(), display: "Regular background program processing daemon".into(), ..Default::default() };
        let (title, body, _) = confirm_text(Action::Stop, &cron, &[], Os::Linux);
        assert_eq!(title, "Stop **cron**?");
        assert!(body.ends_with("systemd also stops the units that require this one."));
        let (_, body, _) = confirm_text(Action::Stop, &cron, &[], Os::MacOs);
        assert!(body.contains("SIGTERM"));
        assert!(confirm_text(Action::Restart, &cron, &[], Os::MacOs).1.ends_with("kills the running process and starts it again."));
    }

    #[test]
    fn command_lines() {
        assert_eq!(systemctl_args(Action::Start, "cron.service", false), ["start", "--no-ask-password", "--", "cron.service"]);
        assert_eq!(systemctl_args(Action::Stop, "-odd.service", false), ["stop", "--no-ask-password", "--", "-odd.service"]);
        assert_eq!(systemctl_args(Action::Restart, "app.service", true), ["--user", "restart", "--no-ask-password", "--", "app.service"]);
        assert_eq!(launchctl_args(Action::Start, "com.example.agent", false, 501), ["kickstart", "gui/501/com.example.agent"]);
        assert_eq!(launchctl_args(Action::Restart, "com.example.agent", false, 501), ["kickstart", "-k", "gui/501/com.example.agent"]);
        assert_eq!(launchctl_args(Action::Stop, "com.example.agent", false, 501), ["kill", "SIGTERM", "gui/501/com.example.agent"]);
        assert_eq!(launchctl_args(Action::Restart, "org.example.daemon", true, 0), ["kickstart", "-k", "system/org.example.daemon"]);
        assert!(launchctl_args(Action::Stop, "x", true, 0).iter().all(|a| a != "bootout"));
    }

    #[test]
    fn command_errors_keep_the_systems_text() {
        let e = command_error("systemctl", Some(5), "", "Failed to start x.service: Unit x.service not found.\n");
        assert_eq!(e, "Failed to start x.service: Unit x.service not found.\n(systemctl, exit code 5)");
        assert_eq!(command_error("launchctl", Some(3), "Could not find service\n", ""), "Could not find service\n(launchctl, exit code 3)");
        assert_eq!(command_error("launchctl", None, "", ""), "launchctl failed with a signal");
    }

    /// Runs [`wait_for`] on scripted statuses with a fake clock that `sleep` advances.
    fn simulate(target: u32, pending: u32, script: &[(u32, u32, u32)]) -> (Result<Settled, String>, Duration, usize) {
        let now = Cell::new(Duration::ZERO);
        let queries = Cell::new(0usize);
        let mut query = || {
            let i = queries.get();
            queries.set(i + 1);
            let (state, checkpoint, wait_hint_ms) = script[i.min(script.len() - 1)];
            Ok(ScmStatus { state, checkpoint, wait_hint_ms, ..Default::default() })
        };
        let r = wait_for(target, pending, &mut query, &mut || now.get(), &mut |d| now.set(now.get() + d));
        (r, now.get(), queries.get())
    }

    #[test]
    fn waits_until_the_target_state() {
        let (r, t, n) = simulate(RUNNING, START_PENDING, &[(START_PENDING, 1, 2000), (START_PENDING, 2, 2000), (RUNNING, 0, 0)]);
        assert_eq!((r, n), (Ok(Settled::Reached), 3));
        assert_eq!(t, Duration::from_millis(400), "polls a tenth of the 2 s hint apart");
        assert_eq!(simulate(STOPPED, STOP_PENDING, &[(STOPPED, 0, 0)]).0, Ok(Settled::Reached));
    }

    #[test]
    fn gives_up_when_the_checkpoint_stalls_and_says_so() {
        // Hint 3 s, checkpoint never moves: gives up a little after 3 s.
        let (r, t, _) = simulate(STOPPED, STOP_PENDING, &[(STOP_PENDING, 7, 3000)]);
        assert!(matches!(r, Ok(Settled::Pending { state: STOP_PENDING, .. })), "{r:?}");
        assert!(t > Duration::from_secs(3) && t < Duration::from_secs(4), "{t:?}");
        // A huge hint is capped at 30 s without progress.
        let (_, t, _) = simulate(RUNNING, START_PENDING, &[(START_PENDING, 1, 600_000)]);
        assert!(t > Duration::from_secs(30) && t <= Duration::from_secs(32), "{t:?}");
        // A tiny hint still waits 2 s.
        let (_, t, _) = simulate(RUNNING, START_PENDING, &[(START_PENDING, 1, 0)]);
        assert!(t > Duration::from_secs(2) && t < Duration::from_secs(3), "{t:?}");
    }

    #[test]
    fn progress_keeps_waiting_up_to_the_cap() {
        // Checkpoint advances on every query: the overall cap ends it.
        let script: Vec<(u32, u32, u32)> = (0..10_000).map(|i| (START_PENDING, i, 10_000)).collect();
        let (r, t, _) = simulate(RUNNING, START_PENDING, &script);
        assert!(matches!(r, Ok(Settled::Pending { .. })));
        assert!(t >= MAX_WAIT && t <= MAX_WAIT + Duration::from_secs(1), "{t:?}");
    }

    #[test]
    fn another_state_ends_the_wait_after_the_grace() {
        // Right after Start a service may still read as stopped for a moment.
        let (r, _, _) = simulate(RUNNING, START_PENDING, &[(STOPPED, 0, 0), (START_PENDING, 1, 1000), (RUNNING, 0, 0)]);
        assert_eq!(r, Ok(Settled::Reached));
        // ... but one that stays stopped has failed to start.
        let (r, t, _) = simulate(RUNNING, START_PENDING, &[(STOPPED, 0, 0)]);
        assert!(matches!(r, Ok(Settled::Ended(ScmStatus { state: STOPPED, .. }))), "{r:?}");
        assert!(t >= GRACE && t < GRACE + Duration::from_millis(200));
        let err = |code: u32| format!("message {code}");
        assert_eq!(simulate(RUNNING, START_PENDING, &[(RUNNING, 0, 0)]).0.map(|s| settled_text("A", s, &err)), Ok(Ok(None)));
    }

    #[test]
    fn settled_texts() {
        let msg = |code: u32| format!("message {code}");
        assert_eq!(settled_text("A", Settled::Pending { state: STOP_PENDING, waited: Duration::from_secs(31) }, &msg), Ok(Some("A is still stopping after 31 s".into())));
        assert_eq!(settled_text("A", Settled::Pending { state: START_PENDING, waited: Duration::from_secs(60) }, &msg), Ok(Some("A is still starting after 60 s".into())));
        let ended = |exit_code, specific_exit| Settled::Ended(ScmStatus { state: STOPPED, exit_code, specific_exit, ..Default::default() });
        assert_eq!(settled_text("A", ended(1067, 0), &msg), Err("A is stopped instead: message 1067 (error 1067)".into()));
        assert_eq!(settled_text("A", ended(1066, 42), &msg), Err("A is stopped instead (service-specific error 42)".into()));
        assert_eq!(settled_text("A", ended(0, 0), &msg), Err("A is stopped instead".into()));
    }

    #[test]
    fn notes_say_what_the_list_read_after_shows() {
        let list = [Service { name: "A".into(), state: "Running", ..Default::default() }];
        assert_eq!(after_note("A", Action::Start, &Done::default(), &list), "A now reads as running");
        assert_eq!(after_note("B", Action::Stop, &Done::default(), &list), "B is no longer listed");
        let late = Done { note: Some("A is still stopping after 31 s".into()) };
        assert_eq!(after_note("A", Action::Stop, &late, &list), "Stop: A is still stopping after 31 s; A now reads as running");
    }

    // ---- Real-system tests: GitHub Actions runners only ----

    /// A line in the test log that `cargo test` does not capture, so the CI log shows it.
    #[allow(dead_code)]
    fn report(line: &str) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    /// The tests that start and stop real services run only on GitHub Actions runners
    /// (throwaway machines); anywhere else they return at once and say so.
    #[allow(dead_code)]
    fn on_ci_runner(test: &str) -> bool {
        let yes = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
        if !yes {
            report(&format!("{test}: skipped, it only runs on GitHub Actions runners"));
        }
        yes
    }

    /// Stops and starts one harmless running service through [`run`], chosen at run time from
    /// an allow-list, and restores its state in a guard however the test ends.
    #[cfg(windows)]
    #[test]
    fn ci_only_stop_and_start_a_windows_service() {
        const TEST: &str = "ci_only_stop_and_start_a_windows_service";
        if !on_ci_runner(TEST) {
            return;
        }
        if !crate::platform::is_elevated() {
            report(&format!("{TEST}: SKIPPED, the runner process is not elevated"));
            return;
        }
        /// Starts the service again if the test left it stopped.
        struct Restore(String);
        impl Drop for Restore {
            fn drop(&mut self) {
                let running = super::super::read(super::super::Kind::Services).ok().and_then(|l| l.into_iter().find(|s| s.name == self.0)).is_some_and(|s| s.state == "Running");
                if !running {
                    let r = scm::start(&self.0);
                    report(&format!("{TEST}: guard started {} again: {r:?}", self.0));
                }
            }
        }
        let find = |name: &str| super::super::read(super::super::Kind::Services).unwrap().into_iter().find(|s| s.name == name);
        let mut chosen = None;
        for name in ["Spooler", "W32Time", "TrkWks"] {
            let Some(s) = find(name) else {
                report(&format!("{TEST}: {name} does not exist on this runner"));
                continue;
            };
            let deps = dependents(&s);
            report(&format!("{TEST}: {name}: state {}, start {}, control {:?}, stop offered {:?}, running dependents {deps:?}", s.state, s.start, s.control, offer(&s, Action::Stop, true)));
            if offer(&s, Action::Stop, true) == Offer::Offered && offer(&s, Action::Restart, true) == Offer::Offered && deps.is_ok_and(|d| d.is_empty()) {
                chosen = Some(s);
                break;
            }
        }
        let Some(s) = chosen else {
            report(&format!("{TEST}: SKIPPED, no allow-listed service is running, accepts Stop and has no running dependents"));
            return;
        };
        let _restore = Restore(s.name.clone());
        let stopped = run(&s, Action::Stop);
        let now = find(&s.name).unwrap();
        report(&format!("{TEST}: Stop {} -> {stopped:?}; reads as {}", s.name, now.state));
        assert_eq!(stopped, Ok(Done::default()));
        assert_eq!(now.state, "Stopped");
        assert_eq!(offer(&now, Action::Start, true), Offer::Offered);
        let started = run(&now, Action::Start);
        let now = find(&s.name).unwrap();
        report(&format!("{TEST}: Start {} -> {started:?}; reads as {} (PID {})", s.name, now.state, now.pid));
        assert_eq!(started, Ok(Done::default()));
        assert_eq!(now.state, "Running");
        let before = now.pid;
        let restarted = run(&now, Action::Restart);
        let now = find(&s.name).unwrap();
        report(&format!("{TEST}: Restart {} -> {restarted:?}; reads as {} (PID {} before, {} after)", s.name, now.state, before, now.pid));
        assert_eq!(restarted, Ok(Done::default()));
        assert_eq!(now.state, "Running");
        // Starting what runs is refused before any call.
        assert_eq!(run(&now, Action::Start), Err("Already running".into()));
    }

    /// Starts, restarts and stops a throwaway unit of the test's own through [`run`]: a system
    /// unit in /run/systemd/system as root, else a user unit when the runner has a user manager.
    #[cfg(target_os = "linux")]
    #[test]
    fn ci_only_systemctl_start_restart_stop() {
        const TEST: &str = "ci_only_systemctl_start_restart_stop";
        if !on_ci_runner(TEST) {
            return;
        }
        use crate::platform::run_capture_any;
        let t = Duration::from_secs(30);
        if !std::path::Path::new("/run/systemd/system").is_dir() {
            report(&format!("{TEST}: SKIPPED, systemd is not PID 1 on this runner"));
            return;
        }
        let root = crate::platform::is_elevated();
        let user = !root;
        let dir = if root {
            std::path::PathBuf::from("/run/systemd/system")
        } else {
            match run_capture_any("systemctl", &["--user", "show-environment"], t) {
                Ok((Some(0), _, _)) => {}
                other => {
                    report(&format!("{TEST}: SKIPPED, not root and no usable systemd user manager: systemctl --user show-environment -> {other:?}"));
                    return;
                }
            }
            let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
                report(&format!("{TEST}: SKIPPED, not root and XDG_RUNTIME_DIR is unset"));
                return;
            };
            std::path::PathBuf::from(runtime).join("systemd/user")
        };
        let unit = format!("tmop-ci-test-{}.service", std::process::id());
        let path = dir.join(&unit);
        let ctl = |args: &[&str]| {
            let mut all: Vec<&str> = if user { vec!["--user"] } else { vec![] };
            all.extend_from_slice(args);
            run_capture_any("systemctl", &all, t)
        };
        /// Stops and removes the unit however the test ends.
        struct Cleanup<'a> {
            path: &'a std::path::Path,
            unit: &'a str,
            user: bool,
        }
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let scope: &[&str] = if self.user { &["--user"] } else { &[] };
                let t = Duration::from_secs(30);
                let _ = crate::platform::run_capture_any("systemctl", &[scope, &["stop", "--", self.unit]].concat(), t);
                let _ = std::fs::remove_file(self.path);
                let _ = crate::platform::run_capture_any("systemctl", &[scope, &["daemon-reload"]].concat(), t);
            }
        }
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, "[Unit]\nDescription=TMOP CI test unit\n\n[Service]\nExecStart=/bin/sleep 600\n")) {
            report(&format!("{TEST}: SKIPPED, could not write {}: {e}", path.display()));
            return;
        }
        let _cleanup = Cleanup { path: &path, unit: &unit, user };
        let reload = ctl(&["daemon-reload"]);
        report(&format!("{TEST}: {} unit {unit} in {}; daemon-reload -> {reload:?}", if user { "user" } else { "system" }, dir.display()));
        let state = || {
            let (_, out, _) = ctl(&["show", "-p", "ActiveState,SubState,MainPID,CanStart,CanStop", "--", &unit]).unwrap();
            out.split_whitespace().map(String::from).collect::<Vec<_>>().join(" ")
        };
        let mut s = Service { name: unit.trim_end_matches(".service").into(), state: "Stopped", start: "Static".into(), control: Control::Systemd { unit: unit.clone(), user, can_start: true, can_stop: true }, ..Default::default() };
        report(&format!("{TEST}: before: {}", state()));
        let r = run(&s, Action::Start);
        let after = state();
        report(&format!("{TEST}: Start -> {r:?}; {after}"));
        assert_eq!(r, Ok(Done::default()));
        assert!(after.contains("ActiveState=active") && after.contains("SubState=running"), "{after}");
        s.state = "Running";
        let r = run(&s, Action::Restart);
        let restarted = state();
        report(&format!("{TEST}: Restart -> {r:?}; {restarted}"));
        assert_eq!(r, Ok(Done::default()));
        assert!(restarted.contains("ActiveState=active"), "{restarted}");
        assert_ne!(after.split(' ').find(|p| p.starts_with("MainPID=")), restarted.split(' ').find(|p| p.starts_with("MainPID=")), "a new main process");
        let r = run(&s, Action::Stop);
        let stopped = state();
        report(&format!("{TEST}: Stop -> {r:?}; {stopped}"));
        assert_eq!(r, Ok(Done::default()));
        assert!(stopped.contains("ActiveState=inactive"), "{stopped}");
        // systemctl's own error text comes back for a unit that does not exist.
        let missing = Service { control: Control::Systemd { unit: "tmop-ci-no-such-unit.service".into(), user, can_start: true, can_stop: true }, ..s.clone() };
        let missing = Service { state: "Stopped", ..missing };
        let e = run(&missing, Action::Start);
        report(&format!("{TEST}: Start of a missing unit -> {e:?}"));
        assert!(e.as_ref().is_err_and(|e| e.contains("tmop-ci-no-such-unit") && e.contains("(systemctl, exit code")), "{e:?}");
    }

    /// Kickstarts, restarts and stops a throwaway agent of the test's own (`/bin/sleep`) in the
    /// runner's GUI domain through [`run`]; skipped when the runner refuses to load it.
    #[cfg(target_os = "macos")]
    #[test]
    fn ci_only_launchctl_kickstart_and_kill() {
        const TEST: &str = "ci_only_launchctl_kickstart_and_kill";
        if !on_ci_runner(TEST) {
            return;
        }
        use crate::platform::run_capture_any;
        let t = Duration::from_secs(20);
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let label = format!("com.tekikaito.tmop.ci-test-{}", std::process::id());
        let target = format!("gui/{uid}/{label}");
        let dir = std::env::temp_dir().join(format!("tmop-ci-launchd-{}", std::process::id()));
        let plist = dir.join(format!("{label}.plist"));
        let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>/bin/sleep</string><string>600</string></array></dict></plist>\n");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&plist, xml).unwrap();
        /// Unloads the job and removes its file however the test ends.
        struct Cleanup(String, std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let r = crate::platform::run_capture_any("launchctl", &["bootout", &self.0], Duration::from_secs(20));
                report(&format!("ci_only_launchctl_kickstart_and_kill: cleanup bootout {} -> {r:?}", self.0));
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let boot = run_capture_any("launchctl", &["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()], t);
        report(&format!("{TEST}: bootstrap gui/{uid} {} -> {boot:?}", plist.display()));
        if !matches!(boot, Ok((Some(0), _, _))) {
            let _ = std::fs::remove_dir_all(&dir);
            report(&format!("{TEST}: SKIPPED, the runner refused to load a test agent into gui/{uid}"));
            return;
        }
        let _cleanup = Cleanup(target.clone(), dir.clone());
        let pid = || -> Option<u32> {
            let (_, out, _) = run_capture_any("launchctl", &["print", &target], t).ok()?;
            out.lines().find_map(|l| l.trim().strip_prefix("pid = ")).and_then(|p| p.trim().parse().ok())
        };
        let wait_pid = |want: bool| {
            for _ in 0..50 {
                if pid().is_some() == want {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            pid()
        };
        let mut s = Service { name: label.clone(), state: "Loaded", control: Control::Launchd { label: label.clone(), daemon: false, keep_alive: false }, ..Default::default() };
        report(&format!("{TEST}: before: pid {:?}", pid()));
        let r = run(&s, Action::Start);
        let first = wait_pid(true);
        report(&format!("{TEST}: Start (kickstart {target}) -> {r:?}; pid {first:?}"));
        assert_eq!(r, Ok(Done::default()));
        assert!(first.is_some(), "running after kickstart");
        s.state = "Running";
        let r = run(&s, Action::Restart);
        std::thread::sleep(Duration::from_millis(500));
        let second = wait_pid(true);
        report(&format!("{TEST}: Restart (kickstart -k {target}) -> {r:?}; pid {second:?}"));
        assert_eq!(r, Ok(Done::default()));
        assert!(second.is_some() && second != first, "a new process");
        let r = run(&s, Action::Stop);
        let after = wait_pid(false);
        report(&format!("{TEST}: Stop (kill SIGTERM {target}) -> {r:?}; pid {after:?}"));
        assert_eq!(r, Ok(Done::default()));
        assert_eq!(after, None, "no process after SIGTERM");
        let still = run_capture_any("launchctl", &["print", &target], t).map(|(c, _, _)| c);
        report(&format!("{TEST}: still loaded after Stop: launchctl print exit {still:?}"));
        assert_eq!(still, Ok(Some(0)), "Stop does not unload the job");
    }
}
