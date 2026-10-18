//! Settings page: two columns of sections of settings.

use super::kit::{file_dialog, FileDialog};
use super::theme::{self, Palette, Role, CORE_ROLES, MIN_CONTRAST, ROLES};
use super::widgets::{self, draw_icon, Icon};
use super::{App, FrameData, Page};
use crate::palette::{self, PaletteCatalog, PaletteFile, SYSTEM};
use crate::settings::{self, Settings, UpdateSpeed};
use crate::skin;
use eframe::egui::{self, pos2, vec2, Align2, RichText, Stroke};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

const LABEL_W: f32 = 150.0;

/// Palette edits are written to disk this long after the last change.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);

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

    /// Saves pending edits and closes the editor.
    fn close_editor(&mut self, pals: &PaletteCatalog) -> Option<String> {
        let err = self.flush(pals, true);
        self.editing = None;
        err
    }
}

/// One colour role: label, colour button, hex value and a contrast warning.
fn color_row(ui: &mut egui::Ui, pal: &Palette, p: &mut Palette, r: Role) -> bool {
    let warning = p.worst_contrast(r.key).filter(|c| *c < MIN_CONTRAST);
    let mut changed = false;
    row(ui, pal, r.label, |ui| {
        let c = p.role_mut(r.key).expect("palette role");
        changed = egui::color_picker::color_edit_button_srgba(ui, c, egui::color_picker::Alpha::Opaque).changed();
        ui.label(RichText::new(theme::to_skin(*c).to_string()).font(theme::mono(12.0)).color(pal.text_dim));
        if let Some(w) = warning {
            ui.label(RichText::new(format!("\u{26a0} {w:.1}:1")).font(theme::ui(12.0)).color(pal.warn))
                .on_hover_text(format!("Text contrast is below {MIN_CONTRAST}:1 (WCAG AA). The palette is still saved."));
        }
    });
    changed
}

/// Theme combo, the palette buttons next to it and the palette editor. `current` is the id of
/// the palette in use. Returns whether settings changed.
fn palette_controls(ui: &mut egui::Ui, pal: &Palette, s: &mut Settings, pals: &mut PaletteCatalog, pui: &mut PaletteUi, current: &str) -> bool {
    let mut changed = false;
    row(ui, pal, "Theme", |ui| {
        let selected = match pals.get(&s.appearance.theme) {
            _ if s.appearance.theme == SYSTEM => "System".to_string(),
            Some(p) => p.name.clone(),
            None => format!("{} (not installed)", s.appearance.theme),
        };
        egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(selected).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut s.appearance.theme, SYSTEM.to_string(), "System").changed();
            for p in &pals.palettes {
                changed |= ui.selectable_value(&mut s.appearance.theme, p.id.clone(), &p.name).changed();
            }
        });
    });
    if changed {
        pui.message = pui.close_editor(pals);
        pui.rename = None;
        pui.confirm_delete = false;
    }
    let Some(cur) = pals.get(current).cloned() else { return changed };
    let custom = !cur.is_built_in();
    let editing = pui.editing.as_ref().is_some_and(|(id, _)| id == current);
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            if widgets::button(ui, "Duplicate", pal, None).on_hover_text(format!("Make an editable copy of {}", cur.name)).clicked() {
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
            ui.add_enabled_ui(custom, |ui| {
                if widgets::button(ui, if editing { "Done" } else { "Edit" }, pal, None).clicked() {
                    if editing {
                        pui.message = pui.close_editor(pals);
                    } else {
                        pui.open_editor(&cur);
                    }
                }
                if widgets::button(ui, "Rename", pal, None).clicked() {
                    pui.rename = Some(cur.name.clone());
                    pui.confirm_delete = false;
                }
                if widgets::button(ui, "Delete", pal, None).clicked() {
                    pui.confirm_delete = true;
                    pui.rename = None;
                }
            })
            .response
            .on_disabled_hover_text("Built-in palettes are read-only; duplicate one to edit it.");
            ui.add_enabled_ui(pui.dialog.is_none(), |ui| {
                if widgets::button(ui, "Import", pal, None).clicked() {
                    let d = FileDialog { save: false, title: "Import palette".into(), filter: ("TMOP palette".into(), palette::EXTENSION.into()), file_name: String::new(), owner: pui.owner };
                    pui.dialog = Some((None, file_dialog(d)));
                }
                if widgets::button(ui, "Export", pal, None).on_hover_text(format!("Save {} as a .{} file", cur.name, palette::EXTENSION)).clicked() {
                    let file_name = format!("{}.{}", cur.id, palette::EXTENSION);
                    let d = FileDialog { save: true, title: "Export palette".into(), filter: ("TMOP palette".into(), palette::EXTENSION.into()), file_name, owner: pui.owner };
                    pui.dialog = Some((Some(cur.id.clone()), file_dialog(d)));
                }
            });
        });
    });
    let mut rename_done = None;
    if let Some(name) = &mut pui.rename {
        row(ui, pal, "New name", |ui| {
            let resp = ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if widgets::button(ui, "Rename", pal, None).clicked() || enter {
                rename_done = Some(true);
            }
            if widgets::button(ui, "Cancel", pal, None).clicked() {
                rename_done = Some(false);
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
            if widgets::button(ui, "Delete", pal, None).clicked() {
                answer = Some(true);
            }
            if widgets::button(ui, "Cancel", pal, None).clicked() {
                answer = Some(false);
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
    ui.separator();
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
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        revert = widgets::button(ui, "Revert to file on disk", pal, Some(Icon::Refresh)).on_hover_text("Undo every change made since the editor was opened").clicked();
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

/// A settings section: icon + bold heading, then a card with rows.
fn section(ui: &mut egui::Ui, pal: &Palette, icon: Icon, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), egui::Sense::hover());
        draw_icon(ui.painter(), r.shrink(1.0), icon, pal.text_dim);
        ui.label(RichText::new(title).font(theme::bold(13.0)).color(pal.text));
    });
    ui.add_space(2.0);
    egui::Frame::none().fill(pal.panel).stroke(Stroke::new(1.0_f32, pal.panel_stroke)).rounding(6.0).inner_margin(egui::Margin::symmetric(14.0, 12.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
        add(ui);
    });
    ui.add_space(14.0);
}

fn row(ui: &mut egui::Ui, pal: &Palette, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, 22.0), egui::Sense::hover());
        ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, label, theme::ui(13.0), pal.text);
        add(ui);
    });
}

fn toggle_row(ui: &mut egui::Ui, pal: &Palette, on: &mut bool, label: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = widgets::toggle(ui, on, pal).changed();
        ui.add_space(4.0);
        ui.label(RichText::new(label).font(theme::ui(13.0)).color(pal.text));
    });
    changed
}

fn hint(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).font(theme::ui(11.5)).color(pal.text_faint));
}

impl App {
    pub fn settings_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Settings", Icon::Gear);
        let inner = body.shrink2(vec2(18.0, 10.0));
        let mut changed = false;
        let gap = 20.0;
        // Leave room for the scroll bar so the right column isn't covered by it.
        let col_w = ((inner.width() - gap - 14.0) / 2.0).floor();
        let catalog_names: Vec<(String, String)> = self.catalog.skins.iter().map(|k| (k.id.clone(), k.name.clone())).collect();
        let mut reload_skins = false;
        let mut import = false;
        let mut popout_change: Option<&'static str> = None;
        let current_palette = self.palette_id();
        // The columns are laid out inside the scroll area (not at fixed screen rects), so the area
        // knows the content height and actually scrolls it.
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner), |ui| {
        egui::ScrollArea::vertical().id_salt("settings-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(col_w);
                let s = &mut self.settings;
                let (pals, pui) = (&mut self.palettes, &mut self.palette_ui);
                section(ui, &pal, Icon::Apps, "Appearance", |ui| {
                    changed |= palette_controls(ui, &pal, s, pals, pui, &current_palette);
                    row(ui, &pal, "Temperature", |ui| {
                        egui::ComboBox::from_id_salt("temp").width(200.0).selected_text(if s.fahrenheit() { "Fahrenheit (\u{b0}F)" } else { "Celsius (\u{b0}C)" }).show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut s.general.temperature_unit, "celsius".into(), "Celsius (\u{b0}C)").changed();
                            changed |= ui.selectable_value(&mut s.general.temperature_unit, "fahrenheit".into(), "Fahrenheit (\u{b0}F)").changed();
                        });
                    });
                    let mut mode = s.popout.mode.clone();
                    row(ui, &pal, "Popout", |ui| {
                        egui::ComboBox::from_id_salt("popout-mode").width(200.0).selected_text(title_case(&mode)).show_ui(ui, |ui| {
                            for m in ["off", "desktop", "normal", "topMost"] {
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
                    row(ui, &pal, "Opacity", |ui| {
                        let mut pct = (s.popout.opacity * 100.0).round();
                        if ui.add(egui::Slider::new(&mut pct, 20.0..=100.0).show_value(false)).changed() {
                            s.popout.opacity = pct / 100.0;
                            changed = true;
                        }
                        ui.label(RichText::new(format!("{pct:.0}%")).font(theme::ui(12.5)).color(pal.text_dim));
                    });
                    changed |= toggle_row(ui, &pal, &mut s.popout.opaque_on_hover, "Full Strength on Mouse Over");
                    hint(ui, &pal, "A faded popout stays out of the way of the desktop behind it, and comes back to full strength while the pointer is over it.");
                    row(ui, &pal, "High Frequency Visuals", |ui| {
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
                    hint(ui, &pal, "Smooth graph motion and meters between data updates. Turn off to redraw only when new data arrives, minimizing CPU use.");
                    changed |= toggle_row(ui, &pal, &mut s.appearance.bloom, "Visual bloom");
                    row(ui, &pal, "Bloom level", |ui| {
                        changed |= ui.add(egui::Slider::new(&mut s.appearance.bloom_level, 1..=10).show_value(false)).changed();
                        ui.label(RichText::new(format!("{}", s.appearance.bloom_level)).font(theme::ui(12.5)).color(pal.text_dim));
                    });
                });
                section(ui, &pal, Icon::Chart, "Graphs", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.graphs.color_keyed, "Color Keyed Graphs");
                    changed |= toggle_row(ui, &pal, &mut s.graphs.dashed_summary, "Use dashed lines on summary graph");
                    changed |= toggle_row(ui, &pal, &mut s.graphs.smooth_curves, "Smooth Graph Curves");
                    changed |= toggle_row(ui, &pal, &mut s.graphs.compressed_time, "Compress Older History");
                    row(ui, &pal, "History multiplier", |ui| {
                        egui::ComboBox::from_id_salt("hist").width(200.0).selected_text(format!("{}\u{d7} history", s.graphs.history_multiplier)).show_ui(ui, |ui| {
                            for m in [1u32, 3, 7, 15] {
                                changed |= ui.selectable_value(&mut s.graphs.history_multiplier, m, format!("{m}\u{d7} history")).changed();
                            }
                        });
                    });
                    hint(ui, &pal, "Older time is compressed smoothly. At 15\u{d7}, the live quarter on the right tracks the grid 1:1 and older time is compressed progressively, so the whole graph holds 15\u{d7} history.");
                    row(ui, &pal, "Pixels per update", |ui| {
                        egui::ComboBox::from_id_salt("ppu").width(200.0).selected_text(s.graphs.pixels_per_update.to_string()).show_ui(ui, |ui| {
                            for px in [4u32, 8, 12, 16, 24, 32] {
                                changed |= ui.selectable_value(&mut s.graphs.pixels_per_update, px, px.to_string()).changed();
                            }
                        });
                    });
                    hint(ui, &pal, "Each data update advances the graph by this many pixels; traces glide smoothly between updates.");
                });
            });
            ui.add_space(gap);
            ui.vertical(|ui| {
                ui.set_width(col_w);
                let s = &mut self.settings;
                section(ui, &pal, Icon::List, "General", |ui| {
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
                        let cur = Page::from_id(&s.general.default_page).title();
                        egui::ComboBox::from_id_salt("page").width(200.0).selected_text(cur).show_ui(ui, |ui| {
                            for p in Page::NAV.iter().chain([Page::Settings].iter()) {
                                changed |= ui.selectable_value(&mut s.general.default_page, p.id().to_string(), p.title()).changed();
                            }
                        });
                    });
                    changed |= toggle_row(ui, &pal, &mut s.general.show_self_in_reports, "Show TMOP in Reports");
                    changed |= toggle_row(ui, &pal, &mut s.general.per_core_cpu_statistics, "Per Core CPU Statistics");
                    let mut never = false;
                    toggle_row(ui, &pal, &mut never, "Check for Updates on App Start");
                    hint(ui, &pal, "Disabled: TMOP never contacts the network.");
                });
                section(ui, &pal, Icon::Popout, "Window management", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.window.always_on_top, "Always on top");
                    changed |= toggle_row(ui, &pal, &mut s.window.uptime_visible, "Show up time in the status bar");
                });
                section(ui, &pal, Icon::Gear, "Other options", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.general.confirm_efficiency_mode, "Ask before applying Efficiency mode");
                    changed |= toggle_row(ui, &pal, &mut s.general.disk_space_suppress_stop_confirm, "Stop Disk Space scans without asking");
                    changed |= toggle_row(ui, &pal, &mut s.general.show_full_account_name, "Show full account name");
                });
                section(ui, &pal, Icon::Record, "Skins and files", |ui| {
                    ui.horizontal(|ui| {
                        if widgets::button(ui, "Reload skins and palettes", &pal, Some(Icon::Refresh)).clicked() {
                            reload_skins = true;
                        }
                        if widgets::button(ui, "Import TMOG settings", &pal, None).clicked() {
                            import = true;
                        }
                    });
                    hint(ui, &pal, &format!("Settings file: {}", settings::default_path().display()));
                    hint(ui, &pal, &format!("Skin folders: {}", skin::default_skin_dirs().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("; ")));
                    hint(ui, &pal, &format!("Palettes: {}", self.palettes.dir.display()));
                    hint(ui, &pal, &format!("Recordings: {}", crate::pro::recorder::default_dir().display()));
                    if !self.catalog.errors.is_empty() {
                        for (p, e) in &self.catalog.errors {
                            hint(ui, &pal, &format!("Skin error: {}: {e}", p.display()));
                        }
                    }
                    for (p, e) in &self.palettes.errors {
                        hint(ui, &pal, &format!("Palette error: {}: {e}", p.display()));
                    }
                });
                section(ui, &pal, Icon::Home, "About", |ui| {
                    ui.label(RichText::new(format!("{} \u{2014} {} 0.1  \u{2022}  Rust / egui  \u{2022}  no network, no licensing", crate::APP_NAME, crate::APP_LONG_NAME)).font(theme::ui(12.5)).color(pal.text));
                    hint(ui, &pal, "Michroma font \u{a9} The Michroma Project Authors, SIL Open Font License 1.1 (assets/fonts/Michroma-OFL.txt).");
                });
            });
        });
        });
        });
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
