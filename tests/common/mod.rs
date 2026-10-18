//! Helpers for tests that need a reachable Docker engine.
//!
//! Tests return early (skip) when no local engine answers, so `cargo test` passes on
//! machines without Docker. Test objects are named `tmop-test-*` and removed afterwards.

#![allow(dead_code)]

use bollard::query_parameters::{CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use bollard::Docker;
use futures_util::StreamExt;
use std::time::Duration;
use tmop::docker::{client, context};

/// Image used for throwaway test containers.
pub const TEST_IMAGE: &str = "busybox:stable";

/// Connects to the first reachable local context running Linux containers ([`TEST_IMAGE`]
/// is a Linux image), or returns `None` (test skipped).
pub fn engine() -> Option<(tokio::runtime::Runtime, Docker)> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().ok()?;
    for ctx in context::default_contexts() {
        if ctx.is_remote() {
            continue;
        }
        let Ok(conn) = client::connect(&ctx.endpoint) else { continue };
        let docker = conn.docker.clone();
        let probe = docker.clone();
        let os = rt.block_on(async {
            let info = tokio::time::timeout(Duration::from_secs(3), probe.info()).await.ok()?.ok()?;
            info.os_type
        });
        match os.as_deref() {
            Some("linux") => return Some((rt, docker)),
            Some(other) => eprintln!("skipped context {}: engine runs {other} containers", ctx.name),
            None => {}
        }
    }
    eprintln!("skipped: no Docker engine with Linux containers reachable");
    None
}

/// Pulls `TEST_IMAGE` through the engine when it is missing.
pub async fn ensure_image(docker: &Docker) -> Result<(), String> {
    if docker.inspect_image(TEST_IMAGE).await.is_ok() {
        return Ok(());
    }
    let opts = CreateImageOptionsBuilder::default().from_image(TEST_IMAGE).build();
    let mut stream = docker.create_image(Some(opts), None, None);
    while let Some(item) = stream.next().await {
        item.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Unique suffix for test object names.
pub fn suffix() -> String {
    format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))
}

/// Removes a container (force), ignoring errors.
pub async fn remove_container(docker: &Docker, name: &str) {
    let opts = RemoveContainerOptionsBuilder::default().force(true).v(true).build();
    let _ = docker.remove_container(name, Some(opts)).await;
}
