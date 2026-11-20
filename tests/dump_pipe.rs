//! `TMOP --dump` with a reader that closes the pipe early must end quietly (#78).
//!
//! The dump samples for a second before it prints anything, so closing our end of the pipe right
//! after the spawn guarantees that its first write hits a closed pipe.

use std::process::{Command, Stdio};

fn dump() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_TMOP"));
    c.arg("--dump").stdin(Stdio::null());
    c
}

#[test]
fn dump_into_a_closed_pipe_exits_quietly() {
    let mut child = dump().stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("start TMOP --dump");
    drop(child.stdout.take());
    let out = child.wait_with_output().expect("wait for TMOP --dump");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "panic on a closed pipe: {stderr}");
    assert!(out.status.success(), "exit status {:?}, stderr: {stderr}", out.status);
}

#[test]
fn dump_prints_the_cpu_line() {
    let out = dump().stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("run TMOP --dump");
    assert!(out.status.success(), "exit status {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.lines().next().is_some_and(|l| l.starts_with("CPU")), "first line: {stdout}");
}
