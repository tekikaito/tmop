//! Container and stack log streams.
//!
//! A [`LogSession`] follows the stdout and stderr of one container, or of several
//! containers merged into one buffer, on the engine runtime. Chunks from the engine are
//! split into lines by [`Partial`], parsed by [`parse_line`] and pushed into a bounded
//! [`LogBuffer`] that the UI reads under a short lock. [`matches`], [`render`] and
//! [`save`] work on buffered lines and never touch the engine.

use super::backend::{err_text, Engine};
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptionsBuilder;
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::task::JoinHandle;

/// Lines kept per session. The oldest lines are dropped beyond this.
pub const MAX_LINES: usize = 20_000;
/// Lines requested from the engine when a session starts.
pub const DEFAULT_TAIL: usize = 1_000;
/// Bytes of an unfinished line that are held back before the line is emitted anyway.
const MAX_PARTIAL: usize = 64 * 1024;

/// Output stream a line came from. Console output of TTY containers counts as stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One log line as shown in the UI.
#[derive(Clone, Debug)]
pub struct Line {
    pub stream: Stream,
    /// RFC3339 prefix written by the engine, e.g. `2024-05-01T12:00:00.123456789Z`.
    pub timestamp: Option<String>,
    /// Message without the timestamp prefix and without the trailing line break.
    pub text: String,
    /// Compose service name (or container name) for stack sessions.
    pub source: Option<String>,
}

/// Bounded line buffer shared between a session task and the UI.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: VecDeque<Line>,
    /// Lines ever received by this buffer, including dropped ones.
    pub total: u64,
    /// Lines removed because the buffer was full.
    pub dropped: u64,
}

impl LogBuffer {
    pub fn push(&mut self, line: Line) {
        self.total += 1;
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Removes the buffered lines. The `total` and `dropped` counters are kept.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

type OnChange = Arc<dyn Fn() + Send + Sync>;

/// A running log follow. Dropping it stops the tasks.
pub struct LogSession {
    pub buffer: Arc<Mutex<LogBuffer>>,
    /// Last error reported by an engine stream.
    pub error: Arc<Mutex<Option<String>>>,
    /// Set once every stream has ended, or when the session is stopped.
    pub finished: Arc<AtomicBool>,
    /// Bumped on each batch of new lines so the UI knows when to repaint or autoscroll.
    pub generation: Arc<AtomicU64>,
    tasks: Vec<JoinHandle<()>>,
    /// Source labels of a stack session, in the order they were given. Empty for one container.
    pub sources: Vec<String>,
}

/// Shared state handed to each follow task.
#[derive(Clone)]
struct Sink {
    buffer: Arc<Mutex<LogBuffer>>,
    error: Arc<Mutex<Option<String>>>,
    finished: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    on_change: OnChange,
}

impl Sink {
    /// Adds one batch of lines under a single lock and notifies the UI once.
    fn push(&self, lines: Vec<Line>) {
        if lines.is_empty() {
            return;
        }
        {
            let mut buffer = lock(&self.buffer);
            for line in lines {
                buffer.push(line);
            }
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        (self.on_change)();
    }

    fn fail(&self, message: String) {
        *lock(&self.error) = Some(message);
        self.generation.fetch_add(1, Ordering::Relaxed);
        (self.on_change)();
    }

    /// Marks one stream as ended. The last one to end marks the session finished.
    fn end(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.finished.store(true, Ordering::Release);
            self.generation.fetch_add(1, Ordering::Relaxed);
            (self.on_change)();
        }
    }
}

impl LogSession {
    /// Follows one container (stdout and stderr, timestamps requested from the engine).
    /// `tail` is the number of existing lines to load first.
    pub fn container(engine: &Engine, id: &str, tail: usize, on_change: impl Fn() + Send + Sync + 'static) -> LogSession {
        start(engine, vec![(id.to_string(), None)], tail, Arc::new(on_change), Vec::new())
    }

    /// Merged logs of several containers. `containers` holds (container id, source label)
    /// pairs. Each container gets its own task; all of them write into one buffer.
    pub fn stack(
        engine: &Engine,
        containers: &[(String, String)],
        tail: usize,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> LogSession {
        let targets = containers.iter().map(|(id, label)| (id.clone(), Some(label.clone()))).collect();
        let sources = containers.iter().map(|(_, label)| label.clone()).collect();
        start(engine, targets, tail, Arc::new(on_change), sources)
    }

    /// Aborts all tasks and marks the session finished.
    pub fn stop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.finished.store(true, Ordering::Release);
    }
}

impl Drop for LogSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start(
    engine: &Engine,
    targets: Vec<(String, Option<String>)>,
    tail: usize,
    on_change: OnChange,
    sources: Vec<String>,
) -> LogSession {
    let finished = Arc::new(AtomicBool::new(targets.is_empty()));
    let sink = Sink {
        buffer: Arc::new(Mutex::new(LogBuffer::default())),
        error: Arc::new(Mutex::new(None)),
        finished: finished.clone(),
        generation: Arc::new(AtomicU64::new(0)),
        pending: Arc::new(AtomicUsize::new(targets.len())),
        on_change,
    };
    let tasks = targets
        .into_iter()
        .map(|(id, source)| engine.runtime.spawn(follow(engine.docker.clone(), id, source, tail, sink.clone())))
        .collect();
    LogSession {
        buffer: sink.buffer.clone(),
        error: sink.error.clone(),
        finished,
        generation: sink.generation.clone(),
        tasks,
        sources,
    }
}

/// Streams one container's logs into the sink until the engine closes the stream.
async fn follow(docker: Docker, id: String, source: Option<String>, tail: usize, sink: Sink) {
    let tail = tail.to_string();
    let opts = LogsOptionsBuilder::default()
        .follow(true)
        .stdout(true)
        .stderr(true)
        .timestamps(true)
        .tail(&tail)
        .build();
    let mut logs = docker.logs(&id, Some(opts));
    let mut partial = Partial::default();
    while let Some(item) = logs.next().await {
        match item {
            Ok(output) => {
                let (kind, bytes) = match output {
                    LogOutput::StdErr { message } => (Stream::Stderr, message),
                    LogOutput::StdOut { message } | LogOutput::Console { message } => (Stream::Stdout, message),
                    LogOutput::StdIn { .. } => continue,
                };
                let lines = partial.feed(kind, &bytes).into_iter().map(|(s, raw)| to_line(s, &raw, &source)).collect();
                sink.push(lines);
            }
            Err(e) => {
                sink.fail(err_text(&e));
                break;
            }
        }
    }
    let rest = partial.flush().into_iter().map(|(s, raw)| to_line(s, &raw, &source)).collect();
    sink.push(rest);
    sink.end();
}

fn to_line(stream: Stream, raw: &str, source: &Option<String>) -> Line {
    let (timestamp, text) = parse_line(raw);
    Line { stream, timestamp, text, source: source.clone() }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Splits an engine log line into (timestamp, text). The engine prefixes `<RFC3339> `
/// when timestamps are requested; lines without a valid prefix are returned whole.
pub fn parse_line(raw: &str) -> (Option<String>, String) {
    match raw.split_once(' ') {
        Some((ts, rest)) if looks_like_timestamp(ts) => (Some(ts.to_string()), rest.to_string()),
        _ => (None, raw.to_string()),
    }
}

/// Checks the `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)` shape.
fn looks_like_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return false;
    }
    if !b[..4].iter().all(u8::is_ascii_digit) || !b[11..13].iter().all(u8::is_ascii_digit) {
        return false;
    }
    b.last() == Some(&b'Z') || (b.len() >= 6 && matches!(b[b.len() - 6], b'+' | b'-') && b[b.len() - 3] == b':')
}

/// Accumulates raw chunks per stream and returns complete lines.
#[derive(Default)]
pub struct Partial {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Partial {
    /// Feeds a chunk that may hold several lines or a partial line. Returns the complete
    /// lines (without `\n` or `\r\n`). Invalid UTF-8 is replaced.
    pub fn feed(&mut self, stream: Stream, chunk: &[u8]) -> Vec<(Stream, String)> {
        let buf = match stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(off) = buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + off;
            out.push((stream, decode(&buf[start..end])));
            start = end + 1;
        }
        buf.drain(..start);
        if buf.len() > MAX_PARTIAL {
            out.push((stream, decode(buf.as_slice())));
            buf.clear();
        }
        out
    }

    /// Returns the unfinished lines of both streams, stdout first. Call when a stream ends.
    pub fn flush(&mut self) -> Vec<(Stream, String)> {
        let mut out = Vec::new();
        for (stream, buf) in [(Stream::Stdout, &mut self.stdout), (Stream::Stderr, &mut self.stderr)] {
            if !buf.is_empty() {
                out.push((stream, decode(buf.as_slice())));
                buf.clear();
            }
        }
        out
    }
}

fn decode(bytes: &[u8]) -> String {
    let bytes = if bytes.last() == Some(&b'\r') { &bytes[..bytes.len() - 1] } else { bytes };
    String::from_utf8_lossy(bytes).into_owned()
}

/// True when the line passes the stream filter and `query` occurs in its text or source.
/// The query is case-insensitive. An empty query matches every line of the stream.
pub fn matches(line: &Line, query: &str, only: Option<Stream>) -> bool {
    if only.is_some_and(|s| s != line.stream) {
        return false;
    }
    if query.is_empty() {
        return true;
    }
    contains_ci(&line.text, query) || line.source.as_deref().is_some_and(|s| contains_ci(s, query))
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.is_ascii() {
        // Non-ASCII bytes never equal an ASCII byte, so this is exact for ASCII needles.
        haystack.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// Formats a line for display or saving. The source is shown as `[name]` and the time as
/// `HH:MM:SS.mmm` (see [`short_time`]), each only when enabled and present.
pub fn render(line: &Line, timestamps: bool, source: bool) -> String {
    let mut out = String::new();
    if source {
        if let Some(name) = &line.source {
            out.push('[');
            out.push_str(name);
            out.push_str("] ");
        }
    }
    if timestamps {
        if let Some(ts) = &line.timestamp {
            out.push_str(&short_time(ts));
            out.push(' ');
        }
    }
    out.push_str(&line.text);
    out
}

/// Shortens an RFC3339 timestamp to `HH:MM:SS.mmm` (UTC, milliseconds). Input that does
/// not have a time part is returned unchanged.
pub fn short_time(ts: &str) -> String {
    let Some((_, time)) = ts.split_once('T') else {
        return ts.to_string();
    };
    let b = time.as_bytes();
    if b.len() < 8 || b[2] != b':' || b[5] != b':' {
        return ts.to_string();
    }
    let hms = &time[..8];
    let frac: String = if b.get(8) == Some(&b'.') {
        time.get(9..).unwrap_or("").bytes().take_while(u8::is_ascii_digit).take(3).map(char::from).collect()
    } else {
        String::new()
    };
    format!("{hms}.{frac:0<3}")
}

/// Writes the matching lines to `path` using [`render`]. Returns the number of lines written.
pub fn save(
    buffer: &LogBuffer,
    path: &Path,
    query: &str,
    only: Option<Stream>,
    timestamps: bool,
    source: bool,
) -> std::io::Result<usize> {
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    let mut count = 0;
    for line in buffer.iter().filter(|line| matches(line, query, only)) {
        writeln!(out, "{}", render(line, timestamps, source))?;
        count += 1;
    }
    out.flush()?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(stream: Stream, text: &str, source: Option<&str>) -> Line {
        Line {
            stream,
            timestamp: Some("2024-05-01T12:00:00.123456789Z".into()),
            text: text.into(),
            source: source.map(str::to_string),
        }
    }

    #[test]
    fn parse_line_splits_engine_timestamp() {
        let (ts, text) = parse_line("2024-05-01T12:00:00.123456789Z hello there");
        assert_eq!(ts.as_deref(), Some("2024-05-01T12:00:00.123456789Z"));
        assert_eq!(text, "hello there");
    }

    #[test]
    fn parse_line_accepts_offset_zone() {
        let (ts, text) = parse_line("2024-05-01T12:00:00+02:00 msg");
        assert_eq!(ts.as_deref(), Some("2024-05-01T12:00:00+02:00"));
        assert_eq!(text, "msg");
    }

    #[test]
    fn parse_line_without_timestamp_keeps_text() {
        assert_eq!(parse_line("plain output"), (None, "plain output".to_string()));
        assert_eq!(parse_line("no-space"), (None, "no-space".to_string()));
        assert_eq!(parse_line("2024 looks like a date"), (None, "2024 looks like a date".to_string()));
        assert_eq!(parse_line(""), (None, String::new()));
    }

    #[test]
    fn partial_joins_lines_across_chunks() {
        let mut p = Partial::default();
        assert!(p.feed(Stream::Stdout, b"hel").is_empty());
        let out = p.feed(Stream::Stdout, b"lo\nwor");
        assert_eq!(out, vec![(Stream::Stdout, "hello".to_string())]);
        let out = p.feed(Stream::Stdout, b"ld\nnext\n");
        assert_eq!(out, vec![(Stream::Stdout, "world".to_string()), (Stream::Stdout, "next".to_string())]);
        assert!(p.flush().is_empty());
    }

    #[test]
    fn partial_strips_crlf_and_keeps_streams_apart() {
        let mut p = Partial::default();
        assert!(p.feed(Stream::Stdout, b"out-a").is_empty());
        let out = p.feed(Stream::Stderr, b"err-1\r\nerr-2\r\nerr-3");
        assert_eq!(out, vec![(Stream::Stderr, "err-1".to_string()), (Stream::Stderr, "err-2".to_string())]);
        let out = p.feed(Stream::Stdout, b"\n");
        assert_eq!(out, vec![(Stream::Stdout, "out-a".to_string())]);
        assert_eq!(p.flush(), vec![(Stream::Stderr, "err-3".to_string())]);
    }

    #[test]
    fn partial_flush_emits_unterminated_line() {
        let mut p = Partial::default();
        p.feed(Stream::Stderr, b"tail without newline");
        assert_eq!(p.flush(), vec![(Stream::Stderr, "tail without newline".to_string())]);
        assert!(p.flush().is_empty());
    }

    #[test]
    fn partial_replaces_invalid_utf8() {
        let mut p = Partial::default();
        let out = p.feed(Stream::Stdout, b"bad \xff byte\n");
        assert_eq!(out.len(), 1);
        assert!(out[0].1.contains('\u{FFFD}'));
    }

    #[test]
    fn partial_emits_oversized_line() {
        let mut p = Partial::default();
        let big = vec![b'x'; MAX_PARTIAL + 1];
        let out = p.feed(Stream::Stdout, &big);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.len(), MAX_PARTIAL + 1);
        assert!(p.flush().is_empty());
    }

    #[test]
    fn matches_filters_by_query_stream_and_source() {
        let l = line(Stream::Stdout, "Hello World", Some("web"));
        assert!(matches(&l, "", None));
        assert!(matches(&l, "hello", None));
        assert!(matches(&l, "WORLD", Some(Stream::Stdout)));
        assert!(matches(&l, "web", None));
        assert!(!matches(&l, "hello", Some(Stream::Stderr)));
        assert!(!matches(&l, "missing", None));
        let plain = line(Stream::Stderr, "ÄRGER occurred", None);
        assert!(matches(&plain, "ärger", None));
    }

    #[test]
    fn short_time_formats_milliseconds() {
        assert_eq!(short_time("2024-05-01T12:00:00.123456789Z"), "12:00:00.123");
        assert_eq!(short_time("2024-05-01T12:00:05.5Z"), "12:00:05.500");
        assert_eq!(short_time("2024-05-01T12:00:05Z"), "12:00:05.000");
        assert_eq!(short_time("not a time"), "not a time");
    }

    #[test]
    fn render_applies_enabled_parts() {
        let l = line(Stream::Stdout, "msg", Some("web"));
        assert_eq!(render(&l, true, true), "[web] 12:00:00.123 msg");
        assert_eq!(render(&l, false, false), "msg");
        assert_eq!(render(&l, true, false), "12:00:00.123 msg");
        let bare = Line { stream: Stream::Stdout, timestamp: None, text: "x".into(), source: None };
        assert_eq!(render(&bare, true, true), "x");
    }

    #[test]
    fn buffer_drops_oldest_beyond_limit() {
        let mut buf = LogBuffer::default();
        for i in 0..MAX_LINES + 5 {
            buf.push(line(Stream::Stdout, &i.to_string(), None));
        }
        assert_eq!(buf.len(), MAX_LINES);
        assert_eq!(buf.total, (MAX_LINES + 5) as u64);
        assert_eq!(buf.dropped, 5);
        assert_eq!(buf.iter().next().map(|l| l.text.as_str()), Some("5"));
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.total, (MAX_LINES + 5) as u64);
    }

    #[test]
    fn save_writes_matching_lines() {
        let mut buf = LogBuffer::default();
        buf.push(line(Stream::Stdout, "alpha one", Some("web")));
        buf.push(line(Stream::Stderr, "alpha two", Some("db")));
        buf.push(line(Stream::Stdout, "beta", Some("web")));
        let path = std::env::temp_dir().join(format!("tmop-logs-test-{}.txt", std::process::id()));
        let count = save(&buf, &path, "alpha", Some(Stream::Stdout), true, false).expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        assert_eq!(count, 1);
        assert_eq!(text, "12:00:00.123 alpha one\n");
    }
}
