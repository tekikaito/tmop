//! Background worker for the Docker feature.
//!
//! [`Backend::start`] spawns a thread that owns a tokio runtime and one engine
//! connection. The worker publishes a [`Shared`] snapshot that the UI reads under a
//! short lock, and executes the [`Command`]s the UI sends. Every publish calls the
//! `on_change` callback so the UI can repaint.
//!
//! Per-session tasks (poller, event stream, one stats stream per tracked container)
//! are aborted when the session ends. Each task also checks a generation counter
//! before writing, so a context switch cannot leave stale values in the snapshot.

use super::client::{self, Connection};
use super::context::Context;
use super::model::{
    cpu_percent, rate, stacks_from, Container, ContainerState, EngineInfo, EngineStatus, Image, Network, PortMapping,
    Stack, StatsSample, Totals, Volume,
};
use super::MAX_TRACKED;
use crate::metrics::Series;
use bollard::errors::Error as BollardError;
use bollard::models as api;
use bollard::query_parameters::{
    ListContainersOptions, ListImagesOptions, ListVolumesOptions, PruneBuildOptions, PruneContainersOptions,
    PruneImagesOptions, PruneNetworksOptions, PruneVolumesOptions, RemoveContainerOptions, RemoveImageOptions,
    RemoveVolumeOptions, RestartContainerOptions, StartContainerOptions, StatsOptions, StopContainerOptions,
};
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};

/// Samples kept per container series and per totals series.
pub const HISTORY_LEN: usize = 240;
/// Interval between container, image, volume and network list refreshes.
pub const LIST_INTERVAL: Duration = Duration::from_secs(2);
/// Interval between totals recomputations (and the poller's wake-ups).
pub const STATS_INTERVAL: Duration = Duration::from_secs(1);

const PING_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
const EVENT_RECONNECT: Duration = Duration::from_secs(3);
/// Minimum gap between two list refreshes requested by events or commands.
const LIST_DEBOUNCE: Duration = Duration::from_millis(500);
/// Consecutive failed list refreshes before the connection is treated as lost.
const LIST_FAILURES_BEFORE_LOST: u32 = 3;
const STOP_TIMEOUT_SECS: i32 = 10;
const EVENTS_KEPT: usize = 500;
const NOTICES_KEPT: usize = 50;
const ALERTS_KEPT: usize = 100;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const RESTART_DEATHS: usize = 3;
const RESTART_ALERT_COOLDOWN: Duration = Duration::from_secs(300);
/// A non-zero exit within this time after a `kill` event is the stop that was asked for.
const KILL_EXIT_WINDOW: Duration = Duration::from_secs(20);
/// A non-zero exit within this time after a stop, kill, restart or remove sent by TMOP is expected.
const REQUESTED_STOP_WINDOW: Duration = Duration::from_secs(60);
/// Event actions that change the container list.
const REFRESH_ACTIONS: [&str; 8] = ["start", "die", "destroy", "create", "pause", "unpause", "rename", "oom"];

const LABEL_PROJECT: &str = "com.docker.compose.project";
const LABEL_SERVICE: &str = "com.docker.compose.service";
const LABEL_CONFIG_FILES: &str = "com.docker.compose.project.config_files";
const LABEL_WORKING_DIR: &str = "com.docker.compose.project.working_dir";

/// Per-container resource history.
#[derive(Clone, Debug)]
pub struct ContainerStats {
    pub last: StatsSample,
    pub cpu: Series,
    pub mem_pct: Series,
    pub net_rx: Series,
    pub net_tx: Series,
    pub blk_read: Series,
    pub blk_write: Series,
    pub updated: Option<Instant>,
}

impl Default for ContainerStats {
    fn default() -> Self {
        Self {
            last: StatsSample::default(),
            cpu: Series::new(HISTORY_LEN),
            mem_pct: Series::new(HISTORY_LEN),
            net_rx: Series::new(HISTORY_LEN),
            net_tx: Series::new(HISTORY_LEN),
            blk_read: Series::new(HISTORY_LEN),
            blk_write: Series::new(HISTORY_LEN),
            updated: None,
        }
    }
}

impl ContainerStats {
    fn push(&mut self, s: StatsSample, now: Instant) {
        self.cpu.push(s.cpu_pct);
        self.mem_pct.push(s.mem_pct);
        self.net_rx.push(s.net_rx_bps as f32);
        self.net_tx.push(s.net_tx_bps as f32);
        self.blk_read.push(s.blk_read_bps as f32);
        self.blk_write.push(s.blk_write_bps as f32);
        self.last = s;
        self.updated = Some(now);
    }
}

/// Engine-wide totals over time.
#[derive(Clone, Debug)]
pub struct TotalsHistory {
    pub cpu: Series,
    /// Bytes as f32.
    pub mem_used: Series,
    pub net_rx: Series,
    pub net_tx: Series,
    pub blk_read: Series,
    pub blk_write: Series,
}

impl Default for TotalsHistory {
    fn default() -> Self {
        let s = || Series::new(HISTORY_LEN);
        Self { cpu: s(), mem_used: s(), net_rx: s(), net_tx: s(), blk_read: s(), blk_write: s() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Success,
    Error,
}

/// A short message for the UI to show as a toast.
#[derive(Clone, Debug)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    UnexpectedExit { code: i64 },
    OomKilled,
    RestartLoop,
}

/// An event that should reach the user as a system notification.
#[derive(Clone, Debug)]
pub struct Alert {
    pub kind: AlertKind,
    /// Container name.
    pub container: String,
    pub context: String,
    pub at: Instant,
}

/// One entry of the engine event stream.
#[derive(Clone, Debug)]
pub struct EngineEvent {
    pub at: Instant,
    /// Event type, such as `container`.
    pub kind: String,
    pub action: String,
    /// Container or object name, or its id when no name is known.
    pub actor: String,
}

/// Everything the UI reads. Written only by the worker and by [`Backend::notify`] and [`Backend::set_busy`].
#[derive(Debug, Default)]
pub struct Shared {
    pub contexts: Vec<Context>,
    pub active: String,
    pub status: EngineStatus,
    pub error: Option<String>,
    pub read_only: bool,
    pub page_open: bool,
    pub info: Option<EngineInfo>,
    pub api_version: String,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
    pub stacks: Vec<Stack>,
    /// Keyed by full container id; only the tracked (running, at most `MAX_TRACKED`) containers.
    pub stats: BTreeMap<String, ContainerStats>,
    pub totals: Totals,
    pub totals_hist: TotalsHistory,
    pub list_seq: u64,
    pub stats_seq: u64,
    /// Last 500 engine events.
    pub events: VecDeque<EngineEvent>,
    /// Drained by the UI and shown as toasts.
    pub notices: VecDeque<Notice>,
    /// Drained by the UI for system notifications.
    pub alerts: VecDeque<Alert>,
    /// Object id or name to the label of the action in progress.
    pub busy: BTreeMap<String, String>,
    pub last_list_at: Option<Instant>,
    pub last_error_at: Option<Instant>,
}

#[derive(Clone, Debug)]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Kill,
    Pause,
    Unpause,
    Remove { force: bool, volumes: bool },
}

#[derive(Clone, Debug)]
pub enum PruneKind {
    Containers,
    /// Dangling images only.
    Images,
    /// All images without a container.
    ImagesAll,
    Volumes,
    Networks,
    BuildCache,
}

#[derive(Debug)]
pub enum Command {
    SwitchContext(String),
    /// Replaces the contexts the worker resolves names against; the active session is kept.
    SetContexts(Vec<Context>),
    Refresh,
    SetPageOpen(bool),
    SetReadOnly(bool),
    Container { id: String, name: String, action: ContainerAction },
    RemoveImage { id: String, name: String, force: bool },
    RemoveVolume { name: String, force: bool },
    RemoveNetwork { id: String, name: String },
    Prune(PruneKind),
    Stop,
}

/// Cheap clone of the live connection for other modules (logs, files, stacks) to run their own tasks.
#[derive(Clone)]
pub struct Engine {
    pub docker: Docker,
    pub runtime: Handle,
    pub context: String,
    pub read_only: bool,
}

/// Result slot for a one-off engine task started from the UI thread.
///
/// The future runs on the worker runtime; the UI polls [`Task::take`] once per frame.
/// Dropping the handle does not cancel the task, so actions always run to completion.
pub struct Task<T> {
    slot: Arc<Mutex<Option<T>>>,
    handle: JoinHandle<()>,
    started: Instant,
}

impl<T: Send + 'static> Task<T> {
    /// Spawns `fut` on `runtime` and returns the handle used to collect its result.
    pub fn spawn<F>(runtime: &Handle, fut: F) -> Task<T>
    where
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let slot = Arc::new(Mutex::new(None));
        let sink = slot.clone();
        let handle = runtime.spawn(async move {
            let value = fut.await;
            *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        });
        Task { slot, handle, started: Instant::now() }
    }

    /// Returns the result once it is available; later calls return `None` again.
    pub fn take(&mut self) -> Option<T> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn is_done(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Cancels the task; the slot stays empty.
    pub fn abort(&self) {
        self.handle.abort();
    }
}

/// Handle to the worker. Dropping it asks the worker to stop; the thread is not joined.
pub struct Backend {
    pub shared: Arc<Mutex<Shared>>,
    tx: UnboundedSender<Command>,
    inner: Arc<Inner>,
}

/// State shared by the worker and its session tasks.
struct Inner {
    shared: Arc<Mutex<Shared>>,
    engine: Arc<Mutex<Option<Engine>>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
    /// Container id to the time TMOP last sent a stop, kill, restart or remove for it.
    requested_stops: Mutex<HashMap<String, Instant>>,
    /// Bumped whenever a session ends; tasks stop writing when their captured value is stale.
    gen: AtomicU64,
}

impl Inner {
    fn publish(&self) {
        (self.on_change)();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_current(inner: &Inner, gen: u64) -> bool {
    inner.gen.load(Ordering::SeqCst) == gen
}

fn push_notice(s: &mut Shared, kind: NoticeKind, text: String) {
    s.notices.push_back(Notice { kind, text, at: Instant::now() });
    while s.notices.len() > NOTICES_KEPT {
        s.notices.pop_front();
    }
}

fn notify_now(inner: &Inner, kind: NoticeKind, text: String) {
    push_notice(&mut lock(&inner.shared), kind, text);
    inner.publish();
}

impl Backend {
    /// Starts the worker thread. `on_change` runs after every publish; the UI passes `ctx.request_repaint`.
    pub fn start(
        contexts: Vec<Context>,
        active: String,
        read_only: bool,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> Backend {
        let shared = Arc::new(Mutex::new(Shared { contexts, active, read_only, ..Default::default() }));
        let inner = Arc::new(Inner {
            shared: Arc::clone(&shared),
            engine: Arc::new(Mutex::new(None)),
            on_change: Arc::new(on_change),
            requested_stops: Mutex::new(HashMap::new()),
            gen: AtomicU64::new(0),
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let worker_inner = Arc::clone(&inner);
        let spawned = std::thread::Builder::new().name("tmop-docker".into()).spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("tmop-docker-io")
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    rt.block_on(worker(rx, worker_inner));
                    rt.shutdown_background();
                }
                Err(e) => {
                    let mut s = lock(&worker_inner.shared);
                    s.status = EngineStatus::Unreachable;
                    s.error = Some(format!("could not start the Docker runtime: {e}"));
                }
            }
        });
        if spawned.is_err() {
            notify_now(&inner, NoticeKind::Error, "Could not start the Docker worker thread".into());
        }
        Backend { shared, tx, inner }
    }

    /// Queues a command for the worker.
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// The live connection, or `None` while not connected.
    pub fn engine(&self) -> Option<Engine> {
        lock(&self.inner.engine).clone()
    }

    /// `(list_seq, stats_seq)`; the UI compares these to detect new snapshots.
    pub fn snapshot_seq(&self) -> (u64, u64) {
        let s = lock(&self.shared);
        (s.list_seq, s.stats_seq)
    }

    /// Adds a notice for the UI to show.
    pub fn notify(&self, kind: NoticeKind, text: impl Into<String>) {
        notify_now(&self.inner, kind, text.into());
    }

    /// Marks an object as busy with `label`, or clears the mark when `label` is `None`.
    pub fn set_busy(&self, key: &str, label: Option<&str>) {
        let mut s = lock(&self.shared);
        match label {
            Some(l) => {
                s.busy.insert(key.to_string(), l.to_string());
            }
            None => {
                s.busy.remove(key);
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Messages from spawned tasks back to the worker loop.
enum Internal {
    Connected { attempt: u64, result: Result<Box<Connected>, String> },
    Lost { gen: u64, error: String },
}

struct Connected {
    conn: Connection,
    context: Context,
    info: EngineInfo,
    api_version: String,
}

/// A live session. Dropping it aborts its tasks and closes the connection.
struct Session {
    gen: u64,
    docker: Docker,
    notify: Arc<Notify>,
    stats: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    tasks: Vec<JoinHandle<()>>,
    _conn: Connection,
}

impl Drop for Session {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        for (_, t) in lock(&self.stats).drain() {
            t.abort();
        }
    }
}

#[derive(Default)]
struct Link {
    session: Option<Session>,
    attempt: u64,
    connecting: bool,
    retry_at: Option<time::Instant>,
    /// Context name to connect to; empty means the current context.
    want: String,
}

async fn worker(mut rx: UnboundedReceiver<Command>, inner: Arc<Inner>) {
    let (tx, mut internal) = mpsc::unbounded_channel::<Internal>();
    let mut link = Link { want: lock(&inner.shared).active.clone(), ..Default::default() };

    reset_for_switch(&inner);
    start_attempt(&mut link, &inner, &tx);

    loop {
        let retry_at = link.retry_at;
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(Command::Stop) => break,
                Some(cmd) => handle_command(&mut link, &inner, &tx, cmd),
            },
            msg = internal.recv() => {
                if let Some(msg) = msg {
                    handle_internal(&mut link, &inner, &tx, msg);
                }
            }
            _ = wait_until(retry_at) => {
                link.retry_at = None;
                if link.session.is_none() && !link.connecting {
                    start_attempt(&mut link, &inner, &tx);
                }
            }
        }
    }
    end_session(&mut link, &inner);
}

async fn wait_until(at: Option<time::Instant>) {
    match at {
        Some(t) => time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// Clears engine data and marks the status as connecting, before a switch or start.
fn reset_for_switch(inner: &Inner) {
    {
        let mut s = lock(&inner.shared);
        s.status = EngineStatus::Connecting;
        s.error = None;
        s.info = None;
        s.api_version.clear();
        s.containers.clear();
        s.images.clear();
        s.volumes.clear();
        s.networks.clear();
        s.stacks.clear();
        s.stats.clear();
        s.totals = Totals::default();
        s.totals_hist = TotalsHistory::default();
        s.events.clear();
        s.last_list_at = None;
        s.list_seq += 1;
        s.stats_seq += 1;
    }
    inner.publish();
}

fn start_attempt(link: &mut Link, inner: &Arc<Inner>, tx: &UnboundedSender<Internal>) {
    link.attempt += 1;
    link.connecting = true;
    link.retry_at = None;
    let attempt = link.attempt;
    let name = link.want.clone();
    let inner = Arc::clone(inner);
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = connect_engine(&inner, &name).await;
        let _ = tx.send(Internal::Connected { attempt, result });
    });
}

/// Ends the session, if any. Stale tasks stop writing because the generation moves on.
fn end_session(link: &mut Link, inner: &Inner) {
    link.session = None;
    inner.gen.fetch_add(1, Ordering::SeqCst);
    *lock(&inner.engine) = None;
}

fn resolve_context(contexts: &[Context], name: &str) -> Result<Context, String> {
    let found = if name.is_empty() {
        contexts.iter().find(|c| c.current).or_else(|| contexts.iter().find(|c| c.name == "default"))
    } else {
        contexts.iter().find(|c| c.name == name)
    };
    found.cloned().ok_or_else(|| {
        if name.is_empty() {
            "no Docker context is configured".to_string()
        } else {
            format!("unknown Docker context {name}")
        }
    })
}

async fn connect_engine(inner: &Inner, name: &str) -> Result<Box<Connected>, String> {
    let context = {
        let s = lock(&inner.shared);
        resolve_context(&s.contexts, name)?
    };
    let endpoint = context.endpoint.clone();
    let conn = tokio::task::spawn_blocking(move || client::connect(&endpoint))
        .await
        .map_err(|e| e.to_string())??;
    let docker = conn.docker.clone();
    time::timeout(PING_TIMEOUT, docker.ping())
        .await
        .map_err(|_| "the engine did not answer within 5 seconds".to_string())?
        .map_err(|e| client::connect_error(&context.endpoint, &e))?;
    let version = docker.version().await.map_err(|e| err_text(&e))?;
    let info = docker.info().await.map_err(|e| err_text(&e))?;
    Ok(Box::new(Connected {
        info: map_info(&info, Some(&version)),
        api_version: version.api_version.clone().unwrap_or_default(),
        conn,
        context,
    }))
}

fn handle_internal(link: &mut Link, inner: &Arc<Inner>, tx: &UnboundedSender<Internal>, msg: Internal) {
    match msg {
        Internal::Connected { attempt, result } => {
            if attempt != link.attempt {
                // A newer attempt replaced this one; dropping the result closes its connection.
                return;
            }
            link.connecting = false;
            match result {
                Ok(c) => {
                    let gen = inner.gen.load(Ordering::SeqCst);
                    let remote = c.context.is_remote();
                    let docker = c.conn.docker.clone();
                    let notify = Arc::new(Notify::new());
                    let stats = Arc::new(Mutex::new(HashMap::new()));
                    let poller = tokio::spawn(poll_loop(Poller {
                        inner: Arc::clone(inner),
                        gen,
                        docker: docker.clone(),
                        remote,
                        notify: Arc::clone(&notify),
                        stats: Arc::clone(&stats),
                        internal: tx.clone(),
                    }));
                    let events = tokio::spawn(events_loop(docker.clone(), Arc::clone(inner), gen, Arc::clone(&notify)));
                    let read_only = lock(&inner.shared).read_only;
                    *lock(&inner.engine) = Some(Engine {
                        docker: docker.clone(),
                        runtime: Handle::current(),
                        context: c.context.name.clone(),
                        read_only,
                    });
                    {
                        let mut s = lock(&inner.shared);
                        s.active = c.context.name.clone();
                        s.info = Some(c.info);
                        s.api_version = c.api_version;
                        s.status = EngineStatus::Connected;
                        s.error = None;
                    }
                    link.session = Some(Session {
                        gen,
                        docker,
                        notify,
                        stats,
                        tasks: vec![poller, events],
                        _conn: c.conn,
                    });
                    inner.publish();
                }
                Err(e) => {
                    {
                        let mut s = lock(&inner.shared);
                        s.status = EngineStatus::Unreachable;
                        s.error = Some(e);
                        s.last_error_at = Some(Instant::now());
                    }
                    link.retry_at = Some(time::Instant::now() + RETRY_INTERVAL);
                    inner.publish();
                }
            }
        }
        Internal::Lost { gen, error } => {
            if link.session.as_ref().is_some_and(|s| s.gen == gen) {
                end_session(link, inner);
                {
                    let mut s = lock(&inner.shared);
                    s.status = EngineStatus::Unreachable;
                    s.error = Some(error);
                    s.last_error_at = Some(Instant::now());
                }
                link.retry_at = Some(time::Instant::now() + RETRY_INTERVAL);
                inner.publish();
            }
        }
    }
}

fn handle_command(link: &mut Link, inner: &Arc<Inner>, tx: &UnboundedSender<Internal>, cmd: Command) {
    match cmd {
        Command::SwitchContext(name) => {
            end_session(link, inner);
            link.want = name;
            reset_for_switch(inner);
            start_attempt(link, inner, tx);
        }
        Command::SetContexts(contexts) => {
            lock(&inner.shared).contexts = contexts;
        }
        Command::Refresh => {
            if let Some(s) = link.session.as_ref() {
                s.notify.notify_one();
            } else if !link.connecting {
                start_attempt(link, inner, tx);
            }
        }
        Command::SetPageOpen(open) => {
            lock(&inner.shared).page_open = open;
            if open {
                if let Some(s) = link.session.as_ref() {
                    s.notify.notify_one();
                }
            }
            inner.publish();
        }
        Command::SetReadOnly(ro) => {
            lock(&inner.shared).read_only = ro;
            if let Some(e) = lock(&inner.engine).as_mut() {
                e.read_only = ro;
            }
            inner.publish();
        }
        cmd @ (Command::Container { .. }
        | Command::RemoveImage { .. }
        | Command::RemoveVolume { .. }
        | Command::RemoveNetwork { .. }
        | Command::Prune(_)) => run_mutation(link, inner, cmd),
        Command::Stop => {}
    }
}

/// Starts a mutating command in its own task, unless the context is read-only.
fn run_mutation(link: &Link, inner: &Arc<Inner>, cmd: Command) {
    let (read_only, context) = {
        let s = lock(&inner.shared);
        (s.read_only, s.active.clone())
    };
    if read_only {
        notify_now(inner, NoticeKind::Error, format!("Context {context} is read-only"));
        return;
    }
    let Some(session) = link.session.as_ref() else {
        notify_now(inner, NoticeKind::Error, "Not connected to Docker".into());
        return;
    };
    if let Some(key) = busy_key(&cmd) {
        lock(&inner.shared).busy.insert(key, progress_label(&cmd));
        inner.publish();
    }
    if let Command::Container { id, action, .. } = &cmd {
        if matches!(
            action,
            ContainerAction::Stop | ContainerAction::Kill | ContainerAction::Restart | ContainerAction::Remove { .. }
        ) {
            note_requested_stop(&mut lock(&inner.requested_stops), id, Instant::now());
        }
    }
    tokio::spawn(run_and_report(
        session.docker.clone(),
        Arc::clone(&session.notify),
        Arc::clone(inner),
        cmd,
    ));
}

async fn run_and_report(docker: Docker, notify: Arc<Notify>, inner: Arc<Inner>, cmd: Command) {
    let key = busy_key(&cmd);
    let result = run(&docker, &cmd).await;
    {
        let mut s = lock(&inner.shared);
        if let Some(k) = key {
            s.busy.remove(&k);
        }
        match result {
            Ok(text) => push_notice(&mut s, NoticeKind::Success, text),
            Err(text) => push_notice(&mut s, NoticeKind::Error, text),
        }
    }
    notify.notify_one();
    inner.publish();
}

fn busy_key(cmd: &Command) -> Option<String> {
    match cmd {
        Command::Container { id, .. } | Command::RemoveImage { id, .. } | Command::RemoveNetwork { id, .. } => {
            Some(id.clone())
        }
        Command::RemoveVolume { name, .. } => Some(name.clone()),
        Command::Prune(kind) => Some(format!("prune:{kind:?}")),
        _ => None,
    }
}

fn progress_label(cmd: &Command) -> String {
    match cmd {
        Command::Container { action, .. } => match action {
            ContainerAction::Start => "Starting",
            ContainerAction::Stop => "Stopping",
            ContainerAction::Restart => "Restarting",
            ContainerAction::Kill => "Killing",
            ContainerAction::Pause => "Pausing",
            ContainerAction::Unpause => "Resuming",
            ContainerAction::Remove { .. } => "Removing",
        },
        Command::Prune(_) => "Pruning",
        _ => "Removing",
    }
    .to_string()
}

async fn run(docker: &Docker, cmd: &Command) -> Result<String, String> {
    match cmd {
        Command::Container { id, name, action } => container_action(docker, id, name, action).await,
        Command::RemoveImage { id, name, force } => {
            let opts = RemoveImageOptions { force: *force, noprune: true, platforms: None };
            docker
                .remove_image(id, Some(opts), None)
                .await
                .map(|_| format!("Removed image {name}"))
                .map_err(|e| fail("remove image", name, &e))
        }
        Command::RemoveVolume { name, force } => docker
            .remove_volume(name, Some(RemoveVolumeOptions { force: *force }))
            .await
            .map(|_| format!("Removed volume {name}"))
            .map_err(|e| fail("remove volume", name, &e)),
        Command::RemoveNetwork { id, name } => docker
            .remove_network(id)
            .await
            .map(|_| format!("Removed network {name}"))
            .map_err(|e| fail("remove network", name, &e)),
        Command::Prune(kind) => prune(docker, kind).await,
        _ => Err("not a Docker action".into()),
    }
}

async fn container_action(docker: &Docker, id: &str, name: &str, action: &ContainerAction) -> Result<String, String> {
    let (verb, done) = match action {
        ContainerAction::Start => ("start", "Started"),
        ContainerAction::Stop => ("stop", "Stopped"),
        ContainerAction::Restart => ("restart", "Restarted"),
        ContainerAction::Kill => ("kill", "Killed"),
        ContainerAction::Pause => ("pause", "Paused"),
        ContainerAction::Unpause => ("resume", "Resumed"),
        ContainerAction::Remove { .. } => ("remove", "Removed"),
    };
    let result: Result<(), BollardError> = match action {
        ContainerAction::Start => docker.start_container(id, None::<StartContainerOptions>).await,
        ContainerAction::Stop => {
            let opts = StopContainerOptions { t: Some(STOP_TIMEOUT_SECS), signal: None };
            docker.stop_container(id, Some(opts)).await
        }
        ContainerAction::Restart => {
            let opts = RestartContainerOptions { t: Some(STOP_TIMEOUT_SECS), signal: None };
            docker.restart_container(id, Some(opts)).await
        }
        ContainerAction::Kill => docker.kill_container(id, None).await,
        ContainerAction::Pause => docker.pause_container(id).await,
        ContainerAction::Unpause => docker.unpause_container(id).await,
        ContainerAction::Remove { force, volumes } => {
            let opts = RemoveContainerOptions { v: *volumes, force: *force, link: false };
            docker.remove_container(id, Some(opts)).await
        }
    };
    result.map(|_| format!("{done} {name}")).map_err(|e| fail(verb, name, &e))
}

async fn prune(docker: &Docker, kind: &PruneKind) -> Result<String, String> {
    match kind {
        PruneKind::Containers => docker
            .prune_containers(None::<PruneContainersOptions>)
            .await
            .map(|r| pruned("containers", r.containers_deleted.map_or(0, |v| v.len()), r.space_reclaimed))
            .map_err(|e| fail("prune", "containers", &e)),
        PruneKind::Images => docker
            .prune_images(None::<PruneImagesOptions>)
            .await
            .map(|r| pruned("images", r.images_deleted.map_or(0, |v| v.len()), r.space_reclaimed))
            .map_err(|e| fail("prune", "images", &e)),
        PruneKind::ImagesAll => {
            let filters = HashMap::from([("dangling".to_string(), vec!["false".to_string()])]);
            let opts = PruneImagesOptions { filters: Some(filters) };
            docker
                .prune_images(Some(opts))
                .await
                .map(|r| pruned("images", r.images_deleted.map_or(0, |v| v.len()), r.space_reclaimed))
                .map_err(|e| fail("prune", "images", &e))
        }
        PruneKind::Volumes => docker
            .prune_volumes(None::<PruneVolumesOptions>)
            .await
            .map(|r| pruned("volumes", r.volumes_deleted.map_or(0, |v| v.len()), r.space_reclaimed))
            .map_err(|e| fail("prune", "volumes", &e)),
        PruneKind::Networks => docker
            .prune_networks(None::<PruneNetworksOptions>)
            .await
            .map(|r| format!("Pruned {} networks", r.networks_deleted.map_or(0, |v| v.len())))
            .map_err(|e| fail("prune", "networks", &e)),
        PruneKind::BuildCache => docker
            .prune_build(None::<PruneBuildOptions>)
            .await
            .map(|r| pruned("build cache entries", r.caches_deleted.map_or(0, |v| v.len()), r.space_reclaimed))
            .map_err(|e| fail("prune", "build cache", &e)),
    }
}

fn pruned(what: &str, count: usize, reclaimed: Option<i64>) -> String {
    let bytes = reclaimed.unwrap_or(0).max(0) as u64;
    format!("Pruned {count} {what}, reclaimed {}", super::model::format_bytes(bytes))
}

fn fail(verb: &str, name: &str, e: &BollardError) -> String {
    format!("Could not {verb} {name}: {}", err_text(e))
}

/// The daemon's message for server errors, otherwise the error's display text.
pub fn err_text(e: &BollardError) -> String {
    match e {
        BollardError::DockerResponseServerError { message, .. } => message.trim().to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

type InspectCache = HashMap<String, (String, api::ContainerInspectResponse)>;

struct Poller {
    inner: Arc<Inner>,
    gen: u64,
    docker: Docker,
    remote: bool,
    notify: Arc<Notify>,
    stats: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    internal: UnboundedSender<Internal>,
}

/// Lists, stats and totals for one session. Runs until aborted or the connection is lost.
async fn poll_loop(p: Poller) {
    let mut tick = time::interval(STATS_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_list: Option<Instant> = None;
    let mut inspects = InspectCache::new();
    let mut failures = 0u32;
    loop {
        let forced = tokio::select! {
            _ = tick.tick() => false,
            _ = p.notify.notified() => {
                if let Some(t) = last_list {
                    let since = t.elapsed();
                    if since < LIST_DEBOUNCE {
                        time::sleep(LIST_DEBOUNCE - since).await;
                    }
                }
                true
            }
        };
        if !is_current(&p.inner, p.gen) {
            return;
        }
        // A remote engine is only polled while its page is open; the connection stays up.
        if p.remote && !lock(&p.inner.shared).page_open {
            abort_stats(&p.stats);
            continue;
        }
        let due = forced || last_list.is_none_or(|t| t.elapsed() >= LIST_INTERVAL);
        if due {
            last_list = Some(Instant::now());
            match refresh_lists(&p.docker, &mut inspects).await {
                Ok(lists) => {
                    failures = 0;
                    apply_lists(&p.inner, lists);
                }
                Err(error) => {
                    failures += 1;
                    {
                        let mut s = lock(&p.inner.shared);
                        s.error = Some(error.clone());
                        s.last_error_at = Some(Instant::now());
                    }
                    p.inner.publish();
                    if failures >= LIST_FAILURES_BEFORE_LOST {
                        let _ = p.internal.send(Internal::Lost { gen: p.gen, error });
                        return;
                    }
                }
            }
        }
        sync_stats(&p);
        publish_totals(&p.inner);
    }
}

struct Lists {
    containers: Vec<Container>,
    images: Vec<Image>,
    volumes: Vec<Volume>,
    networks: Vec<Network>,
}

async fn refresh_lists(docker: &Docker, inspects: &mut InspectCache) -> Result<Lists, String> {
    let raw = docker
        .list_containers(Some(ListContainersOptions { all: true, ..Default::default() }))
        .await
        .map_err(|e| err_text(&e))?;

    let mut containers = Vec::with_capacity(raw.len());
    let mut members: HashMap<String, Vec<String>> = HashMap::new();
    let mut live = HashSet::new();
    for c in &raw {
        let id = c.id.clone().unwrap_or_default();
        let state = c.state.as_ref().map(ToString::to_string).unwrap_or_default();
        // Inspect only when the state changed since the last poll; exit code, OOM and restart count come from it.
        if inspects.get(&id).is_none_or(|(key, _)| *key != state) {
            if let Ok(info) = docker.inspect_container(&id, None).await {
                inspects.insert(id.clone(), (state, info));
            }
        }
        containers.push(map_container(c, inspects.get(&id).map(|(_, i)| i)));
        if let Some(nets) = c.network_settings.as_ref().and_then(|n| n.networks.as_ref()) {
            let name = c.names.iter().flatten().next().map(|n| n.trim_start_matches('/').to_string());
            for net in nets.keys() {
                let entry = members.entry(net.clone()).or_default();
                entry.push(name.clone().unwrap_or_else(|| id.clone()));
            }
        }
        live.insert(id);
    }
    inspects.retain(|id, _| live.contains(id));

    let images = docker
        .list_images(Some(ListImagesOptions { all: false, ..Default::default() }))
        .await
        .map_err(|e| err_text(&e))?
        .iter()
        .map(map_image)
        .collect();
    let volumes = docker
        .list_volumes(None::<ListVolumesOptions>)
        .await
        .map_err(|e| err_text(&e))?
        .volumes
        .unwrap_or_default()
        .iter()
        .map(map_volume)
        .collect();
    let networks = docker
        .list_networks(None)
        .await
        .map_err(|e| err_text(&e))?
        .iter()
        .map(|n| {
            let name = n.name.clone().unwrap_or_default();
            let attached = members.get(&name).cloned().unwrap_or_default();
            map_network(n, &attached)
        })
        .collect();
    Ok(Lists { containers, images, volumes, networks })
}

fn apply_lists(inner: &Inner, lists: Lists) {
    {
        let mut s = lock(&inner.shared);
        s.stacks = stacks_from(&lists.containers);
        s.containers = lists.containers;
        s.images = lists.images;
        s.volumes = lists.volumes;
        s.networks = lists.networks;
        s.list_seq += 1;
        s.last_list_at = Some(Instant::now());
        s.error = None;
    }
    inner.publish();
}

/// Starts a stats stream for each tracked container and stops the others.
fn sync_stats(p: &Poller) {
    let tracked: BTreeSet<String> = {
        let s = lock(&p.inner.shared);
        let mut running: Vec<(&str, &str)> = s
            .containers
            .iter()
            .filter(|c| c.state.is_running())
            .map(|c| (c.name.as_str(), c.id.as_str()))
            .collect();
        running.sort();
        running.into_iter().take(MAX_TRACKED).map(|(_, id)| id.to_string()).collect()
    };

    {
        let mut tasks = lock(&p.stats);
        let stale: Vec<String> = tasks.keys().filter(|id| !tracked.contains(*id)).cloned().collect();
        for id in stale {
            if let Some(h) = tasks.remove(&id) {
                h.abort();
            }
        }
        for id in &tracked {
            if tasks.get(id).is_none_or(JoinHandle::is_finished) {
                let h = tokio::spawn(stats_task(p.docker.clone(), id.clone(), p.gen, Arc::clone(&p.inner)));
                tasks.insert(id.clone(), h);
            }
        }
    }

    let mut s = lock(&p.inner.shared);
    s.stats.retain(|id, _| tracked.contains(id));
    for id in &tracked {
        s.stats.entry(id.clone()).or_default();
    }
}

fn abort_stats(stats: &Mutex<HashMap<String, JoinHandle<()>>>) {
    for (_, h) in lock(stats).drain() {
        h.abort();
    }
}

async fn stats_task(docker: Docker, id: String, gen: u64, inner: Arc<Inner>) {
    let mut stream = Box::pin(docker.stats(&id, Some(StatsOptions { stream: true, one_shot: false })));
    let mut prev: Option<PrevStats> = None;
    while let Some(item) = stream.next().await {
        if !is_current(&inner, gen) {
            return;
        }
        let Ok(resp) = item else { return };
        let now = Instant::now();
        let (sample, next) = sample_from_stats(&resp, prev.as_ref(), now);
        prev = Some(next);
        if let Some(entry) = lock(&inner.shared).stats.get_mut(&id) {
            entry.push(sample, now);
        }
    }
}

fn publish_totals(inner: &Inner) {
    {
        let mut s = lock(&inner.shared);
        let mut t = Totals { containers: s.containers.len(), ..Default::default() };
        for c in &s.containers {
            match c.state {
                ContainerState::Running => t.running += 1,
                ContainerState::Paused => t.paused += 1,
                _ => {}
            }
        }
        t.stopped = t.containers - t.running - t.paused;
        for st in s.stats.values() {
            t.cpu_pct += st.last.cpu_pct;
            t.mem_used += st.last.mem_used;
            t.net_rx_bps += st.last.net_rx_bps;
            t.net_tx_bps += st.last.net_tx_bps;
            t.blk_read_bps += st.last.blk_read_bps;
            t.blk_write_bps += st.last.blk_write_bps;
        }
        let h = &mut s.totals_hist;
        h.cpu.push(t.cpu_pct);
        h.mem_used.push(t.mem_used as f32);
        h.net_rx.push(t.net_rx_bps as f32);
        h.net_tx.push(t.net_tx_bps as f32);
        h.blk_read.push(t.blk_read_bps as f32);
        h.blk_write.push(t.blk_write_bps as f32);
        s.totals = t;
        s.stats_seq += 1;
    }
    inner.publish();
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Tracks container deaths per container to detect restart loops.
#[derive(Debug, Default)]
pub struct RestartTracker {
    deaths: HashMap<String, VecDeque<Instant>>,
    alerted: HashMap<String, Instant>,
    /// Time of the last `kill` event per container.
    kills: HashMap<String, Instant>,
}

impl RestartTracker {
    /// Records a `kill` event for container `id`. Docker emits it before the `die` of a stop,
    /// kill or restart.
    pub fn note_kill(&mut self, id: &str, now: Instant) {
        self.kills.insert(id.to_string(), now);
    }

    /// Whether a non-zero exit of container `id` at `now` is a stop that was asked for: a
    /// `kill` event seen shortly before, or a stop TMOP sent (`requested`) shortly before.
    pub fn exit_expected(&self, id: &str, now: Instant, requested: &HashMap<String, Instant>) -> bool {
        let recent = |t: Option<&Instant>, window: Duration| t.is_some_and(|t| now.saturating_duration_since(*t) <= window);
        recent(self.kills.get(id), KILL_EXIT_WINDOW) || recent(requested.get(id), REQUESTED_STOP_WINDOW)
    }

    /// Records a `die` event for container `id`. Returns true when the container has died
    /// at least three times within the last minute and no restart-loop alert was raised for
    /// it in the last five minutes.
    pub fn record(&mut self, id: &str, now: Instant) -> bool {
        let times = self.deaths.entry(id.to_string()).or_default();
        times.push_back(now);
        while times.front().is_some_and(|t| now.saturating_duration_since(*t) > RESTART_WINDOW) {
            times.pop_front();
        }
        if times.len() < RESTART_DEATHS {
            return false;
        }
        if self.alerted.get(id).is_some_and(|t| now.saturating_duration_since(*t) < RESTART_ALERT_COOLDOWN) {
            return false;
        }
        self.alerted.insert(id.to_string(), now);
        true
    }
}

/// Records a stop, kill, restart or remove that TMOP sent for container `id` and forgets
/// entries older than [`REQUESTED_STOP_WINDOW`].
fn note_requested_stop(stops: &mut HashMap<String, Instant>, id: &str, now: Instant) {
    stops.retain(|_, t| now.saturating_duration_since(*t) <= REQUESTED_STOP_WINDOW);
    stops.insert(id.to_string(), now);
}

/// The result of decoding one engine event.
#[derive(Debug)]
pub struct Decoded {
    pub event: EngineEvent,
    /// Name (or id) of the container, used as the alert's container.
    pub container: String,
    pub alerts: Vec<AlertKind>,
    /// True when the event changes the container list.
    pub refresh: bool,
}

/// Turns an engine event into an [`EngineEvent`], alerts and a refresh request. `requested`
/// holds the stops TMOP sent, see [`RestartTracker::exit_expected`].
pub fn decode_event(
    msg: &api::EventMessage,
    restarts: &mut RestartTracker,
    requested: &HashMap<String, Instant>,
    now: Instant,
) -> Decoded {
    let kind = msg.typ.as_ref().map(ToString::to_string).unwrap_or_default();
    let action = msg.action.clone().unwrap_or_default();
    let verb = action.split(':').next().unwrap_or("").trim().to_string();
    let actor = msg.actor.as_ref();
    let id = actor.and_then(|a| a.id.clone()).unwrap_or_default();
    let attrs = actor.and_then(|a| a.attributes.as_ref());
    let name = attrs
        .and_then(|a| a.get("name"))
        .filter(|n| !n.is_empty())
        .cloned()
        .unwrap_or_else(|| id.clone());

    let mut alerts = Vec::new();
    let mut refresh = false;
    if kind == "container" {
        refresh = REFRESH_ACTIONS.contains(&verb.as_str());
        match verb.as_str() {
            "kill" => restarts.note_kill(&id, now),
            "die" => {
                if let Some(code) = attrs.and_then(|a| a.get("exitCode")).filter(|c| c.as_str() != "0") {
                    if !restarts.exit_expected(&id, now, requested) {
                        alerts.push(AlertKind::UnexpectedExit { code: code.parse().unwrap_or(-1) });
                    }
                }
                if restarts.record(&id, now) {
                    alerts.push(AlertKind::RestartLoop);
                }
            }
            "oom" => alerts.push(AlertKind::OomKilled),
            _ => {}
        }
    }
    Decoded {
        event: EngineEvent { at: now, kind, action, actor: name.clone() },
        container: name,
        alerts,
        refresh,
    }
}

async fn events_loop(docker: Docker, inner: Arc<Inner>, gen: u64, notify: Arc<Notify>) {
    let mut restarts = RestartTracker::default();
    loop {
        let mut stream = Box::pin(docker.events(None));
        while let Some(item) = stream.next().await {
            if !is_current(&inner, gen) {
                return;
            }
            let Ok(msg) = item else { break };
            let decoded = {
                let requested = lock(&inner.requested_stops);
                decode_event(&msg, &mut restarts, &requested, Instant::now())
            };
            {
                let mut s = lock(&inner.shared);
                let context = s.active.clone();
                s.events.push_back(decoded.event);
                while s.events.len() > EVENTS_KEPT {
                    s.events.pop_front();
                }
                for kind in decoded.alerts {
                    s.alerts.push_back(Alert {
                        kind,
                        container: decoded.container.clone(),
                        context: context.clone(),
                        at: Instant::now(),
                    });
                }
                while s.alerts.len() > ALERTS_KEPT {
                    s.alerts.pop_front();
                }
            }
            if decoded.refresh {
                notify.notify_one();
            }
            inner.publish();
        }
        if !is_current(&inner, gen) {
            return;
        }
        time::sleep(EVENT_RECONNECT).await;
    }
}

// ---------------------------------------------------------------------------
// Mapping: bollard models to plain model types
// ---------------------------------------------------------------------------

fn short_id(id: &str) -> String {
    id.strip_prefix("sha256:").unwrap_or(id).chars().take(12).collect()
}

fn label(labels: &BTreeMap<String, String>, key: &str) -> Option<String> {
    labels.get(key).filter(|v| !v.is_empty()).cloned()
}

fn btree(m: &HashMap<String, String>) -> BTreeMap<String, String> {
    m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Maps a container summary, with optional inspect data for exit code, OOM flag and restart count.
pub fn map_container(c: &api::ContainerSummary, inspect: Option<&api::ContainerInspectResponse>) -> Container {
    let id = c.id.clone().unwrap_or_default();
    let labels = c.labels.as_ref().map(btree).unwrap_or_default();
    let state = c.state.as_ref().map(|s| ContainerState::parse(s.as_ref())).unwrap_or_default();
    let ports = c
        .ports
        .iter()
        .flatten()
        .map(|p| PortMapping {
            ip: p.ip.clone().unwrap_or_default(),
            private_port: p.private_port,
            public_port: p.public_port,
            proto: p
                .typ
                .as_ref()
                .map(ToString::to_string)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "tcp".into())
                .to_ascii_lowercase(),
        })
        .collect();
    let name = c
        .names
        .iter()
        .flatten()
        .next()
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_else(|| short_id(&id));
    let mut out = Container {
        short_id: short_id(&id),
        name,
        image: c.image.clone().unwrap_or_default(),
        image_id: c.image_id.clone().unwrap_or_default(),
        command: c.command.clone().unwrap_or_default(),
        created: c.created.unwrap_or(0),
        state,
        status: c.status.clone().unwrap_or_default(),
        ports,
        stack: label(&labels, LABEL_PROJECT),
        service: label(&labels, LABEL_SERVICE),
        compose_file: label(&labels, LABEL_CONFIG_FILES),
        compose_dir: label(&labels, LABEL_WORKING_DIR),
        labels,
        id,
        ..Default::default()
    };
    if let Some(i) = inspect {
        out.exit_code = i.state.as_ref().and_then(|s| s.exit_code);
        out.oom_killed = i.state.as_ref().and_then(|s| s.oom_killed).unwrap_or(false);
        out.restart_count = i.restart_count.unwrap_or(0).max(0) as u64;
    }
    out
}

/// Maps an image. An image without a real tag is dangling.
pub fn map_image(i: &api::ImageSummary) -> Image {
    Image {
        id: i.id.clone(),
        short_id: short_id(&i.id),
        repo_tags: i.repo_tags.clone(),
        repo_digests: i.repo_digests.clone(),
        created: i.created,
        size: i.size.max(0) as u64,
        shared_size: i.shared_size,
        containers: i.containers,
        dangling: i.repo_tags.iter().all(|t| t == "<none>:<none>"),
        labels: btree(&i.labels),
    }
}

pub fn map_volume(v: &api::Volume) -> Volume {
    let labels = btree(&v.labels);
    let usage = v.usage_data.as_ref();
    Volume {
        name: v.name.clone(),
        driver: v.driver.clone(),
        mountpoint: v.mountpoint.clone(),
        created: v.created_at.as_ref().map(ToString::to_string),
        scope: v.scope.as_ref().map(ToString::to_string).unwrap_or_default(),
        size: usage.and_then(|u| u64::try_from(u.size).ok()),
        ref_count: usage.map(|u| u.ref_count),
        stack: label(&labels, LABEL_PROJECT),
        labels,
    }
}

/// Maps a network. `attached` holds the names of the containers connected to it.
pub fn map_network(n: &api::Network, attached: &[String]) -> Network {
    let labels = n.labels.as_ref().map(btree).unwrap_or_default();
    let id = n.id.clone().unwrap_or_default();
    let configs = n.ipam.as_ref().and_then(|i| i.config.as_ref()).map(Vec::as_slice).unwrap_or_default();
    Network {
        short_id: short_id(&id),
        id,
        name: n.name.clone().unwrap_or_default(),
        driver: n.driver.clone().unwrap_or_default(),
        scope: n.scope.clone().unwrap_or_default(),
        internal: n.internal.unwrap_or(false),
        attachable: n.attachable.unwrap_or(false),
        ipv6: n.enable_ipv6.unwrap_or(false),
        subnets: configs.iter().filter_map(|c| c.subnet.clone()).collect(),
        gateways: configs.iter().filter_map(|c| c.gateway.clone()).collect(),
        created: n.created.as_ref().map(ToString::to_string),
        containers: attached.to_vec(),
        stack: label(&labels, LABEL_PROJECT),
        labels,
    }
}

/// Maps engine info and version into [`EngineInfo`].
pub fn map_info(info: &api::SystemInfo, version: Option<&api::SystemVersion>) -> EngineInfo {
    EngineInfo {
        id: info.id.clone().unwrap_or_default(),
        name: info.name.clone().unwrap_or_default(),
        server_version: version
            .and_then(|v| v.version.clone())
            .or_else(|| info.server_version.clone())
            .unwrap_or_default(),
        api_version: version.and_then(|v| v.api_version.clone()).unwrap_or_default(),
        os: info.operating_system.clone().unwrap_or_default(),
        os_type: info.os_type.clone().unwrap_or_default(),
        arch: info.architecture.clone().unwrap_or_default(),
        kernel: info.kernel_version.clone().unwrap_or_default(),
        cpus: info.ncpu.unwrap_or(0).max(0) as u64,
        mem_total: info.mem_total.unwrap_or(0).max(0) as u64,
        containers: info.containers.unwrap_or(0).max(0) as u64,
        containers_running: info.containers_running.unwrap_or(0).max(0) as u64,
        containers_paused: info.containers_paused.unwrap_or(0).max(0) as u64,
        containers_stopped: info.containers_stopped.unwrap_or(0).max(0) as u64,
        images: info.images.unwrap_or(0).max(0) as u64,
        driver: info.driver.clone().unwrap_or_default(),
        docker_root: info.docker_root_dir.clone().unwrap_or_default(),
        cgroup_version: info.cgroup_version.as_ref().map(ToString::to_string).unwrap_or_default(),
        swarm: info
            .swarm
            .as_ref()
            .and_then(|s| s.local_node_state.as_ref())
            .map(ToString::to_string)
            .unwrap_or_default(),
        warnings: info.warnings.clone().unwrap_or_default(),
    }
}

/// Counters from the previous stats sample of one container, for rates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PrevStats {
    pub net_rx: u64,
    pub net_tx: u64,
    pub blk_read: u64,
    pub blk_write: u64,
    pub at: Option<Instant>,
}

/// Converts one stats response into a sample. Returns the sample and the counters to keep for
/// the next call. CPU uses the response's own `precpu_stats`; rates need `prev`, so the first
/// sample reports zero rates.
pub fn sample_from_stats(
    cur: &api::ContainerStatsResponse,
    prev: Option<&PrevStats>,
    now: Instant,
) -> (StatsSample, PrevStats) {
    let cpu = cur.cpu_stats.as_ref();
    let precpu = cur.precpu_stats.as_ref();
    let total = |c: Option<&api::ContainerCpuStats>| {
        c.and_then(|c| c.cpu_usage.as_ref()).and_then(|u| u.total_usage).unwrap_or(0)
    };
    let system = |c: Option<&api::ContainerCpuStats>| c.and_then(|c| c.system_cpu_usage).unwrap_or(0);
    let online = cpu
        .and_then(|c| c.online_cpus)
        .filter(|n| *n > 0)
        .map(u64::from)
        .or_else(|| {
            cpu.and_then(|c| c.cpu_usage.as_ref())
                .and_then(|u| u.percpu_usage.as_ref())
                .map(|v| v.len() as u64)
        })
        .unwrap_or(1)
        .max(1);
    let cpu_pct = cpu_percent(total(cpu), total(precpu), system(cpu), system(precpu), online);

    let mem = cur.memory_stats.as_ref();
    let usage = mem.and_then(|m| m.usage).unwrap_or(0);
    let cache = mem
        .and_then(|m| m.stats.as_ref())
        .and_then(|s| s.get("inactive_file").or_else(|| s.get("cache")).copied())
        .unwrap_or(0);
    let mem_used = usage.saturating_sub(cache);
    let mem_limit = mem.and_then(|m| m.limit).unwrap_or(0);
    let mem_pct = if mem_limit > 0 { (mem_used as f64 * 100.0 / mem_limit as f64) as f32 } else { 0.0 };

    let (net_rx, net_tx) = cur.networks.as_ref().map_or((0, 0), |nets| {
        nets.values().fold((0u64, 0u64), |(rx, tx), n| {
            (rx + n.rx_bytes.unwrap_or(0), tx + n.tx_bytes.unwrap_or(0))
        })
    });
    let (blk_read, blk_write) = cur
        .blkio_stats
        .as_ref()
        .and_then(|b| b.io_service_bytes_recursive.as_ref())
        .map_or((0, 0), |entries| {
            entries.iter().fold((0u64, 0u64), |(r, w), e| {
                let v = e.value.unwrap_or(0);
                match e.op.as_deref() {
                    Some(op) if op.eq_ignore_ascii_case("read") => (r + v, w),
                    Some(op) if op.eq_ignore_ascii_case("write") => (r, w + v),
                    _ => (r, w),
                }
            })
        });

    let dt = prev.and_then(|p| p.at).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
    let (net_rx_bps, net_tx_bps, blk_read_bps, blk_write_bps) = match prev {
        Some(p) => (
            rate(net_rx, p.net_rx, dt),
            rate(net_tx, p.net_tx, dt),
            rate(blk_read, p.blk_read, dt),
            rate(blk_write, p.blk_write, dt),
        ),
        None => (0.0, 0.0, 0.0, 0.0),
    };
    let sample = StatsSample {
        cpu_pct,
        mem_used,
        mem_limit,
        mem_pct,
        net_rx_bps,
        net_tx_bps,
        blk_read_bps,
        blk_write_bps,
        pids: cur.pids_stats.as_ref().and_then(|p| p.current).unwrap_or(0),
    };
    let next = PrevStats { net_rx, net_tx, blk_read, blk_write, at: Some(now) };
    (sample, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Option<HashMap<String, String>> {
        Some(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn container_maps_compose_labels_and_state() {
        let c = api::ContainerSummary {
            id: Some("0123456789abcdef0123".into()),
            names: Some(vec!["/shop-api-1".into()]),
            image: Some("shop-api:latest".into()),
            created: Some(1_700_000_000),
            state: Some(api::ContainerSummaryStateEnum::EXITED),
            status: Some("Exited (137) 2 minutes ago".into()),
            labels: labels(&[
                (LABEL_PROJECT, "shop"),
                (LABEL_SERVICE, "api"),
                (LABEL_CONFIG_FILES, "/src/shop/compose.yaml"),
                (LABEL_WORKING_DIR, "/src/shop"),
            ]),
            ports: Some(vec![api::PortSummary {
                ip: Some("0.0.0.0".into()),
                private_port: 8080,
                public_port: Some(80),
                typ: Some(api::PortSummaryTypeEnum::TCP),
            }]),
            ..Default::default()
        };
        let inspect = api::ContainerInspectResponse {
            state: Some(api::ContainerState { exit_code: Some(137), oom_killed: Some(true), ..Default::default() }),
            restart_count: Some(4),
            ..Default::default()
        };
        let m = map_container(&c, Some(&inspect));
        assert_eq!(m.name, "shop-api-1");
        assert_eq!(m.short_id, "0123456789ab");
        assert_eq!(m.state, ContainerState::Exited);
        assert_eq!(m.stack.as_deref(), Some("shop"));
        assert_eq!(m.service.as_deref(), Some("api"));
        assert_eq!(m.compose_file.as_deref(), Some("/src/shop/compose.yaml"));
        assert_eq!(m.compose_dir.as_deref(), Some("/src/shop"));
        assert_eq!(m.exit_code, Some(137));
        assert!(m.oom_killed);
        assert_eq!(m.restart_count, 4);
        assert_eq!(m.ports[0].proto, "tcp");
        assert_eq!(m.ports[0].public_port, Some(80));
        assert_eq!(m.labels.len(), 4);
    }

    #[test]
    fn container_without_labels_or_inspect() {
        let c = api::ContainerSummary {
            id: Some("abc".into()),
            names: Some(vec!["loose".into()]),
            state: Some(api::ContainerSummaryStateEnum::EMPTY),
            labels: labels(&[(LABEL_PROJECT, "")]),
            ..Default::default()
        };
        let m = map_container(&c, None);
        assert_eq!(m.name, "loose");
        assert_eq!(m.state, ContainerState::Unknown);
        assert_eq!(m.stack, None, "empty label value is ignored");
        assert_eq!(m.exit_code, None);
        assert_eq!(m.restart_count, 0);
    }

    #[test]
    fn image_dangling_when_no_real_tag() {
        let mut img = api::ImageSummary {
            id: "sha256:abcdef1234567890".into(),
            repo_tags: vec![],
            containers: -1,
            size: 2048,
            ..Default::default()
        };
        let m = map_image(&img);
        assert!(m.dangling);
        assert_eq!(m.short_id, "abcdef123456");
        assert_eq!(m.size, 2048);
        assert_eq!(m.containers, -1);

        img.repo_tags = vec!["<none>:<none>".into()];
        assert!(map_image(&img).dangling);

        img.repo_tags = vec!["<none>:<none>".into(), "nginx:latest".into()];
        assert!(!map_image(&img).dangling);
    }

    #[test]
    fn restart_loop_needs_three_deaths_in_a_minute() {
        let mut t = RestartTracker::default();
        let t0 = Instant::now();
        assert!(!t.record("a", t0));
        assert!(!t.record("a", t0 + Duration::from_secs(10)));
        assert!(t.record("a", t0 + Duration::from_secs(20)), "third death in the window");
        assert!(!t.record("a", t0 + Duration::from_secs(30)), "alert cooldown");
        assert!(!t.record("b", t0), "other containers are counted separately");

        let mut slow = RestartTracker::default();
        assert!(!slow.record("c", t0));
        assert!(!slow.record("c", t0 + Duration::from_secs(41)));
        assert!(!slow.record("c", t0 + Duration::from_secs(82)), "first death fell out of the window");
        assert!(slow.record("c", t0 + Duration::from_secs(90)), "41, 82 and 90 are within a minute");
    }

    #[test]
    fn die_event_with_exit_code_alerts_and_refreshes() {
        let mut restarts = RestartTracker::default();
        let msg = api::EventMessage {
            typ: Some(api::EventMessageTypeEnum::CONTAINER),
            action: Some("die".into()),
            actor: Some(api::EventActor {
                id: Some("abc".into()),
                attributes: labels(&[("name", "web"), ("exitCode", "137")]),
            }),
            ..Default::default()
        };
        let none = HashMap::new();
        let d = decode_event(&msg, &mut restarts, &none, Instant::now());
        assert_eq!(d.container, "web");
        assert_eq!(d.event.actor, "web");
        assert_eq!(d.alerts, [AlertKind::UnexpectedExit { code: 137 }]);
        assert!(d.refresh);

        let clean = api::EventMessage {
            action: Some("die".into()),
            actor: Some(api::EventActor { id: Some("abd".into()), attributes: labels(&[("exitCode", "0")]) }),
            ..msg.clone()
        };
        let d = decode_event(&clean, &mut restarts, &none, Instant::now());
        assert!(d.alerts.is_empty(), "exit code 0 is not an alert");
        assert_eq!(d.container, "abd", "falls back to the id");
    }

    fn container_event(action: &str, id: &str, exit_code: Option<&str>) -> api::EventMessage {
        let mut attrs = vec![("name", "web")];
        if let Some(code) = exit_code {
            attrs.push(("exitCode", code));
        }
        api::EventMessage {
            typ: Some(api::EventMessageTypeEnum::CONTAINER),
            action: Some(action.into()),
            actor: Some(api::EventActor { id: Some(id.into()), attributes: labels(&attrs) }),
            ..Default::default()
        }
    }

    #[test]
    fn die_after_kill_is_not_alerted() {
        let mut restarts = RestartTracker::default();
        let none = HashMap::new();
        let t0 = Instant::now();
        let kill = decode_event(&container_event("kill", "k1", None), &mut restarts, &none, t0);
        assert!(kill.alerts.is_empty());
        let die = decode_event(&container_event("die", "k1", Some("137")), &mut restarts, &none, t0 + Duration::from_secs(1));
        assert!(die.alerts.is_empty(), "kill before die is a requested stop");

        let late = decode_event(&container_event("die", "k1", Some("137")), &mut restarts, &none, t0 + Duration::from_secs(25));
        assert_eq!(late.alerts, [AlertKind::UnexpectedExit { code: 137 }], "kill is only trusted for 20 s");
    }

    #[test]
    fn die_without_kill_with_code_137_is_alerted() {
        let mut restarts = RestartTracker::default();
        let d = decode_event(&container_event("die", "u1", Some("137")), &mut restarts, &HashMap::new(), Instant::now());
        assert_eq!(d.alerts, [AlertKind::UnexpectedExit { code: 137 }]);
    }

    #[test]
    fn die_with_code_zero_is_not_alerted() {
        let mut restarts = RestartTracker::default();
        let d = decode_event(&container_event("die", "z1", Some("0")), &mut restarts, &HashMap::new(), Instant::now());
        assert!(d.alerts.is_empty());
    }

    #[test]
    fn die_after_requested_stop_is_not_alerted() {
        let mut restarts = RestartTracker::default();
        let t0 = Instant::now();
        let mut requested = HashMap::new();
        note_requested_stop(&mut requested, "r1", t0);
        let d = decode_event(&container_event("die", "r1", Some("137")), &mut restarts, &requested, t0 + Duration::from_secs(5));
        assert!(d.alerts.is_empty(), "TMOP asked for the stop");
        let d = decode_event(&container_event("die", "r1", Some("137")), &mut restarts, &requested, t0 + Duration::from_secs(61));
        assert_eq!(d.alerts, [AlertKind::UnexpectedExit { code: 137 }], "request window is 60 s");
    }

    #[test]
    fn stats_sample_cpu_memory_and_rates() {
        let cpu = |total, system| api::ContainerCpuStats {
            cpu_usage: Some(api::ContainerCpuUsage { total_usage: Some(total), ..Default::default() }),
            system_cpu_usage: Some(system),
            online_cpus: Some(4),
            ..Default::default()
        };
        let stats = |rx: u64, read: u64| api::ContainerStatsResponse {
            cpu_stats: Some(cpu(1500, 12_000)),
            precpu_stats: Some(cpu(1000, 10_000)),
            memory_stats: Some(api::ContainerMemoryStats {
                usage: Some(500),
                limit: Some(1000),
                stats: Some(HashMap::from([("inactive_file".to_string(), 100u64)])),
                ..Default::default()
            }),
            networks: Some(HashMap::from([(
                "eth0".to_string(),
                api::ContainerNetworkStats { rx_bytes: Some(rx), tx_bytes: Some(rx / 2), ..Default::default() },
            )])),
            blkio_stats: Some(api::ContainerBlkioStats {
                io_service_bytes_recursive: Some(vec![
                    api::ContainerBlkioStatEntry { op: Some("Read".into()), value: Some(read), ..Default::default() },
                    api::ContainerBlkioStatEntry { op: Some("Write".into()), value: Some(10), ..Default::default() },
                ]),
                ..Default::default()
            }),
            pids_stats: Some(api::ContainerPidsStats { current: Some(7), limit: None }),
            ..Default::default()
        };
        let t0 = Instant::now();
        let (first, prev) = sample_from_stats(&stats(3000, 4000), None, t0);
        assert!((first.cpu_pct - 100.0).abs() < 1e-3, "500 / 2000 * 4 * 100");
        assert_eq!(first.mem_used, 400, "usage minus inactive file");
        assert!((first.mem_pct - 40.0).abs() < 1e-4);
        assert_eq!(first.net_rx_bps, 0.0, "no rate without a previous sample");
        assert_eq!(first.pids, 7);

        let (second, _) = sample_from_stats(&stats(5000, 8000), Some(&prev), t0 + Duration::from_secs(2));
        assert!((second.net_rx_bps - 1000.0).abs() < 1e-6, "(5000 - 3000) / 2 s");
        assert!((second.net_tx_bps - 500.0).abs() < 1e-6);
        assert!((second.blk_read_bps - 2000.0).abs() < 1e-6, "(8000 - 4000) / 2 s");
        assert_eq!(second.blk_write_bps, 0.0);
    }

    #[test]
    fn stacks_follow_container_labels() {
        let c = map_container(
            &api::ContainerSummary {
                id: Some("x".into()),
                labels: labels(&[(LABEL_PROJECT, "web"), (LABEL_SERVICE, "nginx")]),
                state: Some(api::ContainerSummaryStateEnum::RUNNING),
                ..Default::default()
            },
            None,
        );
        let stacks = stacks_from(&[c]);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].services, ["nginx"]);
    }

    #[test]
    fn server_error_text_is_the_daemon_message() {
        let e = BollardError::DockerResponseServerError { status_code: 409, message: " conflict: in use\n".into() };
        assert_eq!(err_text(&e), "conflict: in use");
    }

    #[test]
    fn context_resolution() {
        let ctx = |name: &str, current: bool| Context {
            name: name.into(),
            description: String::new(),
            endpoint: super::super::context::Endpoint::Unix("/x".into()),
            current,
        };
        let list = vec![ctx("default", false), ctx("work", true)];
        assert_eq!(resolve_context(&list, "").unwrap().name, "work");
        assert_eq!(resolve_context(&list, "default").unwrap().name, "default");
        assert!(resolve_context(&list, "missing").is_err());
        assert_eq!(resolve_context(&[ctx("default", false)], "").unwrap().name, "default");
        assert!(resolve_context(&[], "").is_err());
    }
}
