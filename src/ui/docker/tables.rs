//! Engine object tables for the Docker page: containers, stacks, images, volumes and networks.
//!
//! All tables share [`draw_table`], which filters, sorts, draws the rows and keeps the
//! selection in step with the tab's `selected_*` field.

use super::{now_unix, DockerUi, Snap, Tab};
use crate::docker::model::{format_age, format_bytes, Container, ContainerState, EngineStatus};
use crate::process::format_rate;
use crate::ui::icons::IconCache;
use crate::ui::kit;
use crate::ui::table::{self, cmp_f, cmp_str, Cell, Col, TableState};
use crate::ui::theme::Palette;
use crate::ui::{App, FrameData};
use eframe::egui::{self, Color32, Rect};
use std::cmp::Ordering;
use std::collections::BTreeMap;

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

/// Draws the table of the current tab (not the engine tab).
pub fn show(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let d = &mut app.docker.ui;
    let icons = &mut app.icons;
    match d.tab {
        Tab::Containers => containers(d, icons, ui, rect, &pal),
        Tab::Stacks => stacks(d, icons, ui, rect, &pal),
        Tab::Images => images(d, icons, ui, rect, &pal),
        Tab::Volumes => volumes(d, icons, ui, rect, &pal),
        Tab::Networks => networks(d, icons, ui, rect, &pal),
        Tab::Engine => {}
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

/// Shared table drawing: filter, sort, empty states, selection sync.
#[allow(clippy::too_many_arguments)]
fn draw_table<T>(
    ui: &mut egui::Ui,
    rect: Rect,
    id: &str,
    pal: &Palette,
    cols: &[Col],
    state: &mut TableState,
    selected: &mut Option<String>,
    items: &[T],
    keep: impl Fn(&T) -> bool,
    cmp: impl Fn(&T, &T, usize) -> Ordering,
    key: impl Fn(&T) -> String,
    cell: impl Fn(&T, usize) -> Cell,
    icons: &mut IconCache,
    empty: &str,
) {
    if items.is_empty() {
        kit::empty_state(ui.painter(), rect, pal, empty);
        *selected = None;
        return;
    }
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| keep(&items[i])).collect();
    state.sort(items, &mut order, &cmp);
    if order.is_empty() {
        kit::empty_state(ui.painter(), rect, pal, "No matches for the filter");
        return;
    }
    let key_of = |i: usize| key(&items[i]);
    let cell_of = |i: usize, col: usize| cell(&items[i], col);
    let activated = table::show(ui, rect, id, pal, cols, &order, state, &key_of, &cell_of, icons);
    if let Some(i) = activated {
        state.selected = Some(key_of(i));
    }
    *selected = state.selected.clone();
}

fn containers(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let DockerUi { snap, filter, containers: state, selected_container, .. } = d;
    let snap: &Snap = snap;
    let now = now_unix();
    let f: &str = filter.as_str();
    let stat = |c: &Container| snap.stats_last.get(&c.id).copied();
    let cpu = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.cpu_pct as f64).unwrap_or(-1.0);
    let mem = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.mem_used as f64).unwrap_or(-1.0);
    let net = |c: &Container| stat(c).filter(|_| c.state.is_running()).map(|s| s.net_rx_bps + s.net_tx_bps).unwrap_or(-1.0);
    draw_table(
        ui,
        rect,
        "docker-containers",
        pal,
        &COLS_CONTAINERS,
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
                    let cell = Cell::new(with_busy(&c.name, busy));
                    if busy.is_some() {
                        cell.dim(pal)
                    } else {
                        cell
                    }
                }
                1 => Cell::new(c.state.label()).dot(state_color(c.state, pal)),
                2 => Cell::new(c.image.clone()),
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
                6 => Cell::new(ports_text(c)),
                7 => Cell::new(format_age(c.created, now)),
                _ => Cell::new(c.stack.clone().unwrap_or_else(|| "-".into())),
            }
        },
        icons,
        empty_text(snap, "No containers"),
    );
}

fn stacks(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let DockerUi { snap, filter, stacks: state, selected_stack, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    draw_table(
        ui,
        rect,
        "docker-stacks",
        pal,
        &COLS_STACKS,
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
    );
}

fn images(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let DockerUi { snap, filter, images: state, selected_image, .. } = d;
    let snap: &Snap = snap;
    let now = now_unix();
    let f: &str = filter.as_str();
    draw_table(
        ui,
        rect,
        "docker-images",
        pal,
        &COLS_IMAGES,
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
    );
}

fn volumes(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let DockerUi { snap, filter, volumes: state, selected_volume, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    draw_table(
        ui,
        rect,
        "docker-volumes",
        pal,
        &COLS_VOLUMES,
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
    );
}

fn networks(d: &mut DockerUi, icons: &mut IconCache, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
    let DockerUi { snap, filter, networks: state, selected_network, .. } = d;
    let snap: &Snap = snap;
    let f: &str = filter.as_str();
    draw_table(
        ui,
        rect,
        "docker-networks",
        pal,
        &COLS_NETWORKS,
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
    );
}
