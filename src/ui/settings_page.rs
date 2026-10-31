//! Settings page: a section list at the left and one column of section cards, each a stack of
//! rows (label at the left, control at the right) separated by hairlines.

use super::kit::{file_dialog, FileDialog};
use super::theme::{self, MetricColors, Palette, Role, CORE_ROLES, METRICS, MIN_CONTRAST, ROLES};
use super::widgets::{self, Button, ButtonSize, Icon};
use super::{App, FrameData, Page};
use crate::fonts;
use crate::palette::{self, PaletteCatalog, PaletteFile, SYSTEM};
use crate::platform::Caps;
use crate::settings::{self, Settings, UpdateSpeed};
use crate::skin;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Palette edits are written to disk this long after the last change.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);

/// Width of the section list, and the sections it lists (in page order).
const NAV_W: f32 = 180.0;
const SECTIONS: [&str; 8] = ["Appearance", "Graphs", "General", "Window management", "Other options", TASK_MANAGER, "Skins and files", "About"];
/// The section only Windows has (see [`shown_sections`]).
const TASK_MANAGER: &str = "Task Manager";

/// [`SECTIONS`] this OS shows. A hidden section's settings keep their values in the file.
fn shown_sections(caps: &Caps) -> Vec<&'static str> {
    SECTIONS.into_iter().filter(|s| *s != TASK_MANAGER || caps.taskmgr_replace).collect()
}

/// The accent swatches after "Palette default": (name, `appearance.accent`).
const ACCENTS: [(&str, &str); 4] = [("Teal", "#5EEAD4"), ("Indigo", "#8B9CFF"), ("Pink", "#FFB4C8"), ("Amber", "#F5B86B")];

/// Palette controls and editor state (Settings → Appearance).
#[derive(Default)]
pub struct PaletteUi {
    /// The custom palette open in the editor, and its colours as its file had them when the
    /// editor opened ("Revert to file on disk").
    editing: Option<(String, Palette)>,
    /// Show all 22 roles, not only the core ones.
    advanced: bool,
    /// The name being typed for Rename.
    rename: Option<String>,
    confirm_delete: bool,
    /// When the unsaved edits are written.
    save_due: Option<Instant>,
    /// An open import (`None`) or export (`Some(palette id)`) dialog.
    dialog: Option<(Option<String>, Receiver<Option<PathBuf>>)>,
    /// The last duplicate/rename/delete/import/export problem, shown under the buttons.
    message: Option<String>,
    /// HWND of the main window, the owner of the import/export dialogs (0 for none).
    owner: isize,
}

impl PaletteUi {
    pub fn new(owner: isize) -> Self {
        Self { owner, ..Default::default() }
    }

    /// After a rescan of the palette folder: keeps the editor open only if its palette is
    /// still a custom one, and drops a pending rename or delete. Save edits before rescanning.
    fn rescanned(&mut self, pals: &PaletteCatalog) {
        if self.editing.as_ref().is_some_and(|(id, _)| pals.get(id).is_none_or(|p| p.is_built_in())) {
            self.editing = None;
        }
        self.rename = None;
        self.confirm_delete = false;
    }

    fn open_editor(&mut self, p: &PaletteFile) {
        let on_disk = match &p.source {
            palette::PaletteSource::File(path) => PaletteFile::load(path).map(|f| f.palette).unwrap_or(p.palette),
            palette::PaletteSource::BuiltIn => p.palette,
        };
        self.editing = Some((p.id.clone(), on_disk));
    }

    /// Writes pending edits when they are due (or at once with `now`). Returns an error line.
    fn flush(&mut self, pals: &PaletteCatalog, now: bool) -> Option<String> {
        let due = self.save_due?;
        if !now && Instant::now() < due {
            return None;
        }
        self.save_due = None;
        let p = pals.get(&self.editing.as_ref()?.0)?;
        p.save().err().map(|e| format!("Could not save palette {}: {e}", p.name))
    }

    /// The theme changed (Theme combo, or the header's light/dark button): saves the edits, closes
    /// the editor and drops a pending rename or delete, which belonged to the old palette.
    pub fn note_theme_change(&mut self, pals: &PaletteCatalog) {
        self.message = self.close_editor(pals);
        self.rename = None;
        self.confirm_delete = false;
    }

    /// Saves pending edits and closes the editor.
    fn close_editor(&mut self, pals: &PaletteCatalog) -> Option<String> {
        let err = self.flush(pals, true);
        self.editing = None;
        err
    }
}

/// Installed font families for the Display font picker, listed on a worker thread the first
/// time the Settings page is shown (enumerating every installed font takes a while).
#[derive(Default)]
pub struct FontList {
    families: Option<Vec<String>>,
    pending: Option<Receiver<Vec<String>>>,
}

impl FontList {
    /// The installed families, or `None` while they are being listed (the first call starts that).
    fn get(&mut self, ctx: &egui::Context) -> Option<&[String]> {
        if self.families.is_none() {
            match &self.pending {
                None => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(fonts::installed());
                        ctx.request_repaint();
                    });
                    self.pending = Some(rx);
                }
                Some(rx) => match rx.try_recv() {
                    Ok(list) => self.families = Some(list),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => self.families = Some(Vec::new()),
                },
            }
        }
        self.families.as_deref()
    }
}

/// The Display font picker: Inter (the default) and Michroma first, then the installed `families` (`None` while they are
/// being listed). `missing`: the saved font isn't installed and the default is used instead.
/// Returns whether settings changed.
fn display_font_row(ui: &mut egui::Ui, pal: &Palette, a: &mut settings::Appearance, families: Option<&[String]>, missing: bool) -> bool {
    let mut changed = false;
    let hint = "Headings, names and large values: page titles, device and process names, and the Classic popout's title. Other text stays Inter.";
    row_hint(ui, pal, "Display font", hint, |ui| {
        egui::ComboBox::from_id_salt("display-font").width(200.0).selected_text(fonts::picker_label(&a.display_font, missing)).show_ui(ui, |ui| {
            let built_in = fonts::is_built_in(&a.display_font);
            if ui.selectable_label(built_in, fonts::BUILT_IN_LABEL).clicked() && !built_in {
                a.display_font = fonts::BUILT_IN.into();
                changed = true;
            }
            let classic = fonts::is_classic(&a.display_font);
            if ui.selectable_label(classic, fonts::CLASSIC_LABEL).clicked() && !classic {
                a.display_font = fonts::CLASSIC.into();
                changed = true;
            }
            match families {
                Some(list) => {
                    // Fonts load by name ignoring case, so a saved "arial" is the listed "Arial".
                    let current = a.display_font.to_lowercase();
                    for f in list {
                        let selected = f.to_lowercase() == current;
                        if ui.selectable_label(selected, f).clicked() && !selected {
                            a.display_font = f.clone();
                            changed = true;
                        }
                    }
                }
                None => {
                    ui.label(RichText::new("Listing installed fonts\u{2026}").font(theme::ui(12.0)).color(pal.text_faint));
                }
            }
        });
    });
    changed
}

/// One colour role: label, colour button, hex value and a contrast warning.
fn color_row(ui: &mut egui::Ui, pal: &Palette, p: &mut Palette, r: Role) -> bool {
    let warning = p.worst_contrast(r.key).filter(|c| *c < MIN_CONTRAST);
    let mut changed = false;
    row(ui, pal, r.label, |ui| {
        let c = p.role_mut(r.key).expect("palette role");
        if let Some(w) = warning {
            ui.label(RichText::new(format!("\u{26a0} {w:.1}:1")).font(theme::ui(12.0)).color(pal.warn))
                .on_hover_text(format!("Text contrast is below {MIN_CONTRAST}:1 (WCAG AA). The palette is still saved."));
        }
        ui.label(RichText::new(theme::to_skin(*c).to_string()).font(theme::mono(12.0)).color(pal.text_dim));
        changed = egui::color_picker::color_edit_button_srgba(ui, c, egui::color_picker::Alpha::Opaque).changed();
    });
    changed
}

/// One row per metric (Settings → Graphs): its colour and a reset to the built-in one. While
/// Color Keyed Graphs is off the overrides aren't used, so the rows are shown dimmed (still
/// editable). Returns whether settings changed.
fn metric_color_rows(ui: &mut egui::Ui, pal: &Palette, g: &mut settings::Graphs) -> bool {
    let mut changed = false;
    let custom = MetricColors::from_map(&g.metric_colors);
    ui.scope(|ui| {
        if !g.color_keyed {
            ui.multiply_opacity(0.45);
        }
        for (key, label) in METRICS {
            let mut c = custom.get(key);
            row(ui, pal, label, |ui| {
                let reset = ui.add_enabled_ui(g.metric_colors.contains_key(key), |ui| Button::new("Reset").size(ButtonSize::Small).icon(Icon::Refresh).show(ui, pal).on_hover_text("Back to the built-in colour").clicked()).inner;
                if reset {
                    g.metric_colors.remove(key);
                    changed = true;
                }
                ui.label(RichText::new(theme::to_skin(c).to_string()).font(theme::mono(12.0)).color(pal.text_dim));
                if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                    g.metric_colors.insert(key.into(), theme::to_skin(c).to_string());
                    changed = true;
                }
            });
        }
        let text = if g.color_keyed {
            "Colours that identify each metric in graphs, sparklines, meters and the popout."
        } else {
            "Color Keyed Graphs is off: graphs and sparklines use a single blue and these colours aren't used."
        };
        hint(ui, pal, text);
    });
    changed
}

/// A small outline button of the palette row.
fn small_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
    Button::new(label).size(ButtonSize::Small).show(ui, pal)
}

/// Theme, palette and the buttons next to it, and the palette editor. `current` is the id of
/// the palette in use. Returns whether settings changed.
fn palette_controls(ui: &mut egui::Ui, pal: &Palette, s: &mut Settings, pals: &mut PaletteCatalog, pui: &mut PaletteUi, current: &str) -> bool {
    let mut changed = false;
    // System / Dark / Light are the three theme ids; a palette picked below shows none of them.
    let theme_ids = [SYSTEM, "dark", "light"];
    let active = theme_ids.iter().position(|id| *id == s.appearance.theme).unwrap_or(usize::MAX);
    row(ui, pal, "Theme", |ui| {
        if let Some(i) = widgets::segmented(ui, &["System", "Dark", "Light"], active, pal, false) {
            s.appearance.theme = theme_ids[i].into();
            changed = true;
        }
    });
    if changed {
        pui.note_theme_change(pals);
    }
    let Some(cur) = pals.get(current).cloned() else { return changed };
    let custom = !cur.is_built_in();
    let editing = pui.editing.as_ref().is_some_and(|(id, _)| id == current);
    let mut picked = false;
    row(ui, pal, "Palette", |ui| {
        // Added right to left: Export is the rightmost, the picker the leftmost.
        ui.add_enabled_ui(pui.dialog.is_none(), |ui| {
            if small_button(ui, pal, "Export").on_hover_text(format!("Save {} as a .{} file", cur.name, palette::EXTENSION)).clicked() {
                let file_name = format!("{}.{}", cur.id, palette::EXTENSION);
                let d = FileDialog { save: true, title: "Export palette".into(), filter: ("TMOP palette".into(), palette::EXTENSION.into()), file_name, owner: pui.owner };
                pui.dialog = Some((Some(cur.id.clone()), file_dialog(d)));
            }
            if small_button(ui, pal, "Import").clicked() {
                let d = FileDialog { save: false, title: "Import palette".into(), filter: ("TMOP palette".into(), palette::EXTENSION.into()), file_name: String::new(), owner: pui.owner };
                pui.dialog = Some((None, file_dialog(d)));
            }
        });
        ui.add_enabled_ui(custom, |ui| {
            if small_button(ui, pal, if editing { "Done" } else { "Edit" }).clicked() {
                if editing {
                    pui.message = pui.close_editor(pals);
                } else {
                    pui.open_editor(&cur);
                }
            }
        })
        .response
        .on_disabled_hover_text("Built-in palettes are read-only; duplicate one to edit it.");
        if small_button(ui, pal, "Duplicate").on_hover_text(format!("Make an editable copy of {}", cur.name)).clicked() {
            pui.message = pui.close_editor(pals);
            match pals.duplicate(current) {
                Ok(id) => {
                    s.appearance.theme = id.clone();
                    changed = true;
                    if let Some(p) = pals.get(&id) {
                        pui.open_editor(p);
                    }
                }
                Err(e) => pui.message = Some(format!("Duplicate failed: {e}")),
            }
        }
        let selected = if s.appearance.theme == SYSTEM {
            format!("{} (system)", cur.name)
        } else {
            match pals.get(&s.appearance.theme) {
                Some(p) => p.name.clone(),
                None => format!("{} (not installed)", s.appearance.theme),
            }
        };
        egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(selected).show_ui(ui, |ui| {
            for p in &pals.palettes {
                picked |= ui.selectable_value(&mut s.appearance.theme, p.id.clone(), &p.name).changed();
            }
        });
    });
    if picked {
        changed = true;
        pui.note_theme_change(pals);
    }
    if custom && pals.get(current).is_some() {
        let mut rename_done = None;
        row(ui, pal, "This palette", |ui| {
            if small_button(ui, pal, "Delete").clicked() {
                pui.confirm_delete = true;
                pui.rename = None;
            }
            if small_button(ui, pal, "Rename").clicked() {
                pui.rename = Some(cur.name.clone());
                pui.confirm_delete = false;
            }
        });
        if let Some(name) = &mut pui.rename {
            row(ui, pal, "New name", |ui| {
                if small_button(ui, pal, "Cancel").clicked() {
                    rename_done = Some(false);
                }
                if small_button(ui, pal, "Rename").clicked() {
                    rename_done = Some(true);
                }
                let resp = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    rename_done = Some(true);
                }
            });
        }
        if let Some(ok) = rename_done {
            let name = pui.rename.take().unwrap_or_default();
            if ok && !name.trim().is_empty() {
                if let Err(e) = pals.rename(current, &name) {
                    pui.message = Some(format!("Rename failed: {e}"));
                }
            }
        }
        if pui.confirm_delete {
            let mut answer = None;
            row(ui, pal, &format!("Delete {}?", cur.name), |ui| {
                if small_button(ui, pal, "Cancel").clicked() {
                    answer = Some(false);
                }
                if Button::danger("Delete").size(ButtonSize::Small).show(ui, pal).clicked() {
                    answer = Some(true);
                }
            });
            if let Some(yes) = answer {
                pui.confirm_delete = false;
                if yes {
                    // Save pending edits first: if the delete fails, the palette stays and keeps them.
                    pui.message = pui.flush(pals, true);
                    match pals.delete(current) {
                        Ok(()) => {
                            pui.editing = None;
                            s.appearance.theme = SYSTEM.into();
                            changed = true;
                        }
                        Err(e) => pui.message = Some(format!("Delete failed: {e}")),
                    }
                }
            }
        }
    }
    if let Some(m) = &pui.message {
        ui.label(RichText::new(m).font(theme::ui(12.0)).color(pal.warn));
    }
    if editing {
        palette_editor(ui, pal, pals, pui, current);
    }
    changed
}

/// The editor for custom palette `id`: core roles, optionally all roles, revert.
fn palette_editor(ui: &mut egui::Ui, pal: &Palette, pals: &mut PaletteCatalog, pui: &mut PaletteUi, id: &str) {
    let Some(p) = pals.get_mut(id) else { return };
    let mut edited = false;
    ui.add_space(6.0);
    ui.label(RichText::new(format!("Editing {}", p.name)).font(theme::bold(13.0)).color(pal.text));
    edited |= toggle_row(ui, pal, &mut p.palette.dark, "Dark palette");
    for r in CORE_ROLES {
        if color_row(ui, pal, &mut p.palette, r) {
            p.palette = p.palette.derived();
            edited = true;
        }
    }
    hint(ui, pal, "Changing a core colour recomputes the other roles from the core ones. Changes apply at once and are saved automatically.");
    if !pui.advanced {
        // Warnings on roles the core view doesn't show (derived text or surfaces).
        let hidden: Vec<String> = p
            .palette
            .contrast_warnings()
            .into_iter()
            .filter(|(k, _)| !CORE_ROLES.iter().any(|r| r.key == *k))
            .filter_map(|(k, c)| ROLES.iter().find(|r| r.key == k).map(|r| format!("{} {c:.1}:1", r.label)))
            .collect();
        if !hidden.is_empty() {
            ui.label(RichText::new(format!("\u{26a0} Text contrast below {MIN_CONTRAST}:1 on {}. Turn on Advanced to adjust these roles.", hidden.join(", "))).font(theme::ui(12.0)).color(pal.warn));
        }
    }
    toggle_row(ui, pal, &mut pui.advanced, "Advanced: all 22 roles");
    if pui.advanced {
        for r in ROLES {
            edited |= color_row(ui, pal, &mut p.palette, r);
        }
    }
    let mut revert = false;
    row(ui, pal, "", |ui| {
        revert = Button::new("Revert to file on disk").size(ButtonSize::Small).icon(Icon::Refresh).show(ui, pal).on_hover_text("Undo every change made since the editor was opened").clicked();
    });
    if revert {
        if let Some((_, original)) = pui.editing {
            p.palette = original;
            edited = true;
        }
    }
    if edited {
        pui.save_due = Some(Instant::now() + AUTOSAVE_DELAY);
    }
}

/// The accent swatches: "Palette default" (drawn with `base`, the palette's own accent) and four
/// fixed colours. Returns whether `appearance.accent` changed.
fn accent_row(ui: &mut egui::Ui, pal: &Palette, a: &mut settings::Appearance, base: Color32) -> bool {
    let mut changed = false;
    row(ui, pal, "Accent colour", |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        // Right to left: the last swatch first.
        let swatches = std::iter::once(("Palette default", "", base)).chain(ACCENTS.iter().map(|(n, hex)| (*n, *hex, skin::Color::parse(hex).map(theme::from_skin).unwrap_or(base))));
        for (name, hex, color) in swatches.collect::<Vec<_>>().into_iter().rev() {
            let (r, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
            ui.painter().rect_filled(r, 6.0, color);
            let on = a.accent.trim().eq_ignore_ascii_case(hex);
            if on {
                ui.painter().rect_stroke(r.shrink(1.0), 5.0, Stroke::new(2.0_f32, pal.text));
            } else if resp.hovered() {
                ui.painter().rect_stroke(r.shrink(1.0), 5.0, Stroke::new(2.0_f32, pal.raise(0.4)));
            }
            if resp.on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                a.accent = hex.into();
                changed = true;
            }
        }
    });
    changed
}

// ---------------------------------------------------------------------------
// Rows and section cards
// ---------------------------------------------------------------------------

/// Where the section list and the cards meet: the card tops of this frame and a pending jump.
#[derive(Default)]
struct Sections {
    tops: Vec<f32>,
    /// Section to scroll to (set by a click in the list).
    target: Option<usize>,
}

/// A section card: its heading (11 px, uppercase, tracked, faint; none when `title` is empty)
/// and rows. Records its top for the section list and scrolls to itself when asked to.
fn section(ui: &mut egui::Ui, pal: &Palette, sections: &mut Sections, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    let index = sections.tops.len();
    let frame = egui::Frame::none().fill(pal.card).stroke(Stroke::new(1.0_f32, pal.card_stroke)).rounding(10.0).inner_margin(egui::Margin { left: 22.0, right: 22.0, top: 18.0, bottom: 8.0 });
    let out = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        if !title.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
            widgets::tracked_text(ui.painter(), r.left_top(), Align2::LEFT_TOP, &title.to_uppercase(), theme::semibold(11.0), pal.text_faint, 0.16);
            ui.add_space(8.0);
        }
        add(ui);
    });
    sections.tops.push(out.response.rect.top());
    if sections.target == Some(index) {
        sections.target = None;
        ui.scroll_to_rect(out.response.rect, Some(Align::TOP));
    }
    ui.add_space(14.0);
}

/// A row without a hint line.
fn row(ui: &mut egui::Ui, pal: &Palette, label: &str, controls: impl FnOnce(&mut egui::Ui)) {
    row_hint(ui, pal, label, "", controls);
}

/// A settings row: a hairline, the label (and a faint hint line under it) at the left and the
/// controls at the right. Controls are added right to left: the first one added is the rightmost.
fn row_hint(ui: &mut egui::Ui, pal: &Palette, label: &str, hint: &str, controls: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    let text_w = if hint.is_empty() { w } else { (w * 0.62).clamp(160.0, 560.0) };
    let label_g = ui.painter().layout(label.to_string(), theme::ui(13.0), pal.text, text_w);
    let hint_g = (!hint.is_empty()).then(|| ui.painter().layout(hint.to_string(), theme::ui(12.0), pal.text_faint, text_w));
    let text_h = label_g.size().y + hint_g.as_ref().map(|g| 3.0 + g.size().y).unwrap_or(0.0);
    let content_h = text_h.max(32.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, content_h + 20.0), Sense::hover());
    let p = ui.painter().clone();
    p.line_segment([rect.left_top() + vec2(0.0, 0.5), rect.right_top() + vec2(0.0, 0.5)], Stroke::new(1.0_f32, pal.row_rule()));
    let y = rect.top() + 10.0 + (content_h - text_h) / 2.0;
    let label_h = label_g.size().y;
    p.galley(pos2(rect.left(), y), label_g, pal.text);
    if let Some(g) = hint_g {
        p.galley(pos2(rect.left(), y + label_h + 3.0), g, pal.text_faint);
    }
    let area = Rect::from_min_max(pos2(rect.left(), rect.top() + 10.0), pos2(rect.right(), rect.bottom() - 10.0));
    // A child of its own: the row has been allocated above, the controls must not allocate it again.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(Layout::right_to_left(Align::Center)));
    child.spacing_mut().item_spacing.x = 8.0;
    controls(&mut child);
}

/// A row with an on/off switch at the right.
pub(super) fn toggle_row(ui: &mut egui::Ui, pal: &Palette, on: &mut bool, label: &str) -> bool {
    toggle_row_hint(ui, pal, on, label, "")
}

/// [`toggle_row`] with a hint line under the label.
pub(super) fn toggle_row_hint(ui: &mut egui::Ui, pal: &Palette, on: &mut bool, label: &str, hint: &str) -> bool {
    let mut changed = false;
    row_hint(ui, pal, label, hint, |ui| changed = widgets::toggle(ui, on, pal).changed());
    changed
}

/// A faint explanation under a group of rows.
pub(super) fn hint(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).font(theme::ui(12.0)).color(pal.text_faint));
    ui.add_space(6.0);
}

/// A mono hint line (paths, counts).
pub(super) fn note(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).font(theme::mono(12.0)).color(pal.text_faint));
}

/// Index of the section whose card is at the top of the viewport (`top`); at the end of the
/// scroll the one last clicked, if it is still further down, else the last.
fn active_section(tops: &[f32], top: f32, at_end: bool, picked: Option<usize>) -> usize {
    let by_scroll = tops.iter().rposition(|t| *t <= top + 24.0).unwrap_or(0);
    if at_end && !tops.is_empty() {
        picked.filter(|p| *p >= by_scroll && *p < tops.len()).unwrap_or(tops.len() - 1)
    } else {
        by_scroll
    }
}

impl App {
    pub fn settings_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let caps = self.caps;
        let subtitle = format!("saved automatically \u{b7} {} \u{b7} {}", settings::default_path().display(), Page::Settings.shortcut());
        let body = self.page_title(ui, &pal, "Settings", &subtitle);
        let area = Rect::from_min_max(pos2(body.left() + 12.0, body.top() + 4.0), pos2(body.right() - 16.0, body.bottom()));
        let nav = Rect::from_min_size(area.min, vec2(NAV_W, area.height()));
        let column = Rect::from_min_max(pos2(nav.right() + 20.0, area.top()), area.max);
        let mut changed = false;
        let catalog_names: Vec<(String, String)> = self.catalog.skins.iter().map(|k| (k.id.clone(), k.name.clone())).collect();
        let mut reload_skins = false;
        let mut import = false;
        let mut open_folder = false;
        let mut popout_change: Option<&'static str> = None;
        let current_palette = self.palette_id();
        let base_accent = self.palettes.get(&current_palette).map(|p| p.palette.accent).unwrap_or(pal.accent);
        let font_missing = self.display_font.1;
        let docker_found = self.docker.found();
        let font_families: Option<Vec<String>> = self.font_list.get(ui.ctx()).map(<[String]>::to_vec);
        let nav_id = egui::Id::new("settings-nav");
        let mut sections = Sections { target: ui.data_mut(|d| d.get_temp(nav_id.with("target"))), ..Default::default() };
        // The cards are laid out inside the scroll area (not at fixed screen rects), so the area
        // knows the content height and actually scrolls it.
        let scroll = ui
            .allocate_new_ui(egui::UiBuilder::new().max_rect(column), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Leave room for the scroll bar so the cards aren't covered by it.
                        ui.set_max_width(ui.available_width() - 14.0);
                        let s = &mut self.settings;
                        let (pals, pui) = (&mut self.palettes, &mut self.palette_ui);
                        section(ui, &pal, &mut sections, SECTIONS[0], |ui| {
                            changed |= palette_controls(ui, &pal, s, pals, pui, &current_palette);
                            changed |= accent_row(ui, &pal, &mut s.appearance, base_accent);
                            changed |= display_font_row(ui, &pal, &mut s.appearance, font_families.as_deref(), font_missing);
                            row(ui, &pal, "Temperature", |ui| {
                                if let Some(i) = widgets::segmented(ui, &["\u{b0}C", "\u{b0}F"], s.fahrenheit() as usize, &pal, false) {
                                    s.general.temperature_unit = if i == 0 { "celsius" } else { "fahrenheit" }.into();
                                    changed = true;
                                }
                            });
                            let mut mode = s.popout.mode.clone();
                            row(ui, &pal, "Popout", |ui| {
                                egui::ComboBox::from_id_salt("popout-mode").width(200.0).selected_text(popout_mode_label(&mode, &caps)).show_ui(ui, |ui| {
                                    for &m in popout_modes(&caps) {
                                        if ui.selectable_value(&mut mode, m.to_string(), title_case(m)).changed() {
                                            popout_change = Some(m);
                                        }
                                    }
                                });
                            });
                            row(ui, &pal, "Popout skin", |ui| {
                                let current = catalog_names.iter().find(|(id, _)| *id == s.popout.skin).map(|(_, n)| n.clone()).unwrap_or_else(|| s.popout.skin.clone());
                                egui::ComboBox::from_id_salt("skin").width(200.0).selected_text(current).show_ui(ui, |ui| {
                                    for (id, name) in &catalog_names {
                                        changed |= ui.selectable_value(&mut s.popout.skin, id.clone(), name).changed();
                                    }
                                });
                            });
                            row(ui, &pal, "Popout opacity", |ui| {
                                let mut pct = (s.popout.opacity * 100.0).round();
                                ui.add_sized([40.0, 20.0], egui::Label::new(RichText::new(format!("{pct:.0}%")).font(theme::mono(12.0)).color(pal.text_faint)));
                                ui.spacing_mut().slider_width = 180.0;
                                if ui.add(egui::Slider::new(&mut pct, 20.0..=100.0).show_value(false)).changed() {
                                    s.popout.opacity = pct / 100.0;
                                    changed = true;
                                }
                            });
                            let hover_hint = "A faded popout stays out of the way of the desktop behind it, and comes back to full strength while the pointer is over it.";
                            changed |= toggle_row_hint(ui, &pal, &mut s.popout.opaque_on_hover, "Full strength on mouse over", hover_hint);
                            let hf_hint = "Smooth graph motion and meters between data updates. Turn off to redraw only when new data arrives, minimizing CPU use.";
                            row_hint(ui, &pal, "High frequency visuals", hf_hint, |ui| {
                                let label = if s.appearance.high_frequency_visuals { format!("{} fps", s.appearance.frame_rate) } else { "Off".into() };
                                egui::ComboBox::from_id_salt("hf").width(200.0).selected_text(label).show_ui(ui, |ui| {
                                    if ui.selectable_label(!s.appearance.high_frequency_visuals, "Off").clicked() {
                                        s.appearance.high_frequency_visuals = false;
                                        changed = true;
                                    }
                                    for fps in [30u32, 60, 120] {
                                        if ui.selectable_label(s.appearance.high_frequency_visuals && s.appearance.frame_rate == fps, format!("{fps} fps")).clicked() {
                                            s.appearance.high_frequency_visuals = true;
                                            s.appearance.frame_rate = fps;
                                            changed = true;
                                        }
                                    }
                                });
                            });
                            changed |= toggle_row(ui, &pal, &mut s.appearance.bloom, "Visual bloom");
                            row(ui, &pal, "Bloom level", |ui| {
                                ui.add_sized([40.0, 20.0], egui::Label::new(RichText::new(format!("{}", s.appearance.bloom_level)).font(theme::mono(12.0)).color(pal.text_faint)));
                                ui.spacing_mut().slider_width = 180.0;
                                changed |= ui.add(egui::Slider::new(&mut s.appearance.bloom_level, 1..=10).show_value(false)).changed();
                            });
                        });
                        let s = &mut self.settings;
                        section(ui, &pal, &mut sections, SECTIONS[1], |ui| {
                            changed |= toggle_row(ui, &pal, &mut s.graphs.color_keyed, "Color keyed graphs");
                            changed |= metric_color_rows(ui, &pal, &mut s.graphs);
                            changed |= toggle_row(ui, &pal, &mut s.graphs.dashed_summary, "Use dashed lines on summary graphs");
                            changed |= toggle_row(ui, &pal, &mut s.graphs.smooth_curves, "Smooth graph curves");
                            changed |= toggle_row(ui, &pal, &mut s.graphs.compressed_time, "Compress older history");
                            let history_hint = "How much history is kept. With Compress older history on, the live part on the right of the Performance and Docker graphs tracks the grid 1:1 and older time is compressed progressively; with it off the Performance graphs fit the whole history.";
                            row_hint(ui, &pal, "History multiplier", history_hint, |ui| {
                                egui::ComboBox::from_id_salt("hist").width(200.0).selected_text(format!("{}\u{d7} history", s.graphs.history_multiplier)).show_ui(ui, |ui| {
                                    for m in [1u32, 3, 7, 15] {
                                        changed |= ui.selectable_value(&mut s.graphs.history_multiplier, m, format!("{m}\u{d7} history")).changed();
                                    }
                                });
                            });
                            row_hint(ui, &pal, "Pixels per update", "With Compress older history on, each data update advances the Performance and Docker graphs by this many pixels; traces glide smoothly between updates.", |ui| {
                                egui::ComboBox::from_id_salt("ppu").width(200.0).selected_text(s.graphs.pixels_per_update.to_string()).show_ui(ui, |ui| {
                                    for px in [4u32, 8, 12, 16, 24, 32] {
                                        changed |= ui.selectable_value(&mut s.graphs.pixels_per_update, px, px.to_string()).changed();
                                    }
                                });
                            });
                        });
                        section(ui, &pal, &mut sections, SECTIONS[2], |ui| {
                            row(ui, &pal, "Real-time update speed", |ui| {
                                let cur = s.update_speed();
                                egui::ComboBox::from_id_salt("speed").width(200.0).selected_text(cur.label()).show_ui(ui, |ui| {
                                    for sp in UpdateSpeed::ALL {
                                        if ui.selectable_label(cur == sp, sp.label()).clicked() {
                                            s.general.update_speed = sp.token().into();
                                            changed = true;
                                        }
                                    }
                                });
                            });
                            row(ui, &pal, "Default start page", |ui| {
                                let cur = Page::from_id_on(&s.general.default_page, &caps).title();
                                egui::ComboBox::from_id_salt("page").width(200.0).selected_text(cur).show_ui(ui, |ui| {
                                    for p in Page::start_choices(&caps) {
                                        changed |= ui.selectable_value(&mut s.general.default_page, p.id().to_string(), p.title()).changed();
                                    }
                                });
                            });
                            changed |= toggle_row(ui, &pal, &mut s.general.show_self_in_reports, "Show TMOP in reports");
                            changed |= toggle_row(ui, &pal, &mut s.general.per_core_cpu_statistics, "Per core CPU statistics");
                            if caps.pawnio {
                                changed |= toggle_row_hint(
                                    ui,
                                    &pal,
                                    &mut s.general.pawnio_cpu_temperature,
                                    "CPU temperature through PawnIO",
                                    "AMD Ryzen and Intel. Needs the PawnIO driver (pawnio.eu) and TMOP running as administrator. Some anti-cheat software blocks PawnIO: leave this off while such a game runs.",
                                );
                            }
                            let mut never = false;
                            ui.add_enabled_ui(false, |ui| toggle_row_hint(ui, &pal, &mut never, "Check for updates on app start", "Disabled: TMOP never contacts the network."));
                        });
                        section(ui, &pal, &mut sections, SECTIONS[3], |ui| {
                            if caps.window_levels {
                                changed |= toggle_row(ui, &pal, &mut s.window.always_on_top, "Always on top");
                            }
                            changed |= toggle_row(ui, &pal, &mut s.window.uptime_visible, "Show up time in the status bar");
                        });
                        section(ui, &pal, &mut sections, SECTIONS[4], |ui| {
                            if caps.proc.efficiency {
                                changed |= toggle_row(ui, &pal, &mut s.general.confirm_efficiency_mode, &format!("Ask before applying {}", crate::process::efficiency_word()));
                            }
                            changed |= toggle_row(ui, &pal, &mut s.general.disk_space_suppress_stop_confirm, "Stop Disk Space scans without asking");
                            changed |= toggle_row(ui, &pal, &mut s.general.show_full_account_name, "Show full account name");
                            // One switch per case; each one clears the other, so the page is
                            // never hidden by a switch that is not on screen.
                            if docker_found || !caps.engine {
                                if toggle_row(ui, &pal, &mut s.docker.hide_page, "Hide the Docker page") {
                                    s.docker.show_page &= !s.docker.hide_page;
                                    changed = true;
                                }
                            } else {
                                let mut shown = s.docker.show_page && !s.docker.hide_page;
                                if toggle_row_hint(ui, &pal, &mut shown, "Show the Docker page", "No Docker was found. The page sets up TMOP's own Docker engine.") {
                                    s.docker.show_page = shown;
                                    s.docker.hide_page = false;
                                    changed = true;
                                }
                            }
                        });
                        if caps.taskmgr_replace {
                            section(ui, &pal, &mut sections, TASK_MANAGER, |ui| self.replace_section(ui, &pal));
                        }
                        section(ui, &pal, &mut sections, SECTIONS[6], |ui| {
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                                reload_skins |= Button::new("Reload skins and palettes").size(ButtonSize::Medium).icon(Icon::Refresh).show(ui, &pal).clicked();
                                import |= Button::new("Import TMOG settings").size(ButtonSize::Medium).show(ui, &pal).clicked();
                                open_folder |= Button::new("Open settings folder").size(ButtonSize::Medium).show(ui, &pal).clicked();
                            });
                            ui.add_space(10.0);
                            ui.spacing_mut().item_spacing.y = 4.0;
                            note(ui, &pal, &format!("{} skins \u{b7} {} palettes", self.catalog.skins.len(), self.palettes.palettes.len()));
                            note(ui, &pal, &format!("Settings file: {}", settings::default_path().display()));
                            note(ui, &pal, &format!("Skin folders: {}", skin::default_skin_dirs().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("; ")));
                            note(ui, &pal, &format!("Palettes: {}", self.palettes.dir.display()));
                            note(ui, &pal, &format!("Recordings: {}", crate::pro::recorder::default_dir().display()));
                            for (p, e) in &self.catalog.errors {
                                note(ui, &pal, &format!("Skin error: {}: {e}", p.display()));
                            }
                            for (p, e) in &self.palettes.errors {
                                note(ui, &pal, &format!("Palette error: {}: {e}", p.display()));
                            }
                            ui.add_space(8.0);
                        });
                        section(ui, &pal, &mut sections, "", |ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
                            let p = ui.painter();
                            widgets::logo(p, pos2(r.left() + 20.0, r.center().y), 40.0, pal.accent);
                            let title = p.text(pos2(r.left() + 56.0, r.center().y - 17.0), Align2::LEFT_CENTER, format!("{} \u{2014} {}", crate::APP_NAME, crate::APP_LONG_NAME), theme::semibold(16.0), pal.text);
                            p.text(pos2(title.right() + 10.0, title.center().y), Align2::LEFT_CENTER, format!("{} \u{b7} {}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH), theme::mono(12.0), pal.text_faint);
                            let text = "Free and open source \u{b7} GPL-3.0 \u{b7} sends no data \u{b7} fonts: Inter, JetBrains Mono, Michroma (SIL OFL)";
                            widgets::text_fit(p, pos2(r.left() + 56.0, r.center().y + 10.0), Align2::LEFT_CENTER, text, theme::ui(12.0), pal.text_faint, r.width() - 56.0);
                        });
                    })
            })
            .inner;

        // The section list: the active entry follows the scroll position.
        let at_end = scroll.state.offset.y + scroll.inner_rect.height() >= scroll.content_size.y - 1.0;
        let picked: Option<usize> = ui.data_mut(|d| d.get_temp(nav_id.with("picked")));
        let active = active_section(&sections.tops, scroll.inner_rect.top(), at_end, picked);
        if !at_end && picked.is_some() {
            ui.data_mut(|d| d.remove_temp::<usize>(nav_id.with("picked")));
        }
        ui.data_mut(|d| d.remove_temp::<usize>(nav_id.with("target")));
        for (i, name) in shown_sections(&caps).into_iter().enumerate() {
            let r = Rect::from_min_size(nav.min + vec2(0.0, i as f32 * 36.0), vec2(NAV_W, 34.0));
            let resp = ui.interact(r, nav_id.with(i), Sense::click());
            let on = i == active;
            if on {
                ui.painter().rect_filled(r, 6.0, pal.selection);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 6.0, pal.hover);
            }
            let (font, color) = if on { (theme::semibold(13.0), pal.accent) } else { (theme::ui(13.0), if resp.hovered() { pal.text } else { pal.text_dim }) };
            ui.painter().text(pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, name, font, color);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                ui.data_mut(|d| {
                    d.insert_temp(nav_id.with("target"), i);
                    d.insert_temp(nav_id.with("picked"), i);
                });
                ui.ctx().request_repaint();
            }
        }

        if changed {
            self.settings_dirty = true;
        }
        match popout_change {
            Some("off") => self.set_popout(false),
            Some(m) => {
                self.settings.popout.mode = m.into();
                self.settings_dirty = true;
                self.set_popout(true);
            }
            None => {}
        }
        if open_folder {
            if let Some(dir) = settings::default_path().parent() {
                if let Err(e) = super::kit::reveal(dir) {
                    self.status = format!("Could not open {}: {e}", dir.display());
                }
            }
        }
        if reload_skins {
            self.catalog = skin::scan(&skin::default_skin_dirs());
            // Write pending palette edits first so the rescan reads them back; if that fails,
            // keep the palettes (and the edits) as they are.
            match self.palette_ui.flush(&self.palettes, true) {
                Some(e) => self.status = format!("{e}; palettes not reloaded"),
                None => {
                    self.palettes = palette::scan(&self.palettes.dir);
                    self.palette_ui.rescanned(&self.palettes);
                    let errors = self.catalog.errors.len() + self.palettes.errors.len();
                    self.status = format!("{} skins, {} palettes, {errors} errors", self.catalog.skins.len(), self.palettes.palettes.len());
                }
            }
        }
        if import {
            match settings::import_tmog() {
                Ok(load) => {
                    self.status = load.describe("Imported TMOG settings");
                    // A read-only file (newer TMOP, or no backup) stays protected after an import.
                    let read_only = self.settings.read_only;
                    self.settings = Settings { read_only, ..load.settings };
                    self.settings_dirty = true;
                }
                Err(e) => self.status = format!("Import failed: {e}"),
            }
        }
    }
}

impl App {
    /// Writes the palette being edited once its last change is [`AUTOSAVE_DELAY`] old, or at
    /// once with `now`.
    pub fn save_palette_edits(&mut self, now: bool) {
        if let Some(e) = self.palette_ui.flush(&self.palettes, now) {
            self.status = e;
        }
    }

    /// Finishes an import or export once its file dialog has closed.
    pub fn poll_palette_dialog(&mut self) {
        let Some((export, rx)) = &self.palette_ui.dialog else { return };
        let path = match rx.try_recv() {
            Ok(p) => p,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        let export = export.clone();
        self.palette_ui.dialog = None;
        let Some(path) = path else { return };
        let pui = &mut self.palette_ui;
        match export {
            Some(id) => {
                let Some(p) = self.palettes.get(&id) else { return };
                match palette::export(p, &path) {
                    Ok(()) => {
                        pui.message = None;
                        self.status = format!("Exported {} to {}", p.name, path.display());
                    }
                    Err(e) => pui.message = Some(format!("Export failed: {e}")),
                }
            }
            None => match self.palettes.import(&path) {
                Ok(id) => {
                    pui.message = pui.close_editor(&self.palettes);
                    let name = self.palettes.get(&id).map(|p| p.name.clone()).unwrap_or_default();
                    self.status = format!("Imported palette {name} ({id})");
                    self.settings.appearance.theme = id;
                    self.settings_dirty = true;
                }
                Err(e) => pui.message = Some(format!("Import failed: {}: {e}", path.display())),
            },
        }
    }
}

/// The popout modes offered: `desktop` and `topMost` are window levels, which Wayland ignores.
fn popout_modes(caps: &Caps) -> &'static [&'static str] {
    if caps.window_levels {
        &["off", "desktop", "normal", "topMost"]
    } else {
        &["off", "normal"]
    }
}

/// The popout combo's text for the saved `mode`. A window level this session cannot apply shows
/// as Normal (the popout opens as a normal window); the saved value is kept.
fn popout_mode_label(mode: &str, caps: &Caps) -> String {
    if caps.window_levels || popout_modes(caps).contains(&mode) {
        title_case(mode)
    } else {
        title_case("normal")
    }
}

fn title_case(s: &str) -> String {
    match s {
        "topMost" => "TopMost".into(),
        _ => {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Task Manager section is Windows-only; the others stay in page order.
    #[test]
    fn sections_follow_caps() {
        assert_eq!(shown_sections(&Caps::WINDOWS), SECTIONS.to_vec());
        let linux = shown_sections(&Caps::LINUX);
        assert_eq!(linux.len(), SECTIONS.len() - 1);
        assert!(!linux.contains(&TASK_MANAGER) && linux.last() == Some(&"About"));
    }

    /// Without window levels (Wayland) the popout offers no desktop or topmost mode, and a saved
    /// one reads as Normal.
    #[test]
    fn popout_modes_follow_window_levels() {
        let wayland = Caps { window_levels: false, ..Caps::LINUX };
        assert_eq!(popout_modes(&Caps::WINDOWS), ["off", "desktop", "normal", "topMost"]);
        assert_eq!(popout_modes(&wayland), ["off", "normal"]);
        assert_eq!(popout_mode_label("topMost", &Caps::WINDOWS), "TopMost");
        assert_eq!(popout_mode_label("topMost", &wayland), "Normal");
        assert_eq!(popout_mode_label("desktop", &wayland), "Normal");
        assert_eq!(popout_mode_label("off", &wayland), "Off");
    }

    /// The metric rows lay out with colour keying on and off and leave the settings alone
    /// until something is edited.
    #[test]
    fn metric_color_rows_draw_without_changing_settings() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx, None);
        let mut g = settings::Graphs::default();
        g.metric_colors.insert("cpu".into(), "#112233".into());
        for keyed in [true, false] {
            g.color_keyed = keyed;
            let before = g.clone();
            let mut changed = true;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| changed = metric_color_rows(ui, &theme::DARK, &mut g));
            });
            assert!(!changed);
            assert_eq!(g, before);
        }
    }

    /// The display font picker lays out while the families are being listed, with them, and
    /// for a saved font that isn't installed, and leaves the settings alone.
    #[test]
    fn display_font_row_draws_without_changing_settings() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx, None);
        let list = ["Arial".to_string(), "Consolas".to_string()];
        for (font, families, missing) in [("Michroma", None, false), ("Arial", Some(&list[..]), false), ("Nope Sans", Some(&list[..]), true)] {
            let mut a = settings::Appearance { display_font: font.into(), ..Default::default() };
            let before = a.clone();
            let mut changed = true;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| changed = display_font_row(ui, &theme::DARK, &mut a, families, missing));
            });
            assert!(!changed);
            assert_eq!(a, before);
        }
    }

    /// Section cards stack their rows, record where they start and take a hint line without
    /// changing the value they show.
    #[test]
    fn sections_stack_rows() {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let mut sections = Sections::default();
        let (mut on, mut changed) = (true, true);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                section(ui, &theme::DARK, &mut sections, "Appearance", |ui| {
                    changed = toggle_row_hint(ui, &theme::DARK, &mut on, "Visual bloom", "A long hint that wraps onto a second line when the row is narrow enough to need it.");
                    note(ui, &theme::DARK, "C:\\Users\\x\\settings.json");
                });
                section(ui, &theme::DARK, &mut sections, "", |ui| hint(ui, &theme::DARK, "About"));
            });
        });
        assert!(!changed && on);
        assert_eq!(sections.tops.len(), 2);
        assert!(sections.tops[0] < sections.tops[1]);
    }

    /// The accent swatches lay out and change nothing until clicked; the active entry follows the
    /// scroll position.
    #[test]
    fn accent_row_and_section_list() {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for accent in ["", "#8B9CFF", "#123456"] {
            let mut a = settings::Appearance { accent: accent.into(), ..Default::default() };
            let before = a.clone();
            let mut changed = true;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| changed = accent_row(ui, &theme::DARK, &mut a, theme::DARK.accent));
            });
            assert!(!changed);
            assert_eq!(a, before);
        }
        let tops = [100.0, 400.0, 700.0, 900.0];
        assert_eq!(active_section(&tops, 100.0, false, None), 0);
        assert_eq!(active_section(&tops, 390.0, false, None), 1);
        assert_eq!(active_section(&tops, 5000.0, false, None), 3);
        assert_eq!(active_section(&tops, 500.0, true, None), 3, "the end of the scroll shows the last entry");
        assert_eq!(active_section(&tops, 500.0, true, Some(2)), 2, "or the one that was clicked");
    }
}
