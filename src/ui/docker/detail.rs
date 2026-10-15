//! Detail area under the Docker tables. The first step only shows the selection; later
//! steps add logs, inspect, stats and files for the selected object.

use crate::ui::kit;
use crate::ui::theme;
use crate::ui::{App, FrameData};
use eframe::egui::{self, pos2, Align2, Rect, Stroke};

/// State of the detail area (tab and scroll positions, added by later steps).
#[derive(Debug, Default)]
pub struct DetailState {}

/// Draws the detail area in `rect`: a top rule, then the selected container or a hint.
pub fn detail_area(app: &mut App, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>) {
    let pal = fd.pal;
    let p = ui.painter().clone();
    p.line_segment([pos2(rect.left(), rect.top()), pos2(rect.right(), rect.top())], Stroke::new(1.0_f32, pal.panel_stroke));
    let snap = &app.docker.ui.snap;
    let selected = app.docker.ui.selected_container.as_deref().and_then(|id| snap.containers.iter().find(|c| c.id == id));
    match selected {
        Some(c) => {
            p.text(pos2(rect.left() + 4.0, rect.top() + 12.0), Align2::LEFT_TOP, &c.name, theme::ui(15.0), pal.text);
            let body = Rect::from_min_max(pos2(rect.left(), rect.top() + 36.0), rect.max);
            kit::empty_state(&p, body, &pal, "Details follow in a later step.");
        }
        None => kit::empty_state(&p, rect, &pal, "Select a container for logs, inspect, stats and files."),
    }
}
