//! Interactive shells in containers.
//!
//! An [`ExecSession`] runs one exec instance with a pseudo-TTY on the engine runtime. The
//! hijacked stream from the engine carries the terminal output, which goes to the
//! [`Sink`]; keystrokes and size changes go the other way through channels that the
//! session task drains. The session ends when the process exits, when the engine closes
//! the stream (for example because the container stopped), or when it is closed.

use super::backend::{err_text, Engine};
use crate::terminal::transport::{Sink, Transport, TransportState};
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use bollard::models::ExecInspectResponse;
use bollard::Docker;
use futures_util::StreamExt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

/// Longest time an engine request may take before the session gives up on it.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(20);
const TIMED_OUT: &str = "The engine did not answer in time";
const NO_SHELL: &str = "No shell found in the container (neither bash nor sh)";

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
/// double quotes and backslash escapes (outside single quotes).
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    // True once the current argument has started, so `""` yields an empty argument.
    let mut started = false;
    let mut chars = line.chars();
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
                        '\\' => current.extend(chars.next()),
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
    closed: bool,
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
        let opts = ExecOptions { command, ..opts };
        let task = engine.runtime.spawn(run(
            engine.docker.clone(),
            container_id.to_string(),
            opts,
            shell.clone(),
            state.clone(),
            sink.clone(),
            input_rx,
            resize_rx,
        ));
        Ok(ExecSession { state, shell, input, resize, tasks: vec![task], sink, closed: false })
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
        if self.closed {
            return;
        }
        self.closed = true;
        if matches!(*lock(&self.state), TransportState::Ended(_)) {
            return;
        }
        for task in self.tasks.drain(..) {
            task.abort();
        }
        *lock(&self.state) = TransportState::Ended("Closed".to_string());
        self.sink.notify();
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
    shell: Arc<Mutex<Option<String>>>,
    state: Arc<Mutex<TransportState>>,
    sink: Sink,
    input_rx: UnboundedReceiver<Vec<u8>>,
    mut resize_rx: UnboundedReceiver<(u16, u16)>,
) {
    let (cols, rows) = (opts.cols, opts.rows);
    let (id, started) = match attach(&docker, &container, &opts, &shell).await {
        Ok(attached) => attached,
        Err(message) => return fail(&state, &sink, message),
    };
    let StartExecResults::Attached { mut output, input } = started else {
        return fail(&state, &sink, "The engine did not attach the session".to_string());
    };

    *lock(&state) = TransportState::Running;
    sink.notify();
    let _ = docker.resize_exec(&id, resize_options(cols, rows)).await;

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
    shell: &Mutex<Option<String>>,
) -> Result<(String, StartExecResults), String> {
    let cmd = match &opts.command {
        Some(cmd) => cmd.clone(),
        None => {
            let program = probe_shell(docker, container).await?;
            *lock(shell) = Some(program.to_string());
            vec![program.to_string()]
        }
    };
    let options = CreateExecOptions::<String> {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(cmd),
        user: opts.user.clone(),
        env: Some(vec!["TERM=xterm-256color".to_string()]),
        ..Default::default()
    };
    let created = request(docker.create_exec(container, options)).await?;
    let start = StartExecOptions { detach: false, tty: true, output_capacity: None };
    let started = request(docker.start_exec(&created.id, Some(start))).await?;
    Ok((created.id, started))
}

/// Returns "bash" when the container has it, "sh" when only sh exists, and an error when
/// neither can be run.
async fn probe_shell(docker: &Docker, container: &str) -> Result<&'static str, String> {
    let options = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        // Exit 1 means sh ran but bash is missing; busybox' `command -v` alone exits 127 then.
        cmd: Some(vec!["sh".into(), "-c".into(), "if command -v bash >/dev/null 2>&1; then exit 0; else exit 1; fi".into()]),
        ..Default::default()
    };
    let created = request(docker.create_exec(container, options)).await?;
    if let StartExecResults::Attached { mut output, .. } = request(docker.start_exec(&created.id, None)).await? {
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

/// Waits for an engine request that should answer quickly, with errors turned into text.
async fn request<T>(fut: impl Future<Output = Result<T, BollardError>>) -> Result<T, String> {
    match tokio::time::timeout(ENGINE_TIMEOUT, fut).await {
        Ok(result) => result.map_err(|e| err_text(&e)),
        Err(_) => Err(TIMED_OUT.to_string()),
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
    fn split_command_handles_quotes() {
        assert_eq!(split_command(r#"sh -c "echo 'a b'" x\ y"#), ["sh", "-c", "echo 'a b'", "x y"]);
        assert_eq!(split_command("  ls   -la  "), ["ls", "-la"]);
        assert_eq!(split_command(r#"echo "" ''"#), ["echo", "", ""]);
        assert_eq!(split_command(r"a\\b"), [r"a\b"]);
        assert!(split_command("   ").is_empty());
    }
}
