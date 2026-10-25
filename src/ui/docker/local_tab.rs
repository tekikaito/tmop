//! Local Docker tab of the Docker page on Linux and macOS, in the place of the Windows Engine
//! tab: which endpoint is in use and what kind of install it is, the engine's facts, the
//! sockets found on this machine and what to do when none answers. Read-only: it starts and
//! installs nothing.

use super::Snap;
use crate::docker::context::{Context, Endpoint};
use crate::docker::local::{self, Candidate, Kind, Reach};
use crate::docker::model::{format_bytes, EngineStatus};
use crate::platform::{Os, OS};
use crate::ui::kit;
use crate::ui::theme::{self, Palette};
use crate::ui::widgets::text_fit;
use crate::ui::{App, FrameData};
use eframe::egui::{self, pos2, Align2, Color32, Rect};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the sockets are probed again while the tab is shown.
const RESCAN: Duration = Duration::from_secs(5);
const ROW_H: f32 = 22.0;
/// Height of the sockets card; the guidance card takes the rest of the right column.
const SOCKETS_H: f32 = 230.0;

/// A socket that exists on this machine and what connecting to it found.
pub type Found = (Candidate, Reach);

/// Probe results, refreshed on a worker thread so the UI never waits on a socket.
#[derive(Default)]
pub struct LocalTabState {
    found: Arc<Mutex<Option<Vec<Found>>>>,
    running: Arc<AtomicBool>,
    last: Option<Instant>,
}

impl LocalTabState {
    /// Starts a probe when the last one is older than [`RESCAN`] (and none is running).
    fn rescan(&mut self, ctx: &egui::Context) {
        if self.last.is_some_and(|t| t.elapsed() < RESCAN) || self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.last = Some(Instant::now());
        let (found, running, ctx) = (self.found.clone(), self.running.clone(), ctx.clone());
        std::thread::spawn(move || {
            let list: Vec<Found> = local::this_machine().into_iter().map(|c| (c.clone(), local::probe(&c.path))).filter(|(_, r)| *r != Reach::Absent).collect();
            *found.lock().unwrap_or_else(|e| e.into_inner()) = Some(list);
            running.store(false, Ordering::Release);
            ctx.request_repaint();
        });
    }

    fn found(&self) -> Option<Vec<Found>> {
        self.found.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// What the guidance card says: a sentence or two and, where there is one, a command to copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guidance {
    pub text: String,
    pub command: Option<String>,
}

impl Guidance {
    fn say(text: impl Into<String>, command: Option<&str>) -> Self {
        Guidance { text: text.into(), command: command.map(str::to_string) }
    }
}

/// The advice for this state. `endpoint`: the active context's; `found`: the sockets present.
pub fn guidance(os: Os, endpoint: Option<&Endpoint>, status: EngineStatus, error: Option<&str>, found: &[Found]) -> Guidance {
    let local_path = match endpoint {
        Some(Endpoint::Unix(p)) => Some(p.as_str()),
        _ => None,
    };
    if status == EngineStatus::Connected {
        return Guidance::say("The engine answers. Containers, stacks, images, volumes and networks are in the other tabs.", None);
    }
    if matches!(status, EngineStatus::Connecting | EngineStatus::Unknown) {
        return Guidance::say("Connecting\u{2026}", None);
    }
    if endpoint.is_some() && local_path.is_none() {
        return Guidance::say("The host in use is remote; check that its engine runs and that it can be reached from here. The sockets below are this machine's.", None);
    }
    let is_active = |c: &Candidate| Some(c.path.to_string_lossy().as_ref()) == local_path;
    let active_reach = found.iter().find(|(c, _)| is_active(c)).map(|(_, r)| r);
    let active_denied = active_reach == Some(&Reach::Denied) || error.is_some_and(|e| e.starts_with("permission denied"));
    if let (true, Some(path)) = (active_denied, local_path) {
        return denied_advice(os, path);
    }
    if let Some((c, _)) = found.iter().find(|(c, r)| *r == Reach::Open && !is_active(c)) {
        let path = c.path.to_string_lossy();
        return Guidance::say(
            format!("{} answers on {path}. Pick a host that uses it in the Host list, or start TMOP with this variable set:", c.kind.label()),
            Some(&format!("DOCKER_HOST=unix://{path}")),
        );
    }
    if let Some((c, _)) = found.iter().find(|(_, r)| *r == Reach::Denied) {
        return denied_advice(os, &c.path.to_string_lossy());
    }
    // A socket file without a listener: the engine is installed but stopped.
    let stopped = match (active_reach, local_path) {
        (Some(_), Some(path)) => Some(local::kind_of(path)),
        _ => found.first().map(|(c, _)| c.kind),
    };
    if let Some(kind) = stopped {
        return Guidance::say(format!("{} is installed but not running. Start it:", kind.label()), Some(kind.start_hint()));
    }
    match os {
        Os::MacOs => Guidance::say(
            "No Docker engine was found. Docker on macOS runs in a VM: install and start Docker Desktop, OrbStack or Colima. With Homebrew, Colima is:",
            Some("brew install colima docker && colima start"),
        ),
        _ => Guidance::say(
            "No Docker engine was found. Install Docker Engine (docs.docker.com/engine/install) or Podman with your package manager, then start it:",
            Some("sudo systemctl enable --now docker"),
        ),
    }
}

/// Advice for a socket this user may not open.
fn denied_advice(os: Os, path: &str) -> Guidance {
    if os == Os::Linux && local::kind_of(path) == Kind::Docker {
        return Guidance::say(
            format!("Your user may not open {path}. Add it to the docker group, then log out and back in (or run `newgrp docker`). Members of that group have root-equivalent access to this machine."),
            Some(local::DOCKER_GROUP_FIX),
        );
    }
    Guidance::say(format!("Your user may not open {path}. Check the socket's owner and permissions."), None)
}

/// Draws the Local Docker tab in `rect`.
pub fn show(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    app.docker.ui.local_tab.rescan(ui.ctx());
    let found = app.docker.ui.local_tab.found();
    let active = app.active_name();
    let context = app.docker.contexts.iter().find(|c| c.name == active).cloned();
    let snap = &app.docker.ui.snap;
    let p = ui.painter().clone();
    let cols = kit::hsplit(rect, &[1.0, 1.0], 14.0);

    let left = kit::section_card(&p, cols[0], &pal, "Local Docker", "");
    let mut items = endpoint_rows(&active, context.as_ref(), snap, &pal);
    if let Some(info) = &snap.info {
        let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        items.extend([
            ("Version".to_string(), format!("{} (API {})", or_dash(&info.server_version), or_dash(&info.api_version)), true, None),
            ("OS".to_string(), or_dash(&info.os), false, None),
            ("Platform".to_string(), format!("{}/{}", or_dash(&info.os_type), or_dash(&info.arch)), true, None),
            ("Kernel".to_string(), or_dash(&info.kernel), true, None),
            ("CPUs".to_string(), info.cpus.to_string(), true, None),
            ("Memory".to_string(), format_bytes(info.mem_total), true, None),
            ("Containers".to_string(), format!("{} running of {}", info.containers_running, info.containers), true, None),
            ("Images".to_string(), info.images.to_string(), true, None),
            ("Storage driver".to_string(), or_dash(&info.driver), true, None),
        ]);
    }
    let bottom = kit::kv_grid(&p, left, &pal, &items, 130.0, ROW_H);
    if snap.info.is_none() && bottom + 40.0 < left.bottom() {
        let rest = Rect::from_min_max(pos2(left.left(), bottom), left.max);
        kit::empty_state(&p, rest, &pal, "Engine facts appear once it answers");
    }

    let right = kit::vsplit(cols[1], &[SOCKETS_H, -1.0], 14.0);
    let sockets = kit::section_card(&p, right[0], &pal, "Sockets on this machine", "");
    match &found {
        None => kit::empty_state(&p, sockets, &pal, "Looking\u{2026}"),
        Some(list) if list.is_empty() => kit::empty_state(&p, sockets, &pal, "None of the usual Docker sockets exists"),
        Some(list) => socket_rows(&p, sockets, &pal, list),
    }

    let help = kit::section_card(&p, right[1], &pal, "What to do", "");
    let g = guidance(OS, context.as_ref().map(|c| &c.endpoint), snap.status, snap.error.as_deref(), found.as_deref().unwrap_or(&[]));
    let galley = p.layout(g.text, theme::ui(13.0), pal.text, help.width());
    let text_h = galley.size().y;
    p.with_clip_rect(help).galley(help.min, galley, pal.text);
    if let Some(cmd) = g.command {
        let top = help.top() + text_h + 14.0;
        if top + 64.0 <= help.bottom() + 1.0 {
            kit::code_box(ui, Rect::from_min_size(pos2(help.left(), top), egui::vec2(help.width(), 64.0)), &pal, "Command", &cmd);
        }
    }
}

/// Context, endpoint, kind and connection state of the host in use.
fn endpoint_rows(active: &str, context: Option<&Context>, snap: &Snap, pal: &Palette) -> Vec<(String, String, bool, Option<Color32>)> {
    let st = theme::status(pal);
    let (state, color) = match snap.status {
        EngineStatus::Connected => ("Connected".to_string(), st.ok),
        EngineStatus::Unreachable => (snap.error.clone().unwrap_or_else(|| "Not reachable".into()), st.warn),
        EngineStatus::Connecting | EngineStatus::Unknown => ("Connecting\u{2026}".to_string(), pal.text_faint),
    };
    let (endpoint, kind) = match context.map(|c| &c.endpoint) {
        Some(Endpoint::Unix(p)) => (p.clone(), local::kind_of(p).label().to_string()),
        Some(ep) => (ep.to_host_string(), "Remote host".to_string()),
        None => ("-".to_string(), "-".to_string()),
    };
    let source = if active == "default" {
        if std::env::var("DOCKER_HOST").is_ok_and(|h| !h.trim().is_empty()) {
            "default (DOCKER_HOST)".to_string()
        } else {
            "default (first socket found)".to_string()
        }
    } else {
        active.to_string()
    };
    vec![
        ("Context".to_string(), source, false, None),
        ("Endpoint".to_string(), endpoint, true, None),
        ("Kind".to_string(), kind, false, None),
        ("Status".to_string(), state, false, Some(color)),
    ]
}

/// One row per socket present: the install, its path and what connecting found.
fn socket_rows(p: &egui::Painter, rect: Rect, pal: &Palette, list: &[Found]) {
    let st = theme::status(pal);
    let kind_w = 150.0;
    let state_w = 140.0;
    let mut y = rect.top();
    for (c, reach) in list {
        if y + ROW_H > rect.bottom() + 1.0 {
            break;
        }
        let mid = y + ROW_H / 2.0;
        let (state, color) = match reach {
            Reach::Open => ("answers".to_string(), st.ok),
            Reach::Denied => ("permission denied".to_string(), st.danger),
            Reach::Refused => ("not running".to_string(), st.warn),
            Reach::Absent => ("absent".to_string(), pal.text_faint),
            Reach::Failed(e) => (e.clone(), st.warn),
        };
        text_fit(p, pos2(rect.left(), mid), Align2::LEFT_CENTER, c.kind.label(), theme::ui(13.0), pal.text, kind_w - 8.0);
        let path_w = (rect.width() - kind_w - state_w).max(40.0);
        text_fit(p, pos2(rect.left() + kind_w, mid), Align2::LEFT_CENTER, &c.path.to_string_lossy(), theme::mono(12.0), pal.text_table(), path_w - 8.0);
        text_fit(p, pos2(rect.right(), mid), Align2::RIGHT_CENTER, &state, theme::ui(12.0), color, state_w);
        y += ROW_H;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn found(kind: Kind, path: &str, reach: Reach) -> Found {
        (Candidate { kind, path: PathBuf::from(path) }, reach)
    }

    fn unix(p: &str) -> Endpoint {
        Endpoint::Unix(p.into())
    }

    #[test]
    fn connected_needs_no_advice() {
        let g = guidance(Os::Linux, Some(&unix("/var/run/docker.sock")), EngineStatus::Connected, None, &[]);
        assert_eq!(g.command, None);
    }

    #[test]
    fn permission_denied_points_at_the_docker_group_on_linux() {
        let sock = "/var/run/docker.sock";
        let list = [found(Kind::Docker, sock, Reach::Denied)];
        let g = guidance(Os::Linux, Some(&unix(sock)), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some(local::DOCKER_GROUP_FIX));
        assert!(g.text.contains("docker group"), "{}", g.text);
        // The error text alone is enough (the probe may not have run yet).
        let g = guidance(Os::Linux, Some(&unix(sock)), EngineStatus::Unreachable, Some("permission denied on /var/run/docker.sock: x"), &[]);
        assert_eq!(g.command.as_deref(), Some(local::DOCKER_GROUP_FIX));
        let g = guidance(Os::MacOs, Some(&unix(sock)), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command, None);
    }

    #[test]
    fn another_socket_that_answers_is_named() {
        let list = [found(Kind::Docker, "/var/run/docker.sock", Reach::Refused), found(Kind::Colima, "/Users/m/.colima/default/docker.sock", Reach::Open)];
        let g = guidance(Os::MacOs, Some(&unix("/var/run/docker.sock")), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some("DOCKER_HOST=unix:///Users/m/.colima/default/docker.sock"));
        assert!(g.text.starts_with("Colima answers"));
    }

    #[test]
    fn a_stopped_engine_gets_its_start_command() {
        let list = [found(Kind::Rootless, "/run/user/1000/docker.sock", Reach::Refused)];
        let g = guidance(Os::Linux, Some(&unix("/run/user/1000/docker.sock")), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some("systemctl --user start docker"));
        let list = [found(Kind::OrbStack, "/Users/m/.orbstack/run/docker.sock", Reach::Refused)];
        let g = guidance(Os::MacOs, Some(&unix("/var/run/docker.sock")), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some("orb start"), "the active socket is absent: the stopped one found decides");
        let list = [found(Kind::Docker, "/var/run/docker.sock", Reach::Refused), found(Kind::Colima, "/Users/m/.colima/default/docker.sock", Reach::Refused)];
        let g = guidance(Os::MacOs, Some(&unix("/Users/m/.colima/default/docker.sock")), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some("colima start"), "the endpoint in use wins");
    }

    #[test]
    fn a_denied_socket_that_is_not_in_use_is_explained() {
        let list = [found(Kind::Docker, "/var/run/docker.sock", Reach::Denied)];
        let g = guidance(Os::Linux, Some(&unix("/run/user/1000/docker.sock")), EngineStatus::Unreachable, None, &list);
        assert_eq!(g.command.as_deref(), Some(local::DOCKER_GROUP_FIX));
    }

    #[test]
    fn nothing_found_explains_how_to_get_an_engine() {
        let g = guidance(Os::Linux, Some(&unix("/var/run/docker.sock")), EngineStatus::Unreachable, Some("no Docker socket at /var/run/docker.sock"), &[]);
        assert!(g.text.starts_with("No Docker engine was found"));
        assert_eq!(g.command.as_deref(), Some("sudo systemctl enable --now docker"));
        let g = guidance(Os::MacOs, None, EngineStatus::Unreachable, None, &[]);
        assert!(g.text.contains("Docker Desktop, OrbStack or Colima"));
    }

    #[test]
    fn remote_hosts_are_not_diagnosed_locally() {
        let ssh = Endpoint::Ssh { host: "nas".into(), user: None, port: None };
        let g = guidance(Os::Linux, Some(&ssh), EngineStatus::Unreachable, Some("timed out"), &[]);
        assert!(g.text.contains("remote"));
        assert_eq!(g.command, None);
    }
}
