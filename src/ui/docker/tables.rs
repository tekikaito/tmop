//! Engine object tables for the Docker page: containers, stacks, images, volumes and networks.
//!
//! All tables share [`draw_table`], which filters, sorts, draws the rows and keeps the
//! selection in step with the tab's `selected_*` field. Each tab's actions (the buttons of the
//! detail aside and the context menu of its rows) are built from the same [`Act`] lists, see
//! [`current_acts`]. A chosen [`Intent`] is carried out by [`act`] once the frame is drawn.

use super::dialogs::{self, PendingAction};
use super::wizards::Wizard;
use super::{now_unix, DockerUi, Snap, Tab};
use crate::docker::backend::{ContainerAction, NoticeKind, PruneKind};
use crate::docker::model::{format_age, format_bytes, Container, ContainerState, EngineStatus, Image, Network, Stack, Volume};
use crate::docker::stack::{local_compose_file, open_in_editor, StackAction};
use crate::process::format_rate;
use crate::ui::icons::IconCache;
use crate::ui::kit;
use crate::ui::table::{self, cmp_f, cmp_str, Cell, Col, TableState};
use crate::ui::theme::Palette;
use crate::ui::{App, FrameData};
use eframe::egui::{self, Color32, Rect};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Columns in the order they are kept when the table is too narrow for all of them.
const PRIORITY_CONTAINERS: [usize; 9] = [0, 1, 3, 4, 2, 6, 5, 7, 8];
const PRIORITY_STACKS: [usize; 5] = [0, 2, 1, 3, 4];
const PRIORITY_IMAGES: [usize; 5] = [0, 2, 3, 4, 1];
const PRIORITY_VOLUMES: [usize; 6] = [0, 3, 4, 1, 5, 2];
const PRIORITY_NETWORKS: [usize; 6] = [0, 4, 1, 3, 2, 5];

const COLS_CONTAINERS: [Col; 9] = [
    Col::text("Name", 160.0),
    Col::text("State", 90.0),
    Col::text("Image", 170.0),
    Col::num("CPU %", 60.0),
    Col::num("Memory", 80.0),
    Col::num("Net rx / tx", 150.0),
    Col::text("Ports", 160.0),
    Col::text("Age", 100.0),
    Col::text("Stack", 100.0),
];
const COLS_STACKS: [Col; 5] = [Col::text("Name", 200.0), Col::num("Services", 80.0), Col::num("Running", 80.0), Col::text("Compose file", 300.0), Col::text("Working dir", 300.0)];
const COLS_IMAGES: [Col; 5] = [Col::text("Repository:tag", 260.0), Col::text("Id", 110.0), Col::num("Size", 90.0), Col::num("Containers", 90.0), Col::text("Age", 120.0)];
const COLS_VOLUMES: [Col; 6] = [
    Col::text("Name", 220.0),
    Col::text("Driver", 90.0),
    Col::text("Mountpoint", 300.0),
    Col::num("Size", 90.0),
    Col::num("Used by", 80.0),
    Col::text("Stack", 120.0),
];
const COLS_NETWORKS: [Col; 6] = [
    Col::text("Name", 180.0),
    Col::text("Driver", 90.0),
    Col::text("Scope", 80.0),
    Col::text("Subnets", 200.0),
    Col::num("Containers", 90.0),
    Col::text("Stack", 120.0),
];

/// What a toolbar button or menu item asks for. Carried out by [`act`].
#[derive(Clone, Debug)]
pub(super) enum Intent {
    /// Runs the action, after a confirmation when it needs one.
    Run(PendingAction),
    /// Opens a wizard.
    Wizard(Wizard),
    /// Opens a compose file in the editor.
    OpenFile(PathBuf),
}

/// One toolbar button or menu line. `reason` explains why it cannot run; `None` means enabled.
pub(super) enum Act {
    Sep,
    Item { label: &'static str, reason: Option<&'static str>, intent: Option<Intent> },
}

fn item(label: &'static str, reason: Option<&'static str>, intent: Option<Intent>) -> Act {
    Act::Item { label, reason, intent }
}

/// Draws the table of the current tab (not the engine tab).
pub fn show(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let d = &mut app.docker.ui;
    let icons = &mut app.icons;
    let intent = match d.tab {
        Tab::Containers => containers(d, icons, ui, rect, &pal),
        Tab::Stacks => stacks(d, icons, ui, rect, &pal),
        Tab::Images => images(d, icons, ui, rect, &pal),
        Tab::Volumes => volumes(d, icons, ui, rect, &pal),
        Tab::Networks => networks(d, icons, ui, rect, &pal),
        Tab::Engine => None,
    };
    if let Some(intent) = intent {
        act(app, intent);
    }
}

/// Carries out a chosen toolbar or menu intent.
pub(super) fn act(app: &mut App, intent: Intent) {
    match intent {
        Intent::Run(action) => dialogs::request(app, action),
        Intent::Wizard(wizard) => app.docker.ui.dialogs.wizards.open = Some(wizard),
        Intent::OpenFile(path) => {
            if let Err(e) = open_in_editor(&path) {
                app.docker.ui.toast(NoticeKind::Error, e);
            }
        }
    }
}

/// Text for an empty table: the reason when the engine is not connected, else `none`.
fn empty_text(snap: &Snap, none: &'static str) -> &'static str {
    match snap.status {
        EngineStatus::Connected => none,
        EngineStatus::Unreachable => "Engine not reachable\nStart Docker Desktop or pick another host.",
        EngineStatus::Connecting | EngineStatus::Unknown => "Connecting...",
    }
}

/// Container port mappings that are published on the host, e.g. `8080->80/tcp`.
pub fn ports_text(c: &Container) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in &c.ports {
        if let Some(public) = p.public_port {
            let s = format!("{}->{}/{}", public, p.private_port, p.proto);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out.join(", ")
}

fn busy_label<'a>(busy: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| busy.get(*k)).map(|s| s.as_str())
}

fn is_busy(snap: &Snap, keys: &[&str]) -> bool {
    busy_label(&snap.busy, keys).is_some()
}

fn with_busy(name: &str, busy: Option<&str>) -> String {
    match busy {
        Some(label) => format!("{name} ({label}...)"),
        None => name.to_string(),
    }
}

fn state_color(s: ContainerState, pal: &Palette) -> Color32 {
    match s {
        ContainerState::Running => pal.ok,
        ContainerState::Paused => pal.warn,
        ContainerState::Exited | ContainerState::Dead => pal.text_faint,
        _ => pal.text_dim,
    }
}

/// Why an action cannot run: read-only host, busy object, or no row chosen.
fn blocked(snap: &Snap, selected: bool, busy: bool) -> Option<&'static str> {
    read_only_reason(snap).or(busy.then_some("This object is busy")).or((!selected).then_some("Select a row first"))
}

/// Why an action that changes the host cannot run.
fn read_only_reason(snap: &Snap) -> Option<&'static str> {
    snap.read_only.then_some("This host is read-only")
}

/// Whether `label` names an action that destroys something (shown as a red outline button).
pub(super) fn is_danger(label: &str) -> bool {
    ["Remove", "Down", "Prune"].iter().any(|v| label.starts_with(v))
}

/// Draws the entries of a context menu. Returns the intent of the clicked entry.
fn menu_ui(ui: &mut egui::Ui, acts: Vec<Act>) -> Option<Intent> {
    let mut out = None;
    for a in acts {
        match a {
            Act::Sep => {
                ui.separator();
            }
            Act::Item { label, reason, intent } => {
                if ui.add_enabled_ui(reason.is_none(), |ui| kit::menu_item(ui, label, None, is_danger(label))).inner {
                    out = intent;
                }
            }
        }
    }
    out
}

/// The columns (indices into `cols`) that fit in `width`, kept in `priority` order and shown in
/// table order. The first two always stay.
fn visible_cols(cols: &[Col], priority: &[usize], width: f32) -> Vec<usize> {
    let mut used = 0.0;
    let mut keep: Vec<usize> = Vec::new();
    for &c in priority {
        let w = cols[c].width;
        if keep.len() < 2 || used + w <= width {
            used += w;
            keep.push(c);
        }
    }
    keep.sort_unstable();
    keep
}

/// Shared table drawing: filter, sort, empty states, selection sync, context menus. Columns that
/// do not fit the width are left out (`priority` says which go last).
#[allow(clippy::too_many_arguments)]
fn draw_table<T>(
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    pal: &Palette,
    cols: &[Col],
    priority: &[usize],
    state: &mut TableState,
    selected: &mut Option<String>,
    items: &[T],
    keep: impl Fn(&T) -> bool,
    cmp: impl Fn(&T, &T, usize) -> Ordering,
    key: impl Fn(&T) -> String,
    cell: impl Fn(&T, usize) -> Cell,
    icons: &mut IconCache,
    empty: &str,
    acts_of: &dyn Fn(&T) -> Vec<Act>,
) -> Option<Intent> {
    if items.is_empty() {
        kit::empty_state(ui.painter(), rect, pal, empty);
        *selected = None;
        return None;
    }
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| keep(&items[i])).collect();
    state.sort(items, &mut order, &cmp);
    if order.is_empty() {
        kit::empty_state(ui.painter(), rect, pal, "No matches for the filter");
        return None;
    }
    let key_of = |i: usize| key(&items[i]);
    let vis = visible_cols(cols, priority, rect.width());
    let vcols: Vec<Col> = vis.iter().map(|&c| cols[c]).collect();
    let cell_of = |i: usize, col: usize| cell(&items[i], vis[col.min(vis.len() - 1)]);
    let chosen: RefCell<Option<Intent>> = RefCell::new(None);
    let menu = |i: usize, ui: &mut egui::Ui| {
        if let Some(intent) = menu_ui(ui, acts_of(&items[i])) {
            *chosen.borrow_mut() = Some(intent);
        }
    };
    // The table counts the columns that are shown; the state keeps counting all of them.
    let all_sort = state.sort;
    let shown_sort = vis.iter().position(|&c| c == all_sort).unwrap_or(usize::MAX);
    state.sort = shown_sort;
    let activated = table::show_with_menu(ui, rect, id, pal, &vcols, &order, state, &key_of, &cell_of, icons, Some(&menu));
    state.sort = if state.sort == shown_sort { all_sort } else { vis.get(state.sort).copied().unwrap_or(all_sort) };
    if let Some(i) = activated {
        state.selected = Some(key_of(i));
    }
    *selected = state.selected.clone();
    chosen.into_inner()
}

/// The actions of the current tab for its selected row (or without one): the buttons of the aside.
pub(super) fn current_acts(d: &DockerUi) -> Vec<Act> {
    let snap = &d.snap;
    match d.tab {
        Tab::Containers => container_acts(snap, d.selected_container.as_ref().and_then(|id| snap.containers.iter().find(|c| &c.id == id))),
        Tab::Stacks => stack_acts(snap, d.selected_stack.as_ref().and_then(|name| snap.stacks.iter().find(|s| &s.name == name))),
        Tab::Images => image_acts(snap, d.selected_image.as_ref().and_then(|id| snap.images.iter().find(|i| &i.id == id))),
        Tab::Volumes => volume_acts(snap, d.selected_volume.as_ref().and_then(|name| snap.volumes.iter().find(|v| &v.name == name))),
        Tab::Networks => network_acts(snap, d.selected_network.as_ref().and_then(|id| snap.networks.iter().find(|n| &n.id == id))),
        Tab::Engine => Vec::new(),
    }
}

fn container_acts(snap: &Snap, c: Option<&Container>) -> Vec<Act> {
    let busy = c.is_some_and(|c| is_busy(snap, &[c.id.as_str(), c.name.as_str()]));
    let base = blocked(snap, c.is_some(), busy);
    let running = c.is_some_and(|c| c.state.is_running());
    let paused = c.is_some_and(|c| c.state == ContainerState::Paused);
    let on = |action: ContainerAction| c.map(|c| Intent::Run(PendingAction::Container { id: c.id.clone(), name: c.name.clone(), action }));
    let not_running = (!running && !paused).then_some("Not running");
    vec![
        item("Start", base.or(running.then_some("Already running")), on(ContainerAction::Start)),
        item("Stop", base.or(not_running), on(ContainerAction::Stop)),
        item("Restart", base.or(not_running), on(ContainerAction::Restart)),
        if paused {
            item("Unpause", base, on(ContainerAction::Unpause))
        } else {
            item("Pause", base.or((!running).then_some("Not running")), on(ContainerAction::Pause))
        },
        item("Remove...", base, on(ContainerAction::Remove { force: running, volumes: false })),
        Act::Sep,
        item(
            "Run image...",
            read_only_reason(snap),
            Some(Intent::Wizard(Wizard::RunImage { image: c.map(|c| c.image.clone()).unwrap_or_default() })),
        ),
        item("Prune stopped...", read_only_reason(snap), Some(Intent::Run(PendingAction::Prune(PruneKind::Containers)))),
    ]
}

fn containers(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) -> Option<Intent> {
    let with_terminal = d.detail.terminals.container_ids();
    let DockerUi { snap, filter, containers: state, selected_container, .. } = d;
    let snap: &Snap = snap;
    let now = now_unix();
    let f: &str = filter.as_str();
    let stat = |c: &Container| snap.stats_last.get(&c.id).copied();
    let cpu = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.cpu_pct as f64).unwrap_or(-1.0);
    let mem = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.mem_used as f64).unwrap_or(-1.0);
    let net = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.net_rx_bps + s.net_tx_bps).unwrap_or(-1.0);
    let from_menu = draw_table(
        ui,
        rect,
        "docker-containers",
        pal,
        &COLS_CONTAINERS,
        &PRIORITY_CONTAINERS,
        state,
        selected_container,
        &snap.containers,
        |c| {
            let ports = ports_text(c);
            table::matches(f, &[c.name.as_str(), c.image.as_str(), c.state.label(), c.stack.as_deref().unwrap_or(""), ports.as_str()])
        },
        |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => a.state.label().cmp(b.state.label()),
            2 => cmp_str(&a.image, &b.image),
            3 => cmp_f(cpu(a), cpu(b)),
            4 => cmp_f(mem(a), mem(b)),
            5 => cmp_f(net(a), net(b)),
            6 => cmp_str(&ports_text(a), &ports_text(b)),
            7 => a.created.cmp(&b.created),
            _ => cmp_str(a.stack.as_deref().unwrap_or(""), b.stack.as_deref().unwrap_or("")),
        },
        |c| c.id.clone(),
        |c, col| {
            let busy = busy_label(&snap.busy, &[c.id.as_str(), c.name.as_str()]);
            let dash = || Cell::new("-").dim(pal);
            match col {
                0 => {
                    let mut cell = Cell::new(with_busy(&c.name, busy)).mono().color(pal.text);
                    if with_terminal.contains(&c.id) {
                        cell = cell.dot(pal.accent);
                    }
                    if busy.is_some() {
                        cell.dim(pal)
                    } else {
                        cell
                    }
                }
                1 => Cell::new(c.state.label()).dot(state_color(c.state, pal)).color(state_color(c.state, pal)),
                2 => Cell::new(c.image.clone()).mono(),
                3 => match stat(c).filter(|_| c.state.is_running()) {
                    Some(s) => Cell::new(format!("{:.1}", s.cpu_pct)).heat(s.cpu_pct / 100.0),
                    None => dash(),
                },
                4 => match stat(c).filter(|_| c.state.is_running()) {
                    Some(s) => Cell::new(format_bytes(s.mem_used)),
                    None => dash(),
                },
                5 => match stat(c).filter(|_| c.state.is_running()) {
                    Some(s) => Cell::new(format!("{} / {}", format_rate(s.net_rx_bps), format_rate(s.net_tx_bps))),
                    None => dash(),
                },
                6 => Cell::new(ports_text(c)).mono(),
                7 => Cell::new(format_age(c.created, now)).dim(pal),
                _ => Cell::new(c.stack.clone().unwrap_or_else(|| "-".into())).dim(pal),
            }
        },
        icons,
        empty_text(snap, "No containers"),
        &|c: &Container| container_acts(snap, Some(c)),
    );
    from_menu
}

fn stack_acts(snap: &Snap, s: Option<&Stack>) -> Vec<Act> {
    let busy = s.is_some_and(|s| s.containers.iter().any(|id| snap.busy.contains_key(id)));
    let base = blocked(snap, s.is_some(), busy);
    let run = |action: StackAction| s.map(|s| Intent::Run(PendingAction::Stack { name: s.name.clone(), action }));
    let running = s.map_or(0, |s| s.running);
    let local = s.and_then(local_compose_file);
    vec![
        item("Start", base, run(StackAction::Start)),
        item("Stop", base.or((running == 0).then_some("Nothing is running")), run(StackAction::Stop)),
        item("Restart", base, run(StackAction::Restart)),
        item("Down...", base, run(StackAction::Down { volumes: false })),
        Act::Sep,
        item(
            "Up from compose file...",
            read_only_reason(snap),
            Some(Intent::Wizard(Wizard::ComposeUp { stack: s.map(|s| s.name.clone()) })),
        ),
        item(
            "Open compose file",
            if s.is_none() { Some("Select a row first") } else { local.is_none().then_some("No compose file on this machine") },
            local.map(Intent::OpenFile),
        ),
    ]
}

fn stacks(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) -> Option<Intent> {
    let DockerUi { snap, filter, stacks: state, selected_stack, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    let from_menu = draw_table(
        ui,
        rect,
        "docker-stacks",
        pal,
        &COLS_STACKS,
        &PRIORITY_STACKS,
        state,
        selected_stack,
        &snap.stacks,
        |s| table::matches(f, &[s.name.as_str(), s.compose_file.as_deref().unwrap_or(""), s.working_dir.as_deref().unwrap_or("")]),
        |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => a.services.len().cmp(&b.services.len()),
            2 => a.running.cmp(&b.running),
            3 => cmp_str(a.compose_file.as_deref().unwrap_or(""), b.compose_file.as_deref().unwrap_or("")),
            _ => cmp_str(a.working_dir.as_deref().unwrap_or(""), b.working_dir.as_deref().unwrap_or("")),
        },
        |s| s.name.clone(),
        |s, col| match col {
            0 => Cell::new(s.name.clone()),
            1 => Cell::new(s.services.len().to_string()),
            2 => Cell::new(format!("{}/{}", s.running, s.total)),
            3 => Cell::new(s.compose_file.clone().unwrap_or_else(|| "-".into())).dim(pal),
            _ => Cell::new(s.working_dir.clone().unwrap_or_else(|| "-".into())).dim(pal),
        },
        icons,
        empty_text(snap, "No Compose stacks"),
        &|s: &Stack| stack_acts(snap, Some(s)),
    );
    from_menu
}

fn image_acts(snap: &Snap, i: Option<&Image>) -> Vec<Act> {
    let busy = i.is_some_and(|i| is_busy(snap, &[i.id.as_str()]));
    let base = blocked(snap, i.is_some(), busy);
    let name = i.map(|i| i.display_name());
    let on = |intent: fn(String, String) -> PendingAction| {
        i.map(|i| Intent::Run(intent(i.id.clone(), i.display_name())))
    };
    vec![
        item("Pull image...", read_only_reason(snap), Some(Intent::Wizard(Wizard::PullImage))),
        item(
            "Run...",
            base,
            name.as_ref().map(|n| Intent::Wizard(Wizard::RunImage { image: n.clone() })),
        ),
        item("Remove...", base, on(|id, name| PendingAction::Image { id, name })),
        Act::Sep,
        item("Prune dangling...", read_only_reason(snap), Some(Intent::Run(PendingAction::Prune(PruneKind::Images)))),
        item("Prune unused...", read_only_reason(snap), Some(Intent::Run(PendingAction::Prune(PruneKind::ImagesAll)))),
    ]
}

fn images(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) -> Option<Intent> {
    let DockerUi { snap, filter, images: state, selected_image, .. } = d;
    let snap: &Snap = snap;
    let now = now_unix();
    let f: &str = filter.as_str();
    let from_menu = draw_table(
        ui,
        rect,
        "docker-images",
        pal,
        &COLS_IMAGES,
        &PRIORITY_IMAGES,
        state,
        selected_image,
        &snap.images,
        |i| table::matches(f, &[i.display_name().as_str(), i.short_id.as_str()]),
        |a, b, col| match col {
            0 => cmp_str(&a.display_name(), &b.display_name()),
            1 => cmp_str(&a.short_id, &b.short_id),
            2 => a.size.cmp(&b.size),
            3 => a.containers.cmp(&b.containers),
            _ => a.created.cmp(&b.created),
        },
        |i| i.id.clone(),
        |i, col| {
            let busy = busy_label(&snap.busy, &[i.id.as_str()]);
            match col {
                0 => {
                    let name = i.display_name();
                    if name == "<none>" || i.dangling {
                        Cell::new(with_busy("<none>", busy)).dim(pal)
                    } else {
                        let cell = Cell::new(with_busy(&name, busy));
                        if busy.is_some() {
                            cell.dim(pal)
                        } else {
                            cell
                        }
                    }
                }
                1 => Cell::new(i.short_id.clone()).dim(pal),
                2 => Cell::new(format_bytes(i.size)),
                3 => Cell::new(if i.containers < 0 { "-".to_string() } else { i.containers.to_string() }),
                _ => Cell::new(format_age(i.created, now)),
            }
        },
        icons,
        empty_text(snap, "No images"),
        &|i: &Image| image_acts(snap, Some(i)),
    );
    from_menu
}

fn volume_acts(snap: &Snap, v: Option<&Volume>) -> Vec<Act> {
    let busy = v.is_some_and(|v| is_busy(snap, &[v.name.as_str()]));
    let base = blocked(snap, v.is_some(), busy);
    let name = v.map(|v| v.name.clone());
    vec![
        item("Remove...", base, name.clone().map(|name| Intent::Run(PendingAction::Volume { name }))),
        item(
            "Export...",
            v.is_none().then_some("Select a row first"),
            name.map(|volume| Intent::Wizard(Wizard::ExportVolume { volume })),
        ),
        item("Import...", read_only_reason(snap), Some(Intent::Wizard(Wizard::ImportVolume { volume: v.map(|v| v.name.clone()) }))),
        item("Prune...", read_only_reason(snap), Some(Intent::Run(PendingAction::Prune(PruneKind::Volumes)))),
    ]
}

fn volumes(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) -> Option<Intent> {
    let DockerUi { snap, filter, volumes: state, selected_volume, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    let from_menu = draw_table(
        ui,
        rect,
        "docker-volumes",
        pal,
        &COLS_VOLUMES,
        &PRIORITY_VOLUMES,
        state,
        selected_volume,
        &snap.volumes,
        |v| table::matches(f, &[v.name.as_str(), v.driver.as_str(), v.mountpoint.as_str(), v.stack.as_deref().unwrap_or("")]),
        |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.driver, &b.driver),
            2 => cmp_str(&a.mountpoint, &b.mountpoint),
            3 => a.size.cmp(&b.size),
            4 => a.ref_count.cmp(&b.ref_count),
            _ => cmp_str(a.stack.as_deref().unwrap_or(""), b.stack.as_deref().unwrap_or("")),
        },
        |v| v.name.clone(),
        |v, col| {
            let busy = busy_label(&snap.busy, &[v.name.as_str()]);
            match col {
                0 => {
                    let cell = Cell::new(with_busy(&v.name, busy));
                    if busy.is_some() {
                        cell.dim(pal)
                    } else {
                        cell
                    }
                }
                1 => Cell::new(v.driver.clone()),
                2 => Cell::new(v.mountpoint.clone()).dim(pal),
                3 => Cell::new(v.size.map(format_bytes).unwrap_or_else(|| "-".into())),
                4 => Cell::new(v.ref_count.map(|n| n.to_string()).unwrap_or_else(|| "-".into())),
                _ => Cell::new(v.stack.clone().unwrap_or_else(|| "-".into())),
            }
        },
        icons,
        empty_text(snap, "No volumes"),
        &|v: &Volume| volume_acts(snap, Some(v)),
    );
    from_menu
}

fn network_acts(snap: &Snap, n: Option<&Network>) -> Vec<Act> {
    let busy = n.is_some_and(|n| is_busy(snap, &[n.id.as_str(), n.name.as_str()]));
    let base = blocked(snap, n.is_some(), busy);
    let builtin = n.is_some_and(|n| n.is_builtin()).then_some("Built-in networks cannot be removed");
    vec![
        item("Remove...", base.or(builtin), n.map(|n| Intent::Run(PendingAction::Network { id: n.id.clone(), name: n.name.clone() }))),
        item("Prune...", read_only_reason(snap), Some(Intent::Run(PendingAction::Prune(PruneKind::Networks)))),
    ]
}

fn networks(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) -> Option<Intent> {
    let DockerUi { snap, filter, networks: state, selected_network, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    let from_menu = draw_table(
        ui,
        rect,
        "docker-networks",
        pal,
        &COLS_NETWORKS,
        &PRIORITY_NETWORKS,
        state,
        selected_network,
        &snap.networks,
        |n| table::matches(f, &[n.name.as_str(), n.driver.as_str(), n.scope.as_str(), n.stack.as_deref().unwrap_or("")]),
        |a, b, col| match col {
            0 => cmp_str(&a.name, &b.name),
            1 => cmp_str(&a.driver, &b.driver),
            2 => cmp_str(&a.scope, &b.scope),
            3 => cmp_str(&a.subnets.join(", "), &b.subnets.join(", ")),
            4 => a.containers.len().cmp(&b.containers.len()),
            _ => cmp_str(a.stack.as_deref().unwrap_or(""), b.stack.as_deref().unwrap_or("")),
        },
        |n| n.id.clone(),
        |n, col| {
            let busy = busy_label(&snap.busy, &[n.id.as_str(), n.name.as_str()]);
            match col {
                0 => {
                    let cell = Cell::new(with_busy(&n.name, busy));
                    if busy.is_some() {
                        cell.dim(pal)
                    } else {
                        cell
                    }
                }
                1 => Cell::new(n.driver.clone()),
                2 => Cell::new(n.scope.clone()).dim(pal),
                3 => Cell::new(if n.subnets.is_empty() { "-".to_string() } else { n.subnets.join(", ") }),
                4 => Cell::new(n.containers.len().to_string()),
                _ => Cell::new(n.stack.clone().unwrap_or_else(|| "-".into())),
            }
        },
        icons,
        empty_text(snap, "No networks"),
        &|n: &Network| network_acts(snap, Some(n)),
    );
    from_menu
}
