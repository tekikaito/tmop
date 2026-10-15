//! Integration tests for log sessions against a local Docker engine.
//!
//! Each test skips (passes without checking) when no engine answers. Containers are
//! named `tmop-test-logs-*` and force-removed when the test ends, even on failure.

mod common;

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::CreateContainerOptionsBuilder;
use bollard::Docker;
use common::{ensure_image, engine, remove_container, suffix, TEST_IMAGE};
use std::time::{Duration, Instant};
use tmop::docker::backend::Engine;
use tmop::docker::logs::{Line, LogSession, Stream};

/// Prints one line to each stream, then keeps the container running so the follow stays open.
const CMD: &str = "echo hello-out; echo hello-err 1>&2; sleep 20";

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

fn engine_for(rt: &tokio::runtime::Runtime, docker: &Docker) -> Engine {
    Engine { docker: docker.clone(), runtime: rt.handle().clone(), context: "test".into(), read_only: false }
}

/// Polls the session buffer until `done` accepts it or 15 seconds pass. Returns a snapshot.
fn wait_until(session: &LogSession, done: impl Fn(&[Line]) -> bool) -> Vec<Line> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot: Vec<Line> = session.buffer.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
        if done(&snapshot) || Instant::now() >= deadline {
            return snapshot;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn has(lines: &[Line], stream: Stream, text: &str) -> bool {
    lines.iter().any(|l| l.stream == stream && l.text == text)
}

#[test]
fn container_session_reads_stdout_and_stderr() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-logs-{}", suffix());
    let cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    rt.block_on(create_and_start(&docker, &name)).expect("create and start container");

    let mut session = LogSession::container(&engine_for(&rt, &docker), &name, 100, || {});
    let lines = wait_until(&session, |l| has(l, Stream::Stdout, "hello-out") && has(l, Stream::Stderr, "hello-err"));
    session.stop();
    drop(cleanup);

    let out = lines.iter().find(|l| l.stream == Stream::Stdout && l.text == "hello-out");
    let err = lines.iter().find(|l| l.stream == Stream::Stderr && l.text == "hello-err");
    assert!(out.is_some_and(|l| l.timestamp.is_some()), "stdout line with timestamp missing: {lines:?}");
    assert!(err.is_some_and(|l| l.timestamp.is_some()), "stderr line with timestamp missing: {lines:?}");
    assert!(session.error.lock().unwrap_or_else(|e| e.into_inner()).is_none());
}

#[test]
fn stack_session_merges_containers_with_sources() {
    let Some((rt, docker)) = engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let id = suffix();
    let names = [format!("tmop-test-logs-{id}-a"), format!("tmop-test-logs-{id}-b")];
    let cleanup = Cleanup { rt: &rt, docker: docker.clone(), names: names.to_vec() };
    for name in &names {
        rt.block_on(create_and_start(&docker, name)).expect("create and start container");
    }

    let containers = vec![(names[0].clone(), "svc-a".to_string()), (names[1].clone(), "svc-b".to_string())];
    let mut session = LogSession::stack(&engine_for(&rt, &docker), &containers, 100, || {});
    let lines = wait_until(&session, |l| {
        ["svc-a", "svc-b"].iter().all(|src| l.iter().any(|x| x.source.as_deref() == Some(src) && x.text == "hello-out"))
    });
    session.stop();
    drop(cleanup);

    assert_eq!(session.sources, vec!["svc-a".to_string(), "svc-b".to_string()]);
    for src in ["svc-a", "svc-b"] {
        assert!(
            lines.iter().any(|l| l.source.as_deref() == Some(src) && l.text == "hello-out"),
            "no line from {src}: {lines:?}"
        );
    }
}
