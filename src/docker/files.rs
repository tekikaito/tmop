//! File browser for containers and volumes.
//!
//! Containers are browsed with `ls` inside the container when it is running, and
//! through the archive API when it is stopped. Volumes have no filesystem of their
//! own, so they are mounted into a throwaway busybox helper container (labelled
//! `tmop.helper=1`) that is removed when the operation ends.
//!
//! Paths in [`Entry`] and in the public functions are absolute POSIX paths in the
//! target's own view: for a volume, `/` is the root of the volume.

use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, VolumeCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, DownloadFromContainerOptions, RemoveContainerOptionsBuilder,
    UploadToContainerOptionsBuilder,
};
use bollard::{body_full, body_try_stream, Docker};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::backend::err_text;

/// Image used for the helper container that gives access to volumes.
pub const HELPER_IMAGE: &str = "busybox:stable";
/// Where a volume is mounted inside the helper container.
pub const VOLUME_MOUNT: &str = "/volume";
/// Maximum number of archive bytes read when listing a stopped container's directory.
pub const ARCHIVE_LIST_LIMIT: u64 = 32 * 1024 * 1024;

/// Chunk size used when streaming files to and from disk.
const CHUNK: usize = 256 * 1024;

/// One directory entry as shown by the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Absolute path inside the target (volume root is `/`).
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Permission string such as `drwxr-xr-x`.
    pub mode: String,
    /// Modification time as printed by `ls`, `YYYY-MM-DD HH:MM` for archives, or empty.
    pub modified: String,
    pub link_target: Option<String>,
}

/// Where a file operation runs.
#[derive(Clone, Debug)]
pub enum Target {
    Container { id: String, running: bool },
    Volume { name: String },
}

/// A throwaway busybox container with a volume mounted at [`VOLUME_MOUNT`].
pub struct Helper {
    pub id: String,
    pub volume: String,
}

/// A helper kept alive for several operations on one volume.
pub struct VolumeBrowser {
    pub helper: Helper,
}

impl VolumeBrowser {
    /// Starts a helper for `volume`. The volume must exist.
    pub async fn open(docker: &Docker, volume: &str) -> Result<VolumeBrowser, String> {
        Ok(VolumeBrowser { helper: create_helper(docker, volume).await? })
    }

    /// Lists `path` (volume-relative) through the helper.
    pub async fn list(&self, docker: &Docker, path: &str) -> Result<Vec<Entry>, String> {
        let entries = list_dir_in(docker, &self.helper.id, true, &volume_path(path)).await?;
        Ok(to_volume_entries(entries))
    }

    /// Removes the helper container.
    pub async fn close(self, docker: &Docker) {
        remove_helper(docker, &self.helper).await;
    }
}

/// Starts a helper container with `volume` mounted at [`VOLUME_MOUNT`].
///
/// The image is pulled through the engine when it is not present. The volume must
/// already exist, so that browsing never creates one by accident.
pub async fn create_helper(docker: &Docker, volume: &str) -> Result<Helper, String> {
    docker.inspect_volume(volume).await.map_err(|e| err_text(&e))?;
    ensure_image(docker, HELPER_IMAGE).await?;

    let name = format!("tmop-helper-{}", unique_token());
    let options = CreateContainerOptionsBuilder::default().name(&name).build();
    let mut labels = HashMap::new();
    labels.insert("tmop.helper".to_string(), "1".to_string());
    let body = ContainerCreateBody {
        image: Some(HELPER_IMAGE.to_string()),
        cmd: Some(vec!["sleep".to_string(), "86400".to_string()]),
        labels: Some(labels),
        host_config: Some(HostConfig {
            binds: Some(vec![format!("{volume}:{VOLUME_MOUNT}")]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let created = docker.create_container(Some(options), body).await.map_err(|e| err_text(&e))?;
    let helper = Helper { id: created.id, volume: volume.to_string() };
    if let Err(e) = docker.start_container(&helper.id, None).await {
        remove_helper(docker, &helper).await;
        return Err(err_text(&e));
    }
    Ok(helper)
}

/// Force-removes the helper container. Errors are ignored.
pub async fn remove_helper(docker: &Docker, helper: &Helper) {
    let options = RemoveContainerOptionsBuilder::default().force(true).v(true).build();
    let _ = docker.remove_container(&helper.id, Some(options)).await;
}

/// Lists the directory `path` of `target`, directories first, then by name.
pub async fn list_dir(docker: &Docker, target: &Target, path: &str) -> Result<Vec<Entry>, String> {
    match target {
        Target::Container { id, running } => list_dir_in(docker, id, *running, &normalize(path)).await,
        Target::Volume { name } => {
            let helper = create_helper(docker, name).await?;
            let result = list_dir_in(docker, &helper.id, true, &volume_path(path)).await;
            remove_helper(docker, &helper).await;
            Ok(to_volume_entries(result?))
        }
    }
}

/// Lists `path` inside a container (or helper) with the given running state.
///
/// Running containers are listed with `ls` through an exec; when that is not possible
/// (no shell or no `ls`) or the container is stopped, the archive API is used instead.
pub async fn list_dir_in(docker: &Docker, container_id: &str, running: bool, path: &str) -> Result<Vec<Entry>, String> {
    let path = normalize(path);
    if running {
        if let Some(entries) = list_with_exec(docker, container_id, &path).await? {
            return Ok(entries);
        }
    }
    list_with_archive(docker, container_id, &path).await
}

/// Downloads `path` from `target` and extracts it into `dest_dir`.
///
/// A file lands at `dest_dir/<name>`, a directory at `dest_dir/<name>/...`. Entries
/// whose path would leave `dest_dir` are rejected, and symbolic and hard links are
/// skipped. Returns the path of the created top-level file or directory.
pub async fn download(docker: &Docker, target: &Target, path: &str, dest_dir: &Path) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join(format!("tmop-download-{}.tar", unique_token()));
    let result = download_to(docker, target, path, &tmp, dest_dir).await;
    let _ = fs::remove_file(&tmp);
    result
}

async fn download_to(docker: &Docker, target: &Target, path: &str, tmp: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    match target {
        Target::Container { id, .. } => {
            let stream = archive_stream(docker, id, &normalize(path));
            write_stream(stream, tmp).await?;
        }
        Target::Volume { name } => {
            let helper = create_helper(docker, name).await?;
            let stream = archive_stream(docker, &helper.id, &volume_path(path));
            let written = write_stream(stream, tmp).await;
            remove_helper(docker, &helper).await;
            written?;
        }
    }
    let tmp = tmp.to_path_buf();
    let dest = dest_dir.to_path_buf();
    blocking(move || extract_archive(&tmp, &dest)).await
}

/// Packs the local file or directory `local` and uploads it into `dest_dir` of `target`.
pub async fn upload(docker: &Docker, target: &Target, local: &Path, dest_dir: &str) -> Result<(), String> {
    let name = local
        .file_name()
        .ok_or_else(|| format!("Invalid local path: {}", local.display()))?
        .to_string_lossy()
        .into_owned();
    let source = local.to_path_buf();
    let tar = blocking(move || build_tar(&source, &name)).await?;
    match target {
        Target::Container { id, .. } => upload_bytes(docker, id, &normalize(dest_dir), tar).await,
        Target::Volume { name } => {
            let helper = create_helper(docker, name).await?;
            let result = upload_bytes(docker, &helper.id, &volume_path(dest_dir), tar).await;
            remove_helper(docker, &helper).await;
            result
        }
    }
}

/// Writes the contents of `volume` to `dest_tar` (entries are prefixed `volume/`).
/// Returns the number of bytes written.
pub async fn export_volume(docker: &Docker, volume: &str, dest_tar: &Path) -> Result<u64, String> {
    let helper = create_helper(docker, volume).await?;
    let stream = archive_stream(docker, &helper.id, VOLUME_MOUNT);
    let written = write_stream(stream, dest_tar).await;
    remove_helper(docker, &helper).await;
    written
}

/// Replaces the contents of `volume` with the tar file `src_tar`, as written by
/// [`export_volume`]. The volume is created when it does not exist.
pub async fn import_volume(docker: &Docker, volume: &str, src_tar: &Path) -> Result<(), String> {
    if docker.inspect_volume(volume).await.is_err() {
        let request = VolumeCreateRequest { name: Some(volume.to_string()), ..Default::default() };
        docker.create_volume(request).await.map_err(|e| err_text(&e))?;
    }
    let source = src_tar.to_path_buf();
    let dest = blocking(move || first_entry_dest(&source)).await?;
    let helper = create_helper(docker, volume).await?;
    let body = body_try_stream(file_chunks(src_tar.to_path_buf()).await?);
    let options = UploadToContainerOptionsBuilder::default().path(&dest).build();
    let result = docker.upload_to_container(&helper.id, Some(options), body).await.map_err(|e| err_text(&e));
    remove_helper(docker, &helper).await;
    result
}

/// Joins a POSIX directory and a name. `join("/a", "b") == "/a/b"`.
pub fn join(dir: &str, name: &str) -> String {
    let dir = normalize(dir);
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Returns the parent of a POSIX path. The parent of `/` is `/`.
pub fn parent(path: &str) -> String {
    let path = normalize(path);
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    }
}

/// Parses the output of `ls -la` (busybox or GNU format) for directory `dir`.
/// The `total` line and the `.` and `..` entries are skipped.
pub fn parse_ls(output: &str, dir: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = output.lines().filter_map(|line| parse_ls_line(line.trim_end_matches('\r'), dir)).collect();
    sort_entries(&mut entries);
    entries
}

/// Builds the depth-1 entries of the directory `dir` from the tar bytes returned by the
/// archive API for that path.
pub fn entries_from_archive(tar_bytes: &[u8], dir: &str) -> Result<Vec<Entry>, String> {
    let dir = normalize(dir);
    let base = if dir == "/" { String::new() } else { dir.rsplit('/').next().unwrap_or_default().to_string() };
    let mut archive = tar::Archive::new(tar_bytes);
    let mut entries = Vec::new();
    for item in archive.entries().map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let raw = item.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        let rel = raw.trim_start_matches("./").trim_end_matches('/');
        let inner = if base.is_empty() {
            rel
        } else {
            match rel.strip_prefix(base.as_str()).and_then(|r| r.strip_prefix('/')) {
                Some(r) => r,
                None => continue,
            }
        };
        if inner.is_empty() || inner.contains('/') {
            continue;
        }
        let header = item.header();
        let kind = header.entry_type();
        let is_dir = kind.is_dir();
        let is_symlink = kind.is_symlink();
        let type_char = if is_dir {
            'd'
        } else if is_symlink {
            'l'
        } else {
            '-'
        };
        let link_target = if is_symlink {
            item.link_name().ok().flatten().map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        let name = inner.to_string();
        entries.push(Entry {
            path: join(&dir, &name),
            name,
            is_dir,
            is_symlink,
            size: header.size().unwrap_or(0),
            mode: mode_string(type_char, header.mode().unwrap_or(0)),
            modified: format_unix_time(header.mtime().unwrap_or(0)),
            link_target,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

// ----------------------------------------------------------------------------
// Listing helpers
// ----------------------------------------------------------------------------

/// `ls` run through `sh`; the path is passed as an argument, never interpolated.
const LS_SCRIPT: &str = r#"LC_ALL=C ls -la -- "$1""#;

/// Returns `Ok(None)` when the listing cannot be done with `ls` in the container.
async fn list_with_exec(docker: &Docker, container_id: &str, path: &str) -> Result<Option<Vec<Entry>>, String> {
    let options = CreateExecOptions::<String> {
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        cmd: Some(vec!["sh".into(), "-c".into(), LS_SCRIPT.into(), "sh".into(), path.into()]),
        ..Default::default()
    };
    let Ok(created) = docker.create_exec(container_id, options).await else {
        return Ok(None);
    };
    let Ok(started) = docker.start_exec(&created.id, None).await else {
        return Ok(None);
    };
    let mut output = match started {
        StartExecResults::Attached { output, .. } => output,
        StartExecResults::Detached => return Ok(None),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(item) = output.next().await {
        match item.map_err(|e| err_text(&e))? {
            LogOutput::StdOut { message } => stdout.extend_from_slice(&message),
            LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
            _ => {}
        }
    }
    let inspect = docker.inspect_exec(&created.id).await.map_err(|e| err_text(&e))?;
    match inspect.exit_code {
        Some(0) => Ok(Some(parse_ls(&String::from_utf8_lossy(&stdout), path))),
        // 126 and 127: shell or `ls` could not be run in this image.
        None | Some(126) | Some(127) => Ok(None),
        Some(_) => {
            let message = String::from_utf8_lossy(&stderr).trim().to_string();
            Err(if message.is_empty() { "Cannot list directory".to_string() } else { message })
        }
    }
}

async fn list_with_archive(docker: &Docker, container_id: &str, path: &str) -> Result<Vec<Entry>, String> {
    let mut stream = Box::pin(archive_stream(docker, container_id, path));
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > ARCHIVE_LIST_LIMIT {
            return Err("Directory too large to list while the container is stopped".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    entries_from_archive(&bytes, path)
}

/// Archive bytes of `path`, with bollard errors turned into text.
fn archive_stream(docker: &Docker, container_id: &str, path: &str) -> impl Stream<Item = Result<Bytes, String>> + Unpin {
    let options = DownloadFromContainerOptions { path: path.to_string() };
    Box::pin(docker.download_from_container(container_id, Some(options)).map(|item| item.map_err(|e| err_text(&e))))
}

fn to_volume_entries(mut entries: Vec<Entry>) -> Vec<Entry> {
    for entry in &mut entries {
        entry.path = from_volume_path(&entry.path);
    }
    entries
}

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn parse_ls_line(line: &str, dir: &str) -> Option<Entry> {
    if line.trim().is_empty() || line.starts_with("total") {
        return None;
    }
    let mut rest = line;
    let mode = next_field(&mut rest)?;
    let type_char = mode.chars().next()?;
    let mode = mode.get(..10)?;
    if !"-dlcbps".contains(type_char) {
        return None;
    }
    // Link count, owner and group precede the size.
    for _ in 0..3 {
        next_field(&mut rest)?;
    }
    let size = next_field(&mut rest)?.parse::<u64>().unwrap_or(0);
    let first = next_field(&mut rest)?;
    let modified = if first.starts_with(|c: char| c.is_ascii_alphabetic()) {
        // busybox and GNU: "Oct 15 10:00" or "Oct 15  2025".
        let day = next_field(&mut rest)?;
        let third = next_field(&mut rest)?;
        format!("{first} {day} {third}")
    } else if first.contains('-') {
        // GNU with ISO dates: "2026-10-15 10:00".
        let time = next_field(&mut rest)?;
        format!("{first} {time}")
    } else {
        return None;
    };
    let text = rest.trim_start();
    if text.is_empty() {
        return None;
    }
    let (name, link_target) = if type_char == 'l' {
        match text.split_once(" -> ") {
            Some((n, t)) => (n.to_string(), Some(t.to_string())),
            None => (text.to_string(), None),
        }
    } else {
        (text.to_string(), None)
    };
    if name == "." || name == ".." {
        return None;
    }
    Some(Entry {
        path: join(dir, &name),
        name,
        is_dir: type_char == 'd',
        is_symlink: type_char == 'l',
        size,
        mode: mode.to_string(),
        modified,
        link_target,
    })
}

/// Splits off the next whitespace-separated token and advances `rest` past it.
fn next_field<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let s = rest.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    let (token, after) = s.split_at(end);
    *rest = after;
    Some(token)
}

fn mode_string(type_char: char, mode: u32) -> String {
    let mut out = String::with_capacity(10);
    out.push(type_char);
    for shift in [6u32, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        out.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Formats seconds since the Unix epoch as `YYYY-MM-DD HH:MM` in UTC.
fn format_unix_time(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let days = (secs / 86_400) as i64;
    let minutes = (secs % 86_400) / 60;
    // Civil date from day count (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", minutes / 60, minutes % 60)
}

/// Normalises an absolute POSIX path: leading `/`, no trailing `/`, no `.` or `..`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Maps a volume-relative path to the path inside the helper container.
fn volume_path(path: &str) -> String {
    let path = normalize(path);
    if path == "/" {
        VOLUME_MOUNT.to_string()
    } else {
        format!("{VOLUME_MOUNT}{path}")
    }
}

/// Maps a helper path back to a volume-relative path.
fn from_volume_path(path: &str) -> String {
    match path.strip_prefix(VOLUME_MOUNT) {
        Some("") => "/".to_string(),
        Some(rest) if rest.starts_with('/') => rest.to_string(),
        _ => path.to_string(),
    }
}

async fn ensure_image(docker: &Docker, image: &str) -> Result<(), String> {
    if docker.inspect_image(image).await.is_ok() {
        return Ok(());
    }
    let options = CreateImageOptionsBuilder::default().from_image(image).build();
    let mut stream = docker.create_image(Some(options), None, None);
    while let Some(item) = stream.next().await {
        item.map_err(|e| err_text(&e))?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Transfer helpers
// ----------------------------------------------------------------------------

async fn upload_bytes(docker: &Docker, container_id: &str, dest_dir: &str, tar: Vec<u8>) -> Result<(), String> {
    let options = UploadToContainerOptionsBuilder::default().path(dest_dir).build();
    docker
        .upload_to_container(container_id, Some(options), body_full(Bytes::from(tar)))
        .await
        .map_err(|e| err_text(&e))
}

/// Destination of the first tar entry when it is a `volume/` export, or `/volume`
/// otherwise. Runs on the blocking pool.
fn first_entry_dest(src: &Path) -> Result<String, String> {
    let file = File::open(src).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(file);
    let first = match archive.entries().map_err(|e| e.to_string())?.next() {
        Some(entry) => entry.map_err(|e| e.to_string())?.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned(),
        None => return Ok(VOLUME_MOUNT.to_string()),
    };
    let rel = first.trim_start_matches("./");
    if rel == "volume" || rel.starts_with("volume/") {
        Ok("/".to_string())
    } else {
        Ok(VOLUME_MOUNT.to_string())
    }
}

/// Streams a file as a request body without loading it into memory.
async fn file_chunks(path: PathBuf) -> Result<impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static, String> {
    let file = blocking(move || File::open(&path).map_err(|e| e.to_string())).await?;
    let chunks = futures_util::stream::unfold(Some(file), |state| async move {
        let file = state?;
        let read = tokio::task::spawn_blocking(move || {
            let mut file = file;
            let mut buf = vec![0u8; CHUNK];
            let result = file.read(&mut buf).map(|n| {
                buf.truncate(n);
                buf
            });
            (file, result)
        })
        .await;
        match read {
            Ok((file, Ok(buf))) if buf.is_empty() => {
                drop(file);
                None
            }
            Ok((file, Ok(buf))) => Some((Ok(Bytes::from(buf)), Some(file))),
            Ok((_, Err(e))) => Some((Err(e), None)),
            Err(e) => Some((Err(std::io::Error::other(e)), None)),
        }
    });
    Ok(chunks)
}

/// Writes a byte stream to `path` on the blocking pool, returning the byte count.
async fn write_stream<S>(mut stream: S, path: &Path) -> Result<u64, String>
where
    S: Stream<Item = Result<Bytes, String>> + Unpin,
{
    let target = path.to_path_buf();
    let mut file = Some(blocking(move || File::create(&target).map_err(|e| e.to_string())).await?);
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len() as u64;
        let mut current = file.take().ok_or_else(|| "Internal error: missing file handle".to_string())?;
        let (returned, result) = tokio::task::spawn_blocking(move || {
            let result = current.write_all(&chunk);
            (current, result)
        })
        .await
        .map_err(|e| e.to_string())?;
        result.map_err(|e| e.to_string())?;
        file = Some(returned);
    }
    if let Some(f) = file {
        blocking(move || f.sync_all().map_err(|e| e.to_string())).await?;
    }
    Ok(total)
}

/// Runs a blocking closure on the blocking pool.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Packs `local` (a file or a directory tree) into a tar archive whose top-level entry is `name`.
fn build_tar(local: &Path, name: &str) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    if local.is_dir() {
        builder.append_dir_all(name, local).map_err(|e| e.to_string())?;
    } else {
        builder.append_path_with_name(local, name).map_err(|e| e.to_string())?;
    }
    builder.into_inner().map_err(|e| e.to_string())
}

/// Extracts `tar_path` into `dest_dir` and returns the top-level path created.
fn extract_archive(tar_path: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let file = File::open(tar_path).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(file);
    let mut top: Option<String> = None;
    for item in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = item.map_err(|e| e.to_string())?;
        let raw = entry.path().map_err(|e| e.to_string())?.into_owned();
        let Some(target) = sanitized_target(dest_dir, &raw)? else {
            continue;
        };
        if top.is_none() {
            top = raw.components().find_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            });
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            entry.unpack(&target).map_err(|e| e.to_string())?;
        }
        // Links and special files are not extracted.
    }
    let top = top.ok_or_else(|| "The archive is empty".to_string())?;
    Ok(dest_dir.join(top))
}

/// Joins an archive entry path onto `dest`, rejecting anything that would leave it.
/// Returns `None` for entries without a name (such as `./`).
fn sanitized_target(dest: &Path, entry: &Path) -> Result<Option<PathBuf>, String> {
    let mut out = dest.to_path_buf();
    let mut any = false;
    for component in entry.components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                any = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("Archive entry escapes the destination: {}", entry.display()));
            }
        }
    }
    Ok(if any { Some(out) } else { None })
}

fn unique_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}{:x}", std::process::id(), nanos, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUSYBOX: &str = "total 16
drwxr-xr-x    2 root     root          4096 Oct 15 10:00 .
drwxrwxrwt    1 root     root          4096 Oct 15 10:00 ..
-rw-r--r--    1 root     root            12 Oct 15 10:00 hello.txt
drwxr-xr-x    2 root     root          4096 Sep  3  2025 my dir
lrwxrwxrwx    1 root     root             7 Oct 15 10:00 link -> hello.txt
";

    const GNU: &str = "total 8
drwxr-xr-x 2 root root 4096 2026-10-15 10:00 .
drwxrwxrwt 1 root root 4096 2026-10-15 10:00 ..
-rw-r--r-- 1 root root   12 2026-10-15 10:00 hello.txt
drwxr-xr-x 3 root root 4096 2026-10-14 08:30 Data Files
lrwxrwxrwx 1 root root    7 2026-10-15 10:00 link -> hello.txt
";

    #[test]
    fn parses_busybox_listing() {
        let entries = parse_ls(BUSYBOX, "/tmp");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["my dir", "hello.txt", "link"]);
        let dir = &entries[0];
        assert!(dir.is_dir && !dir.is_symlink);
        assert_eq!(dir.mode, "drwxr-xr-x");
        assert_eq!(dir.path, "/tmp/my dir");
        assert_eq!(dir.modified, "Sep 3 2025");
        let file = &entries[1];
        assert!(!file.is_dir);
        assert_eq!(file.size, 12);
        assert_eq!(file.modified, "Oct 15 10:00");
        let link = &entries[2];
        assert!(link.is_symlink);
        assert_eq!(link.link_target.as_deref(), Some("hello.txt"));
        assert_eq!(link.name, "link");
    }

    #[test]
    fn parses_gnu_listing() {
        let entries = parse_ls(GNU, "/");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Data Files", "hello.txt", "link"]);
        assert_eq!(entries[0].path, "/Data Files");
        assert_eq!(entries[0].modified, "2026-10-14 08:30");
        assert_eq!(entries[1].size, 12);
        assert_eq!(entries[2].link_target.as_deref(), Some("hello.txt"));
    }

    #[test]
    fn skips_total_and_dot_entries() {
        assert!(parse_ls("total 0\n", "/").is_empty());
        assert!(parse_ls("", "/").is_empty());
    }

    #[test]
    fn path_helpers() {
        assert_eq!(parent("/"), "/");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/a/b/"), "/a");
        assert_eq!(parent(""), "/");
        assert_eq!(join("/", "x"), "/x");
        assert_eq!(join("/a/", "x"), "/a/x");
        assert_eq!(join("/a", "x"), "/a/x");
        assert_eq!(normalize("/a/../../b//c/."), "/b/c");
        assert_eq!(volume_path("/"), VOLUME_MOUNT);
        assert_eq!(volume_path("/x/y"), "/volume/x/y");
        assert_eq!(from_volume_path("/volume"), "/");
        assert_eq!(from_volume_path("/volume/x"), "/x");
        assert_eq!(from_volume_path("/volumes"), "/volumes");
    }

    #[test]
    fn mode_and_time_formatting() {
        assert_eq!(mode_string('d', 0o755), "drwxr-xr-x");
        assert_eq!(mode_string('-', 0o640), "-rw-r-----");
        assert_eq!(format_unix_time(0), "");
        assert_eq!(format_unix_time(1_000_000_000), "2001-09-09 01:46");
    }

    fn build_archive(files: &[(&str, Option<&[u8]>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            match data {
                Some(bytes) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder.append_data(&mut header, path, *bytes).unwrap();
                }
                None => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_cksum();
                    builder.append_data(&mut header, path, std::io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn archive_listing_is_depth_one() {
        let tar = build_archive(&[
            ("tmp/", None),
            ("tmp/hello.txt", Some(b"hello")),
            ("tmp/sub/", None),
            ("tmp/sub/deep.txt", Some(b"deep")),
        ]);
        let entries = entries_from_archive(&tar, "/tmp").unwrap();
        let names: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.is_dir)).collect();
        assert_eq!(names, [("sub", true), ("hello.txt", false)]);
        assert_eq!(entries[1].size, 5);
        assert_eq!(entries[1].path, "/tmp/hello.txt");
        assert_eq!(entries[1].mode, "-rw-r--r--");
    }

    #[test]
    fn archive_listing_of_root() {
        let tar = build_archive(&[("etc/", None), ("etc/hosts", Some(b"x")), ("bin/", None)]);
        let entries = entries_from_archive(&tar, "/").unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["bin", "etc"]);
    }

    #[test]
    fn sanitized_target_rejects_escapes() {
        let dest = Path::new("/out");
        assert!(sanitized_target(dest, Path::new("../evil")).is_err());
        assert!(sanitized_target(dest, Path::new("/abs")).is_err());
        assert_eq!(sanitized_target(dest, Path::new("./a/b")).unwrap(), Some(dest.join("a").join("b")));
        assert_eq!(sanitized_target(dest, Path::new("./")).unwrap(), None);
    }

    #[test]
    fn extraction_rejects_parent_entries() {
        let dir = std::env::temp_dir().join(format!("tmop-files-test-{}", unique_token()));
        fs::create_dir_all(&dir).unwrap();
        let tar_path = dir.join("evil.tar");
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(4);
            header.set_mode(0o644);
            // Write the name bytes directly; the builder would refuse a `..` path.
            header.as_old_mut().name[..8].copy_from_slice(b"../evil\0");
            header.set_cksum();
            builder.append(&header, &b"evil"[..]).unwrap();
            builder.finish().unwrap();
        }
        fs::write(&tar_path, &raw).unwrap();
        let out = dir.join("out");
        let result = extract_archive(&tar_path, &out);
        assert!(result.is_err());
        assert!(!dir.join("evil").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
