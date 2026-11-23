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
/// The seconds of [`CMD`], for a container whose main process is sleep itself.
const CMD_SECONDS: &str = "60";

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
    let created = match docker.create_exec(container, options).await {
        Ok(created) => created,
        Err(e) => {
            let state = docker.inspect_container(container, None).await.ok().and_then(|i| i.state);
            panic!("create exec: {e}\ncontainer state: {state:?}");
        }
    };
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

/// Lists the container's processes as `pid start marked args`, where `start` is the start time
/// in clock ticks and `marked` says whether the environment holds a TMOP_SESSION entry. The
/// listing shell and its helpers are left out.
const LIST: &str = r#"for d in /proc/[0-9]*; do
  p=${d#/proc/}; [ "$p" = "$$" ] && continue
  pp=; while read -r k v rest; do [ "$k" = PPid: ] && pp=$v; done < $d/status 2>/dev/null
  [ "$pp" = "$$" ] && continue
  m=no; grep -q TMOP_SESSION= $d/environ 2>/dev/null && m=yes
  echo "$p $(cut -d' ' -f22 $d/stat 2>/dev/null) $m $(tr '\0' ' ' < $d/cmdline 2>/dev/null)"
done"#;

/// The container's processes: (pid, start, marked, args).
fn processes(rt: &tokio::runtime::Runtime, docker: &Docker, name: &str) -> Vec<(String, String, bool, String)> {
    rt.block_on(exec_stdout(docker, name, &["sh", "-c", LIST]))
        .lines()
        .filter_map(|line| {
            let mut cols = line.splitn(4, ' ');
            let (pid, start, marked) = (cols.next()?, cols.next()?, cols.next()?);
            Some((pid.to_string(), start.to_string(), marked == "yes", cols.next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

/// Closing a session whose program ignores Ctrl+C ends the session's processes and nothing
/// else: the container keeps running, and its main process and a process another exec
/// started (without the session's marker) keep their start times. Hanging up a parent before
/// its children used to hand them to PID 1, and a busybox `sh -c` as PID 1 ended on the
/// SIGCHLD of their exit, which stopped the container (#179).
fn closing_a_session_leaves_the_rest_of_the_container(cmd: &[&str], init: bool) {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-exec-{}", suffix());
    let _cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(async {
        let opts = CreateContainerOptionsBuilder::default().name(&name).build();
        let body = ContainerCreateBody {
            image: Some(TEST_IMAGE.into()),
            cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
            host_config: Some(bollard::models::HostConfig { init: Some(init), ..Default::default() }),
            ..Default::default()
        };
        docker.create_container(Some(opts), body).await.expect("create container");
        docker.start_container(&name, None).await.expect("start container");
        // An unrelated process from another exec, detached: it carries no marker.
        let other = CreateExecOptions::<String> { cmd: Some(vec!["sleep".into(), "301".into()]), ..Default::default() };
        let created = docker.create_exec(&name, other).await.expect("create exec");
        let start = bollard::exec::StartExecOptions { detach: true, ..Default::default() };
        docker.start_exec(&created.id, Some(start)).await.expect("start exec");
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let before = loop {
        let list = processes(&rt, &docker, &name);
        if list.iter().any(|p| p.3 == "sleep 301") || Instant::now() >= deadline {
            break list;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(before.iter().any(|p| p.3 == "sleep 301"), "the unrelated process did not start: {before:?}");
    assert!(before.iter().all(|p| !p.2), "marked processes before the session: {before:?}");

    let out = Arc::new(Mutex::new(Vec::new()));
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine_for(&rt, &docker, false), &name, opts, collecting_sink(&out)).expect("start");
    let state = wait_state(&session, |s| matches!(s, TransportState::Running) || is_ended(s));
    assert_eq!(state, TransportState::Running, "session did not start: {}", output_text(&out));
    session.write(b"sh -c \"trap '' INT; echo stub''born; sleep 600\"\r");
    assert!(wait_output(&out, "stubborn"), "the program did not start: {}", output_text(&out));
    let during = processes(&rt, &docker, &name);
    assert!(during.iter().filter(|p| p.2).count() >= 2, "expected the shell and its program to be marked: {during:?}");

    session.close();
    let deadline = Instant::now() + Duration::from_secs(10);
    let after = loop {
        let list = processes(&rt, &docker, &name);
        if list.iter().all(|p| !p.2) || Instant::now() >= deadline {
            break list;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let running = rt.block_on(docker.inspect_container(&name, None)).ok().and_then(|i| i.state).and_then(|s| s.running);
    eprintln!("before {before:?}\nafter {after:?}");
    assert_eq!(running, Some(true), "the container stopped");
    assert!(after.iter().all(|p| !p.2), "session processes still running: {after:?}");
    for p in &before {
        assert!(after.iter().any(|q| q.0 == p.0 && q.1 == p.1), "process {p:?} did not survive the close: {after:?}");
    }
}

#[test]
fn closing_a_session_leaves_a_plain_main_process() {
    closing_a_session_leaves_the_rest_of_the_container(&["sleep", CMD_SECONDS], false);
}

#[test]
fn closing_a_session_leaves_a_shell_main_process() {
    closing_a_session_leaves_the_rest_of_the_container(&["sh", "-c", CMD], false);
}

#[test]
fn closing_a_session_leaves_a_main_process_under_init() {
    closing_a_session_leaves_the_rest_of_the_container(&["sh", "-c", CMD], true);
}
