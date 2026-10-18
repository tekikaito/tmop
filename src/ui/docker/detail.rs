//! Detail area under the Docker tables: logs, inspect, stats and files for the selected
//! container, the merged logs of a stack, or the files of a volume.
//!
//! Engine work runs as [`Task`]s on the worker runtime and log follows as [`LogSession`]s.
//! Both are stopped or cancelled when the selection changes or the engine goes away.

use super::terminal_tab::{body_rect, terminal_tab, TerminalSessions};
use super::{DockerUi, Snap, Tab};
use crate::docker::backend::{err_text, Backend, Engine, NoticeKind, Task};
use crate::docker::exec::ExecOptions;
use crate::docker::files::{self, Entry, Target, VolumeBrowser};
use crate::docker::logs::{self, LogBuffer, LogSession, Stream, DEFAULT_TAIL};
use crate::docker::model::{format_bytes, Container, EngineStatus, StatsSample};
use crate::metrics::Series;
use crate::process::format_rate;
use crate::ui::icons::IconCache;
use crate::ui::kit;
use crate::ui::table::{self, cmp_str, Cell, Col, TableState};
use crate::ui::terminal::grid_for;
use crate::ui::theme::{self, Palette};
use crate::ui::widgets::{self, Graph, GraphLine, Icon};
use crate::ui::{App, FrameData};
use eframe::egui::{self, pos2, text::LayoutJob, text::TextFormat, vec2, Align, Align2, Layout, Rect, RichText, Sense, Stroke};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Height of the toolbars of the logs, inspect and files tabs.
pub(super) const TOOLBAR_H: f32 = 26.0;
/// Height of the line under the files toolbar and the logs footer.
const LINE_H: f32 = 16.0;
/// Width of the key/value column of the stats tab.
const KV_W: f32 = 230.0;
/// Right part of the files toolbar that holds the buttons.
const FILES_BUTTONS_W: f32 = 420.0;
/// Refresh rate while an engine task runs and its result is not yet shown.
const POLL_EVERY: Duration = Duration::from_millis(100);

const FILES_COLS: [Col; 5] = [Col::text("Name", 300.0), Col::num("Size", 90.0), Col::text("Mode", 110.0), Col::text("Modified", 150.0), Col::text("Link", 200.0)];

pub(super) type Toasts = Vec<(NoticeKind, String)>;

/// Tab of the detail area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Logs,
    Inspect,
    Stats,
    Files,
    Terminal,
}

impl DetailTab {
    fn label(self) -> &'static str {
        match self {
            DetailTab::Logs => "Logs",
            DetailTab::Inspect => "Inspect",
            DetailTab::Stats => "Stats",
            DetailTab::Files => "Files",
            DetailTab::Terminal => "Terminal",
        }
    }
}

/// The object the detail area describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    Container(String),
    Stack(String),
    Volume(String),
    None,
}

impl Subject {
    /// Tabs that apply to this subject, in strip order.
    fn tabs(&self) -> &'static [DetailTab] {
        match self {
            Subject::Container(_) => &[DetailTab::Logs, DetailTab::Inspect, DetailTab::Stats, DetailTab::Files, DetailTab::Terminal],
            Subject::Stack(_) => &[DetailTab::Logs],
            Subject::Volume(_) => &[DetailTab::Files],
            Subject::None => &[],
        }
    }
}

/// One rendered log line, with the source index and label of stack sessions.
struct Row {
    stream: Stream,
    source: Option<(usize, String)>,
    text: String,
}

/// What the cached rows were built from.
#[derive(Clone, Debug, PartialEq)]
struct RowsKey {
    generation: u64,
    len: usize,
    total: u64,
    query: String,
    only: Option<Stream>,
    timestamps: bool,
}

#[derive(Default)]
struct LogsState {
    session: Option<LogSession>,
    session_key: String,
    query: String,
    only: Option<Stream>,
    timestamps: bool,
    paused: bool,
    rows: Arc<Vec<Row>>,
    rows_key: Option<RowsKey>,
}

#[derive(Default)]
struct InspectState {
    task: Option<Task<Result<String, String>>>,
    key: String,
    text: Option<String>,
    error: Option<String>,
}

/// Graph history of one container, copied from the worker.
struct StatsGraphs {
    cpu: Series,
    mem: Series,
    rx: Series,
    tx: Series,
    read: Series,
    write: Series,
}

#[derive(Default)]
struct StatsState {
    id: String,
    seq: u64,
    graphs: Option<Arc<StatsGraphs>>,
}

#[derive(Default)]
struct FilesState {
    key: String,
    path: String,
    entries: Vec<Entry>,
    /// Pending directory listing.
    task: Option<Task<Result<Vec<Entry>, String>>>,
    /// Pending download or upload; the message is shown as a toast.
    transfer: Option<Task<Result<String, String>>>,
    /// Helper container of the volume being browsed.
    volume: Option<Arc<VolumeBrowser>>,
    /// Helper container being started for the volume.
    opening: Option<Task<Result<Arc<VolumeBrowser>, String>>>,
    /// Helpers whose subject was left while they were starting; removed once they exist.
    closing: Vec<Task<Result<Arc<VolumeBrowser>, String>>>,
    error: Option<String>,
    table: TableState,
    want_list: bool,
}

/// State of the detail area (tab, logs, inspect, stats and files).
#[derive(Default)]
pub struct DetailState {
    tab: DetailTab,
    logs: LogsState,
    inspect: InspectState,
    stats: StatsState,
    files: FilesState,
    pub terminals: TerminalSessions,
}

impl DetailState {
    /// Ends every terminal session. Call it when the engine changes; sessions belong to the engine.
    pub fn close_terminals(&mut self) {
        self.terminals.close_all();
    }

    /// Stops the log follow, cancels pending reads and closes the volume helper.
    /// Call it when the Docker page is left; the area also calls it when nothing is selected.
    /// Terminal sessions are kept.
    pub fn close(&mut self, engine: Option<&Engine>) {
        self.logs.session = None;
        self.logs.session_key.clear();
        self.logs.rows = Arc::default();
        self.logs.rows_key = None;
        cancel(&mut self.inspect.task);
        self.inspect.key.clear();
        self.inspect.text = None;
        self.inspect.error = None;
        self.files.close(engine);
        self.files.key.clear();
    }
}

impl FilesState {
    /// Cancels the listing, releases the volume helper and resets the view.
    fn close(&mut self, engine: Option<&Engine>) {
        cancel(&mut self.task);
        if let Some(open) = self.opening.take() {
            self.closing.push(open);
        }
        if let Some(vb) = self.volume.take() {
            remove_browser(engine, vb);
        }
        self.entries.clear();
        self.error = None;
        self.want_list = false;
        self.table.selected = None;
        self.reap(engine);
    }

    /// Removes helpers that finished starting after their subject was left.
    fn reap(&mut self, engine: Option<&Engine>) {
        let mut done = Vec::new();
        self.closing.retain_mut(|t| match t.take() {
            Some(Ok(vb)) => {
                done.push(vb);
                false
            }
            Some(Err(_)) => false,
            None => true,
        });
        for vb in done {
            remove_browser(engine, vb);
        }
    }
}

/// Draws the detail area in `rect`: header, tab strip and the selected tab.
pub fn detail_area(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let p = ui.painter().clone();
    p.line_segment([pos2(rect.left(), rect.top()), pos2(rect.right(), rect.top())], Stroke::new(1.0_f32, pal.panel_stroke));
    let inner = Rect::from_min_max(pos2(rect.left() + 4.0, rect.top() + 6.0), pos2(rect.right() - 4.0, rect.bottom() - 4.0));

    if app.docker.ui.snap.status == EngineStatus::Connected {
        let alive: HashSet<&str> = app.docker.ui.snap.containers.iter().map(|c| c.id.as_str()).collect();
        app.docker.ui.detail.terminals.retain(&alive);
    }
    // A list read since connecting: `list_seq` is bumped on every reset, so it cannot tell "not read yet".
    let loaded = app.docker.ui.snap.status == EngineStatus::Connected && app.docker.ui.snap.last_list_at.is_some();
    // Take first: a request whose container is listed must open, never be reported as missing.
    if let Some((id, options)) = app.docker.ui.detail.terminals.take_pending(&app.docker.ui.snap.containers) {
        // The containers table keeps its own selection and writes it back every frame, so it must agree.
        app.docker.ui.selected_container = Some(id.clone());
        app.docker.ui.containers.selected = Some(id.clone());
        app.docker.ui.detail.tab = DetailTab::Terminal;
        app.docker.ui.tab = Tab::Containers;
        // The session opens at the size its terminal body will have, so the first frame does not resize it.
        let grid = grid_for(ui, body_rect(detail_split(inner)[2]));
        open_terminal(app, ui, &id, options, grid);
    }
    if let Some(name) = app.docker.ui.detail.terminals.drop_unmatched(&app.docker.ui.snap.containers, loaded) {
        app.docker.ui.toast(NoticeKind::Error, format!("Container {name} not found for the terminal request"));
    }
    app.docker.ui.detail.terminals.pump();

    let page_tab = app.docker.ui.tab;
    let subject = subject_of(&app.docker.ui);
    let engine = app.docker.backend().and_then(|b| b.engine());
    if subject == Subject::None {
        app.docker.ui.detail.terminals.set_visible(None);
        app.docker.ui.detail.close(engine.as_ref());
        kit::empty_state(&p, inner, &pal, hint(page_tab));
        return;
    }

    let members = match &subject {
        Subject::Stack(name) => stack_members(&app.docker.ui.snap.containers, name),
        _ => Vec::new(),
    };
    let (title, sub) = describe(&subject, &app.docker.ui.snap);
    let running = match &subject {
        Subject::Container(id) => app.docker.ui.snap.containers.iter().any(|c| &c.id == id && c.state.is_running()),
        _ => false,
    };
    let sample: Option<StatsSample> = match &subject {
        Subject::Container(id) => app.docker.ui.snap.stats_last.get(id).copied(),
        _ => None,
    };
    let read_only = app.docker.ui.snap.read_only;
    let marked: &[DetailTab] = match &subject {
        Subject::Container(id) if app.docker.ui.detail.terminals.has_sessions(id) => &[DetailTab::Terminal],
        _ => &[],
    };

    let mut toasts: Toasts = Vec::new();
    let ctx = ui.ctx().clone();
    let split = detail_split(inner);
    let state = &mut app.docker.ui.detail;

    let allowed = subject.tabs();
    let tab = if allowed.contains(&state.tab) { state.tab } else { allowed[0] };

    // Header: name, then dim details.
    let name_rect = widgets::text(&p, pos2(split[0].left(), split[0].center().y), Align2::LEFT_CENTER, &title, theme::semibold(14.0), pal.text);
    widgets::text(&p, pos2(name_rect.right() + 12.0, split[0].center().y), Align2::LEFT_CENTER, &sub, theme::ui(12.5), pal.text_dim);

    let clicked = tab_strip(ui, split[1], &pal, allowed, marked, tab);
    let tab = clicked.unwrap_or(tab);
    state.tab = tab;

    let content = split[2];
    let Some(engine) = engine else {
        state.terminals.set_visible(None);
        state.close(None);
        kit::empty_state(&p, content, &pal, "Engine not reachable");
        return;
    };

    // Only the shown terminal session may request repaints.
    let shown_terminal = match (&subject, tab) {
        (Subject::Container(id), DetailTab::Terminal) => Some(id.as_str()),
        _ => None,
    };
    state.terminals.set_visible(shown_terminal);

    match tab {
        DetailTab::Logs => logs_tab(&mut state.logs, &engine, &subject, &members, &title, &ctx, ui, content, &pal, &mut toasts),
        DetailTab::Inspect => {
            if let Subject::Container(id) = &subject {
                inspect_tab(&mut state.inspect, &engine, id, ui, content, &pal, &mut toasts);
            }
        }
        DetailTab::Stats => {
            let graphs = match &subject {
                Subject::Container(id) => stats_graphs(&mut state.stats, app.docker.backend.as_ref(), id),
                _ => None,
            };
            stats_tab(app, ui, content, fd, running, sample, graphs);
        }
        DetailTab::Files => files_tab(&mut state.files, &engine, &subject, running, read_only, &mut app.icons, ui, content, &pal, &mut toasts),
        DetailTab::Terminal => {
            if let Subject::Container(id) = &subject {
                terminal_tab(&mut state.terminals, &engine, id, &title, running, read_only, &ctx, ui, content, &pal, &mut toasts);
            }
        }
    }

    for (kind, text) in toasts {
        app.docker.ui.toast(kind, text);
    }
}

/// Opens a terminal session of `grid` cells for a container chosen by the start-up request; errors become toasts.
fn open_terminal(app: &mut App, ui: &egui::Ui, id: &str, options: ExecOptions, grid: (usize, usize)) {
    let Some(engine) = app.docker.backend().and_then(|b| b.engine()) else {
        app.docker.ui.toast(NoticeKind::Error, "Engine not reachable");
        return;
    };
    if let Err(e) = app.docker.ui.detail.terminals.open(&engine, ui.ctx(), id, options, grid) {
        app.docker.ui.toast(NoticeKind::Error, e);
    }
}

/// Header, tab strip and content rects of the detail area inside `inner`.
fn detail_split(inner: Rect) -> Vec<Rect> {
    kit::vsplit(inner, &[24.0, 26.0, -1.0], 6.0)
}

/// The subject for the page tab and its selection. Unknown or removed objects give `None`.
fn subject_of(ui: &DockerUi) -> Subject {
    let snap: &Snap = &ui.snap;
    match ui.tab {
        Tab::Containers => match &ui.selected_container {
            Some(id) if snap.containers.iter().any(|c| &c.id == id) => Subject::Container(id.clone()),
            _ => Subject::None,
        },
        Tab::Stacks => match &ui.selected_stack {
            Some(name) if snap.stacks.iter().any(|s| &s.name == name) => Subject::Stack(name.clone()),
            _ => Subject::None,
        },
        Tab::Volumes => match &ui.selected_volume {
            Some(name) if snap.volumes.iter().any(|v| &v.name == name) => Subject::Volume(name.clone()),
            _ => Subject::None,
        },
        _ => Subject::None,
    }
}

/// Text shown when no object is selected.
fn hint(tab: Tab) -> &'static str {
    match tab {
        Tab::Containers => "Select a container for logs, inspect, stats and files.",
        Tab::Stacks => "Select a stack for its merged logs.",
        Tab::Volumes => "Select a volume to browse its files.",
        _ => "Logs, inspect, stats and files follow the selection on the Containers, Stacks and Volumes tabs.",
    }
}

/// Containers of a stack as (id, source label) pairs, sorted by label.
fn stack_members(containers: &[Container], name: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = containers
        .iter()
        .filter(|c| c.stack.as_deref() == Some(name))
        .map(|c| (c.id.clone(), c.service.clone().unwrap_or_else(|| c.name.clone())))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Header text: the name, then a dim line with the state and identity of the object.
fn describe(subject: &Subject, snap: &Snap) -> (String, String) {
    match subject {
        Subject::Container(id) => match snap.containers.iter().find(|c| &c.id == id) {
            Some(c) => (c.name.clone(), format!("{} - {} - {}", c.state.label(), c.image, c.short_id)),
            None => (String::new(), String::new()),
        },
        Subject::Stack(name) => {
            let services = snap.stacks.iter().find(|s| &s.name == name).map(|s| s.services.len()).unwrap_or(0);
            (name.clone(), format!("stack - {services} services"))
        }
        Subject::Volume(name) => {
            let driver = snap.volumes.iter().find(|v| &v.name == name).map(|v| v.driver.clone()).unwrap_or_default();
            (name.clone(), format!("volume - {driver}"))
        }
        Subject::None => (String::new(), String::new()),
    }
}

/// Hand-drawn tab strip; returns the tab clicked this frame.
fn tab_strip(ui: &egui::Ui, rect: Rect, pal: &Palette, tabs: &[DetailTab], marked: &[DetailTab], current: DetailTab) -> Option<DetailTab> {
    let font = theme::ui(13.0);
    let mut next = None;
    let mut x = rect.left();
    for (i, t) in tabs.iter().enumerate() {
        let w = widgets::text_width(ui.painter(), t.label(), font.clone()) + 24.0;
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
        let resp = ui.interact(r, ui.id().with(("detail-tab", i)), Sense::click());
        let p = ui.painter();
        let selected = *t == current;
        if selected {
            p.rect_filled(r, 4.0, pal.selection);
            p.line_segment([pos2(r.left() + 6.0, r.bottom() - 1.0), pos2(r.right() - 6.0, r.bottom() - 1.0)], Stroke::new(2.0_f32, pal.accent));
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, pal.hover);
        }
        let color = if selected { pal.text } else { pal.text_dim };
        p.text(r.center(), Align2::CENTER_CENTER, t.label(), font.clone(), color);
        if marked.contains(t) {
            p.circle_filled(pos2(r.right() - 7.0, r.top() + 7.0), 3.0, pal.ok);
        }
        if resp.clicked() {
            next = Some(*t);
        }
        x += w + 4.0;
    }
    next
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Cancels a task that is no longer wanted. Its result is dropped with the handle.
fn cancel<T: Send + 'static>(task: &mut Option<Task<T>>) {
    if let Some(t) = task.take() {
        t.abort();
    }
}

/// Force-removes a volume helper on the runtime. Without an engine the helper is only released.
fn remove_browser(engine: Option<&Engine>, vb: Arc<VolumeBrowser>) {
    if let Some(e) = engine {
        let docker = e.docker.clone();
        drop(e.runtime.spawn(async move { files::remove_helper(&docker, &vb.helper).await }));
    }
}

/// Logs tab: filter, stream and timestamp settings, pause, clear and save; then the lines.
#[allow(clippy::too_many_arguments)]
fn logs_tab(
    state: &mut LogsState,
    engine: &Engine,
    subject: &Subject,
    members: &[(String, String)],
    name: &str,
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    rect: Rect,
    pal: &Palette,
    toasts: &mut Toasts,
) {
    let key = match subject {
        Subject::Container(id) => format!("container:{id}"),
        Subject::Stack(stack) => format!("stack:{stack}:{}", members.iter().map(|m| m.0.as_str()).collect::<Vec<_>>().join(",")),
        _ => String::new(),
    };
    if state.session_key != key || state.session.is_none() {
        state.session = None;
        state.session_key = key;
        state.rows = Arc::default();
        state.rows_key = None;
        let repaint = ctx.clone();
        let on_change = move || repaint.request_repaint();
        state.session = match subject {
            Subject::Stack(_) => Some(LogSession::stack(engine, members, DEFAULT_TAIL, on_change)),
            Subject::Container(id) => Some(LogSession::container(engine, id, DEFAULT_TAIL, on_change)),
            _ => None,
        };
    }

    let split = kit::vsplit(rect, &[TOOLBAR_H, -1.0, LINE_H], 4.0);
    let mut clear = false;
    let mut save = false;
    kit::row_ui(ui, split[0], |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        widgets::search_box(ui, &mut state.query, "Filter", 200.0, pal);
        for (label, only) in [("All", None), ("stdout", Some(Stream::Stdout)), ("stderr", Some(Stream::Stderr))] {
            if ui.selectable_label(state.only == only, label).clicked() {
                state.only = only;
            }
        }
        ui.add_space(8.0);
        widgets::toggle(ui, &mut state.timestamps, pal);
        ui.label(RichText::new("Timestamps").font(theme::ui(13.0)).color(pal.text_dim));
        let pause_label = if state.paused { "Resume" } else { "Pause" };
        if widgets::button(ui, pause_label, pal, None).clicked() {
            state.paused = !state.paused;
        }
        if widgets::button(ui, "Clear", pal, None).clicked() {
            clear = true;
        }
        if widgets::button(ui, "Save as...", pal, None).clicked() {
            save = true;
        }
    });

    if clear {
        if let Some(s) = &state.session {
            lock(&s.buffer).clear();
        }
    }
    if save {
        if let Some(s) = &state.session {
            if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{name}.log")).add_filter("Log", &["log", "txt"]).save_file() {
                let buf = lock(&s.buffer);
                match logs::save(&buf, &path, &state.query, state.only, state.timestamps, !s.sources.is_empty()) {
                    Ok(n) => toasts.push((NoticeKind::Success, format!("Saved {n} lines to {}", path.display()))),
                    Err(e) => toasts.push((NoticeKind::Error, format!("Save failed: {e}"))),
                }
            }
        }
    }

    let rows = rows_of(state);
    let row_h = ui.fonts(|f| f.row_height(&theme::mono(12.0))) + 2.0;
    let paused = state.paused;
    let body = split[1];
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("docker-detail-logs")
            .stick_to_bottom(!paused)
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for row in &rows[range] {
                    let mut job = LayoutJob::default();
                    if let Some((idx, label)) = &row.source {
                        job.append(&format!("[{label}] "), 0.0, TextFormat::simple(theme::mono(12.0), kit::category_color(*idx)));
                    }
                    let color = match row.stream {
                        Stream::Stdout => pal.text,
                        Stream::Stderr => pal.warn,
                    };
                    job.append(&row.text, 0.0, TextFormat::simple(theme::mono(12.0), color));
                    job.wrap.max_width = f32::INFINITY;
                    ui.label(job);
                }
            });
    });

    let (total_len, dropped, error) = match &state.session {
        Some(s) => {
            let b = lock(&s.buffer);
            (b.len(), b.dropped, lock(&s.error).clone())
        }
        None => (0, 0, None),
    };
    let p = ui.painter();
    p.text(
        pos2(split[2].left(), split[2].center().y),
        Align2::LEFT_CENTER,
        format!("{total_len} lines ({dropped} dropped), {} shown", rows.len()),
        theme::ui(12.0),
        pal.text_faint,
    );
    if let Some(e) = error {
        p.text(pos2(split[2].right(), split[2].center().y), Align2::RIGHT_CENTER, e, theme::ui(12.0), pal.warn);
    }
}

/// The filtered lines of the session, rebuilt only when the buffer or the filter changed.
fn rows_of(state: &mut LogsState) -> Arc<Vec<Row>> {
    let Some(s) = &state.session else {
        return Arc::default();
    };
    let buf: MutexGuard<'_, LogBuffer> = lock(&s.buffer);
    let key = RowsKey {
        generation: s.generation.load(AtomicOrdering::Relaxed),
        len: buf.len(),
        total: buf.total,
        query: state.query.clone(),
        only: state.only,
        timestamps: state.timestamps,
    };
    if state.rows_key.as_ref() == Some(&key) {
        return state.rows.clone();
    }
    let sources = &s.sources;
    let rows: Vec<Row> = buf
        .iter()
        .filter(|line| logs::matches(line, &key.query, key.only))
        .map(|line| Row {
            stream: line.stream,
            source: line.source.as_ref().map(|name| (sources.iter().position(|x| x == name).unwrap_or(0), name.clone())),
            text: logs::render(line, key.timestamps, false),
        })
        .collect();
    drop(buf);
    state.rows = Arc::new(rows);
    state.rows_key = Some(key);
    state.rows.clone()
}

fn spawn_inspect(engine: &Engine, id: &str) -> Task<Result<String, String>> {
    let docker = engine.docker.clone();
    let id = id.to_string();
    Task::spawn(&engine.runtime, async move {
        let info = docker.inspect_container(id.as_str(), None).await.map_err(|e| err_text(&e))?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    })
}

/// Inspect tab: the container's inspect output as pretty JSON.
#[allow(clippy::too_many_arguments)]
fn inspect_tab(state: &mut InspectState, engine: &Engine, id: &str, ui: &mut egui::Ui, rect: Rect, pal: &Palette, toasts: &mut Toasts) {
    let mut start = false;
    if state.key != id {
        cancel(&mut state.task);
        state.key = id.to_string();
        state.text = None;
        state.error = None;
        start = true;
    }
    if let Some(res) = state.task.as_mut().and_then(|t| t.take()) {
        state.task = None;
        match res {
            Ok(text) => {
                state.text = Some(text);
                state.error = None;
            }
            Err(e) => {
                state.text = None;
                state.error = Some(e);
            }
        }
    }

    let split = kit::vsplit(rect, &[TOOLBAR_H, -1.0], 4.0);
    let mut refresh = false;
    let mut copy = false;
    kit::row_ui(ui, split[0], |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if widgets::button(ui, "Refresh", pal, Some(Icon::Refresh)).clicked() {
            refresh = true;
        }
        if widgets::button(ui, "Copy", pal, None).clicked() {
            copy = true;
        }
    });
    if copy {
        if let Some(text) = &state.text {
            ui.ctx().copy_text(text.clone());
            toasts.push((NoticeKind::Success, "Copied the inspect output".to_string()));
        }
    }
    if (start || refresh) && state.task.is_none() {
        state.task = Some(spawn_inspect(engine, id));
    }
    if state.task.is_some() {
        ui.ctx().request_repaint_after(POLL_EVERY);
    }

    let body = split[1];
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body), |ui| {
        if let Some(text) = &state.text {
            egui::ScrollArea::both().id_salt("docker-detail-inspect").auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(text.as_str()).font(theme::mono(12.0)).color(pal.text)));
            });
        } else if let Some(e) = &state.error {
            ui.painter().text(pos2(body.left() + 4.0, body.top() + 8.0), Align2::LEFT_TOP, e, theme::ui(12.5), pal.warn);
        } else {
            kit::empty_state(ui.painter(), body, pal, "Loading...");
        }
    });
}

/// Copies the stats history of the container from the worker when it changed.
fn stats_graphs(state: &mut StatsState, backend: Option<&Backend>, id: &str) -> Option<Arc<StatsGraphs>> {
    let b = backend?;
    let shared = lock(&b.shared);
    if state.id != id || state.seq != shared.stats_seq {
        state.graphs = shared.stats.get(id).map(|s| {
            Arc::new(StatsGraphs {
                cpu: s.cpu.clone(),
                mem: s.mem_pct.clone(),
                rx: s.net_rx.clone(),
                tx: s.net_tx.clone(),
                read: s.blk_read.clone(),
                write: s.blk_write.clone(),
            })
        });
        state.id = id.to_string();
        state.seq = shared.stats_seq;
    }
    state.graphs.clone()
}

/// Stats tab: key/value figures on the left, four history graphs on the right.
fn stats_tab(app: &App, ui: &egui::Ui, rect: Rect, fd: &FrameData<'_>, running: bool, sample: Option<StatsSample>, graphs: Option<Arc<StatsGraphs>>) {
    let pal = fd.pal;
    let p = ui.painter().clone();
    let (Some(s), true) = (sample, running) else {
        let text = if running { "Waiting for the first sample." } else { "Stats are shown for running containers." };
        kit::empty_state(&p, rect, &pal, text);
        return;
    };
    let left = Rect::from_min_size(rect.min, vec2(KV_W, rect.height()));
    let items = vec![
        ("CPU".to_string(), format!("{:.1} %", s.cpu_pct)),
        ("Memory".to_string(), format!("{} / {}", format_bytes(s.mem_used), format_bytes(s.mem_limit))),
        ("Memory %".to_string(), format!("{:.1} %", s.mem_pct)),
        ("Net rx".to_string(), format_rate(s.net_rx_bps)),
        ("Net tx".to_string(), format_rate(s.net_tx_bps)),
        ("Block read".to_string(), format_rate(s.blk_read_bps)),
        ("Block write".to_string(), format_rate(s.blk_write_bps)),
        ("PIDs".to_string(), s.pids.to_string()),
    ];
    kit::kv_list(&p, left, &pal, &items, 96.0, 20.0);

    let right = Rect::from_min_max(pos2(left.right() + 16.0, rect.top()), rect.max);
    let Some(g) = graphs else {
        kit::empty_state(&p, right, &pal, "Waiting for the first sample.");
        return;
    };
    let rows = kit::vsplit(right, &[-1.0, -1.0], 14.0);
    let top = kit::hsplit(rows[0], &[1.0, 1.0], 16.0);
    let bottom = kit::hsplit(rows[1], &[1.0, 1.0], 16.0);

    let cpu = [GraphLine { series: &g.cpu, color: fd.metric("cpu"), range: (0.0, 100.0) }];
    graph_cell(app, ui, top[0], fd, "CPU %", &format!("{:.1} %", s.cpu_pct), cpu.into());
    let mem = [GraphLine { series: &g.mem, color: fd.metric("memory"), range: (0.0, 100.0) }];
    graph_cell(app, ui, top[1], fd, "Memory %", &format!("{:.1} %", s.mem_pct), mem.into());

    let net_max = g.rx.max().max(g.tx.max()).max(1024.0);
    let net_color = fd.metric("network");
    let net = [
        GraphLine { series: &g.rx, color: net_color, range: (0.0, net_max) },
        GraphLine { series: &g.tx, color: theme::lighten(net_color, 0.35), range: (0.0, net_max) },
    ];
    graph_cell(app, ui, bottom[0], fd, "Network rx / tx", &format!("{} / {}", format_rate(s.net_rx_bps), format_rate(s.net_tx_bps)), net.into());

    let disk_max = g.read.max().max(g.write.max()).max(1024.0);
    let disk_color = fd.metric("disk");
    let disk = [
        GraphLine { series: &g.read, color: disk_color, range: (0.0, disk_max) },
        GraphLine { series: &g.write, color: theme::lighten(disk_color, 0.35), range: (0.0, disk_max) },
    ];
    graph_cell(app, ui, bottom[1], fd, "Block read / write", &format!("{} / {}", format_rate(s.blk_read_bps), format_rate(s.blk_write_bps)), disk.into());
}

/// One graph with a title and the current value above it.
fn graph_cell(app: &App, ui: &egui::Ui, rect: Rect, fd: &FrameData<'_>, title: &str, value: &str, lines: Vec<GraphLine<'_>>) {
    let p = ui.painter().clone();
    p.text(pos2(rect.left(), rect.top()), Align2::LEFT_TOP, title, theme::ui(12.0), fd.pal.text_dim);
    p.text(pos2(rect.right(), rect.top()), Align2::RIGHT_TOP, value, theme::mono(12.0), fd.pal.text);
    let area = Rect::from_min_max(pos2(rect.left(), rect.top() + 18.0), rect.max);
    let g: Graph<'_> = app.graph(lines, fd);
    widgets::draw_graph(&p, area, &g);
}

fn cmp_entry(a: &Entry, b: &Entry, col: usize) -> Ordering {
    match col {
        0 => cmp_str(&a.name, &b.name),
        1 => a.size.cmp(&b.size),
        2 => cmp_str(&a.mode, &b.mode),
        3 => cmp_str(&a.modified, &b.modified),
        _ => cmp_str(a.link_target.as_deref().unwrap_or(""), b.link_target.as_deref().unwrap_or("")),
    }
}

/// Target of file operations for the subject.
fn target_of(subject: &Subject, running: bool) -> Option<Target> {
    match subject {
        Subject::Container(id) => Some(Target::Container { id: id.clone(), running }),
        Subject::Volume(name) => Some(Target::Volume { name: name.clone() }),
        _ => None,
    }
}

/// Files tab: breadcrumb and actions, then the directory listing.
#[allow(clippy::too_many_arguments)]
fn files_tab(
    state: &mut FilesState,
    engine: &Engine,
    subject: &Subject,
    running: bool,
    read_only: bool,
    icons: &mut IconCache,
    ui: &mut egui::Ui,
    rect: Rect,
    pal: &Palette,
    toasts: &mut Toasts,
) {
    let key = match subject {
        Subject::Container(id) => format!("container:{id}"),
        Subject::Volume(name) => format!("volume:{name}"),
        _ => String::new(),
    };
    if state.key != key {
        state.close(Some(engine));
        state.key = key;
        state.path = "/".to_string();
        state.want_list = true;
    }
    state.reap(Some(engine));

    if let Some(res) = state.opening.as_mut().and_then(|t| t.take()) {
        state.opening = None;
        match res {
            Ok(vb) => state.volume = Some(vb),
            Err(e) => {
                state.error = Some(e);
                state.want_list = false;
            }
        }
    }
    if let Some(res) = state.task.as_mut().and_then(|t| t.take()) {
        state.task = None;
        match res {
            Ok(list) => {
                state.entries = list;
                state.error = None;
            }
            Err(e) => {
                state.entries.clear();
                state.error = Some(e);
            }
        }
    }
    if let Some(res) = state.transfer.as_mut().and_then(|t| t.take()) {
        state.transfer = None;
        state.want_list = true;
        match res {
            Ok(msg) => toasts.push((NoticeKind::Success, msg)),
            Err(e) => toasts.push((NoticeKind::Error, e)),
        }
    }

    if state.want_list && state.task.is_none() {
        let docker = engine.docker.clone();
        let path = state.path.clone();
        match subject {
            Subject::Volume(name) => {
                if let Some(vb) = state.volume.clone() {
                    state.task = Some(Task::spawn(&engine.runtime, async move { vb.list(&docker, &path).await }));
                    state.want_list = false;
                } else if state.opening.is_none() {
                    let name = name.clone();
                    state.opening = Some(Task::spawn(&engine.runtime, async move { VolumeBrowser::open(&docker, &name).await.map(Arc::new) }));
                }
            }
            Subject::Container(id) => {
                let target = Target::Container { id: id.clone(), running };
                state.task = Some(Task::spawn(&engine.runtime, async move { files::list_dir(&docker, &target, &path).await }));
                state.want_list = false;
            }
            _ => {}
        }
    }
    let transfer_busy = state.transfer.is_some();
    if state.task.is_some() || state.opening.is_some() || transfer_busy {
        ui.ctx().request_repaint_after(POLL_EVERY);
    }

    let split = kit::vsplit(rect, &[TOOLBAR_H, LINE_H, -1.0], 4.0);
    let right = Rect::from_min_max(pos2(split[0].right() - FILES_BUTTONS_W, split[0].top()), split[0].max);
    let left = Rect::from_min_max(split[0].min, pos2(right.left(), split[0].bottom()));
    let path = state.path.clone();
    let mut nav: Option<String> = None;
    let mut refresh = false;
    let mut download = false;
    let mut upload = false;
    let mut up = false;
    let has_selection = state.table.selected.is_some();
    let idle = !transfer_busy;

    kit::row_ui(ui, left, |ui| {
        ui.set_clip_rect(left);
        ui.spacing_mut().item_spacing.x = 2.0;
        if ui.link("/").clicked() {
            nav = Some("/".to_string());
        }
        let mut acc = "/".to_string();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            if acc != "/" {
                ui.label(RichText::new("/").font(theme::ui(13.0)).color(pal.text_faint));
            }
            acc = files::join(&acc, seg);
            if ui.link(seg).clicked() {
                nav = Some(acc.clone());
            }
        }
    });
    kit::row_ui(ui, right, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_enabled_ui(idle && !read_only, |ui| {
                if widgets::button(ui, "Upload...", pal, None).clicked() {
                    upload = true;
                }
            });
            ui.add_enabled_ui(idle && has_selection, |ui| {
                if widgets::button(ui, "Download...", pal, None).clicked() {
                    download = true;
                }
            });
            if widgets::button(ui, "Refresh", pal, Some(Icon::Refresh)).clicked() {
                refresh = true;
            }
            ui.add_enabled_ui(path != "/", |ui| {
                if widgets::button(ui, "Up", pal, None).clicked() {
                    up = true;
                }
            });
        });
    });
    if up {
        nav = Some(files::parent(&path));
    }
    if refresh {
        state.want_list = true;
    }

    let status = if let Some(e) = &state.error {
        Some(e.clone())
    } else if transfer_busy {
        Some("Working...".to_string())
    } else if state.task.is_some() || state.opening.is_some() {
        Some("Loading...".to_string())
    } else {
        None
    };
    if let Some(text) = status {
        let color = if state.error.is_some() { pal.warn } else { pal.text_dim };
        ui.painter().text(pos2(split[1].left() + 2.0, split[1].center().y), Align2::LEFT_CENTER, text, theme::ui(12.0), color);
    }

    let target = target_of(subject, running);
    if download {
        if let (Some(sel), Some(target)) = (state.table.selected.clone(), target.clone()) {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let docker = engine.docker.clone();
                state.transfer = Some(Task::spawn(&engine.runtime, async move {
                    files::download(&docker, &target, &sel, &dir).await.map(|p: PathBuf| format!("Downloaded to {}", p.display()))
                }));
            }
        }
    }
    if upload {
        if let Some(target) = target.clone() {
            if let Some(local) = rfd::FileDialog::new().pick_file() {
                let docker = engine.docker.clone();
                let dest = path.clone();
                let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                state.transfer = Some(Task::spawn(&engine.runtime, async move {
                    files::upload(&docker, &target, &local, &dest).await.map(|_| format!("Uploaded {name} to {dest}"))
                }));
            }
        }
    }

    let entries = &state.entries;
    let mut order: Vec<usize> = (0..entries.len()).collect();
    state.table.sort(entries, &mut order, cmp_entry);
    let key_of = |i: usize| entries[i].path.clone();
    let cell_of = |i: usize, col: usize| {
        let e = &entries[i];
        match col {
            0 => {
                let text = if e.is_dir { format!("{}/", e.name) } else { e.name.clone() };
                let c = Cell::new(text);
                if e.is_dir {
                    c.color(pal.accent)
                } else {
                    c
                }
            }
            1 => Cell::new(if e.is_dir { "-".to_string() } else { format_bytes(e.size) }).dim(pal),
            2 => Cell::new(e.mode.clone()).dim(pal),
            3 => Cell::new(e.modified.clone()).dim(pal),
            _ => Cell::new(e.link_target.as_deref().map(|t| format!("-> {t}")).unwrap_or_default()).dim(pal),
        }
    };
    if entries.is_empty() {
        let text = if state.task.is_some() || state.opening.is_some() { "Loading..." } else { "Empty directory" };
        kit::empty_state(ui.painter(), split[2], pal, text);
    } else {
        let activated = table::show(ui, split[2], "docker-detail-files", pal, &FILES_COLS, &order, &mut state.table, &key_of, &cell_of, icons);
        if let Some(i) = activated {
            let e = &entries[i];
            if e.is_dir {
                nav = Some(e.path.clone());
            }
        }
    }

    if let Some(next) = nav {
        if next != state.path {
            state.path = next;
            state.want_list = true;
            state.table.selected = None;
            state.error = None;
        }
    }
}
