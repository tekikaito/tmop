//! Engine-backed tests for container run, compose-style stack actions and image search.
//! They skip (pass) when no local Docker engine answers.

mod common;

use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{CreateContainerOptions, ListContainersOptions};
use bollard::Docker;
use common::{ensure_image, remove_container, suffix, TEST_IMAGE};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tmop::docker::backend::map_container;
use tmop::docker::model::{stacks_from, ContainerState};
use tmop::docker::run::{self, PullProgress, Restart, RunSpec};
use tmop::docker::stack::{stack_action, StackAction};

/// Removes the named test containers when dropped, even when the test panics.
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

#[test]
fn run_starts_container_with_env() {
    let Some((rt, docker)) = common::engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let name = format!("tmop-test-run-{}", suffix());
    let _guard = Cleanup { rt: &rt, docker: docker.clone(), names: vec![name.clone()] };
    let spec = RunSpec {
        image: TEST_IMAGE.into(),
        name: name.clone(),
        env: "TMOP_TEST=1".into(),
        command: "sleep 60".into(),
        restart: Restart::No,
        pull: true,
        ..Default::default()
    };
    let progress = Arc::new(Mutex::new(PullProgress::default()));
    let id = rt.block_on(run::run(&docker, &spec, progress)).expect("run container");

    let info = rt.block_on(docker.inspect_container(&id, None)).expect("inspect container");
    let running = info.state.as_ref().and_then(|s| s.running);
    assert_eq!(running, Some(true), "container should be running");
    let env = info.config.as_ref().and_then(|c| c.env.clone()).unwrap_or_default();
    assert!(env.iter().any(|e| e == "TMOP_TEST=1"), "env missing: {env:?}");
}

#[test]
fn stack_start_stop_down() {
    let Some((rt, docker)) = common::engine() else { return };
    rt.block_on(ensure_image(&docker)).expect("pull test image");

    let project = format!("tmop-test-stack-{}", suffix());
    let names: Vec<String> = ["a", "b"].iter().map(|s| format!("{project}-{s}")).collect();
    let _guard = Cleanup { rt: &rt, docker: docker.clone(), names: names.clone() };

    for (name, service) in names.iter().zip(["a", "b"]) {
        let labels = HashMap::from([
            ("com.docker.compose.project".to_string(), project.clone()),
            ("com.docker.compose.service".to_string(), service.to_string()),
        ]);
        let body = ContainerCreateBody {
            image: Some(TEST_IMAGE.into()),
            cmd: Some(vec!["sleep".into(), "60".into()]),
            labels: Some(labels),
            ..Default::default()
        };
        let opts = CreateContainerOptions { name: Some(name.clone()), ..Default::default() };
        rt.block_on(docker.create_container(Some(opts), body)).expect("create test container");
        rt.block_on(docker.start_container(name, None)).expect("start test container");
    }

    let load = |docker: &Docker| {
        let list = rt
            .block_on(docker.list_containers(Some(ListContainersOptions { all: true, ..Default::default() })))
            .expect("list containers");
        list.iter().map(|c| map_container(c, None)).collect::<Vec<_>>()
    };
    let find_stack = |containers: &[_]| {
        stacks_from(containers).into_iter().find(|s| s.name == project).expect("stack present")
    };

    let containers = load(&docker);
    let stack = find_stack(&containers);
    assert_eq!((stack.running, stack.total), (2, 2));
    assert_eq!(stack.services, ["a", "b"]);

    let summary =
        rt.block_on(stack_action(&docker, &stack, &containers, &[], &[], StackAction::Stop)).expect("stop stack");
    assert_eq!(summary, "Stopped 2 containers");
    let containers = load(&docker);
    let stack = find_stack(&containers);
    assert_eq!(stack.running, 0);
    assert!(containers
        .iter()
        .filter(|c| c.stack.as_deref() == Some(project.as_str()))
        .all(|c| c.state == ContainerState::Exited));

    rt.block_on(stack_action(&docker, &stack, &containers, &[], &[], StackAction::Down { volumes: false }))
        .expect("down stack");
    let containers = load(&docker);
    assert!(
        containers.iter().all(|c| c.stack.as_deref() != Some(project.as_str())),
        "stack containers should be removed"
    );
}

#[test]
fn search_finds_busybox() {
    let Some((rt, docker)) = common::engine() else { return };
    match rt.block_on(run::search(&docker, "busybox", 5)) {
        Ok(hits) => assert!(hits.iter().any(|h| h.name == "busybox"), "no busybox hit: {hits:?}"),
        Err(e) => eprintln!("search skipped, engine could not query the registry: {e}"),
    }
}
