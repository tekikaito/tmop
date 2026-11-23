//! Interactive shells in containers.
//!
//! An [`ExecSession`] runs one exec instance with a pseudo-TTY on the engine runtime. The
//! hijacked stream from the engine carries the terminal output, which goes to the
//! [`Sink`]; keystrokes and size changes go the other way through channels that the
//! session task drains. The session ends when the process exits, when the engine closes
//! the stream (for example because the container stopped), or when it is closed.
//!
//! Closing hangs the session up. The engine does not end a TTY exec when its client goes
//! away: the daemon runs the exec with a context that is never cancelled and, with a TTY,
//! does not close the process' input when the attach stream ends; the Engine API has no
//! call to signal an exec either. So every session carries a marker variable
//! ([`SESSION_ENV`]), and close runs a second exec that sends SIGHUP to the processes that
//! carry it, which is what closing a real terminal does.

use super::backend::{err_text, Engine};
use crate::terminal::transport::{Sink, Transport, TransportState};
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ExecConfig, ExecInspectResponse};
use bollard::Docker;
use futures_util::StreamExt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

/// Longest time an engine request may take before the session gives up on it.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(20);
/// Time the shell gets to exit after the keystroke fallback of close before the tasks are aborted.
const CLOSE_GRACE: Duration = Duration::from_millis(300);
/// Longest wait for the session's process to exit after the hangup.
const HANGUP_WAIT: Duration = Duration::from_secs(3);
const HANGUP_POLL: Duration = Duration::from_millis(20);
/// Environment variable that marks the processes of one session (value: [`marker`]).
pub const SESSION_ENV: &str = "TMOP_SESSION";
/// Length of a marker in hex digits.
const MARKER_LEN: usize = 32;
/// Hangs up the session's processes: those whose environment holds `$0`, the
/// `TMOP_SESSION=<marker>` entry, passed as an argument and matched as a fixed string (all
/// markers have the same length, so one session's entry never matches another's). PID 1 and
/// the script itself are never signalled.
///
/// The processes are hung up from the leaves up: each round signals only the members that
/// have no member child, then waits until they are gone (reaped by their parent, which is a
/// member still alive) before the next round. Signalling a parent first would hand its
/// children to the container's PID 1 when it dies, and their exit then sends PID 1 a
/// SIGCHLD; a PID 1 such as busybox `sh -c "sleep 60"` ends on that signal (its sleep
/// returns early and the shell exits with status 0), which stopped the container (#179).
/// A member that survives SIGHUP (nohup) is left alone; its parent is still hung up.
#[doc(hidden)]
pub const HANGUP_SCRIPT: &str = r#"command -v grep >/dev/null || exit 127
entry=$0 self=$$ sent=" "
members() {
  for d in /proc/[0-9]*; do
    p=${d#/proc/}
    [ "$p" = 1 ] || [ "$p" = "$self" ] && continue
    case "$sent" in *" $p "*) continue ;; esac
    grep -qF "$entry" "$d/environ" 2>/dev/null && echo "$p"
  done
}
ppid() {
  while read -r k v rest; do [ "$k" = PPid: ] && echo "$v" && return; done < "/proc/$1/status"
}
round=0
while [ $round -lt 32 ]; do
  round=$((round + 1))
  all=$(members)
  [ -z "$all" ] && exit 0
  parents=" "
  for p in $all; do parents="$parents$(ppid "$p" 2>/dev/null) "; done
  leaves=
  for p in $all; do
    case "$parents" in *" $p "*) ;; *) leaves="$leaves $p" ;; esac
  done
  [ -z "$leaves" ] && leaves=$all
  for p in $leaves; do kill -HUP "$p" 2>/dev/null; done
  sent="$sent${leaves# } "
  n=0
  while [ $n -lt 20 ]; do
    left=
    for p in $leaves; do [ -d "/proc/$p" ] && left=1; done
    [ -z "$left" ] && break
    sleep 0.05 2>/dev/null || sleep 1
    n=$((n + 1))
  done
done
exit 0"#;
const TIMED_OUT: &str = "The engine did not answer in time";
const NO_SHELL: &str = "No shell found in the container (neither bash nor sh)";
/// Ctrl+C: clears a partial line or interrupts the foreground program.
const ETX: u8 = 0x03;
/// Ctrl+D: ends the shell when its line is empty.
const EOT: u8 = 0x04;

/// How to start an interactive session in a container.
#[derive(Clone, Debug, Default)]
pub struct ExecOptions {
    /// Program and arguments; `None` picks `bash`, falling back to `sh`.
    pub command: Option<Vec<String>>,
    /// User (and optional group) for the exec, e.g. "root" or "1000:1000".
    pub user: Option<String>,
    /// Initial terminal size in cells.
    pub cols: u16,
    pub rows: u16,
}

/// Splits a command line into arguments: whitespace separated, with single and
/// double quotes and backslash escapes. Outside quotes a backslash escapes the next
/// character; inside double quotes only `"`, `\`, `$` and backtick are escaped, and
/// inside single quotes nothing is.
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    // True once the current argument has started, so `""` yields an empty argument.
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        // Inside double quotes a backslash only escapes these characters;
                        // before any other one it is kept literally.
                        '\\' => match chars.peek() {
                            Some(&next) if matches!(next, '"' | '\\' | '$' | '`') => {
                                current.push(next);
                                chars.next();
                            }
                            _ => current.push('\\'),
                        },
                        _ => current.push(c),
                    }
                }
            }
            '\\' => {
                started = true;
                current.extend(chars.next());
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// A running interactive exec. Dropping it closes the session.
pub struct ExecSession {
    state: Arc<Mutex<TransportState>>,
    shell: Arc<Mutex<Option<String>>>,
    input: UnboundedSender<Vec<u8>>,
    resize: UnboundedSender<(u16, u16)>,
    tasks: Vec<JoinHandle<()>>,
    sink: Sink,
    runtime: Handle,
    closed: bool,
    hangup: Hangup,
}

/// What close needs to hang the session up.
#[derive(Clone)]
struct Hangup {
    docker: Docker,
    container: String,
    user: Option<String>,
    /// The session's marker, the value of [`SESSION_ENV`].
    marker: String,
    /// Id of the session's exec once it is created.
    exec_id: Arc<Mutex<Option<String>>>,
}

impl Hangup {
    /// The `TMOP_SESSION=<marker>` entry of the session's environment.
    fn entry(&self) -> String {
        format!("{SESSION_ENV}={}", self.marker)
    }
}

/// A random marker of [`MARKER_LEN`] hex digits, new for every session. The bits come from
/// two hashers with fresh random keys (std seeds `RandomState` from the OS), fed with the
/// clock, the process id and a counter so that two calls never hash the same input.
fn marker() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let half = |salt: u8| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u8(salt);
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.write_u64(n);
        h.finish()
    };
    format!("{:016x}{:016x}", half(0), half(1))
}

/// True for a marker as [`marker`] makes them: exactly [`MARKER_LEN`] lowercase hex digits.
fn valid_marker(marker: &str) -> bool {
    marker.len() == MARKER_LEN && marker.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl ExecSession {
    /// Starts the session; returns Err immediately when the engine is read-only.
    pub fn start(engine: &Engine, container_id: &str, opts: ExecOptions, sink: Sink) -> Result<ExecSession, String> {
        if engine.read_only {
            return Err("The engine is read-only; interactive shells are not available".to_string());
        }
        let command = opts.command.clone().filter(|c| !c.is_empty());
        let shell = Arc::new(Mutex::new(command.as_ref().and_then(|c| c.first().cloned())));
        let state = Arc::new(Mutex::new(TransportState::Starting));
        let (input, input_rx) = mpsc::unbounded_channel();
        let (resize, resize_rx) = mpsc::unbounded_channel();
        let hangup = Hangup {
            docker: engine.docker.clone(),
            container: container_id.to_string(),
            user: opts.user.clone(),
            marker: marker(),
            exec_id: Arc::new(Mutex::new(None)),
        };
        let opts = ExecOptions { command, ..opts };
        let task = engine.runtime.spawn(run(
            engine.docker.clone(),
            container_id.to_string(),
            opts,
            hangup.clone(),
            shell.clone(),
            state.clone(),
            sink.clone(),
            input_rx,
            resize_rx,
        ));
        Ok(ExecSession {
            state,
            shell,
            input,
            resize,
            tasks: vec![task],
            sink,
            runtime: engine.runtime.clone(),
            closed: false,
            hangup,
        })
    }

    /// The program that was started ("bash", "sh" or the first custom argument), once known.
    pub fn shell(&self) -> Option<String> {
        lock(&self.shell).clone()
    }
}

impl Transport for ExecSession {
    fn write(&self, bytes: &[u8]) {
        let _ = self.input.send(bytes.to_vec());
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resize.send((cols, rows));
    }

    fn state(&self) -> TransportState {
        lock(&self.state).clone()
    }

    fn close(&mut self) {
        self.begin_close();
    }
}

impl ExecSession {
    /// Closes every session in `sessions` and waits until their processes are hung up, for
    /// at most `bound`. For app exit, where the runtime may be gone before a background
    /// hangup ran. Must not be called from a runtime thread.
    pub fn close_within<'a>(sessions: impl IntoIterator<Item = &'a mut ExecSession>, bound: Duration) {
        let deadline = std::time::Instant::now() + bound;
        let closing: Vec<_> = sessions.into_iter().filter_map(ExecSession::begin_close).collect();
        for done in closing {
            // Err: the hangup finished (its sender is gone), timed out, or never ran because
            // the runtime has shut down.
            let _ = done.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }

    /// Marks the session closed and starts ending its process in the background. Returns a
    /// receiver that disconnects when that is done, if there is anything to do.
    fn begin_close(&mut self) -> Option<std::sync::mpsc::Receiver<()>> {
        if self.closed {
            return None;
        }
        self.closed = true;
        if matches!(*lock(&self.state), TransportState::Ended(_)) {
            return None;
        }
        let tasks = std::mem::take(&mut self.tasks);
        // Dropped by the closing task when it ends, which disconnects the receiver.
        let (done, closed) = std::sync::mpsc::channel::<()>();
        let mut closing = None;
        if matches!(*lock(&self.state), TransportState::Running) {
            // Dropping the stream does not end the process in the container (see the module
            // docs), so it is hung up first. The tasks live on until it has exited.
            let hangup = self.hangup.clone();
            let input = self.input.clone();
            closing = Some(closed);
            self.runtime.spawn(async move {
                let _done = done;
                if !hang_up(&hangup).await {
                    // The hangup could not run (no sh, grep or kill in the container) or the
                    // process ignores SIGHUP. Ctrl+C clears a partial line or stops a
                    // program, Ctrl+D ends an empty shell; this does not always work.
                    let _ = input.send(vec![ETX]);
                    let _ = input.send(vec![EOT]);
                    tokio::time::sleep(CLOSE_GRACE).await;
                }
                for task in tasks {
                    task.abort();
                }
            });
        } else {
            for task in tasks {
                task.abort();
            }
            // Closed while starting: once the exec exists, the start request may already
            // have reached the engine, so the process may run without anyone attached.
            if lock(&self.hangup.exec_id).is_some() {
                let hangup = self.hangup.clone();
                closing = Some(closed);
                self.runtime.spawn(async move {
                    let _done = done;
                    hang_up(&hangup).await
                });
            }
        }
        *lock(&self.state) = TransportState::Ended("Closed".to_string());
        self.sink.notify();
        closing
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Aborts the tasks it holds when dropped, so they end together with the session task.
struct Children(Vec<JoinHandle<()>>);

impl Drop for Children {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Session task: attaches the exec, then forwards input and output until the stream ends.
#[allow(clippy::too_many_arguments)]
async fn run(
    docker: Docker,
    container: String,
    opts: ExecOptions,
    hangup: Hangup,
    shell: Arc<Mutex<Option<String>>>,
    state: Arc<Mutex<TransportState>>,
    sink: Sink,
    input_rx: UnboundedReceiver<Vec<u8>>,
    mut resize_rx: UnboundedReceiver<(u16, u16)>,
) {
    let (id, started) = match attach(&docker, &container, &opts, &hangup, &shell).await {
        Ok(attached) => attached,
        Err(message) => return fail(&state, &sink, message),
    };
    let StartExecResults::Attached { mut output, input } = started else {
        return fail(&state, &sink, "The engine did not attach the session".to_string());
    };

    *lock(&state) = TransportState::Running;
    sink.notify();

    let writer = tokio::spawn(async move {
        let mut input = input;
        let mut input_rx = input_rx;
        while let Some(bytes) = input_rx.recv().await {
            if input.write_all(&bytes).await.is_err() || input.flush().await.is_err() {
                break;
            }
        }
    });
    let resizer = {
        let docker = docker.clone();
        let id = id.clone();
        tokio::spawn(async move {
            while let Some((cols, rows)) = resize_rx.recv().await {
                let _ = docker.resize_exec(&id, resize_options(cols, rows)).await;
            }
        })
    };
    let _children = Children(vec![writer, resizer]);

    let mut error = None;
    while let Some(item) = output.next().await {
        match item {
            Ok(LogOutput::StdOut { message })
            | Ok(LogOutput::StdErr { message })
            | Ok(LogOutput::StdIn { message })
            | Ok(LogOutput::Console { message }) => sink.feed(&message),
            Err(e) => {
                error = Some(err_text(&e));
                break;
            }
        }
    }

    let text = match error {
        Some(text) => text,
        None => match request(docker.inspect_exec(&id)).await {
            Ok(ExecInspectResponse { exit_code: Some(code), .. }) => format!("Process exited with code {code}"),
            _ => "Connection closed".to_string(),
        },
    };
    *lock(&state) = TransportState::Ended(text.clone());
    sink.feed(format!("\r\n[session ended: {text}]\r\n").as_bytes());
}

/// Creates the exec instance (probing for a shell first when none was given) and attaches
/// to it. Returns the exec id and the attached streams.
async fn attach(
    docker: &Docker,
    container: &str,
    opts: &ExecOptions,
    hangup: &Hangup,
    shell: &Mutex<Option<String>>,
) -> Result<(String, StartExecResults), String> {
    // The text shown when the program is missing from the container.
    let (cmd, missing) = match &opts.command {
        Some(cmd) => {
            let missing = format!("Command not found: {}", cmd.first().map_or("", String::as_str));
            (cmd.clone(), missing)
        }
        None => {
            let program = probe_shell(docker, container, opts.user.as_deref()).await?;
            *lock(shell) = Some(program.to_string());
            (vec![program.to_string()], NO_SHELL.to_string())
        }
    };
    let options = CreateExecOptions::<String> {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(cmd),
        user: opts.user.clone(),
        env: Some(vec!["TERM=xterm-256color".to_string(), hangup.entry()]),
        ..Default::default()
    };
    // The size goes in at create time: a resize after the start makes busybox ash redraw
    // the prompt it has already printed.
    let mut config = ExecConfig::from(options);
    config.console_size = Some(vec![usize::from(opts.rows.max(1)), usize::from(opts.cols.max(1))]);
    let created = request(docker.create_exec(container, config)).await.map_err(|e| missing_text(e, &missing))?;
    *lock(&hangup.exec_id) = Some(created.id.clone());
    let start = StartExecOptions { detach: false, tty: true, output_capacity: None };
    let started = request(docker.start_exec(&created.id, Some(start))).await.map_err(|e| missing_text(e, &missing))?;
    Ok((created.id, started))
}

/// Returns "bash" when the container has it, "sh" when only sh exists, and an error when
/// neither can be run.
async fn probe_shell(docker: &Docker, container: &str, user: Option<&str>) -> Result<&'static str, String> {
    let options = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        user: user.map(str::to_string),
        // Exit 1 means sh ran but bash is missing; busybox' `command -v` alone exits 127 then.
        cmd: Some(vec!["sh".into(), "-c".into(), "if command -v bash >/dev/null 2>&1; then exit 0; else exit 1; fi".into()]),
        ..Default::default()
    };
    let created = request(docker.create_exec(container, options)).await.map_err(|e| missing_text(e, NO_SHELL))?;
    let started = request(docker.start_exec(&created.id, None)).await.map_err(|e| missing_text(e, NO_SHELL))?;
    if let StartExecResults::Attached { mut output, .. } = started {
        let drain = async { while output.next().await.is_some() {} };
        if tokio::time::timeout(ENGINE_TIMEOUT, drain).await.is_err() {
            return Err(TIMED_OUT.to_string());
        }
    }
    let inspect = request(docker.inspect_exec(&created.id)).await?;
    match inspect.exit_code {
        Some(0) => Ok("bash"),
        Some(1) => Ok("sh"),
        // 126 and 127: sh itself could not be run; None: no exit code was reported.
        _ => Err(NO_SHELL.to_string()),
    }
}

/// Sends SIGHUP to the session's processes through a second exec and waits until the
/// session's exec has exited. Returns false when the hangup could not run or the process
/// was still running after [`HANGUP_WAIT`].
async fn hang_up(hangup: &Hangup) -> bool {
    let Some(id) = lock(&hangup.exec_id).clone() else { return false };
    // The marker goes into the command line of a shell; only the form `marker` makes is let through.
    if !valid_marker(&hangup.marker) {
        return false;
    }
    let docker = &hangup.docker;
    let options = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        user: hangup.user.clone(),
        // The entry is `$0` of the script, an argument, never part of the script text.
        cmd: Some(vec!["sh".into(), "-c".into(), HANGUP_SCRIPT.into(), hangup.entry()]),
        ..Default::default()
    };
    let Ok(created) = request(docker.create_exec(&hangup.container, options)).await else { return false };
    let Ok(started) = request(docker.start_exec(&created.id, None)).await else { return false };
    if let StartExecResults::Attached { mut output, .. } = started {
        let drain = async { while output.next().await.is_some() {} };
        if tokio::time::timeout(ENGINE_TIMEOUT, drain).await.is_err() {
            return false;
        }
    }
    if !matches!(request(docker.inspect_exec(&created.id)).await, Ok(ExecInspectResponse { exit_code: Some(0), .. })) {
        return false;
    }
    let deadline = tokio::time::Instant::now() + HANGUP_WAIT;
    loop {
        match request(docker.inspect_exec(&id)).await {
            Ok(ExecInspectResponse { running: Some(false), .. }) => return true,
            Ok(_) if tokio::time::Instant::now() < deadline => tokio::time::sleep(HANGUP_POLL).await,
            _ => return false,
        }
    }
}

/// Waits for an engine request that should answer quickly, with errors turned into text.
async fn request<T>(fut: impl Future<Output = Result<T, BollardError>>) -> Result<T, String> {
    match tokio::time::timeout(ENGINE_TIMEOUT, fut).await {
        Ok(result) => result.map_err(|e| err_text(&e)),
        Err(_) => Err(TIMED_OUT.to_string()),
    }
}

/// `missing` when the engine says the program does not exist in the container, else the engine's text.
fn missing_text(error: String, missing: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("executable file not found") || lower.contains("no such file or directory") {
        missing.to_string()
    } else {
        error
    }
}

fn resize_options(cols: u16, rows: u16) -> ResizeExecOptions {
    ResizeExecOptions { height: rows.max(1), width: cols.max(1) }
}

/// Ends the session with `message` shown in the terminal.
fn fail(state: &Mutex<TransportState>, sink: &Sink, message: String) {
    *lock(state) = TransportState::Ended(message.clone());
    sink.feed(format!("\r\n{message}\r\n").as_bytes());
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_valid_and_differ() {
        let a = marker();
        let b = marker();
        assert!(valid_marker(&a), "{a}");
        assert!(valid_marker(&b), "{b}");
        assert_ne!(a, b);
        assert!(!valid_marker(""));
        assert!(!valid_marker(&a[1..]));
        assert!(!valid_marker(&format!("{a}0")));
        assert!(!valid_marker(&format!("{}\"", &a[1..])));
        assert!(!valid_marker(&a.to_uppercase().replace(|c: char| c.is_ascii_digit(), "A")));
    }

    #[test]
    fn split_command_handles_quotes() {
        assert_eq!(split_command(r#"sh -c "echo 'a b'" x\ y"#), ["sh", "-c", "echo 'a b'", "x y"]);
        assert_eq!(split_command("  ls   -la  "), ["ls", "-la"]);
        assert_eq!(split_command(r#"echo "" ''"#), ["echo", "", ""]);
        assert_eq!(split_command(r"a\\b"), [r"a\b"]);
        assert_eq!(split_command(r#"sh -c "printf '\033[31mred\n'""#), ["sh", "-c", r"printf '\033[31mred\n'"]);
        assert_eq!(split_command(r#""a\"b""#), [r#"a"b"#]);
        assert_eq!(split_command(r#""a\\b""#), [r"a\b"]);
        assert_eq!(split_command(r"x\ y"), ["x y"]);
        assert!(split_command("   ").is_empty());
    }
}
