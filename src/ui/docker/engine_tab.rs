//! Engine tab of the Docker page: engine facts, recent engine events and the TMOP engine placeholder.

use super::Snap;
use crate::docker::model::{format_bytes, EngineStatus};
use crate::ui::kit::{self, GAP};
use crate::ui::theme;
use crate::ui::widgets::text_fit;
use crate::ui::{App, FrameData};
use eframe::egui::{self, pos2, Align, Align2, Layout, Rect, RichText};
use std::time::Instant;

const EVENT_ROW_H: f32 = 18.0;
const TMOP_ENGINE_TEXT: &str = "Setting up TMOP's own engine will be available in a later version.";

/// Draws the engine tab in `rect`.
pub fn show(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let bloom = fd.bloom;
    let snap = &app.docker.ui.snap;
    let p = ui.painter().clone();
    let cols = kit::hsplit(rect, &[1.0, 1.0], GAP);

    // Engine facts, with the engine's warnings underneath.
    let left = kit::titled_card(&p, cols[0], &pal, bloom, "Engine", "");
    match &snap.info {
        Some(info) => {
            let or_dash = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
            let items = vec![
                ("Name".to_string(), or_dash(&info.name)),
                ("Server version".to_string(), or_dash(&info.server_version)),
                ("API version".to_string(), or_dash(&info.api_version)),
                ("OS".to_string(), or_dash(&info.os)),
                ("Architecture".to_string(), or_dash(&info.arch)),
                ("Kernel".to_string(), or_dash(&info.kernel)),
                ("CPUs".to_string(), info.cpus.to_string()),
                ("Memory".to_string(), format_bytes(info.mem_total)),
                ("Storage driver".to_string(), or_dash(&info.driver)),
                ("Docker root".to_string(), or_dash(&info.docker_root)),
                ("Cgroup version".to_string(), or_dash(&info.cgroup_version)),
                (
                    "Containers".to_string(),
                    format!("{} running, {} paused, {} stopped", info.containers_running, info.containers_paused, info.containers_stopped),
                ),
                ("Images".to_string(), info.images.to_string()),
                ("Swarm".to_string(), or_dash(&info.swarm)),
            ];
            let bottom = kit::kv_list(&p, left, &pal, &items, 130.0, 20.0);
            let mut y = bottom + 8.0;
            for w in &info.warnings {
                if y + 16.0 > left.bottom() {
                    break;
                }
                text_fit(&p, pos2(left.left(), y), Align2::LEFT_TOP, w, theme::ui(12.0), pal.warn, left.width());
                y += 16.0;
            }
        }
        None => kit::empty_state(&p, left, &pal, engine_status_text(snap)),
    }

    // Recent events and the TMOP engine placeholder on the right.
    let right = kit::vsplit(cols[1], &[-1.0, 112.0], GAP);
    let events = kit::titled_card(&p, right[0], &pal, bloom, "Recent events", "");
    if snap.events.is_empty() {
        kit::empty_state(&p, events, &pal, "No events yet");
    }
    let time_w = 90.0;
    let kind_w = 90.0;
    let action_w = 110.0;
    let mut y = events.top();
    for e in &snap.events {
        if y + EVENT_ROW_H > events.bottom() {
            break;
        }
        let mid = y + EVENT_ROW_H / 2.0;
        let mut x = events.left();
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &ago(e.at), theme::ui(12.0), pal.text_faint, time_w - 8.0);
        x += time_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.kind, theme::ui(12.0), pal.text_dim, kind_w - 8.0);
        x += kind_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.action, theme::ui(12.0), pal.text, action_w - 8.0);
        x += action_w;
        text_fit(&p, pos2(x, mid), Align2::LEFT_CENTER, &e.actor, theme::ui(12.0), pal.text_dim, events.right() - x);
        y += EVENT_ROW_H;
    }

    let tm = kit::titled_card(&p, right[1], &pal, bloom, "TMOP engine", "");
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tm).layout(Layout::top_down(Align::LEFT)), |ui| {
        ui.label(RichText::new(TMOP_ENGINE_TEXT).font(theme::ui(12.5)).color(pal.text_dim));
    });
}

/// Empty-state text for the engine card.
fn engine_status_text(snap: &Snap) -> &'static str {
    match snap.status {
        EngineStatus::Connected => "No engine information",
        EngineStatus::Unreachable => "Engine not reachable",
        EngineStatus::Connecting | EngineStatus::Unknown => "Connecting...",
    }
}

/// "12 s ago" style age of an event.
fn ago(at: Instant) -> String {
    let s = at.elapsed().as_secs();
    if s < 60 {
        format!("{s} s ago")
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
}
