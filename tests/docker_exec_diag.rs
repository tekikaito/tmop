//! TEMPORARY diagnostics for issue #179 (removed before the PR is final).
//!
//! Measures how long the interactive shell of an exec session lives after the session is
//! closed. Variants (DIAG_VARIANT):
//! - product: ExecSession::close as shipped
//! - drop: raw bollard session, the stream is dropped without sending anything
//! - ctrl_d: raw session, only Ctrl+D is written, then the stream is dropped after 300 ms
//! - ctrl_c_d: raw session, Ctrl+C and Ctrl+D as two writes back to back (what close does)
//! - ctrl_c_wait_d: raw session, Ctrl+C, wait for the new prompt, then Ctrl+D
//!
//! Runs DIAG_ITERS iterations on DIAG_THREADS threads; prints one DIAG line per iteration.

mod common;

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::CreateContainerOptionsBuilder;
use bollard::Docker;
use common::{ensure_image, engine, remove_container, suffix, TEST_IMAGE};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tmop::docker::backend::Engine;
use tmop::docker::exec::{ExecOptions, ExecSession};
use tmop::terminal::{Sink, Transport, TransportState};
use tokio::io::AsyncWriteExt;

const LIMIT: Duration = Duration::from_secs(10);

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

async fn create_and_start(docker: &Docker, name: &str) {
    let opts = CreateContainerOptionsBuilder::default().name(name).build();
    let body = ContainerCreateBody {
        image: Some(TEST_IMAGE.into()),
        cmd: Some(vec!["sh".into(), "-c".into(), "sleep 600".into()]),
        ..Default::default()
    };
    docker.create_container(Some(opts), body).await.expect("create");
    docker.start_container(name, None).await.expect("start");
}

/// Id of the container's TTY exec (the interactive session).
async fn tty_exec(docker: &Docker, name: &str) -> Option<String> {
    let info = docker.inspect_container(name, None).await.ok()?;
    for id in info.exec_ids.unwrap_or_default() {
        if let Ok(e) = docker.inspect_exec(&id).await {
            if e.process_config.and_then(|p| p.tty) == Some(true) {
                return Some(id);
            }
        }
    }
    None
}

/// Milliseconds from `t0` until the exec stops running, or None after LIMIT.
async fn wait_exit(docker: &Docker, id: &str, t0: Instant) -> Option<u128> {
    loop {
        if let Ok(e) = docker.inspect_exec(id).await {
            if e.running == Some(false) {
                return Some(t0.elapsed().as_millis());
            }
        }
        if t0.elapsed() > LIMIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn text(out: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&out.lock().unwrap()).escape_debug().to_string()
}

async fn wait_contains(out: &Arc<Mutex<Vec<u8>>>, needle: &str, from: usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let s = {
            let buf = out.lock().unwrap();
            String::from_utf8_lossy(&buf[from.min(buf.len())..]).into_owned()
        };
        if s.contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

fn product(rt: &tokio::runtime::Runtime, docker: &Docker, name: &str) -> (Option<u128>, String) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let o = out.clone();
    let sink = Sink::new(move |b| o.lock().unwrap().extend_from_slice(b), || {});
    let engine = Engine { docker: docker.clone(), runtime: rt.handle().clone(), context: "diag".into(), read_only: false };
    let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
    let mut session = ExecSession::start(&engine, name, opts, sink).expect("start");
    let deadline = Instant::now() + Duration::from_secs(15);
    while session.state() != TransportState::Running && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    session.write(b"echo ready\r");
    assert!(rt.block_on(wait_contains(&out, "\nready", 0)), "no ready: {}", text(&out));
    let id = rt.block_on(tty_exec(docker, name)).expect("tty exec");
    let t0 = Instant::now();
    session.close();
    let ms = rt.block_on(wait_exit(docker, &id, t0));
    (ms, text(&out))
}

async fn raw(docker: &Docker, name: &str, variant: &str) -> (Option<u128>, String) {
    let options = CreateExecOptions::<String> {
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        tty: Some(true),
        cmd: Some(vec!["sh".into()]),
        env: Some(vec!["TERM=xterm-256color".to_string()]),
        ..Default::default()
    };
    let created = docker.create_exec(name, options).await.expect("create exec");
    let start = StartExecOptions { detach: false, tty: true, output_capacity: None };
    let StartExecResults::Attached { mut output, mut input } = docker.start_exec(&created.id, Some(start)).await.expect("start exec") else {
        panic!("not attached");
    };
    let out = Arc::new(Mutex::new(Vec::new()));
    let o = out.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(item)) = output.next().await {
            if let LogOutput::Console { message } | LogOutput::StdOut { message } = item {
                o.lock().unwrap().extend_from_slice(&message);
            }
        }
    });
    input.write_all(b"echo ready\r").await.unwrap();
    input.flush().await.unwrap();
    assert!(wait_contains(&out, "\nready", 0).await, "no ready: {}", text(&out));
    // Wait for the prompt after the output so the shell is back in read_line_input.
    let mark = out.lock().unwrap().len();
    let _ = wait_contains(&out, "# ", mark.saturating_sub(4)).await;
    let t0 = Instant::now();
    match variant {
        "drop" => {}
        "ctrl_d" => {
            input.write_all(&[0x04]).await.unwrap();
            input.flush().await.unwrap();
        }
        "ctrl_c_d" => {
            input.write_all(&[0x03]).await.unwrap();
            input.flush().await.unwrap();
            input.write_all(&[0x04]).await.unwrap();
            input.flush().await.unwrap();
        }
        "ctrl_c_wait_d" => {
            let mark = out.lock().unwrap().len();
            input.write_all(&[0x03]).await.unwrap();
            input.flush().await.unwrap();
            let _ = wait_contains(&out, "# ", mark).await;
            input.write_all(&[0x04]).await.unwrap();
            input.flush().await.unwrap();
        }
        other => panic!("unknown variant {other}"),
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // What the product does after the grace: abort the tasks (drops the stream).
    let after = text(&out);
    drop(input);
    reader.abort();
    let ms = wait_exit(docker, &created.id, t0).await;
    (ms, after)
}

async fn exec_out(docker: &Docker, name: &str, cmd: &[&str]) -> String {
    let o = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    };
    let c = match docker.create_exec(name, o).await {
        Ok(c) => c,
        Err(e) => return format!("<create exec failed: {e}>"),
    };
    let mut s = String::new();
    if let Ok(StartExecResults::Attached { mut output, .. }) = docker.start_exec(&c.id, None).await {
        while let Some(Ok(item)) = output.next().await {
            s.push_str(&item.to_string());
        }
    }
    s
}

async fn state_line(docker: &Docker, name: &str) -> String {
    match docker.inspect_container(name, None).await {
        Ok(i) => {
            let s = i.state.unwrap_or_default();
            format!(
                "running={:?} status={:?} exit={:?} oom={:?} error={:?} started={:?} finished={:?} pid={:?}",
                s.running, s.status, s.exit_code, s.oom_killed, s.error, s.started_at, s.finished_at, s.pid
            )
        }
        Err(e) => format!("<inspect failed: {e}>"),
    }
}

/// Every process with its ids, state, and the TMOP_SESSION entry if it has one.
const SNAPSHOT: &str = r#"for d in /proc/[0-9]*; do p=${d#/proc/}; [ "$p" = "$$" ] && continue; set -- $(cat $d/stat 2>/dev/null | sed 's/^.*) //'); printf '%s comm=%s state=%s ppid=%s pgid=%s sid=%s start=%s mark=%s args=%s\n' "$p" "$(cat $d/comm 2>/dev/null)" "$1" "$2" "$3" "$4" "${20}" "$(tr '\0' '\n' < $d/environ 2>/dev/null | grep TMOP_SESSION)" "$(tr '\0' ' ' < $d/cmdline 2>/dev/null)"; done"#;

/// The product's hangup script, printing what it signals instead of only signalling.
const VERBOSE_HANGUP: &str = r#"for d in /proc/[0-9]*; do
  p=${d#/proc/}
  [ "$p" = 1 ] || [ "$p" = "$$" ] && continue
  if grep -qF "$0" "$d/environ" 2>/dev/null; then echo "HUP $p $(cat $d/comm)"; kill -HUP "$p"; echo "kill rc=$?"; fi
done
echo "script done self=$$""#;

/// The stubborn-program scenario of `closing_the_session_ends_a_program_that_ignores_ctrl_c`,
/// with the container ("sh -c 'sleep 60'" as in the real test) observed around the close.
/// DIAG_STUBBORN=manual runs the verbose hangup first, then waits, then closes.
#[test]
fn diag_stubborn() {
    let Some((rt, docker)) = engine() else { return };
    if std::env::var("DIAG_STUBBORN").is_err() {
        return;
    }
    rt.block_on(ensure_image(&docker)).expect("pull test image");
    let manual = env_or("DIAG_STUBBORN", "product") == "manual";
    let iters: usize = env_or("DIAG_ITERS", "10").parse().unwrap();
    let mut died = 0;
    for i in 0..iters {
        let name = format!("tmop-test-diag179-{}", suffix());
        rt.block_on(async {
            let opts = CreateContainerOptionsBuilder::default().name(&name).build();
            let body = ContainerCreateBody {
                image: Some(TEST_IMAGE.into()),
                cmd: Some(vec!["sh".into(), "-c".into(), "sleep 60".into()]),
                ..Default::default()
            };
            docker.create_container(Some(opts), body).await.expect("create");
            docker.start_container(&name, None).await.expect("start");
        });
        let t_start = Instant::now();
        let out = Arc::new(Mutex::new(Vec::new()));
        let o = out.clone();
        let sink = Sink::new(move |b| o.lock().unwrap().extend_from_slice(b), || {});
        let engine = Engine { docker: docker.clone(), runtime: rt.handle().clone(), context: "diag".into(), read_only: false };
        let opts = ExecOptions { command: None, user: None, cols: 80, rows: 24 };
        let mut session = ExecSession::start(&engine, &name, opts, sink).expect("start");
        let deadline = Instant::now() + Duration::from_secs(15);
        while session.state() != TransportState::Running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        session.write(b"sh -c \"trap '' INT; echo stub''born; sleep 600\"\r");
        assert!(rt.block_on(wait_contains(&out, "stubborn", 0)), "no stubborn: {}", text(&out));
        println!("DIAGS iter={i} t={:?} before close: {}", t_start.elapsed(), rt.block_on(state_line(&docker, &name)));
        for line in rt.block_on(exec_out(&docker, &name, &["sh", "-c", SNAPSHOT])).lines() {
            println!("DIAGS iter={i}   proc {line}");
        }
        if manual {
            let marker = rt
                .block_on(exec_out(&docker, &name, &["sh", "-c", SNAPSHOT]))
                .lines()
                .find_map(|l| l.split("mark=").nth(1).and_then(|m| m.split_whitespace().next()).map(str::to_string))
                .unwrap_or_default();
            println!("DIAGS iter={i} marker entry={marker}");
            let v = rt.block_on(exec_out(&docker, &name, &["sh", "-c", VERBOSE_HANGUP, &marker]));
            for line in v.lines() {
                println!("DIAGS iter={i}   hangup {line}");
            }
        } else {
            session.close();
        }
        let t0 = Instant::now();
        let mut last = String::new();
        let mut dead = false;
        while t0.elapsed() < Duration::from_secs(4) {
            let s = rt.block_on(state_line(&docker, &name));
            if s != last {
                println!("DIAGS iter={i} +{:?} {s}", t0.elapsed());
                if s.contains("running=Some(false)") {
                    dead = true;
                }
                last = s;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !dead {
            for line in rt.block_on(exec_out(&docker, &name, &["sh", "-c", SNAPSHOT])).lines() {
                println!("DIAGS iter={i}   after proc {line}");
            }
        }
        if manual {
            session.close();
            std::thread::sleep(Duration::from_secs(1));
            println!("DIAGS iter={i} after product close: {}", rt.block_on(state_line(&docker, &name)));
        }
        died += usize::from(dead);
        drop(session);
        rt.block_on(remove_container(&docker, &name));
    }
    println!("DIAG SUMMARY stubborn mode={} runs={iters} container_died={died}", if manual { "manual" } else { "product" });
}

#[test]
fn diag_close_timing() {
    if std::env::var("DIAG_STUBBORN").is_ok() {
        return;
    }
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");
    let variant = env_or("DIAG_VARIANT", "product");
    let iters: usize = env_or("DIAG_ITERS", "20").parse().unwrap();
    let threads: usize = env_or("DIAG_THREADS", "1").parse().unwrap();
    let rt = Arc::new(rt);
    let results = Arc::new(Mutex::new(Vec::new()));
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..threads {
        let (rt, docker, results, next, variant) = (rt.clone(), docker.clone(), results.clone(), next.clone(), variant.clone());
        handles.push(std::thread::spawn(move || loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if i >= iters {
                break;
            }
            let name = format!("tmop-test-diag179-{}", suffix());
            rt.block_on(create_and_start(&docker, &name));
            let (ms, out) = if variant == "product" { product(&rt, &docker, &name) } else { rt.block_on(raw(&docker, &name, &variant)) };
            let mut ps = String::new();
            if ms.is_none() {
                let listing = rt.block_on(async {
                    let o = CreateExecOptions::<String> {
                        attach_stdout: Some(true),
                        cmd: Some(vec!["ps".into(), "-o".into(), "pid,stat,comm".into()]),
                        ..Default::default()
                    };
                    let c = docker.create_exec(&name, o).await.unwrap();
                    let mut s = String::new();
                    if let StartExecResults::Attached { mut output, .. } = docker.start_exec(&c.id, None).await.unwrap() {
                        while let Some(Ok(item)) = output.next().await {
                            s.push_str(&item.to_string());
                        }
                    }
                    s
                });
                ps = listing.escape_debug().to_string();
            }
            match ms {
                Some(ms) => println!("DIAG variant={variant} iter={i} exit_ms={ms}"),
                None => println!("DIAG variant={variant} iter={i} exit_ms=NEVER(>{}s) ps={ps} out={out}", LIMIT.as_secs()),
            }
            results.lock().unwrap().push(ms);
            rt.block_on(remove_container(&docker, &name));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let r = results.lock().unwrap();
    let mut done: Vec<u128> = r.iter().flatten().copied().collect();
    done.sort();
    let never = r.len() - done.len();
    let pct = |p: f64| done.get(((done.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0);
    let over1500 = done.iter().filter(|&&m| m > 1500).count();
    println!(
        "DIAG SUMMARY variant={variant} threads={threads} runs={} never={never} exited={} over1500ms={over1500} min={} p50={} p90={} p99={} max={}",
        r.len(),
        done.len(),
        done.first().copied().unwrap_or(0),
        pct(0.5),
        pct(0.9),
        pct(0.99),
        done.last().copied().unwrap_or(0)
    );
}
