//! Integration tests for exec terminal sessions against a local Docker engine.
//!
//! Each test skips (passes without checking) when no engine answers. Containers are
//! named `tmop-test-exec-*` and force-removed when the test ends, even on failure.

mod common;

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateContainerOptionsBuilder, StopContainerOptionsBuilder};
use bollard::Docker;
use common::{ensure_image, engine, remove_container, suffix, TEST_IMAGE};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tmop::docker::backend::Engine;
use tmop::docker::exec::{ExecOptions, ExecSession};
use tmop::terminal::{Sink, Transport, TransportState};

/// Keeps the container running while the exec sessions use it.
const CMD: &str = "sleep 60";

/// Removes the listed containers when dropped.
struct Cleanup<'a> {
    rt: &'a tokio::runtime::Runtime,
    docker: Docker,
    names: Vec<String>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for name in &self.names {
            self.rt.block_on(remove_container(&self.docker, name));
        }
    }
}

async fn create_and_start(docker: &Docker, name: &str) -> Result<(), String> {
    let opts = CreateContainerOptionsBuilder::default().name(name).build();
    let body = ContainerCreateBody {
        image: Some(TEST_IMAGE.into()),
        cmd: Some(vec!["sh".into(), "-c".into(), CMD.into()]),
        ..Default::default()
    };
    docker.create_container(Some(opts), body).await.map_err(|e| e.to_string())?;
    docker.start_container(name, None).await.map_err(|e| e.to_string())?;
    Ok(())
}

fn engine_for(rt: &tokio::runtime::Runtime, docker: &Docker, read_only: bool) -> Engine {
    Engine { docker: docker.clone(), runtime: rt.handle().clone(), context: "test".into(), read_only }
}

/// Sink that appends the output bytes to `out`.
fn collecting_sink(out: &Arc<Mutex<Vec<u8>>>) -> Sink {
    let out = out.clone();
    Sink::new(move |bytes| out.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(bytes), || {})
}

fn output_text(out: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&out.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
}

/// Polls the session state until `done` accepts it or 15 seconds pass. Returns the last state.
fn wait_state(session: &ExecSession, done: impl Fn(&TransportState) -> bool) -> TransportState {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = session.state();
        if done(&state) || Instant::now() >= deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Polls the collected output until it contains `text` or 15 seconds pass.
fn wait_output(out: &Arc<Mutex<Vec<u8>>>, text: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if output_text(out).contains(text) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn is_ended(state: &TransportState) -> bool {
    matches!(state, TransportState::Ended(_))
}

/// Runs `cmd` in the container without a TTY and returns its stdout.
async fn exec_stdout(docker: &Docker, container: &str, cmd: &[&str]) -> String {
    let options = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    };
    let created = docker.create_exec(container, options).await.expect("create exec");
    let StartExecResults::Attached { mut output, .. } = docker.start_exec(&created.id, None).await.expect("start exec") else {
        panic!("exec was not attached");
    };
    let mut text = String::new();
    while let Some(item) = output.next().await {
        if let Ok(LogOutput::StdOut { message }) = item {
            text.push_str(&String::from_utf8_lossy(&message));
        }
    }
    text
}

#[test]
fn exec_runs_command_and_reports_exit() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: Some(vec!["echo".into(), "hello-exec".into()]), user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");
    let state = wait_state(&session, is_ended);
    session.close();

    let TransportState::Ended(text) = &state else {
        panic!("session did not end: {state:?}");
    };
    assert!(text.contains("code 0"), "unexpected end text: {text}");
    let output = output_text(&out);
    assert!(output.contains("hello-exec"), "output missing: {output:?}");
    assert_eq!(session.shell(), Some("echo".to_string()));
}

#[test]
fn default_shell_falls_back_to_sh_and_accepts_input() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");

    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));
    assert_eq!(session.shell(), Some("sh".to_string()));

    session.write(b"echo marker-$((40+2))\r");
    assert!(wait_output(&out, "marker-42"), "no marker in output: {}", output_text(&out));

    session.resize(100, 30);

    session.write(b"exit\r");
    let state = wait_state(&session, is_ended);
    session.close();
    let TransportState::Ended(text) = &state else {
        panic!("session did not end after exit: {state:?}");
    };
    assert!(text.contains("code 0"), "unexpected end text: {text}");
}

#[test]
fn read_only_engine_refuses_exec() {
    let Some((rt, docker)) = engine() else { return };
    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    match ExecSession::start(&engine_for(&rt, &docker, true), "tmop-test-exec-missing", opts, collecting_sink(&out)) {
        Ok(_) => panic!("read-only engine started an exec session"),
        Err(e) => assert!(e.contains("read-only"), "unexpected error: {e}"),
    }
}

#[test]
fn stopping_the_container_ends_the_session() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");

    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));

    session.write(b"echo alive\r");
    // The typed line is echoed back as "echo alive", so match only the shell's own line break before "alive".
    assert!(wait_output(&out, "\nalive"), "no output from the shell: {}", output_text(&out));

    let stop = StopContainerOptionsBuilder::default().t(1).build();
    rt.block_on(docker.stop_container(&name, Some(stop))).expect("stop container");

    let deadline = Instant::now() + Duration::from_secs(20);
    let state = loop {
        let state = session.state();
        if is_ended(&state) || Instant::now() >= deadline {
            break state;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    session.close();

    let TransportState::Ended(text) = &state else {
        panic!("session did not end after the container stopped: {state:?}");
    };
    eprintln!("Ended text after stop: {text}");
    assert!(wait_output(&out, "[session ended:"), "no end marker in output: {}", output_text(&out));
}

#[test]
fn closing_the_session_ends_the_shell() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");
    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));

    session.write(b"echo ready\r");
    assert!(wait_output(&out, "\nready"), "no output from the shell: {}", output_text(&out));

    session.close();
    let (left, listing, waited) = wait_for_session_processes_gone(&rt, &docker, &name);
    eprintln!("session processes gone after {waited:?}\n{listing}");
    assert!(left.is_empty(), "the interactive shell is still running {waited:?} after close:\n{listing}");
}

/// A program in the foreground that ignores Ctrl+C keeps Ctrl+C and Ctrl+D from reaching the
/// shell; closing the session must still end both (it hangs the session up).
#[test]
fn closing_the_session_ends_a_program_that_ignores_ctrl_c() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");
    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));

    session.write(b"sh -c \"trap '' INT; echo stub''born; sleep 600\"\r");
    assert!(wait_output(&out, "stubborn"), "the program did not start: {}", output_text(&out));

    session.close();
    let (left, listing, waited) = wait_for_session_processes_gone(&rt, &docker, &name);
    eprintln!("session processes gone after {waited:?}\n{listing}");
    assert!(left.is_empty(), "session processes still running {waited:?} after close: {left:?}\n{listing}");
}

/// The app-exit path: `close_within` returns only once the shell is gone (or the bound passed).
#[test]
fn close_within_returns_after_the_shell_is_gone() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");
    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));
    session.write(b"echo ready\r");
    assert!(wait_output(&out, "\nready"), "no output from the shell: {}", output_text(&out));

    let start = Instant::now();
    ExecSession::close_within([&mut session], Duration::from_secs(5));
    let took = start.elapsed();
    let listing = rt.block_on(exec_stdout(&docker, &name, &["ps", "-o", "pid,comm"]));
    eprintln!("close_within took {took:?}\n{listing}");
    let left: Vec<&str> = listing.lines().skip(1).filter(|l| !l.trim_start().starts_with("1 ") && !l.ends_with(" ps")).collect();
    assert!(left.is_empty(), "still running right after close_within ({took:?}): {left:?}\n{listing}");
}

/// Waits up to 10 seconds until the container runs nothing but PID 1 (`sh -c sleep 60`).
/// Returns the other processes (pid and name) of the last check, its `ps` listing and the
/// time waited.
fn wait_for_session_processes_gone(rt: &tokio::runtime::Runtime, docker: &Docker, name: &str) -> (Vec<String>, String, Duration) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(10);
    loop {
        // `ps` runs without a shell so that the check adds no process but itself.
        let listing = rt.block_on(exec_stdout(docker, name, &["ps", "-o", "pid,comm"]));
        let left: Vec<String> = listing
            .lines()
            .skip(1)
            .filter(|line| {
                let mut cols = line.split_whitespace();
                let (pid, comm) = (cols.next(), cols.next());
                pid != Some("1") && comm != Some("ps")
            })
            .map(|line| line.trim().to_string())
            .collect();
        if left.is_empty() || Instant::now() >= deadline {
            return (left, listing, start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
