//! Integration tests for the Docker file browser. They skip when no engine answers.

mod common;

use bollard::models::{ContainerCreateBody, VolumeCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveVolumeOptionsBuilder,
};
use bollard::Docker;
use common::{engine, ensure_image, remove_container, suffix, TEST_IMAGE};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use tmop::docker::files::{self, Entry, Target};

/// Removes test containers, volumes, helper containers created during the test and
/// local temporary paths, even when an assertion fails.
struct Cleanup {
    handle: tokio::runtime::Handle,
    docker: Docker,
    containers: Vec<String>,
    volumes: Vec<String>,
    paths: Vec<PathBuf>,
    helpers_before: HashSet<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        for name in &self.containers {
            self.handle.block_on(remove_container(&docker, name));
        }
        let helpers = self.handle.block_on(helper_ids(&docker));
        for id in helpers.difference(&self.helpers_before) {
            self.handle.block_on(remove_container(&docker, id));
        }
        for name in &self.volumes {
            let opts = RemoveVolumeOptionsBuilder::default().force(true).build();
            let _ = self.handle.block_on(docker.remove_volume(name, Some(opts)));
        }
        for path in &self.paths {
            let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
        }
    }
}

/// Ids of all containers carrying the helper label, running or not.
async fn helper_ids(docker: &Docker) -> HashSet<String> {
    let mut filters = HashMap::new();
    filters.insert("label".to_string(), vec!["tmop.helper=1".to_string()]);
    let opts = ListContainersOptionsBuilder::default().all(true).filters(&filters).build();
    docker
        .list_containers(Some(opts))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| c.id)
        .collect()
}

fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("entry {name} not found in {entries:?}"))
}

#[test]
fn container_upload_list_download() {
    let Some((rt, docker)) = engine() else { return };
    let base = std::env::temp_dir().join(format!("tmop-test-files-{}", suffix()));
    let src = base.join("src");
    let dl = base.join("dl");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::create_dir_all(&dl).unwrap();
    let content = b"hello from tmop\n";
    fs::write(src.join("hello.txt"), content).unwrap();
    fs::write(src.join("sub").join("nested.txt"), b"nested").unwrap();

    let name = format!("tmop-test-files-{}", suffix());
    let mut cleanup = Cleanup {
        handle: rt.handle().clone(),
        docker: docker.clone(),
        containers: vec![name.clone()],
        volumes: Vec::new(),
        paths: vec![base.clone()],
        helpers_before: rt.block_on(helper_ids(&docker)),
    };
    let _ = &mut cleanup;

    rt.block_on(async {
        ensure_image(&docker).await.unwrap();
        let options = CreateContainerOptionsBuilder::default().name(&name).build();
        let body = ContainerCreateBody {
            image: Some(TEST_IMAGE.to_string()),
            cmd: Some(vec!["sleep".to_string(), "120".to_string()]),
            ..Default::default()
        };
        docker.create_container(Some(options), body).await.unwrap();
        docker.start_container(&name, None).await.unwrap();

        let target = Target::Container { id: name.clone(), running: true };
        files::upload(&docker, &target, &src.join("hello.txt"), "/tmp").await.unwrap();
        files::upload(&docker, &target, &src.join("sub"), "/tmp").await.unwrap();

        // Running container: listed with ls inside the container.
        let entries = files::list_dir(&docker, &target, "/tmp").await.unwrap();
        let hello = find(&entries, "hello.txt");
        assert_eq!(hello.size, content.len() as u64);
        assert!(!hello.is_dir);
        assert_eq!(hello.path, "/tmp/hello.txt");
        assert!(find(&entries, "sub").is_dir);

        let downloaded = files::download(&docker, &target, "/tmp/hello.txt", &dl).await.unwrap();
        assert_eq!(downloaded, dl.join("hello.txt"));
        assert_eq!(fs::read(&downloaded).unwrap(), content);

        let dir = files::download(&docker, &target, "/tmp/sub", &dl).await.unwrap();
        assert_eq!(fs::read(dir.join("nested.txt")).unwrap(), b"nested");

        // Stopped container: listed through the archive API.
        docker.stop_container(&name, None).await.unwrap();
        let stopped = Target::Container { id: name.clone(), running: false };
        let entries = files::list_dir(&docker, &stopped, "/tmp").await.unwrap();
        let hello = find(&entries, "hello.txt");
        assert_eq!(hello.size, content.len() as u64);
        assert_eq!(hello.path, "/tmp/hello.txt");
        assert!(find(&entries, "sub").is_dir);
    });
}

#[test]
fn volume_export_import() {
    let Some((rt, docker)) = engine() else { return };
    let base = std::env::temp_dir().join(format!("tmop-test-files-vol-{}", suffix()));
    let src = base.join("src");
    fs::create_dir_all(&src).unwrap();
    let content = b"volume payload";
    fs::write(src.join("data.txt"), content).unwrap();
    let tar_path = base.join("volume.tar");

    let id = suffix();
    let vol = format!("tmop-test-files-vol-{id}");
    let vol2 = format!("tmop-test-files-vol2-{id}");
    let helpers_before = rt.block_on(helper_ids(&docker));
    let mut cleanup = Cleanup {
        handle: rt.handle().clone(),
        docker: docker.clone(),
        containers: Vec::new(),
        volumes: vec![vol.clone(), vol2.clone()],
        paths: vec![base.clone()],
        helpers_before: helpers_before.clone(),
    };

    rt.block_on(async {
        ensure_image(&docker).await.unwrap();
        for name in [&vol, &vol2] {
            let request = VolumeCreateRequest { name: Some(name.clone()), ..Default::default() };
            docker.create_volume(request).await.unwrap();
        }
        let target = Target::Volume { name: vol.clone() };
        files::upload(&docker, &target, &src.join("data.txt"), "/").await.unwrap();

        let entries = files::list_dir(&docker, &target, "/").await.unwrap();
        let data = find(&entries, "data.txt");
        assert_eq!(data.size, content.len() as u64);
        assert_eq!(data.path, "/data.txt");

        let written = files::export_volume(&docker, &vol, &tar_path).await.unwrap();
        assert!(written > 0);
        assert_eq!(fs::metadata(&tar_path).unwrap().len(), written);

        files::import_volume(&docker, &vol2, &tar_path).await.unwrap();
        let target2 = Target::Volume { name: vol2.clone() };
        let entries = files::list_dir(&docker, &target2, "/").await.unwrap();
        assert_eq!(find(&entries, "data.txt").size, content.len() as u64);
    });

    // Helper containers created by the run must be gone.
    let leftover: Vec<String> = rt.block_on(helper_ids(&docker)).difference(&helpers_before).cloned().collect();
    assert!(leftover.is_empty(), "helper containers left behind: {leftover:?}");

    // Remove the volumes now, then check that they are really gone.
    rt.block_on(async {
        for name in [&vol, &vol2] {
            let opts = RemoveVolumeOptionsBuilder::default().force(true).build();
            docker.remove_volume(name, Some(opts)).await.unwrap();
            assert!(docker.inspect_volume(name).await.is_err(), "volume {name} still exists");
        }
    });
    cleanup.volumes.clear();
}
