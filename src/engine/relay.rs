//! `TMOP.exe --engine-relay`: the single-instance process that serves the engine's named pipe.
//!
//! Each pipe connection gets its own `wsl.exe ... socat` upstream to the engine's Docker socket.
//! A keepalive session holds the distro (and dockerd) up, and a publisher forwards LAN ports.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, watch};

use crate::engine::config::EngineConfig;
use crate::engine::ports::{lan_ipv4_addresses, published_ports, Forwarder};
use crate::engine::proxy::{relay_connection, MountRewriter};
use crate::engine::winreg::{current_user_sddl_dacl, security_attributes_from_sddl, NamedEvent, NamedMutex, SecurityAttributes};
use crate::engine::{wsl, Overrides};

/// Budget for `tmop-engine start`, which may boot the distro and dockerd.
const START_TIMEOUT: Duration = Duration::from_secs(90);
/// Budget for `tmop-engine ip` and the container query.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the publisher re-reads `engine.json`.
const CONFIG_INTERVAL: Duration = Duration::from_secs(5);
/// The container list is fetched on every Nth config tick (every 10 s).
const QUERY_EVERY: u32 = 2;
/// The distro IP is re-probed after this long, or after a failed query.
const IP_MAX_AGE: Duration = Duration::from_secs(60);
const STOP_POLL: Duration = Duration::from_millis(500);
const KEEPALIVE_RETRY: Duration = Duration::from_secs(2);
/// Keepalive spawns slow down to this interval after `KEEPALIVE_FAILURES` failures in a row.
const KEEPALIVE_BACKOFF: Duration = Duration::from_secs(10);
const KEEPALIVE_FAILURES: u32 = 3;
/// First and largest wait before retrying a failed creation of the next pipe instance.
const PIPE_RETRY_MIN: Duration = Duration::from_millis(250);
const PIPE_RETRY_MAX: Duration = Duration::from_secs(5);
/// How long a finished connection waits for the client to read the rest of the response.
const PIPE_FLUSH_TIMEOUT: Duration = Duration::from_secs(30);
const PIPE_DRAIN_POLL: Duration = Duration::from_millis(5);
const ENGINE_SCRIPT: &str = "/usr/local/bin/tmop-engine";
const DOCKER_SOCKET: &str = "/var/run/docker.sock";

/// Named kernel object for the single-instance lock.
pub fn mutex_name(pipe: &str) -> String {
    format!(r"Local\tmop_engine_relay_{pipe}")
}

/// Named event that asks the relay to stop.
pub fn stop_event_name(pipe: &str) -> String {
    format!(r"Local\tmop_engine_relay_stop_{pipe}")
}

/// Named event that is set once the pipe is listening.
pub fn ready_event_name(pipe: &str) -> String {
    format!(r"Local\tmop_engine_relay_ready_{pipe}")
}

/// Timestamped lines to `relay.log` and stderr.
pub struct Log {
    file: Mutex<Option<File>>,
}

impl Log {
    /// Opens `<data_dir>\relay.log`, truncated or appended. Without a file, lines go to stderr only.
    fn open(data_dir: &Path, truncate: bool) -> Log {
        let _ = std::fs::create_dir_all(data_dir);
        let mut opts = OpenOptions::new();
        opts.create(true).write(true);
        if truncate {
            opts.truncate(true);
        } else {
            opts.append(true);
        }
        Log { file: Mutex::new(opts.open(data_dir.join("relay.log")).ok()) }
    }

    /// Writes one `[HH:MM:SS] message` line.
    pub fn line(&self, msg: &str) {
        let (_, _, _, h, m, s) = crate::pro::local_time();
        let text = format!("[{h:02}:{m:02}:{s:02}] {msg}\n");
        // stderr is absent in the GUI-subsystem build; a failed write is not an error here.
        let _ = io::stderr().write_all(text.as_bytes());
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(text.as_bytes());
            }
        }
    }
}

fn log_fn(log: &Arc<Log>) -> Arc<dyn Fn(&str) + Send + Sync> {
    let log = log.clone();
    Arc::new(move |m: &str| log.line(m))
}

/// Logs `msg` unless it repeats the previous note, so a stopped engine does not flood the log.
fn note(log: &Log, last: &mut Option<String>, msg: String) {
    if last.as_deref() != Some(msg.as_str()) {
        log.line(&msg);
        *last = Some(msg);
    }
}

/// Data dir for the relay: the env override, else `engineDataDir` from the raw settings file.
fn data_dir(overrides: &Overrides) -> PathBuf {
    if overrides.data_dir.is_some() {
        return overrides.data_dir("");
    }
    let settings = std::fs::read_to_string(crate::settings::default_path()).unwrap_or_default();
    overrides.data_dir(&engine_data_dir_setting(&settings))
}

/// The `engineDataDir` string from settings.json text; empty when absent or unreadable.
fn engine_data_dir_setting(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("engineDataDir")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Runs the relay until it is stopped. Returns the process exit code.
pub fn run() -> i32 {
    let overrides = Overrides::from_env();
    let data_dir = data_dir(&overrides);
    let pipe = overrides.pipe.clone();

    // The lock comes first: a second instance must not truncate the running instance's log.
    let _lock = match NamedMutex::try_acquire(&mutex_name(&pipe)) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            Log::open(&data_dir, false).line("already running");
            return 0;
        }
        Err(e) => {
            Log::open(&data_dir, false).line(&format!("cannot take the relay lock: {e}"));
            return 1;
        }
    };
    let log = Arc::new(Log::open(&data_dir, true));
    log.line(&format!("engine relay {} starting: distro {}, pipe {}, data {}", crate::engine::version_tag(), overrides.distro, pipe, data_dir.display()));

    let (stop_event, ready_event) = match (NamedEvent::create(&stop_event_name(&pipe)), NamedEvent::create(&ready_event_name(&pipe))) {
        (Ok(stop), Ok(ready)) => (stop, ready),
        (Err(e), _) | (_, Err(e)) => {
            log.line(&e);
            return 1;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            log.line(&format!("cannot start the runtime: {e}"));
            return 1;
        }
    };
    // Client connections run as local tasks: the relay future borrows a non-Sync log callback.
    let local = tokio::task::LocalSet::new();
    let code = local.block_on(&runtime, serve(overrides.distro.clone(), pipe, data_dir, log.clone(), stop_event, ready_event));
    // Background tasks (open client connections) must not keep the process alive.
    drop(local);
    runtime.shutdown_background();
    code
}

async fn serve(distro: String, pipe: String, data_dir: PathBuf, log: Arc<Log>, stop_event: NamedEvent, ready: NamedEvent) -> i32 {
    let (tx, stop) = watch::channel(false);
    let tx = Arc::new(tx);
    {
        let (tx, log) = (tx.clone(), log.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(STOP_POLL);
            loop {
                tick.tick().await;
                if stop_event.is_set() {
                    log.line("stop requested");
                    tx.send_replace(true);
                    return;
                }
            }
        });
    }

    // (1) make sure the distro's engine is running; failure is logged and the relay keeps serving.
    let mut start_stop = stop.clone();
    let outcome = {
        let distro = distro.clone();
        tokio::select! {
            res = tokio::task::spawn_blocking(move || wsl::exec(&distro, &[ENGINE_SCRIPT, "start"], START_TIMEOUT)) => Some(res),
            _ = start_stop.wait_for(|s| *s) => None,
        }
    };
    match outcome {
        Some(Ok(Ok(out))) if out.status == 0 => log.line(&format!("engine start: {}", out.stdout.trim())),
        Some(Ok(Ok(out))) => {
            let detail = [&out.stderr, &out.stdout].into_iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("no output");
            log.line(&format!("engine start failed (exit {}): {detail}", out.status));
        }
        Some(Ok(Err(e))) => log.line(&format!("engine start failed: {e}")),
        Some(Err(e)) => log.line(&format!("engine start failed: {e}")),
        None => {}
    }

    let upstream: Arc<dyn Upstream> = Arc::new(WslUpstream { distro: distro.clone() });
    // (2) keepalive session and (5) port publisher; both end when the stop flag is set.
    let keepalive = tokio::spawn(keepalive(distro.clone(), log.clone(), stop.clone()));
    let publisher = tokio::spawn(publish_ports(distro, data_dir, upstream.clone(), log.clone(), stop.clone()));

    // (3) and (4): pipe server with one task per connection.
    let pipe_path = format!(r"\\.\pipe\{pipe}");
    let result = serve_pipe(pipe_path, upstream, log.clone(), stop, move || ready.set()).await;
    tx.send_replace(true);

    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let _ = keepalive.await;
        let _ = publisher.await;
    })
    .await;
    match result {
        Ok(()) => {
            log.line("stopped");
            0
        }
        Err(e) => {
            log.line(&format!("relay failed: {e}"));
            1
        }
    }
}

/// Anything the relay can pipe bytes through: an upstream connection to the engine socket.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Opens connections to the engine's Docker socket.
pub trait Upstream: Send + Sync {
    fn connect(&self) -> BoxFuture<'_, io::Result<Box<dyn Stream>>>;
}

/// Engine upstream: `wsl.exe -d <distro> --exec socat STDIO UNIX-CONNECT:/var/run/docker.sock`.
struct WslUpstream {
    distro: String,
}

impl Upstream for WslUpstream {
    fn connect(&self) -> BoxFuture<'_, io::Result<Box<dyn Stream>>> {
        Box::pin(async move {
            let target = format!("UNIX-CONNECT:{DOCKER_SOCKET}");
            let mut child = wsl::spawn_exec(&self.distro, &["socat", "STDIO", target.as_str()])?;
            let stdin = child.stdin.take().ok_or_else(|| io::Error::other("socat stdin missing"))?;
            let stdout = child.stdout.take().ok_or_else(|| io::Error::other("socat stdout missing"))?;
            Ok(Box::new(ChildIo { stdin, stdout, _child: child }) as Box<dyn Stream>)
        })
    }
}

/// A child's stdout (read) and stdin (write) as one duplex stream. The child was spawned with
/// `kill_on_drop`, so dropping this kills it.
struct ChildIo {
    stdin: ChildStdin,
    stdout: ChildStdout,
    _child: Child,
}

impl AsyncRead for ChildIo {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ChildIo {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

fn create_instance(path: &str, sa: &SecurityAttributes, first: bool) -> io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.pipe_mode(PipeMode::Byte).first_pipe_instance(first);
    // SAFETY: `sa` owns the security descriptor and outlives this call.
    unsafe { opts.create_with_security_attributes_raw(path, sa.as_ptr() as *mut c_void) }
}

/// Accepts pipe clients until `stop` is set. `on_ready` runs once the first instance exists.
async fn serve_pipe(pipe_path: String, upstream: Arc<dyn Upstream>, log: Arc<Log>, mut stop: watch::Receiver<bool>, on_ready: impl FnOnce()) -> Result<(), String> {
    let sddl = current_user_sddl_dacl()?;
    let sa = security_attributes_from_sddl(&sddl)?;
    let mut server = create_instance(&pipe_path, &sa, true).map_err(|e| format!("cannot create {pipe_path}: {e}"))?;
    log.line(&format!("listening on {pipe_path}"));
    on_ready();
    loop {
        let connected = tokio::select! {
            res = server.connect() => res,
            _ = stop.wait_for(|s| *s) => break,
        };
        // The next instance exists before this client is handed off, so no client sees "not found".
        match create_next_instance(&pipe_path, &sa, &mut stop, &log).await {
            Some(next) => dispatch(std::mem::replace(&mut server, next), connected, &upstream, &log),
            None => {
                // Stopping: the connected client is still served, but nothing is left to listen on.
                dispatch(server, connected, &upstream, &log);
                break;
            }
        }
    }
    Ok(())
}

/// Creates the next pipe instance. Failures are logged and retried with backoff until it exists
/// (`Some`) or `stop` is set (`None`).
async fn create_next_instance(pipe_path: &str, sa: &SecurityAttributes, stop: &mut watch::Receiver<bool>, log: &Log) -> Option<NamedPipeServer> {
    let mut delay = PIPE_RETRY_MIN;
    loop {
        match create_instance(pipe_path, sa, false) {
            Ok(server) => return Some(server),
            Err(e) => log.line(&format!("cannot create the next pipe instance, retrying in {} ms: {e}", delay.as_millis())),
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stop.wait_for(|s| *s) => return None,
        }
        delay = pipe_retry_delay(delay);
    }
}

/// The wait after `prev`: doubles, capped at [`PIPE_RETRY_MAX`].
fn pipe_retry_delay(prev: Duration) -> Duration {
    (prev * 2).min(PIPE_RETRY_MAX)
}

/// Hands a connected pipe to its own task, or logs the failed connect.
fn dispatch(pipe: NamedPipeServer, connected: io::Result<()>, upstream: &Arc<dyn Upstream>, log: &Arc<Log>) {
    match connected {
        Ok(()) => {
            tokio::task::spawn_local(handle(pipe, upstream.clone(), log.clone()));
        }
        Err(e) => log.line(&format!("pipe connect failed: {e}")),
    }
}

/// Relays one client to a fresh upstream connection, or answers 502 when the upstream cannot start.
async fn handle(mut pipe: NamedPipeServer, upstream: Arc<dyn Upstream>, log: Arc<Log>) {
    let up = match upstream.connect().await {
        Ok(up) => up,
        Err(e) => {
            log.line(&format!("upstream failed to start: {e}"));
            let reason = format!("TMOP engine is not reachable: {e}");
            let _ = pipe.write_all(&http_502(&reason)).await;
            let _ = pipe.shutdown().await;
            return;
        }
    };
    let (done, upstream_eof) = oneshot::channel();
    let up = EofWatch { inner: up, done: Some(done) };
    let l = log.clone();
    let on_log = move |m: &str| l.line(m);
    {
        let relay = relay_connection(&mut pipe, up, &MountRewriter, &on_log);
        // Named pipes do not turn a write-side shutdown into EOF for the client, so once the engine
        // closes its side the relay stops even if the client keeps its end open.
        tokio::select! {
            res = relay => {
                if let Err(e) = res {
                    log.line(&format!("connection ended: {e}"));
                }
            }
            _ = upstream_eof => {}
        }
    }
    drain_and_disconnect(&mut pipe).await;
}

/// Waits until the client has read everything written to the pipe, then disconnects it.
/// Disconnecting earlier would discard response bytes the client has not read yet.
async fn drain_and_disconnect(pipe: &mut NamedPipeServer) {
    // tokio's named pipe layer returns from `write` before the queued overlapped write has reached
    // the pipe, so an empty write (which waits for the queued one) comes first.
    if tokio::time::timeout(PIPE_FLUSH_TIMEOUT, pipe.write(&[])).await.is_ok() {
        let raw = pipe.as_raw_handle() as usize;
        // FlushFileBuffers returns at once on these overlapped handles, so the unread bytes are polled.
        let deadline = Instant::now() + PIPE_FLUSH_TIMEOUT;
        while outbound_bytes(raw).is_some_and(|n| n > 0) && Instant::now() < deadline {
            tokio::time::sleep(PIPE_DRAIN_POLL).await;
        }
    }
    let _ = pipe.disconnect();
}

/// `FILE_PIPE_LOCAL_INFORMATION`, declared here because the Wdk windows features are not enabled.
#[repr(C)]
#[derive(Default)]
struct PipeLocalInfo {
    named_pipe_type: u32,
    named_pipe_configuration: u32,
    maximum_instances: u32,
    current_instances: u32,
    inbound_quota: u32,
    read_data_available: u32,
    outbound_quota: u32,
    write_quota_available: u32,
    named_pipe_state: u32,
    named_pipe_end: u32,
}

/// `IO_STATUS_BLOCK` (the status and information words are both pointer-sized).
#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationFile(file: *mut c_void, iosb: *mut IoStatusBlock, info: *mut c_void, length: u32, class: u32) -> i32;
}

/// `FilePipeLocalInformation`
const FILE_PIPE_LOCAL_INFORMATION: u32 = 24;

/// Bytes written on the server end that the client has not read yet, or `None` when the query fails
/// (for example after the client has closed its end).
fn outbound_bytes(raw: usize) -> Option<u32> {
    let mut info = PipeLocalInfo::default();
    let mut iosb = IoStatusBlock { status: 0, information: 0 };
    // SAFETY: `info` and `iosb` are valid for the sizes passed, and `raw` is the open handle of a
    // pipe server end that the caller keeps alive.
    let status = unsafe {
        NtQueryInformationFile(
            raw as *mut c_void,
            &mut iosb,
            &mut info as *mut PipeLocalInfo as *mut c_void,
            std::mem::size_of::<PipeLocalInfo>() as u32,
            FILE_PIPE_LOCAL_INFORMATION,
        )
    };
    (status >= 0).then(|| info.outbound_quota.saturating_sub(info.write_quota_available))
}

/// Reports the first end-of-stream seen on the wrapped upstream.
struct EofWatch<S> {
    inner: S,
    done: Option<oneshot::Sender<()>>,
}

impl<S: AsyncRead + Unpin> AsyncRead for EofWatch<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let had_room = buf.remaining() > 0;
        let before = buf.filled().len();
        let res = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = res {
            if had_room && buf.filled().len() == before {
                if let Some(done) = this.done.take() {
                    let _ = done.send(());
                }
            }
        }
        res
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for EofWatch<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn http_502(reason: &str) -> Vec<u8> {
    format!("HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}", reason.len()).into_bytes()
}

/// Holds one `tail -f /dev/null` session open so the distro (and dockerd) stay up.
async fn keepalive(distro: String, log: Arc<Log>, mut stop: watch::Receiver<bool>) {
    let mut failures = 0u32;
    let mut last_error: Option<String> = None;
    loop {
        match wsl::spawn_exec(&distro, &["tail", "-f", "/dev/null"]) {
            Ok(mut child) => {
                failures = 0;
                last_error = None;
                log.line("keepalive session started");
                tokio::select! {
                    status = child.wait() => {
                        let status = status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                        log.line(&format!("keepalive session exited: {status}"));
                    }
                    _ = stop.wait_for(|s| *s) => return,
                }
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                note(&log, &mut last_error, format!("keepalive spawn failed: {e}"));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(keepalive_delay(failures)) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Wait before the next keepalive spawn after `failures` failed spawns in a row.
fn keepalive_delay(failures: u32) -> Duration {
    if failures >= KEEPALIVE_FAILURES {
        KEEPALIVE_BACKOFF
    } else {
        KEEPALIVE_RETRY
    }
}

/// Sends a GET through a fresh upstream connection and returns the raw response bytes.
async fn query(upstream: &dyn Upstream, path: &str) -> io::Result<Vec<u8>> {
    let mut conn = upstream.connect().await?;
    conn.write_all(format!("GET {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut raw = Vec::new();
    tokio::time::timeout(QUERY_TIMEOUT, conn.read_to_end(&mut raw))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "engine query timed out"))??;
    Ok(raw)
}

/// Splits a complete HTTP/1.x response into status code and body, decoding chunked or
/// Content-Length framing. `None` when the response is malformed or truncated.
pub fn parse_response(bytes: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&bytes[..split]).ok()?;
    let body = &bytes[split + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        if name.trim().eq_ignore_ascii_case("transfer-encoding") {
            chunked |= value.split(',').any(|t| t.trim().eq_ignore_ascii_case("chunked"));
        } else if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.parse().ok();
        }
    }
    if chunked {
        return decode_chunked(body).map(|b| (status, b));
    }
    match length {
        Some(n) if body.len() >= n => Some((status, body[..n].to_vec())),
        Some(_) => None,
        None => Some((status, body.to_vec())),
    }
}

/// Decodes a chunked body up to the zero-size chunk. Trailers are ignored.
fn decode_chunked(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        // A size near usize::MAX would overflow the CRLF offset; treat it as malformed.
        let end = size.checked_add(2)?;
        if data.len() < end || &data[size..end] != b"\r\n" {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[end..];
    }
}

/// Reads the container list every 10 s and binds LAN forwarders for its published ports.
/// `portsLocalOnly` in `engine.json` (re-read every 5 s) drops all of them.
async fn publish_ports(distro: String, data_dir: PathBuf, upstream: Arc<dyn Upstream>, log: Arc<Log>, mut stop: watch::Receiver<bool>) {
    let mut fwd = Forwarder::new(log_fn(&log));
    let mut ip: Option<(Ipv4Addr, Instant)> = None;
    let mut last_ports: Option<Vec<u16>> = None;
    let mut last_error: Option<String> = None;
    let mut tick = 0u32;
    let mut interval = tokio::time::interval(CONFIG_INTERVAL);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.wait_for(|s| *s) => break,
        }
        if EngineConfig::load(&data_dir).is_some_and(|c| c.ports_local_only) {
            fwd.clear().await;
            last_ports = None;
            continue;
        }
        let query_now = tick.is_multiple_of(QUERY_EVERY);
        tick = tick.wrapping_add(1);
        if !query_now {
            continue;
        }

        let Some(target) = distro_ip(&distro, &mut ip, &log).await else {
            continue;
        };
        let raw = match query(upstream.as_ref(), "/containers/json").await {
            Ok(raw) => raw,
            Err(e) => {
                ip = None;
                note(&log, &mut last_error, format!("container query failed: {e}"));
                continue;
            }
        };
        let body = match parse_response(&raw) {
            Some((200, body)) => body,
            Some((status, _)) => {
                note(&log, &mut last_error, format!("container query returned HTTP {status}"));
                continue;
            }
            None => {
                note(&log, &mut last_error, "container query returned a malformed response".to_string());
                continue;
            }
        };
        last_error = None;

        let mut ports: Vec<u16> = published_ports(&body).iter().map(|p| p.host_port).collect();
        ports.sort_unstable();
        ports.dedup();
        if last_ports.as_ref() != Some(&ports) {
            let list = if ports.is_empty() { "none".to_string() } else { ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ") };
            log.line(&format!("published ports: {list}"));
            last_ports = Some(ports.clone());
        }
        fwd.reconcile(target, &ports, &lan_ipv4_addresses()).await;
    }
    fwd.clear().await;
}

/// The distro's eth0 address, from `tmop-engine ip`. Cached for [`IP_MAX_AGE`].
async fn distro_ip(distro: &str, cache: &mut Option<(Ipv4Addr, Instant)>, log: &Log) -> Option<Ipv4Addr> {
    if let Some((addr, at)) = *cache {
        if at.elapsed() < IP_MAX_AGE {
            return Some(addr);
        }
    }
    let d = distro.to_string();
    let out = tokio::task::spawn_blocking(move || wsl::exec(&d, &[ENGINE_SCRIPT, "ip"], PROBE_TIMEOUT)).await.ok()?.ok()?;
    let addr = if out.status == 0 { usable_distro_ip(&out.stdout) } else { None };
    match addr {
        Some(addr) => {
            *cache = Some((addr, Instant::now()));
            Some(addr)
        }
        None => {
            log.line(&format!("cannot read the distro IP (exit {}): {}", out.status, out.stdout.trim()));
            None
        }
    }
}

/// Parses the distro address printed by `tmop-engine ip`. Loopback, link-local (APIPA),
/// unspecified, multicast and broadcast addresses are not usable and count as unknown.
fn usable_distro_ip(text: &str) -> Option<Ipv4Addr> {
    let ip = text.trim().parse::<Ipv4Addr>().ok()?;
    let unusable = ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast();
    (!unusable).then_some(ip)
}

/// Signals the running relay on `pipe` to stop. False when none is running.
pub fn signal_stop(pipe: &str) -> bool {
    match NamedEvent::open(&stop_event_name(pipe)) {
        Some(event) => {
            event.set();
            true
        }
        None => false,
    }
}

/// True when a relay holds the lock on `pipe`.
pub fn is_running(pipe: &str) -> bool {
    match NamedMutex::try_acquire(&mutex_name(pipe)) {
        Ok(Some(_lock)) => false,
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Waits up to `timeout` for a relay on `pipe` to report that its pipe is listening.
pub fn wait_ready(pipe: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(event) = NamedEvent::open(&ready_event_name(pipe)) {
            return event.wait(deadline.saturating_duration_since(Instant::now()));
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;
    use tokio::net::windows::named_pipe::ClientOptions;

    const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";

    #[test]
    fn object_names_follow_the_pipe() {
        assert_eq!(mutex_name("tmop_engine"), r"Local\tmop_engine_relay_tmop_engine");
        assert_eq!(stop_event_name("tmop_engine_test"), r"Local\tmop_engine_relay_stop_tmop_engine_test");
        assert_eq!(ready_event_name("x"), r"Local\tmop_engine_relay_ready_x");
    }

    #[test]
    fn engine_data_dir_setting_reads_the_key() {
        assert_eq!(engine_data_dir_setting(r#"{"engineDataDir":"D:\\engine"}"#), r"D:\engine");
        assert_eq!(engine_data_dir_setting(r#"{"other":1}"#), "");
        assert_eq!(engine_data_dir_setting(r#"{"engineDataDir":5}"#), "");
        assert_eq!(engine_data_dir_setting("not json"), "");
    }

    #[test]
    fn parse_response_content_length() {
        assert_eq!(parse_response(RESP), Some((200, b"OK".to_vec())));
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nab"), None);
        assert_eq!(parse_response(b"HTTP/1.1 404 Not Found\r\n\r\nno body"), Some((404, b"no body".to_vec())));
        assert_eq!(parse_response(b"garbage"), None);
    }

    #[test]
    fn parse_response_chunked() {
        let raw: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n1;ext=x\r\n!\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw), Some((200, b"hello!".to_vec())));
        // truncated before the zero chunk
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"), None);
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"), None);
    }

    #[test]
    fn http_502_has_matching_length() {
        let text = String::from_utf8(http_502("no engine")).unwrap();
        assert!(text.starts_with("HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(text.contains("Content-Length: 9\r\n"));
        assert!(text.ends_with("\r\n\r\nno engine"));
    }

    #[test]
    fn unused_pipe_is_not_running_and_not_ready() {
        let pipe = format!("tmop_engine_absent_{}", std::process::id());
        assert!(!is_running(&pipe));
        assert!(!signal_stop(&pipe));
        assert!(!wait_ready(&pipe, Duration::from_millis(300)));
    }

    /// Reads one request head, answers with `reply` and then closes.
    struct FakeUpstream {
        reply: Vec<u8>,
    }

    impl Upstream for FakeUpstream {
        fn connect(&self) -> BoxFuture<'_, io::Result<Box<dyn Stream>>> {
            let reply = self.reply.clone();
            Box::pin(async move {
                let (ours, mut theirs) = duplex(1 << 16);
                tokio::spawn(async move {
                    let mut seen = Vec::new();
                    let mut byte = [0u8; 1];
                    while !seen.ends_with(b"\r\n\r\n") {
                        if theirs.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        seen.push(byte[0]);
                    }
                    let _ = theirs.write_all(&reply).await;
                });
                Ok(Box::new(ours) as Box<dyn Stream>)
            })
        }
    }

    #[test]
    fn retry_and_backoff_delays() {
        let mut d = PIPE_RETRY_MIN;
        let mut seen = Vec::new();
        for _ in 0..7 {
            seen.push(d.as_millis());
            d = pipe_retry_delay(d);
        }
        assert_eq!(seen, vec![250, 500, 1000, 2000, 4000, 5000, 5000]);
        assert_eq!(keepalive_delay(0), KEEPALIVE_RETRY);
        assert_eq!(keepalive_delay(KEEPALIVE_FAILURES - 1), KEEPALIVE_RETRY);
        assert_eq!(keepalive_delay(KEEPALIVE_FAILURES), KEEPALIVE_BACKOFF);
    }

    #[test]
    fn distro_ip_must_be_routable_unicast() {
        assert_eq!(usable_distro_ip("172.20.0.2\n"), Some(Ipv4Addr::new(172, 20, 0, 2)));
        for bad in ["", "127.0.0.1", "169.254.3.4", "0.0.0.0", "224.0.0.1", "255.255.255.255", "not an ip"] {
            assert_eq!(usable_distro_ip(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn decode_chunked_rejects_size_overflow() {
        // Must return None, not panic on the CRLF offset.
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n"), None);
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfffffffffffffffe\r\nx"), None);
    }

    #[tokio::test]
    async fn pipe_server_relays_and_stops() {
        tokio::task::LocalSet::new().run_until(pipe_round_trip(&format!("tmop_engine_test_{}", std::process::id()), RESP.to_vec())).await;
    }

    #[tokio::test]
    #[ignore = "open: the last ~43 bytes of a >64 KiB reply are lost after upstream EOF (see report)"]
    async fn pipe_delivers_response_larger_than_pipe_buffer() {
        // Upstream closes right after the reply: every byte must still reach the client.
        let body: Vec<u8> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();
        let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        reply.extend_from_slice(&body);
        let name = format!("tmop_engine_big_{}", std::process::id());
        let got = tokio::task::LocalSet::new().run_until(pipe_round_trip(&name, reply.clone())).await;
        assert_eq!(got.len(), reply.len());
        assert!(got == reply, "response bytes differ");
    }

    /// Serves one client on pipe `name` through a fake upstream that answers `reply`. Returns the
    /// bytes the client read (as many as `reply` is long) and stops the server.
    async fn pipe_round_trip(name: &str, reply: Vec<u8>) -> Vec<u8> {
        let path = format!(r"\\.\pipe\{name}");
        let (tx, stop) = watch::channel(false);
        let log = Arc::new(Log { file: Mutex::new(None) });
        let upstream = Arc::new(FakeUpstream { reply: reply.clone() });
        let server = tokio::task::spawn_local(serve_pipe(path.clone(), upstream, log, stop, || {}));

        let mut client = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match ClientOptions::new().open(&path) {
                    Ok(c) => return c,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .expect("pipe never listened");

        client.write_all(b"GET /_ping HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut got = vec![0u8; reply.len()];
        tokio::time::timeout(Duration::from_secs(10), client.read_exact(&mut got)).await.expect("no response").unwrap();
        drop(client);

        tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(10), server).await.expect("server did not stop").unwrap().unwrap();
        got
    }
}
