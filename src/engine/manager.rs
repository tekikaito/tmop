//! Engine manager: the state machine the Docker page drives.
//!
//! One worker thread (`tmop-engine-mgr`) runs every operation in order, so wsl.exe, WinHTTP,
//! registry and file work never happen on the UI thread. Each step updates the shared
//! [`Status`] and calls the `on_change` callback; the UI drains [`Event`]s with
//! [`EngineManager::poll`]. Outside Windows no worker runs and every operation reports that
//! the engine is unavailable.

// The helpers below are called from the Windows worker only; elsewhere they serve the tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::docker::backend::NoticeKind;
use crate::engine::config::EngineConfig;
pub use crate::engine::config::Limits;
use crate::engine::wslconfig;
use crate::engine::Overrides;

/// Answer for every operation on targets other than Windows.
const UNSUPPORTED: &str = "TMOP engine is available on Windows only";
/// Value name under `HKCU\...\Run` for the relay autostart.
const AUTOSTART_NAME: &str = "TMOP engine relay";
/// The engine script inside the distro.
const ENGINE_BIN: &str = "/usr/local/bin/tmop-engine";
/// `engine.json` key recording that we copied the compose plugin ourselves.
const COMPOSE_FLAG: &str = "installedComposePlugin";
/// Install location of the distro and download folder inside the data dir. Named so they do not
/// collide with a user's own `rootfs` or `Downloads` folder.
const ROOTFS_DIR: &str = "engine-rootfs";
const DOWNLOADS_DIR: &str = "engine-downloads";
/// Entries a previous attempt may have left in the data dir; anything else blocks a setup there.
const OWN_ENTRIES: [&str; 3] = [ROOTFS_DIR, DOWNLOADS_DIR, "relay.log"];
/// Description written into the context `meta.json`; a context with another one is not ours.
const CONTEXT_DESCRIPTION: &str = "TMOP engine";
/// `netsh` arguments that delete the inbound rule added by [`EngineManager::add_firewall_rule`].
const FIREWALL_DELETE_ARGS: &str = r#"advfirewall firewall delete rule name="TMOP engine ports""#;
const MIB: u64 = 1024 * 1024;
const START_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const QUICK_TIMEOUT: Duration = Duration::from_secs(30);
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RELAY_READY_TIMEOUT: Duration = Duration::from_secs(15);
const RELAY_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const SHA_MAX_BYTES: usize = 4096;

/// Lifecycle phase shown by the Docker page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Not Windows: nothing can run.
    Unsupported,
    /// No `engine.json` in the data dir.
    #[default]
    NotInstalled,
    Installing,
    Stopped,
    Starting,
    Running,
    Stopping,
    Updating,
    Removing,
    /// The last operation failed; [`Status::error`] says why.
    Failed,
}

/// Progress of the running operation. `total` is 0 when unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub step: String,
    pub done: u64,
    pub total: u64,
}

/// Result of the WSL check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum WslCheck {
    /// Not checked yet, or the check itself failed.
    #[default]
    Unknown,
    /// WSL 2 is present; `version` as printed by `wsl --version`.
    Ready { version: String },
    /// wsl.exe is not installed.
    Missing,
    /// wsl.exe is present but too old; the string is a short note.
    Outdated(String),
}

/// Everything the Docker page shows about the engine.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub phase: Phase,
    pub progress: Option<Progress>,
    /// Message of the last failure, cleared when the next operation starts.
    pub error: Option<String>,
    pub wsl: WslCheck,
    pub data_dir: PathBuf,
    pub installed_version: Option<String>,
    /// Release tag offered by the update check.
    pub update_available: Option<String>,
    pub relay_running: bool,
    pub ports_local_only: bool,
    pub limits: Limits,
    pub autostart: bool,
    pub default_docker: bool,
    /// Docker context name the engine registers under.
    pub context_name: String,
    /// True while any operation is queued or running.
    pub busy: bool,
}

/// Something the UI reacts to, drained with [`EngineManager::poll`].
#[derive(Clone, Debug)]
pub enum Event {
    /// Show a toast.
    Toast { kind: NoticeKind, text: String },
    /// Docker contexts were added, removed or switched; refresh the context list.
    ContextsChanged,
}

/// One queued operation.
enum Op {
    Probe,
    CheckWsl,
    InstallWsl,
    Setup(PathBuf),
    Start,
    Stop,
    Restart,
    Remove,
    Update,
    PortsLocalOnly(bool),
    Limits(Limits),
    Autostart(bool),
    DefaultDocker(bool),
    FirewallRule,
}

/// State shared by the UI handle and the worker.
struct Shared {
    inner: Mutex<Inner>,
    on_change: Arc<dyn Fn() + Send + Sync>,
    /// Set by [`EngineManager::cancel`]; checked by downloads.
    cancel: AtomicBool,
    overrides: Overrides,
    update_check: bool,
}

struct Inner {
    status: Status,
    /// Operations queued or running.
    queued: usize,
    events: Vec<Event>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes the inner state, then notifies the UI without holding the lock.
    fn modify(&self, f: impl FnOnce(&mut Inner)) {
        {
            let mut inner = self.lock();
            f(&mut inner);
        }
        (self.on_change)();
    }

    fn update(&self, f: impl FnOnce(&mut Status)) {
        self.modify(|i| f(&mut i.status));
    }

    fn push(&self, event: Event) {
        self.modify(|i| i.events.push(event));
    }

    fn toast(&self, kind: NoticeKind, text: impl Into<String>) {
        self.push(Event::Toast { kind, text: text.into() });
    }

    fn progress(&self, step: impl Into<String>, done: u64, total: u64) {
        let step = step.into();
        self.update(|s| s.progress = Some(Progress { step, done, total }));
    }

    fn finish_op(&self) {
        self.modify(|i| {
            i.queued = i.queued.saturating_sub(1);
            i.status.busy = i.queued > 0;
        });
    }
}

/// Owner of the engine state. Cheap to query; all work happens on the worker thread.
pub struct EngineManager {
    shared: Arc<Shared>,
    /// `None` outside Windows, where no worker runs.
    tx: Option<mpsc::Sender<Op>>,
}

impl EngineManager {
    /// Reads `engine.json` from `data_dir` (the env override wins) and probes the engine on the
    /// worker. `update_check` enables the release lookup that sets `update_available`.
    pub fn new(data_dir: PathBuf, update_check: bool, on_change: Arc<dyn Fn() + Send + Sync>) -> Self {
        let overrides = Overrides::from_env();
        let data_dir = overrides.data_dir.clone().unwrap_or(data_dir);
        let cfg = EngineConfig::load(&data_dir);
        let phase = if !cfg!(windows) {
            Phase::Unsupported
        } else if cfg.is_some() {
            Phase::Stopped
        } else {
            Phase::NotInstalled
        };
        let status = Status {
            phase,
            installed_version: cfg.as_ref().map(|c| c.engine_version.clone()),
            ports_local_only: cfg.as_ref().is_some_and(|c| c.ports_local_only),
            limits: cfg.as_ref().map(|c| c.limits.clone()).unwrap_or_default(),
            default_docker: cfg.as_ref().is_some_and(|c| c.default_docker),
            autostart: cfg.as_ref().is_some_and(|c| c.autostart),
            context_name: overrides.context.clone(),
            data_dir,
            ..Default::default()
        };
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner { status, queued: 0, events: Vec::new() }),
            on_change,
            cancel: AtomicBool::new(false),
            overrides,
            update_check,
        });
        let tx = spawn_worker(&shared);
        let manager = EngineManager { shared, tx };
        // Without a worker (non-Windows) there is nothing to probe; only a failed Windows spawn is an error.
        if manager.tx.is_some() {
            manager.queue(Op::Probe);
        } else if cfg!(windows) {
            manager.shared.toast(NoticeKind::Error, "Could not start the engine worker thread");
        }
        manager
    }

    /// Cheap copy of the current status.
    pub fn status(&self) -> Status {
        self.shared.lock().status.clone()
    }

    /// Takes the events queued since the last call. Call once per frame.
    pub fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.shared.lock().events)
    }

    /// Re-runs the WSL check.
    pub fn check_wsl(&self) {
        self.queue(Op::CheckWsl);
    }

    /// Starts `wsl --install --no-distribution` with a UAC prompt. `Err` only when it cannot be queued.
    pub fn install_wsl(&self) -> Result<(), String> {
        self.submit(Op::InstallWsl)
    }

    /// Full install into `data_dir`: download (or `TMOP_ENGINE_TARBALL`), verify, import, start,
    /// create the Docker context. The folder must be empty or hold only leftovers of an earlier
    /// attempt; a folder with an `engine.json` is started instead. A failure after the import is
    /// rolled back (distro unregistered, rootfs and `engine.json` deleted) and ends in
    /// `Phase::NotInstalled` with `error` set; other failures end in `Phase::Failed`.
    pub fn setup(&self, data_dir: PathBuf) {
        self.queue(Op::Setup(data_dir));
    }

    /// Starts dockerd and the relay.
    pub fn start(&self) {
        self.queue(Op::Start);
    }

    /// Stops the relay, dockerd and the distro. Refused when the engine is not installed.
    /// `Err` from the worker when the relay survives two stop signals (the distro is then left running).
    pub fn stop(&self) {
        self.queue(Op::Stop);
    }

    /// [`stop`](Self::stop) then [`start`](Self::start).
    pub fn restart(&self) {
        self.queue(Op::Restart);
    }

    /// Removes the engine: best-effort steps (relay, default-docker settings, autostart, firewall
    /// rule, distro, context, files), errors collected into one toast. Files are only deleted when
    /// `engine.json` exists and parses, and the distro is gone. Without it only the status is reset.
    pub fn remove(&self) {
        self.queue(Op::Remove);
    }

    /// Upgrades the engine to this build's release, keeping containers, images and volumes.
    /// Skipped with "already up to date" when the installed version matches (unless
    /// `TMOP_ENGINE_TARBALL` is set). A stopped engine stays stopped.
    pub fn update(&self) {
        self.queue(Op::Update);
    }

    /// Saved to `engine.json`; the relay picks it up within seconds.
    pub fn set_ports_local_only(&self, local_only: bool) {
        self.queue(Op::PortsLocalOnly(local_only));
    }

    /// Applies CPU and memory limits (immediately when running) and saves them.
    pub fn set_limits(&self, limits: Limits) {
        self.queue(Op::Limits(limits));
    }

    /// Adds or removes the HKCU Run entry that starts the relay at logon.
    /// `Err` when the engine is not installed; registry failures are toasts.
    pub fn set_autostart(&self, on: bool) -> Result<(), String> {
        self.check_installed()?;
        self.submit(Op::Autostart(on))
    }

    /// "Use TMOP as default Docker": installs the CLI in the bin dir, puts it on the user PATH and
    /// switches the Docker context. `false` reverses all of it.
    /// `Err` when the engine is not installed; failures are toasts.
    pub fn set_default_docker(&self, on: bool) -> Result<(), String> {
        self.check_installed()?;
        self.submit(Op::DefaultDocker(on))
    }

    /// Writes `memory` and `processors` into `[wsl2]` of the `.wslconfig` (`TMOP_WSLCONFIG`
    /// overrides the path). Runs synchronously. Returns the backup path, or `None` when there was
    /// no file to back up. WSL must be restarted by the user for it to apply.
    pub fn apply_wslconfig(&self, memory_gb: Option<u32>, processors: Option<u32>) -> Result<Option<PathBuf>, String> {
        if !cfg!(windows) {
            return Err(UNSUPPORTED.into());
        }
        let path = wslconfig_path()?;
        wslconfig::apply(&path, memory_gb, processors, unix_now())
    }

    /// Queues the UAC-elevated `netsh` rule that lets other machines reach the published ports and
    /// records it in `engine.json`, so [`remove`](Self::remove) deletes it again. `Err` when the engine
    /// is not installed or the request cannot be queued; a declined prompt is a toast.
    pub fn add_firewall_rule(&self) -> Result<(), String> {
        self.check_installed()?;
        self.submit(Op::FirewallRule)
    }

    /// Stops a running setup or update download. Takes effect at the next chunk.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }

    fn queue(&self, op: Op) {
        if let Err(e) = self.submit(op) {
            self.shared.toast(NoticeKind::Error, e);
        }
    }

    fn submit(&self, op: Op) -> Result<(), String> {
        let tx = self.tx.as_ref().ok_or_else(|| UNSUPPORTED.to_string())?;
        self.shared.modify(|i| {
            i.queued += 1;
            i.status.busy = true;
        });
        tx.send(op).map_err(|_| "the engine worker has stopped".to_string())
    }

    fn check_installed(&self) -> Result<(), String> {
        if self.tx.is_none() {
            return Err(UNSUPPORTED.into());
        }
        if self.shared.lock().status.installed_version.is_none() {
            return Err("TMOP engine is not installed".into());
        }
        Ok(())
    }
}

fn spawn_worker(shared: &Arc<Shared>) -> Option<mpsc::Sender<Op>> {
    if !cfg!(windows) {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let shared = shared.clone();
    thread::Builder::new().name("tmop-engine-mgr".into()).spawn(move || worker(rx, shared)).ok()?;
    Some(tx)
}

fn worker(rx: mpsc::Receiver<Op>, shared: Arc<Shared>) {
    for op in rx {
        if let Err(e) = execute(&shared, op) {
            // Only a step that already set a transitional phase fails the engine; a refused
            // request (wrong phase, not installed) leaves the state as it was.
            let transitional = matches!(
                shared.lock().status.phase,
                Phase::Installing | Phase::Starting | Phase::Stopping | Phase::Updating | Phase::Removing
            );
            if transitional {
                shared.update(|s| {
                    s.phase = Phase::Failed;
                    s.error = Some(e.clone());
                    s.progress = None;
                });
            }
            shared.toast(NoticeKind::Error, e);
        }
        shared.finish_op();
    }
}

#[cfg(windows)]
fn execute(shared: &Shared, op: Op) -> Result<(), String> {
    ops::run(shared, op)
}

#[cfg(not(windows))]
fn execute(_shared: &Shared, _op: Op) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn wslconfig_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("TMOP_WSLCONFIG").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    Ok(PathBuf::from(home).join(".wslconfig"))
}

/// The Docker CLI config dir, or an error when it cannot be determined.
fn docker_dir_path() -> Result<PathBuf, String> {
    crate::docker::context::docker_dir().ok_or_else(|| "cannot find the Docker config folder".to_string())
}

/// True when `dir` may hold the engine: see [`is_safe_data_dir_in`] for the rules.
fn is_safe_data_dir(dir: &Path) -> bool {
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    is_safe_data_dir_in(dir, profile.as_deref(), local_app_data.as_deref())
}

/// Refuses a data dir that could hold anything but ours: a relative path, a drive root or a single
/// folder below it, the user profile itself and `%LOCALAPPDATA%` itself. Path comparison ignores
/// case and trailing separators, as Windows does.
fn is_safe_data_dir_in(dir: &Path, profile: Option<&Path>, local_app_data: Option<&Path>) -> bool {
    if !dir.is_absolute() {
        return false;
    }
    let folders = dir.components().filter(|c| matches!(c, Component::Normal(_))).count();
    if folders < 2 {
        return false;
    }
    ![profile, local_app_data].into_iter().flatten().any(|p| same_path(dir, p))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    norm(a) == norm(b)
}

/// `Ok` when `dir` is missing, empty, or holds only [`OWN_ENTRIES`]. Any other entry (including an
/// `engine.json` that does not parse) means the folder belongs to someone else.
fn check_setup_dir(dir: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !OWN_ENTRIES.iter().any(|own| own.eq_ignore_ascii_case(&name)) {
            return Err("Choose an empty folder or the previous engine folder".into());
        }
    }
    Ok(())
}

/// True when the installed version is the one `target` names.
pub fn installed_matches(installed: &str, target: &str) -> bool {
    !installed.is_empty() && installed == target
}

/// Sends a stop signal and waits up to `RELAY_STOP_TIMEOUT` for the relay to exit; signals once more
/// when it is still alive. `Err` when it survives both rounds.
fn signal_until_gone(mut signal: impl FnMut(), mut gone: impl FnMut(Duration) -> bool) -> Result<(), String> {
    for _ in 0..2 {
        signal();
        if gone(RELAY_STOP_TIMEOUT) {
            return Ok(());
        }
    }
    Err("relay did not stop".into())
}

/// Records one best-effort step's failure; the caller keeps going.
fn collect(errors: &mut Vec<String>, result: Result<(), String>) {
    if let Err(e) = result {
        errors.push(e);
    }
}

/// `Ok` when no step failed, else all messages joined.
fn join_errors(errors: &[String]) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// `tmop-engine-<tag>-<arch>.tar.gz` (or `.tar`) to its release tag, e.g.
/// `tmop-engine-v0.0.0-test-x64.tar.gz` gives `v0.0.0-test`. `None` when the name has another shape.
pub fn tag_from_asset_name(name: &str) -> Option<String> {
    let core = name.strip_prefix("tmop-engine-")?;
    let core = core.strip_suffix(".tar.gz").or_else(|| core.strip_suffix(".tar"))?;
    let tag = core.strip_suffix("-x64").or_else(|| core.strip_suffix("-arm64"))?;
    (tag.starts_with('v') && tag.len() > 1).then(|| tag.to_string())
}

/// `contexts\meta\<sha256 hex of name>` under the Docker config dir.
pub fn context_meta_dir(docker_dir: &Path, name: &str) -> PathBuf {
    let hash: String = Sha256::digest(name.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    docker_dir.join("contexts").join("meta").join(hash)
}

/// The `meta.json` of the engine context: a named pipe endpoint.
pub fn context_meta_json(name: &str, pipe: &str) -> String {
    json!({
        "Name": name,
        "Metadata": {"Description": CONTEXT_DESCRIPTION},
        "Endpoints": {"docker": {"Host": format!("npipe:////./pipe/{pipe}"), "SkipTLSVerify": false}}
    })
    .to_string()
}

/// `Metadata.Description` of a context `meta.json`; `None` when it does not parse.
fn meta_description(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    v["Metadata"]["Description"].as_str().map(str::to_string)
}

/// `Ok` when the context slot is free or already holds the engine's context; `Err` when another
/// Docker context has the name.
fn check_context_slot(docker_dir: &Path, name: &str) -> Result<(), String> {
    let file = context_meta_dir(docker_dir, name).join("meta.json");
    match fs::read_to_string(&file) {
        Ok(text) if meta_description(&text).as_deref() != Some(CONTEXT_DESCRIPTION) => {
            Err(format!("a Docker context named {name} already exists"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// Writes the engine's context. Refuses to overwrite a context with another description.
fn write_context(docker_dir: &Path, name: &str, pipe: &str) -> Result<(), String> {
    check_context_slot(docker_dir, name)?;
    let dir = context_meta_dir(docker_dir, name);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let file = dir.join("meta.json");
    fs::write(&file, context_meta_json(name, pipe)).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// Deletes the context folder, but only when its `meta.json` carries the engine's description.
/// Any other context (or none) is left alone and counts as success.
fn remove_context(docker_dir: &Path, name: &str) -> Result<(), String> {
    let dir = context_meta_dir(docker_dir, name);
    let file = dir.join("meta.json");
    match fs::read_to_string(&file) {
        Ok(text) if meta_description(&text).as_deref() == Some(CONTEXT_DESCRIPTION) => remove_dir_if_exists(&dir),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// A JSON object from `json`; blank input is an empty object; anything else is `None`.
fn parse_object(json: &str) -> Option<Map<String, Value>> {
    if json.trim().is_empty() {
        return Some(Map::new());
    }
    match serde_json::from_str::<Value>(json).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn to_pretty(map: Map<String, Value>) -> String {
    serde_json::to_string_pretty(&Value::Object(map)).expect("json object serializes")
}

/// Sets `currentContext` in a Docker `config.json` text, keeping every other key in place.
/// Returns the new text and the previous value. Blank input counts as `{}`; input that is not a
/// JSON object is returned unchanged with no previous value.
pub fn set_current_context(json: &str, name: &str) -> (String, Option<String>) {
    let Some(mut map) = parse_object(json) else {
        return (json.to_string(), None);
    };
    let previous = map.get("currentContext").and_then(Value::as_str).map(str::to_string);
    map.insert("currentContext".into(), Value::String(name.into()));
    (to_pretty(map), previous)
}

/// Undoes [`set_current_context`]: when `currentContext` still equals `ours`, it is set back to
/// `previous`, or removed when there was none. Anything else is returned unchanged.
pub fn restore_current_context(json: &str, ours: &str, previous: Option<&str>) -> String {
    let Some(map) = parse_object(json) else {
        return json.to_string();
    };
    if map.get("currentContext").and_then(Value::as_str) != Some(ours) {
        return json.to_string();
    }
    let map = match previous {
        Some(prev) => {
            let mut map = map;
            map.insert("currentContext".into(), Value::String(prev.into()));
            map
        }
        // Rebuilt rather than removed in place, so the order of the other keys is kept.
        None => map.into_iter().filter(|(k, _)| k != "currentContext").collect(),
    };
    to_pretty(map)
}

fn read_text_or_empty(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
    }
}

#[cfg(windows)]
mod ops {
    use super::*;
    use crate::engine::asset;
    use crate::engine::{arch, version_tag};
    use crate::engine::http;
    use crate::engine::pathenv;
    use crate::engine::winreg::{self, NamedEvent, NamedMutex};
    use crate::engine::wsl::{self, WslProblem};
    use std::time::Instant;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, BOOL};
    use windows::Win32::System::Threading::{CreateEventW, ResetEvent};

    pub(super) fn run(sh: &Shared, op: Op) -> Result<(), String> {
        match op {
            Op::Probe => {
                probe(sh);
                Ok(())
            }
            Op::CheckWsl => {
                let (check, _) = wsl_status();
                sh.update(|s| s.wsl = check);
                Ok(())
            }
            Op::InstallWsl => {
                wsl::install_wsl_elevated()?;
                sh.toast(
                    NoticeKind::Info,
                    "WSL installation started; restart Windows when it finishes, then set up the engine again",
                );
                Ok(())
            }
            Op::Setup(dir) => setup(sh, dir),
            Op::Start => start(sh),
            Op::Stop => stop(sh),
            Op::Restart => restart(sh),
            Op::Remove => remove(sh),
            Op::Update => update(sh),
            Op::PortsLocalOnly(on) => set_ports_local_only(sh, on),
            Op::Limits(limits) => set_limits(sh, limits),
            Op::Autostart(on) => set_autostart(sh, on),
            Op::DefaultDocker(on) => {
                if on {
                    enable_default_docker(sh)
                } else {
                    disable_default_docker_op(sh)
                }
            }
            Op::FirewallRule => {
                let dir = data_dir(sh);
                let mut cfg = load_config(sh)?;
                let exe = std::env::current_exe().map_err(|e| format!("cannot find TMOP.exe: {e}"))?;
                winreg::run_elevated("netsh.exe", &pathenv::firewall_rule_args(&exe).join(" "))?;
                cfg.firewall_rule = true;
                cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
                sh.toast(NoticeKind::Info, "Approve the Windows prompt to add the firewall rule");
                Ok(())
            }
        }
    }

    fn data_dir(sh: &Shared) -> PathBuf {
        sh.lock().status.data_dir.clone()
    }

    fn load_config(sh: &Shared) -> Result<EngineConfig, String> {
        EngineConfig::load(&data_dir(sh)).ok_or_else(|| "TMOP engine is not installed".to_string())
    }

    /// Named kernel object of the relay: `Local\tmop_engine_relay_<kind><pipe>`.
    fn relay_name(kind: &str, pipe: &str) -> String {
        format!(r"Local\tmop_engine_relay_{kind}{pipe}")
    }

    /// True while a relay holds the single-instance mutex.
    fn relay_running(pipe: &str) -> bool {
        matches!(NamedMutex::try_acquire(&relay_name("", pipe)), Ok(None))
    }

    fn wait_relay_gone(pipe: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while relay_running(pipe) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(200));
        }
        true
    }

    /// Makes sure a manual-reset event exists and is unset. Its handle is closed again, so the
    /// object disappears when nobody else holds it.
    fn reset_event(name: &str) -> Result<(), String> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and alive for the calls; the handle is closed before returning.
        unsafe {
            let handle = CreateEventW(None, BOOL(1), BOOL(0), PCWSTR(wide.as_ptr())).map_err(|e| format!("CreateEventW failed: {e}"))?;
            let reset = ResetEvent(handle);
            let _ = CloseHandle(handle);
            reset.map_err(|e| format!("ResetEvent failed: {e}"))
        }
    }

    /// Runs `tmop-engine <args>` in the distro; returns stdout on exit code 0.
    fn engine_exec(ov: &Overrides, args: &[&str], timeout: Duration) -> Result<String, String> {
        let mut argv = vec![ENGINE_BIN];
        argv.extend_from_slice(args);
        let out = wsl::exec(&ov.distro, &argv, timeout)?;
        if out.status == 0 {
            return Ok(out.stdout);
        }
        let detail = [&out.stderr, &out.stdout].into_iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("no output");
        Err(format!("tmop-engine {} failed (exit code {}): {detail}", args.join(" "), out.status))
    }

    /// True when the distro runs and `tmop-engine status` exits 0. Never starts the distro.
    fn engine_is_up(ov: &Overrides) -> bool {
        wsl::is_running(&ov.distro) && engine_exec(ov, &["status"], QUICK_TIMEOUT).is_ok()
    }

    /// Starts the relay if none is running and waits for its ready event.
    fn ensure_relay(ov: &Overrides) -> Result<(), String> {
        if relay_running(&ov.pipe) {
            return Ok(());
        }
        let ready_name = relay_name("ready_", &ov.pipe);
        reset_event(&ready_name)?;
        reset_event(&relay_name("stop_", &ov.pipe))?;
        let exe = std::env::current_exe().map_err(|e| format!("cannot find TMOP.exe: {e}"))?;
        winreg::spawn_detached(&exe, &["--engine-relay"]).map_err(|e| format!("could not start the relay: {e}"))?;
        let ready = NamedEvent::create(&ready_name)?;
        if ready.wait(RELAY_READY_TIMEOUT) {
            Ok(())
        } else {
            Err("the relay did not start; see relay.log in the engine data folder".into())
        }
    }

    /// Starts dockerd and optionally the relay. The caller sets the phase.
    fn start_engine(ov: &Overrides, relay: bool) -> Result<(), String> {
        let out = engine_exec(ov, &["start"], START_TIMEOUT)?;
        if !out.lines().any(|l| l.trim() == "ok") {
            return Err(format!("the engine did not confirm the start: {}", out.trim()));
        }
        if relay {
            ensure_relay(ov)?;
        }
        Ok(())
    }

    /// Signals the relay to exit and waits for it (see [`signal_until_gone`]). `Err` when it stays alive.
    fn stop_relay(ov: &Overrides) -> Result<(), String> {
        let stop = NamedEvent::create(&relay_name("stop_", &ov.pipe))?;
        let result = signal_until_gone(|| stop.set(), |timeout| wait_relay_gone(&ov.pipe, timeout));
        drop(stop);
        result
    }

    /// Stops dockerd and terminates the distro; does nothing when the distro is not running.
    /// Both steps are attempted even when the first fails.
    fn stop_distro(ov: &Overrides) -> Result<(), String> {
        if !wsl::is_running(&ov.distro) {
            return Ok(());
        }
        let stopped = engine_exec(ov, &["stop"], STOP_TIMEOUT).map(|_| ());
        let terminated = wsl::terminate(&ov.distro);
        stopped.and(terminated)
    }

    /// Stops the relay, then dockerd and the distro. A relay that survives is an error and the
    /// distro keeps running. The caller sets the phase.
    fn stop_engine(ov: &Overrides) -> Result<(), String> {
        stop_relay(ov)?;
        stop_distro(ov)
    }

    fn write_engine_context(sh: &Shared) -> Result<(), String> {
        let ov = &sh.overrides;
        write_context(&docker_dir_path()?, &ov.context, &ov.pipe)?;
        sh.push(Event::ContextsChanged);
        Ok(())
    }

    /// WSL state for the UI, plus the error text when it is not ready.
    fn wsl_status() -> (WslCheck, Result<(), String>) {
        match wsl::check() {
            Ok(version) => (WslCheck::Ready { version }, Ok(())),
            Err(WslProblem::NotInstalled) => (WslCheck::Missing, Err("WSL is not installed".into())),
            Err(WslProblem::NoVersionCommand) => {
                let note = "WSL is too old; update it from Windows Update or the Microsoft Store";
                (WslCheck::Outdated(note.into()), Err(note.into()))
            }
            Err(WslProblem::Failed(e)) => (WslCheck::Unknown, Err(format!("WSL check failed: {e}"))),
        }
    }

    /// Reads everything from disk and the OS once at startup.
    fn probe(sh: &Shared) {
        let ov = &sh.overrides;
        let dir = data_dir(sh);
        let (check, wsl_ok) = wsl_status();
        let relay = relay_running(&ov.pipe);
        let autostart = matches!(winreg::run_key_get(AUTOSTART_NAME), Ok(Some(_)));
        let Some(cfg) = EngineConfig::load(&dir) else {
            sh.update(|s| {
                s.wsl = check;
                s.phase = Phase::NotInstalled;
                s.relay_running = relay;
                s.autostart = autostart;
            });
            return;
        };
        let running = wsl_ok.is_ok() && engine_is_up(ov);
        let current = version_tag();
        let update = sh.update_check
            && asset::update_available(&cfg.engine_version, &current)
            && matches!(http::head_status(&asset::asset_url(&current, arch())), Ok(200));
        sh.update(|s| {
            s.wsl = check;
            s.error = wsl_ok.err();
            s.phase = if running { Phase::Running } else { Phase::Stopped };
            s.installed_version = Some(cfg.engine_version.clone());
            s.update_available = update.then_some(current);
            s.relay_running = relay;
            s.ports_local_only = cfg.ports_local_only;
            s.limits = cfg.limits.clone();
            s.autostart = autostart;
            s.default_docker = cfg.default_docker;
        });
    }

    /// The release tarball for this build, from `TMOP_ENGINE_TARBALL` or GitHub.
    struct Source {
        tag: String,
        path: PathBuf,
        /// Downloaded by us, so it is deleted after use. Overrides are never deleted.
        owned: bool,
    }

    /// Progress reporter: updates the status at most once per MiB, and at the end.
    fn progress_sink<'a>(sh: &'a Shared, label: impl Fn(u64, u64) -> String + 'a) -> impl FnMut(u64, u64) + 'a {
        let mut last = 0u64;
        move |done, total| {
            if done == total || done.saturating_sub(last) >= MIB {
                last = done;
                sh.progress(label(done, total), done, total);
            }
        }
    }

    /// Downloads and verifies the release (or takes the override). `Ok(None)` when cancelled.
    fn acquire(sh: &Shared, dir: &Path) -> Result<Option<Source>, String> {
        let ov = &sh.overrides;
        if let Some(path) = &ov.tarball {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let tag = tag_from_asset_name(name).unwrap_or_else(version_tag);
            return Ok(Some(Source { tag, path: path.clone(), owned: false }));
        }
        let tag = version_tag();
        let dst = dir.join(DOWNLOADS_DIR).join(asset::asset_name(&tag, arch()));
        sh.progress("Downloading engine", 0, 0);
        let expected = http::fetch_text(&asset::sha_url(&tag, arch()), SHA_MAX_BYTES)
            .and_then(|t| asset::parse_sha256_file(&t).ok_or_else(|| "the checksum file is malformed".to_string()))?;
        let sink = progress_sink(sh, |done, total| {
            if total == 0 {
                "Downloading engine".to_string()
            } else {
                format!("Downloading engine ({} of {} MB)", done / MIB, total.div_ceil(MIB))
            }
        });
        match http::download_to_file(&asset::asset_url(&tag, arch()), &dst, &sh.cancel, sink) {
            Ok(()) => {}
            Err(_) if sh.cancel.load(Ordering::SeqCst) => return Ok(None),
            Err(e) => return Err(e),
        }
        let size = fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
        let mut sink = progress_sink(sh, |_, _| "Verifying download".to_string());
        if let Err(e) = asset::verify_file(&dst, &expected, |d| sink(d, size)) {
            let _ = fs::remove_file(&dst);
            return Err(e);
        }
        Ok(Some(Source { tag, path: dst, owned: true }))
    }

    fn is_plain_tar(path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tar"))
    }

    /// Unpacks the source to a plain tar and imports it as the engine distro.
    fn unpack_and_import(sh: &Shared, dir: &Path, src: &Source) -> Result<(), String> {
        let ov = &sh.overrides;
        let tar = if is_plain_tar(&src.path) {
            src.path.clone()
        } else {
            sh.progress("Unpacking", 0, 0);
            let tar = dir.join(DOWNLOADS_DIR).join("rootfs.tar");
            asset::gunzip_to(&src.path, &tar, |_| {}).map_err(|e| format!("could not unpack the engine: {e}"))?;
            tar
        };
        let rootfs = dir.join(ROOTFS_DIR);
        sh.progress("Importing into WSL", 0, 0);
        // Not registered (checked by the caller), so anything here is left over from an earlier attempt.
        remove_dir_if_exists(&rootfs)?;
        fs::create_dir_all(&rootfs).map_err(|e| format!("cannot create {}: {e}", rootfs.display()))?;
        let imported = wsl::import(&ov.distro, &rootfs, &tar);
        if tar != src.path {
            let _ = fs::remove_file(&tar);
        }
        imported
    }

    /// Installs into `requested` (or the override). Refuses unsafe folders and folders with foreign
    /// files before anything is changed. A folder with `engine.json` and a registered distro is
    /// started instead.
    fn setup(sh: &Shared, requested: PathBuf) -> Result<(), String> {
        let ov = &sh.overrides;
        let dir = ov.data_dir.clone().unwrap_or(requested);
        if !is_safe_data_dir(&dir) {
            return Err(format!("refusing to use {} as the engine folder", dir.display()));
        }
        if let Some(cfg) = EngineConfig::load(&dir) {
            if !wsl::is_registered(&ov.distro) {
                return Err("TMOP engine is already set up here; remove it first".into());
            }
            sh.update(|s| {
                s.data_dir = dir.clone();
                s.installed_version = Some(cfg.engine_version.clone());
            });
            return start(sh);
        }
        check_setup_dir(&dir)?;
        check_context_slot(&docker_dir_path()?, &ov.context)?;
        sh.cancel.store(false, Ordering::SeqCst);
        sh.update(|s| {
            s.phase = Phase::Installing;
            s.error = None;
            s.data_dir = dir.clone();
        });
        sh.progress("Checking WSL", 0, 0);
        let (check, wsl_ok) = wsl_status();
        sh.update(|s| s.wsl = check);
        wsl_ok?;
        if wsl::is_registered(&ov.distro) {
            return Err(format!("a WSL distro named {} already exists; remove it first", ov.distro));
        }
        fs::create_dir_all(dir.join(DOWNLOADS_DIR)).map_err(|e| format!("cannot create the downloads folder: {e}"))?;

        let Some(src) = acquire(sh, &dir)? else {
            sh.update(|s| {
                s.phase = Phase::NotInstalled;
                s.progress = None;
            });
            sh.toast(NoticeKind::Info, "Engine setup cancelled");
            return Ok(());
        };
        let imported = unpack_and_import(sh, &dir, &src);
        if src.owned {
            let _ = fs::remove_file(&src.path);
        }
        if let Err(e) = imported {
            // The error stays in the status; the op itself completed.
            sh.update(|s| {
                s.phase = Phase::NotInstalled;
                s.error = Some(e.clone());
                s.progress = None;
            });
            sh.toast(NoticeKind::Error, e);
            return Ok(());
        }

        if let Err(e) = finish_setup(sh, &dir, &src.tag) {
            let message = match rollback_import(sh, &dir) {
                Ok(()) => e,
                Err(rollback) => format!("{e} (rollback also failed: {rollback})"),
            };
            sh.update(|s| {
                s.phase = Phase::NotInstalled;
                s.error = Some(message.clone());
                s.progress = None;
                s.installed_version = None;
            });
            sh.toast(NoticeKind::Error, message);
            return Ok(());
        }
        sh.update(|s| {
            s.phase = Phase::Running;
            s.progress = None;
            s.relay_running = true;
        });
        sh.toast(NoticeKind::Success, "TMOP engine is set up and running");
        Ok(())
    }

    /// Steps after the import: engine.json, start (with relay), Docker context.
    fn finish_setup(sh: &Shared, dir: &Path, tag: &str) -> Result<(), String> {
        let ov = &sh.overrides;
        EngineConfig::new(tag, arch(), &ov.distro).save(dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        sh.update(|s| s.installed_version = Some(tag.to_string()));
        sh.progress("Starting engine", 0, 0);
        start_engine(ov, true)?;
        sh.progress("Creating Docker context", 0, 0);
        write_engine_context(sh)
    }

    /// Undoes an import whose setup failed afterwards: stops the relay and the distro, unregisters
    /// the distro and deletes the rootfs and engine.json. Errors are collected and returned together.
    fn rollback_import(sh: &Shared, dir: &Path) -> Result<(), String> {
        let ov = &sh.overrides;
        let mut errors = Vec::new();
        collect(&mut errors, stop_relay(ov));
        collect(&mut errors, stop_distro(ov));
        if wsl::is_registered(&ov.distro) {
            collect(&mut errors, wsl::unregister(&ov.distro));
        }
        collect(&mut errors, remove_dir_if_exists(&dir.join(ROOTFS_DIR)));
        collect(&mut errors, remove_file_if_exists(&dir.join(crate::engine::config::FILE_NAME)));
        join_errors(&errors)
    }

    fn start(sh: &Shared) -> Result<(), String> {
        let ov = &sh.overrides;
        load_config(sh)?;
        sh.update(|s| {
            s.phase = Phase::Starting;
            s.error = None;
        });
        start_engine(ov, true)?;
        write_engine_context(sh)?;
        let relay = relay_running(&ov.pipe);
        sh.update(|s| {
            s.phase = Phase::Running;
            s.relay_running = relay;
        });
        Ok(())
    }

    fn stop(sh: &Shared) -> Result<(), String> {
        let ov = &sh.overrides;
        load_config(sh)?;
        sh.update(|s| {
            s.phase = Phase::Stopping;
            s.error = None;
        });
        stop_engine(ov)?;
        let relay = relay_running(&ov.pipe);
        sh.update(|s| {
            s.phase = Phase::Stopped;
            s.relay_running = relay;
        });
        Ok(())
    }

    fn restart(sh: &Shared) -> Result<(), String> {
        let ov = &sh.overrides;
        load_config(sh)?;
        sh.update(|s| {
            s.phase = Phase::Stopping;
            s.error = None;
        });
        stop_engine(ov)?;
        sh.update(|s| s.phase = Phase::Starting);
        start_engine(ov, true)?;
        write_engine_context(sh)?;
        let relay = relay_running(&ov.pipe);
        sh.update(|s| {
            s.phase = Phase::Running;
            s.relay_running = relay;
        });
        Ok(())
    }

    /// Removes everything the engine put on this PC. Each step is best effort and its failure is
    /// collected. The files go only when the distro is gone, and `engine.json` (the marker) is
    /// deleted last, after the other files, so a partial removal can be retried.
    fn remove(sh: &Shared) -> Result<(), String> {
        let dir = data_dir(sh);
        if !is_safe_data_dir(&dir) {
            return Err(format!("refusing to delete {}", dir.display()));
        }
        let Some(mut cfg) = EngineConfig::load(&dir) else {
            // Without our marker nothing in the folder is known to be ours.
            reset_status(sh, None);
            sh.toast(NoticeKind::Info, "nothing to remove");
            return Ok(());
        };
        // The distro recorded at setup, so a changed override cannot point at another distro.
        let distro = if cfg.distro.is_empty() { sh.overrides.distro.clone() } else { cfg.distro.clone() };
        let ov = &Overrides { distro, ..sh.overrides.clone() };
        sh.update(|s| {
            s.phase = Phase::Removing;
            s.error = None;
        });
        let mut errors = Vec::new();
        collect(&mut errors, stop_relay(ov));
        collect(&mut errors, disable_default_docker(sh, &mut cfg));
        collect(&mut errors, winreg::run_key_remove(AUTOSTART_NAME));
        if cfg.firewall_rule {
            collect(&mut errors, winreg::run_elevated("netsh.exe", FIREWALL_DELETE_ARGS));
        }
        collect(&mut errors, stop_distro(ov));
        if wsl::is_registered(&ov.distro) {
            collect(&mut errors, wsl::unregister(&ov.distro));
        }
        match docker_dir_path() {
            Ok(docker) => collect(&mut errors, remove_context(&docker, &ov.context)),
            Err(e) => errors.push(e),
        }
        if wsl::is_registered(&ov.distro) {
            errors.push(format!("the WSL distro {} is still registered; its files were kept", ov.distro));
        } else {
            let files_before = errors.len();
            collect(&mut errors, remove_dir_if_exists(&dir.join(ROOTFS_DIR)));
            collect(&mut errors, remove_dir_if_exists(&dir.join(DOWNLOADS_DIR)));
            collect(&mut errors, remove_file_if_exists(&dir.join("relay.log")));
            if errors.len() == files_before {
                collect(&mut errors, remove_file_if_exists(&dir.join(crate::engine::config::FILE_NAME)));
            }
        }
        sh.push(Event::ContextsChanged);
        if errors.is_empty() {
            reset_status(sh, None);
            sh.toast(NoticeKind::Success, "TMOP engine removed");
            return Ok(());
        }
        let message = join_errors(&errors).unwrap_err();
        let marker_left = dir.join(crate::engine::config::FILE_NAME).exists();
        reset_status(sh, Some(message.clone()));
        if marker_left {
            sh.update(|s| {
                s.phase = Phase::Failed;
                s.installed_version = Some(cfg.engine_version.clone());
            });
        }
        sh.toast(NoticeKind::Error, format!("TMOP engine removal did not finish: {message}"));
        Ok(())
    }

    /// Back to "not installed" with the OS and settings fields the UI reads kept.
    fn reset_status(sh: &Shared, error: Option<String>) {
        sh.update(|s| {
            let context_name = s.context_name.clone();
            let data_dir = s.data_dir.clone();
            *s = Status { phase: Phase::NotInstalled, wsl: s.wsl.clone(), context_name, data_dir, error, ..Default::default() };
        });
    }

    /// Upgrades in place. Skipped when the installed version is this build's (unless the tarball
    /// override is set). A Stopped engine is stopped again afterwards and no relay is started.
    fn update(sh: &Shared) -> Result<(), String> {
        let dir = data_dir(sh);
        let mut cfg = load_config(sh)?;
        let before = sh.lock().status.phase;
        if !matches!(before, Phase::Running | Phase::Stopped) {
            return Err("the engine must be running or stopped to update".into());
        }
        if sh.overrides.tarball.is_none() && installed_matches(&cfg.engine_version, &version_tag()) {
            sh.toast(NoticeKind::Info, "Engine is already up to date");
            return Ok(());
        }
        sh.cancel.store(false, Ordering::SeqCst);
        sh.update(|s| {
            s.phase = Phase::Updating;
            s.error = None;
        });
        let Some(src) = acquire(sh, &dir)? else {
            sh.update(|s| {
                s.phase = before;
                s.progress = None;
            });
            sh.toast(NoticeKind::Info, "Engine update cancelled");
            return Ok(());
        };
        let applied = record_and_upgrade(sh, &dir, &mut cfg, &src, before);
        if src.owned {
            let _ = fs::remove_file(&src.path);
        }
        applied?;
        let relay = relay_running(&sh.overrides.pipe);
        sh.update(|s| {
            s.phase = before;
            s.progress = None;
            s.installed_version = Some(src.tag.clone());
            s.update_available = None;
            s.relay_running = relay;
        });
        sh.toast(NoticeKind::Success, format!("TMOP engine updated to {}", src.tag));
        Ok(())
    }

    /// Writes the new version to engine.json, then upgrades the distro. A failed upgrade restores
    /// the old version so the next update is not skipped as "up to date".
    fn record_and_upgrade(sh: &Shared, dir: &Path, cfg: &mut EngineConfig, src: &Source, before: Phase) -> Result<(), String> {
        let ov = &sh.overrides;
        let previous = std::mem::replace(&mut cfg.engine_version, src.tag.clone());
        cfg.save(dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        if let Err(e) = apply_upgrade(sh, src) {
            cfg.engine_version = previous;
            let _ = cfg.save(dir);
            return Err(e);
        }
        if before == Phase::Stopped {
            stop_engine(ov)
        } else {
            ensure_relay(ov)
        }
    }

    /// Copies the tarball into the distro and runs `tmop-engine upgrade` on it.
    fn apply_upgrade(sh: &Shared, src: &Source) -> Result<(), String> {
        let ov = &sh.overrides;
        sh.progress("Starting engine", 0, 0);
        start_engine(ov, false)?;
        let name = src.path.file_name().and_then(|n| n.to_str()).unwrap_or("engine.tar.gz");
        let linux = format!("/tmp/{name}");
        sh.progress("Copying into WSL", 0, 0);
        let copy = wsl::unc_path(&ov.distro, &linux);
        fs::copy(&src.path, &copy).map_err(|e| format!("could not copy the engine into WSL: {e}"))?;
        sh.progress("Upgrading engine", 0, 0);
        let upgraded = engine_exec(ov, &["upgrade", linux.as_str()], UPGRADE_TIMEOUT).map(|_| ());
        let _ = fs::remove_file(&copy);
        upgraded
    }

    fn set_ports_local_only(sh: &Shared, on: bool) -> Result<(), String> {
        let dir = data_dir(sh);
        let mut cfg = load_config(sh)?;
        cfg.ports_local_only = on;
        cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        sh.update(|s| s.ports_local_only = on);
        Ok(())
    }

    fn set_limits(sh: &Shared, limits: Limits) -> Result<(), String> {
        let ov = &sh.overrides;
        let dir = data_dir(sh);
        let mut cfg = load_config(sh)?;
        if sh.lock().status.phase == Phase::Running {
            let cpus = limits.cpus.to_string();
            let memory = limits.memory_mb.to_string();
            engine_exec(ov, &["limits", cpus.as_str(), memory.as_str()], QUICK_TIMEOUT)?;
        }
        cfg.limits = limits.clone();
        cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        sh.update(|s| s.limits = limits);
        Ok(())
    }

    fn set_autostart(sh: &Shared, on: bool) -> Result<(), String> {
        let dir = data_dir(sh);
        let mut cfg = load_config(sh)?;
        if on {
            let exe = std::env::current_exe().map_err(|e| format!("cannot find TMOP.exe: {e}"))?;
            winreg::run_key_set(AUTOSTART_NAME, &pathenv::autostart_command(&exe))?;
        } else {
            winreg::run_key_remove(AUTOSTART_NAME)?;
        }
        cfg.autostart = on;
        cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        sh.update(|s| s.autostart = on);
        Ok(())
    }

    /// Copies a file out of the distro's Windows CLI folder to `dst`.
    fn copy_from_distro(ov: &Overrides, arch: &str, file: &str, dst: &Path) -> Result<(), String> {
        let src = wsl::unc_path(&ov.distro, &format!("/usr/share/tmop/windows/{arch}/{file}"));
        fs::copy(&src, dst).map(|_| ()).map_err(|e| format!("could not copy {file} from the engine: {e}"))
    }

    /// Installs the CLI, switches the Docker context and adds the bin dir to the user PATH. The
    /// PATH is changed last: if the config files cannot be written, neither they nor the PATH change.
    fn enable_default_docker(sh: &Shared) -> Result<(), String> {
        let ov = &sh.overrides;
        let dir = data_dir(sh);
        let original = load_config(sh)?;
        let mut cfg = original.clone();
        let docker = docker_dir_path()?;
        let config_file = docker.join("config.json");
        let text = read_text_or_empty(&config_file)?;
        if !text.trim().is_empty() && !matches!(serde_json::from_str::<Value>(&text), Ok(Value::Object(_))) {
            return Err(format!("{} is not a JSON object; not changing it", config_file.display()));
        }

        // Reading \\wsl$ needs the distro up; its boot command starts dockerd too.
        let was_up = engine_is_up(ov);
        if !wsl::is_running(&ov.distro) {
            wsl::exec(&ov.distro, &["/bin/true"], QUICK_TIMEOUT)?;
        }
        let arch_name = if cfg.arch.is_empty() { arch().to_string() } else { cfg.arch.clone() };
        let bin = ov.bin_dir();
        fs::create_dir_all(&bin).map_err(|e| format!("cannot create {}: {e}", bin.display()))?;
        copy_from_distro(ov, &arch_name, "docker.exe", &bin.join("docker.exe"))?;

        let plugin_dir = docker.join("cli-plugins");
        let plugin = plugin_dir.join("docker-compose.exe");
        if !plugin.exists() {
            fs::create_dir_all(&plugin_dir).map_err(|e| format!("cannot create {}: {e}", plugin_dir.display()))?;
            copy_from_distro(ov, &arch_name, "docker-compose.exe", &plugin)?;
            cfg.extra.insert(COMPOSE_FLAG.into(), Value::Bool(true));
        }

        let (json, previous) = set_current_context(&text, &ov.context);
        if previous.as_deref() != Some(ov.context.as_str()) {
            cfg.previous_context = previous;
        }
        cfg.default_docker = true;
        let config_changed = json != text;
        let saved = (|| -> Result<(), String> {
            if config_changed {
                fs::create_dir_all(&docker).map_err(|e| format!("cannot create {}: {e}", docker.display()))?;
                fs::write(&config_file, &json).map_err(|e| format!("cannot write {}: {e}", config_file.display()))?;
            }
            cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))
        })();
        if let Err(e) = saved {
            restore_docker_config(&config_file, &text, config_changed);
            let _ = original.save(&dir);
            return Err(e);
        }

        let path_changed = winreg::read_user_path().and_then(|path| {
            let (new_path, changed) = pathenv::add_path_entry(&path, &bin.to_string_lossy());
            if changed {
                winreg::write_user_path(&new_path)?;
            }
            Ok(())
        });
        if let Err(e) = path_changed {
            restore_docker_config(&config_file, &text, config_changed);
            let _ = original.save(&dir);
            return Err(e);
        }

        let is_up = engine_is_up(ov);
        sh.update(|s| {
            s.default_docker = true;
            if !was_up && is_up && s.phase == Phase::Stopped {
                s.phase = Phase::Running;
            }
        });
        sh.push(Event::ContextsChanged);
        sh.toast(NoticeKind::Success, "TMOP is now the default docker command; open new terminals to use it");
        Ok(())
    }

    /// Puts the Docker `config.json` back to `text` when it was changed. Empty text means the file
    /// did not exist, so it is removed.
    fn restore_docker_config(file: &Path, text: &str, changed: bool) {
        if !changed {
            return;
        }
        if text.is_empty() {
            let _ = fs::remove_file(file);
        } else {
            let _ = fs::write(file, text);
        }
    }

    /// Undoes [`enable_default_docker`] for the given config; the config is updated in memory only.
    fn disable_default_docker(sh: &Shared, cfg: &mut EngineConfig) -> Result<(), String> {
        let ov = &sh.overrides;
        let bin = ov.bin_dir();
        let path = winreg::read_user_path()?;
        let (new_path, changed) = pathenv::remove_path_entry(&path, &bin.to_string_lossy());
        if changed {
            winreg::write_user_path(&new_path)?;
        }
        remove_file_if_exists(&bin.join("docker.exe"))?;
        if cfg.extra.remove(COMPOSE_FLAG).is_some_and(|v| v == Value::Bool(true)) {
            remove_file_if_exists(&docker_dir_path()?.join("cli-plugins").join("docker-compose.exe"))?;
        }
        if let Ok(docker) = docker_dir_path() {
            let file = docker.join("config.json");
            let text = read_text_or_empty(&file)?;
            let restored = restore_current_context(&text, &ov.context, cfg.previous_context.as_deref());
            if restored != text {
                fs::write(&file, restored).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
            }
        }
        cfg.previous_context = None;
        cfg.default_docker = false;
        Ok(())
    }

    fn disable_default_docker_op(sh: &Shared) -> Result<(), String> {
        let dir = data_dir(sh);
        let mut cfg = load_config(sh)?;
        disable_default_docker(sh, &mut cfg)?;
        cfg.save(&dir).map_err(|e| format!("cannot write engine.json: {e}"))?;
        sh.update(|s| s.default_docker = false);
        sh.push(Event::ContextsChanged);
        sh.toast(NoticeKind::Info, "TMOP is no longer the default docker command");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::context::{list_contexts, Endpoint};
    use std::time::Instant;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmop-mgr-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn keys(json: &str) -> Vec<String> {
        match serde_json::from_str::<Value>(json).unwrap() {
            Value::Object(map) => map.keys().cloned().collect(),
            other => panic!("not an object: {other}"),
        }
    }

    #[test]
    fn tag_from_asset_name_cases() {
        assert_eq!(tag_from_asset_name("tmop-engine-v0.0.0-test-x64.tar.gz").as_deref(), Some("v0.0.0-test"));
        assert_eq!(tag_from_asset_name("tmop-engine-v0.1.0-arm64.tar.gz").as_deref(), Some("v0.1.0"));
        assert_eq!(tag_from_asset_name("tmop-engine-v0.1.0-x64.tar").as_deref(), Some("v0.1.0"));
        assert_eq!(tag_from_asset_name("tmop-engine-0.1.0-x64.tar.gz"), None);
        assert_eq!(tag_from_asset_name("other-v0.1.0-x64.tar.gz"), None);
        assert_eq!(tag_from_asset_name("tmop-engine-v0.1.0-x64.zip"), None);
    }

    #[test]
    fn context_meta_dir_is_sha256_of_name() {
        let docker = Path::new(r"C:\docker");
        let hex: String = Sha256::digest(b"tmop-test").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(context_meta_dir(docker, "tmop-test"), docker.join("contexts").join("meta").join(hex));
    }

    #[test]
    fn context_meta_json_is_listed_as_named_pipe() {
        let dir = temp_dir("ctx");
        write_context(&dir, "tmop-test", "tmop_engine_test").unwrap();
        let file = context_meta_dir(&dir, "tmop-test").join("meta.json");
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(text, context_meta_json("tmop-test", "tmop_engine_test"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["Endpoints"]["docker"]["Host"], "npipe:////./pipe/tmop_engine_test");

        let contexts = list_contexts(&dir);
        let ctx = contexts.iter().find(|c| c.name == "tmop-test").expect("written context is listed");
        assert_eq!(ctx.endpoint, Endpoint::NamedPipe(r"//./pipe/tmop_engine_test".into()));
        assert_eq!(ctx.description, "TMOP engine");

        remove_context(&dir, "tmop-test").unwrap();
        assert!(!file.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_current_context_keeps_keys_and_reports_previous() {
        let src = r#"{"auths":{},"currentContext":"desktop-linux","stackOrchestrator":"swarm"}"#;
        let (out, previous) = set_current_context(src, "tmop");
        assert_eq!(previous.as_deref(), Some("desktop-linux"));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["currentContext"], "tmop");
        assert_eq!(v["stackOrchestrator"], "swarm");
        assert_eq!(keys(&out), ["auths", "currentContext", "stackOrchestrator"]);
    }

    #[test]
    fn set_current_context_blank_and_non_object_input() {
        let (out, previous) = set_current_context("", "tmop");
        assert_eq!(previous, None);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["currentContext"], "tmop");

        let (out, previous) = set_current_context("[1]", "tmop");
        assert_eq!(out, "[1]");
        assert_eq!(previous, None);
    }

    #[test]
    fn set_current_context_when_already_ours() {
        let (_, previous) = set_current_context(r#"{"currentContext":"tmop"}"#, "tmop");
        assert_eq!(previous.as_deref(), Some("tmop"));
    }

    #[test]
    fn restore_current_context_round_trip() {
        let src = r#"{"auths":{},"currentContext":"desktop-linux","stackOrchestrator":"swarm"}"#;
        let (set, previous) = set_current_context(src, "tmop");
        let back = restore_current_context(&set, "tmop", previous.as_deref());
        assert_eq!(keys(&back), ["auths", "currentContext", "stackOrchestrator"]);
        assert_eq!(serde_json::from_str::<Value>(&back).unwrap()["currentContext"], "desktop-linux");
    }

    #[test]
    fn restore_current_context_without_previous_removes_key_in_order() {
        let src = r#"{"auths":{},"stackOrchestrator":"swarm"}"#;
        let (set, previous) = set_current_context(src, "tmop");
        assert_eq!(previous, None);
        let back = restore_current_context(&set, "tmop", None);
        assert_eq!(keys(&back), ["auths", "stackOrchestrator"]);
    }

    #[test]
    fn restore_current_context_leaves_other_values_alone() {
        let other = r#"{"currentContext":"work"}"#;
        assert_eq!(restore_current_context(other, "tmop", Some("x")), other);
        assert_eq!(restore_current_context("", "tmop", None), "");
        assert_eq!(restore_current_context("{}", "tmop", Some("x")), "{}");
    }

    #[test]
    fn safe_data_dir_refuses_empty_and_relative() {
        assert!(!is_safe_data_dir_in(Path::new(""), None, None));
        assert!(!is_safe_data_dir_in(Path::new(std::path::MAIN_SEPARATOR_STR), None, None));
        assert!(!is_safe_data_dir_in(Path::new("engine"), None, None));
        let dir = temp_dir("safe").join("engine");
        assert!(is_safe_data_dir_in(&dir, None, None));
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn safe_data_dir_refuses_drive_roots_and_single_folders() {
        let (root, single, deep) = if cfg!(windows) {
            (r"C:\", r"C:\Temp", r"D:\TMOP\engine")
        } else {
            ("/", "/tmp", "/srv/tmop/engine")
        };
        assert!(!is_safe_data_dir_in(Path::new(root), None, None));
        assert!(!is_safe_data_dir_in(Path::new(single), None, None));
        assert!(is_safe_data_dir_in(Path::new(deep), None, None));
    }

    #[test]
    fn safe_data_dir_refuses_profile_and_local_app_data_themselves() {
        let base = temp_dir("profile");
        let profile = base.join("home");
        let local = base.join("local");
        let trailing = PathBuf::from(format!("{}{}", profile.display(), std::path::MAIN_SEPARATOR));
        let upper = PathBuf::from(profile.display().to_string().to_uppercase());
        assert!(!is_safe_data_dir_in(&profile, Some(&profile), Some(&local)));
        assert!(!is_safe_data_dir_in(&trailing, Some(&profile), Some(&local)));
        assert!(!is_safe_data_dir_in(&upper, Some(&profile), Some(&local)));
        assert!(!is_safe_data_dir_in(&local, Some(&profile), Some(&local)));
        assert!(is_safe_data_dir_in(&profile.join("engine"), Some(&profile), Some(&local)));
        assert!(is_safe_data_dir_in(&local.join("TMOP").join("engine"), Some(&profile), Some(&local)));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_dir_accepts_missing_empty_and_own_leftovers() {
        let base = temp_dir("setupdir-ok");
        assert!(check_setup_dir(&base.join("missing")).is_ok());
        fs::create_dir_all(base.join("empty")).unwrap();
        assert!(check_setup_dir(&base.join("empty")).is_ok());
        let leftovers = base.join("left");
        fs::create_dir_all(leftovers.join(ROOTFS_DIR)).unwrap();
        fs::create_dir_all(leftovers.join("ENGINE-DOWNLOADS")).unwrap();
        fs::write(leftovers.join("relay.log"), "x").unwrap();
        assert!(check_setup_dir(&leftovers).is_ok());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_dir_refuses_foreign_entries_and_a_marker() {
        let base = temp_dir("setupdir-no");
        let foreign = base.join("foreign");
        fs::create_dir_all(foreign.join("Downloads")).unwrap();
        let msg = "Choose an empty folder or the previous engine folder";
        assert_eq!(check_setup_dir(&foreign).unwrap_err(), msg);

        let marker = base.join("marker");
        fs::create_dir_all(&marker).unwrap();
        fs::write(marker.join("engine.json"), "{}").unwrap();
        assert_eq!(check_setup_dir(&marker).unwrap_err(), msg);

        let mixed = base.join("mixed");
        fs::create_dir_all(mixed.join(ROOTFS_DIR)).unwrap();
        fs::write(mixed.join("notes.txt"), "x").unwrap();
        assert_eq!(check_setup_dir(&mixed).unwrap_err(), msg);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn context_slot_refuses_a_foreign_context_and_keeps_it() {
        let docker = temp_dir("ctx-foreign");
        let file = context_meta_dir(&docker, "tmop").join("meta.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let foreign = r#"{"Name":"tmop","Metadata":{"Description":"my own"}}"#;
        fs::write(&file, foreign).unwrap();
        assert_eq!(check_context_slot(&docker, "tmop"), Err("a Docker context named tmop already exists".into()));
        assert_eq!(write_context(&docker, "tmop", "tmop_engine").unwrap_err(), "a Docker context named tmop already exists");
        assert_eq!(fs::read_to_string(&file).unwrap(), foreign);
        remove_context(&docker, "tmop").unwrap();
        assert!(file.exists(), "a context with another description is not deleted");

        fs::write(&file, "not json").unwrap();
        assert!(write_context(&docker, "tmop", "tmop_engine").is_err(), "unparsable meta is not ours");
        let _ = fs::remove_dir_all(&docker);
    }

    #[test]
    fn context_slot_replaces_and_deletes_only_our_own() {
        let docker = temp_dir("ctx-own");
        let file = context_meta_dir(&docker, "tmop").join("meta.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, context_meta_json("tmop", "old_pipe")).unwrap();
        write_context(&docker, "tmop", "tmop_engine").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), context_meta_json("tmop", "tmop_engine"));
        remove_context(&docker, "tmop").unwrap();
        assert!(!context_meta_dir(&docker, "tmop").exists());
        remove_context(&docker, "tmop").unwrap();
        let _ = fs::remove_dir_all(&docker);
    }

    #[test]
    fn signal_until_gone_signals_at_most_twice() {
        let (mut signals, mut waits) = (0, 0);
        let result = signal_until_gone(|| signals += 1, |t| {
            assert_eq!(t, RELAY_STOP_TIMEOUT);
            waits += 1;
            true
        });
        assert_eq!(result, Ok(()));
        assert_eq!((signals, waits), (1, 1));

        let (mut signals, mut waits) = (0, 0);
        let result = signal_until_gone(|| signals += 1, |_| {
            waits += 1;
            waits == 2
        });
        assert_eq!(result, Ok(()));
        assert_eq!((signals, waits), (2, 2));

        let (mut signals, mut waits) = (0, 0);
        let result = signal_until_gone(|| signals += 1, |_| {
            waits += 1;
            false
        });
        assert_eq!(result, Err("relay did not stop".into()));
        assert_eq!((signals, waits), (2, 2));
    }

    #[test]
    fn installed_matches_needs_the_same_non_empty_version() {
        assert!(installed_matches("v0.1.0", "v0.1.0"));
        assert!(!installed_matches("", "v0.1.0"));
        assert!(!installed_matches("v0.0.9", "v0.1.0"));
    }

    #[test]
    fn new_on_a_plain_dir_reports_no_error() {
        let mut mgr = EngineManager::new(temp_dir("noerr"), false, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(mgr.poll());
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!events.iter().any(|e| matches!(e, Event::Toast { kind: NoticeKind::Error, .. })), "events: {events:?}");
    }

    #[test]
    fn new_without_engine_json_is_not_installed_and_idle() {
        let dir = temp_dir("fresh");
        let mgr = EngineManager::new(dir.clone(), false, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(5);
        while mgr.status().busy && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let status = mgr.status();
        assert!(!status.busy, "probe finished within 5 s");
        let expected = if cfg!(windows) { Phase::NotInstalled } else { Phase::Unsupported };
        assert_eq!(status.phase, expected);
        assert_eq!(status.installed_version, None);
        assert_eq!(status.data_dir, dir);
        assert_eq!(status.context_name, crate::engine::DEFAULT_CONTEXT);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_sets_flag() {
        let mgr = EngineManager::new(temp_dir("cancel"), false, Arc::new(|| {}));
        assert!(!mgr.shared.cancel.load(Ordering::SeqCst));
        mgr.cancel();
        assert!(mgr.shared.cancel.load(Ordering::SeqCst));
    }

    /// Real install from a local tarball. Not run by default: it creates the `tmop-engine-test`
    /// distro. Set `TMOP_ENGINE_DISTRO=tmop-engine-test`, `TMOP_ENGINE_PIPE=tmop_engine_test`,
    /// `TMOP_ENGINE_CONTEXT=tmop-test`, `TMOP_ENGINE_DATA_DIR=<temp dir>`,
    /// `TMOP_ENGINE_TARBALL=<path to tmop-engine-v*-x64.tar.gz>`, then run
    /// `cargo test real_setup_from_local_tarball -- --ignored`. The test removes the engine again.
    #[test]
    #[ignore = "real WSL distro: needs the TMOP_ENGINE_* test overrides described above"]
    fn real_setup_from_local_tarball() {
        let ov = Overrides::from_env();
        let dir = ov.data_dir.clone().expect("TMOP_ENGINE_DATA_DIR is set");
        let mut mgr = EngineManager::new(dir.clone(), false, Arc::new(|| {}));
        mgr.setup(dir);
        wait_idle(&mut mgr, Duration::from_secs(15 * 60));
        assert_eq!(mgr.status().phase, Phase::Running, "error: {:?}", mgr.status().error);
        mgr.remove();
        wait_idle(&mut mgr, Duration::from_secs(10 * 60));
        assert_eq!(mgr.status().phase, Phase::NotInstalled);
    }

    /// Setters against the running engine: ports flag, limits (checked inside the distro) and
    /// `.wslconfig` merge with backups. Needs the same overrides as above and an installed, running
    /// engine; `TMOP_WSLCONFIG` must point at a scratch file. Run with `-- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "real WSL distro: needs the TMOP_ENGINE_* test overrides and a running engine"]
    fn real_setters_on_running_engine() {
        use crate::engine::wsl;
        let ov = Overrides::from_env();
        let dir = ov.data_dir.clone().expect("TMOP_ENGINE_DATA_DIR is set");
        let wslconfig = wslconfig_path().expect("TMOP_WSLCONFIG is set");
        let mut mgr = EngineManager::new(dir.clone(), false, Arc::new(|| {}));
        wait_idle(&mut mgr, Duration::from_secs(60));
        assert_eq!(mgr.status().phase, Phase::Running, "error: {:?}", mgr.status().error);

        mgr.set_ports_local_only(true);
        wait_idle(&mut mgr, Duration::from_secs(30));
        assert!(EngineConfig::load(&dir).expect("engine.json").ports_local_only);
        mgr.set_ports_local_only(false);
        wait_idle(&mut mgr, Duration::from_secs(30));
        assert!(!EngineConfig::load(&dir).expect("engine.json").ports_local_only);

        mgr.set_limits(Limits { cpus: 2, memory_mb: 1024 });
        wait_idle(&mut mgr, Duration::from_secs(60));
        assert_eq!(mgr.status().error, None);
        let limits = wsl::exec(&ov.distro, &["/bin/cat", "/etc/tmop/limits"], QUICK_TIMEOUT).expect("cat limits");
        assert!(limits.stdout.contains("CPUS=2") && limits.stdout.contains("MEMORY_MB=1024"), "limits: {}", limits.stdout);
        assert_eq!(EngineConfig::load(&dir).expect("engine.json").limits, Limits { cpus: 2, memory_mb: 1024 });
        mgr.set_limits(Limits::default());
        wait_idle(&mut mgr, Duration::from_secs(60));

        let _ = fs::remove_file(&wslconfig);
        let first = mgr.apply_wslconfig(Some(4), Some(2)).expect("apply to a missing file");
        assert_eq!(first, None, "no backup for a missing file");
        assert_eq!(fs::read_to_string(&wslconfig).unwrap(), "[wsl2]\r\nmemory=4GB\r\nprocessors=2\r\n");
        let second = mgr.apply_wslconfig(Some(6), None).expect("apply to an existing file").expect("backup path");
        assert_ne!(second, wslconfig);
        assert!(second.exists(), "backup written");
        assert_eq!(fs::read_to_string(&second).unwrap(), "[wsl2]\r\nmemory=4GB\r\nprocessors=2\r\n");
        assert_eq!(fs::read_to_string(&wslconfig).unwrap(), "[wsl2]\r\nmemory=6GB\r\n");
    }

    fn wait_idle(mgr: &mut EngineManager, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while (mgr.status().busy || mgr.status().phase == Phase::Installing) && Instant::now() < deadline {
            let _ = mgr.poll();
            thread::sleep(Duration::from_millis(250));
        }
    }
}
