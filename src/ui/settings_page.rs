//! Settings page: two columns of sections of settings.

use super::theme::{self, Palette};
use super::widgets::{self, draw_icon, Icon};
use super::{App, FrameData, Page};
use crate::settings::{self, Settings, UpdateSpeed};
use crate::skin;
use eframe::egui::{self, pos2, vec2, Align2, RichText, Stroke};

const LABEL_W: f32 = 150.0;

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
        // The columns are laid out inside the scroll area (not at fixed screen rects), so the area
        // knows the content height and actually scrolls it.
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner), |ui| {
        egui::ScrollArea::vertical().id_salt("settings-scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(col_w);
                let s = &mut self.settings;
                section(ui, &pal, Icon::Apps, "Appearance", |ui| {
                    row(ui, &pal, "Theme", |ui| {
                        egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(title_case(&s.theme)).show_ui(ui, |ui| {
                            for t in ["system", "dark", "light"] {
                                changed |= ui.selectable_value(&mut s.theme, t.to_string(), title_case(t)).changed();
                            }
                        });
                    });
                    row(ui, &pal, "Application font", |ui| {
                        egui::ComboBox::from_id_salt("font").width(200.0).selected_text("Michroma (display) / Segoe UI").show_ui(ui, |ui| {
                            ui.label("Michroma for headings, Segoe UI for text");
                        });
                    });
                    row(ui, &pal, "Temperature", |ui| {
                        egui::ComboBox::from_id_salt("temp").width(200.0).selected_text(if s.fahrenheit() { "Fahrenheit (\u{b0}F)" } else { "Celsius (\u{b0}C)" }).show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut s.temperature_unit, "celsius".into(), "Celsius (\u{b0}C)").changed();
                            changed |= ui.selectable_value(&mut s.temperature_unit, "fahrenheit".into(), "Fahrenheit (\u{b0}F)").changed();
                        });
                    });
                    let mut mode = s.extra.get("popoutMode").and_then(|v| v.as_str()).unwrap_or("off").to_string();
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
                        let current = catalog_names.iter().find(|(id, _)| *id == s.popout_skin).map(|(_, n)| n.clone()).unwrap_or_else(|| s.popout_skin.clone());
                        egui::ComboBox::from_id_salt("skin").width(200.0).selected_text(current).show_ui(ui, |ui| {
                            for (id, name) in &catalog_names {
                                changed |= ui.selectable_value(&mut s.popout_skin, id.clone(), name).changed();
                            }
                        });
                    });
                    row(ui, &pal, "Opacity", |ui| {
                        let mut pct = (s.popout_opacity * 100.0).round();
                        if ui.add(egui::Slider::new(&mut pct, 20.0..=100.0).show_value(false)).changed() {
                            s.popout_opacity = pct / 100.0;
                            changed = true;
                        }
                        ui.label(RichText::new(format!("{pct:.0}%")).font(theme::ui(12.5)).color(pal.text_dim));
                    });
                    changed |= toggle_row(ui, &pal, &mut s.popout_opaque_on_hover, "Full Strength on Mouse Over");
                    hint(ui, &pal, "A faded popout stays out of the way of the desktop behind it, and comes back to full strength while the pointer is over it.");
                    row(ui, &pal, "High Frequency Visuals", |ui| {
                        let label = if s.high_frequency_visuals { format!("{} fps", s.visual_frame_rate) } else { "Off".into() };
                        egui::ComboBox::from_id_salt("hf").width(200.0).selected_text(label).show_ui(ui, |ui| {
                            if ui.selectable_label(!s.high_frequency_visuals, "Off").clicked() {
                                s.high_frequency_visuals = false;
                                changed = true;
                            }
                            for fps in [30u32, 60, 120] {
                                if ui.selectable_label(s.high_frequency_visuals && s.visual_frame_rate == fps, format!("{fps} fps")).clicked() {
                                    s.high_frequency_visuals = true;
                                    s.visual_frame_rate = fps;
                                    changed = true;
                                }
                            }
                        });
                    });
                    hint(ui, &pal, "Smooth graph motion and meters between data updates. Turn off to redraw only when new data arrives, minimizing CPU use.");
                    changed |= toggle_row(ui, &pal, &mut s.visual_bloom, "Visual bloom");
                    row(ui, &pal, "Bloom level", |ui| {
                        changed |= ui.add(egui::Slider::new(&mut s.visual_bloom_level, 1..=10).show_value(false)).changed();
                        ui.label(RichText::new(format!("{}", s.visual_bloom_level)).font(theme::ui(12.5)).color(pal.text_dim));
                    });
                });
                section(ui, &pal, Icon::Chart, "Graphs", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.color_keyed_graphs, "Color Keyed Graphs");
                    changed |= toggle_row(ui, &pal, &mut s.dashed_summary_graph_lines, "Use dashed lines on summary graph");
                    changed |= toggle_row(ui, &pal, &mut s.smooth_graph_curves, "Smooth Graph Curves");
                    changed |= toggle_row(ui, &pal, &mut s.compressed_graph_time, "Compress Older History");
                    row(ui, &pal, "History multiplier", |ui| {
                        egui::ComboBox::from_id_salt("hist").width(200.0).selected_text(format!("{}\u{d7} history", s.graph_history_multiplier)).show_ui(ui, |ui| {
                            for m in [1u32, 3, 7, 15] {
                                changed |= ui.selectable_value(&mut s.graph_history_multiplier, m, format!("{m}\u{d7} history")).changed();
                            }
                        });
                    });
                    hint(ui, &pal, "Older time is compressed smoothly. At 15\u{d7}, the live quarter on the right tracks the grid 1:1 and older time is compressed progressively, so the whole graph holds 15\u{d7} history.");
                    row(ui, &pal, "Pixels per update", |ui| {
                        egui::ComboBox::from_id_salt("ppu").width(200.0).selected_text(s.graph_pixels_per_update.to_string()).show_ui(ui, |ui| {
                            for px in [4u32, 8, 12, 16, 24, 32] {
                                changed |= ui.selectable_value(&mut s.graph_pixels_per_update, px, px.to_string()).changed();
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
                                    s.update_speed = sp.token().into();
                                    changed = true;
                                }
                            }
                        });
                    });
                    row(ui, &pal, "Default start page", |ui| {
                        let cur = Page::from_id(&s.default_page).title();
                        egui::ComboBox::from_id_salt("page").width(200.0).selected_text(cur).show_ui(ui, |ui| {
                            for p in Page::NAV.iter().chain([Page::Settings].iter()) {
                                changed |= ui.selectable_value(&mut s.default_page, p.id().to_string(), p.title()).changed();
                            }
                        });
                    });
                    changed |= toggle_row(ui, &pal, &mut s.show_self_in_reports, "Show TMOP in Reports");
                    changed |= toggle_row(ui, &pal, &mut s.per_core_cpu_statistics, "Per Core CPU Statistics");
                    let mut never = false;
                    toggle_row(ui, &pal, &mut never, "Check for Updates on App Start");
                    hint(ui, &pal, "Disabled: TMOP never contacts the network.");
                });
                section(ui, &pal, Icon::Popout, "Window management", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.always_on_top, "Always on top");
                    changed |= toggle_row(ui, &pal, &mut s.uptime_visible, "Show up time in the status bar");
                });
                section(ui, &pal, Icon::Gear, "Other options", |ui| {
                    changed |= toggle_row(ui, &pal, &mut s.confirm_efficiency_mode, "Ask before applying Efficiency mode");
                    changed |= toggle_row(ui, &pal, &mut s.disk_space_suppress_stop_confirm, "Stop Disk Space scans without asking");
                    changed |= toggle_row(ui, &pal, &mut s.show_full_account_name, "Show full account name");
                });
                section(ui, &pal, Icon::Record, "Skins and files", |ui| {
                    ui.horizontal(|ui| {
                        if widgets::button(ui, "Reload skins", &pal, Some(Icon::Refresh)).clicked() {
                            reload_skins = true;
                        }
                        if widgets::button(ui, "Import TMOG settings", &pal, None).clicked() {
                            import = true;
                        }
                    });
                    hint(ui, &pal, &format!("Settings file: {}", settings::default_path().display()));
                    hint(ui, &pal, &format!("Skin folders: {}", skin::default_skin_dirs().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("; ")));
                    hint(ui, &pal, &format!("Recordings: {}", crate::pro::recorder::default_dir().display()));
                    if !self.catalog.errors.is_empty() {
                        for (p, e) in &self.catalog.errors {
                            hint(ui, &pal, &format!("Skin error: {}: {e}", p.display()));
                        }
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
                self.settings.extra.insert("popoutMode".into(), serde_json::Value::String(m.into()));
                self.settings_dirty = true;
                self.set_popout(true);
            }
            None => {}
        }
        if reload_skins {
            self.catalog = skin::scan(&skin::default_skin_dirs());
            self.status = format!("{} skins, {} errors", self.catalog.skins.len(), self.catalog.errors.len());
        }
        if import {
            match std::fs::read_to_string(settings::tmog_path()).map_err(|e| e.to_string()).and_then(|t| Settings::from_json(&t)) {
                Ok((s, outcome)) => {
                    self.settings = s;
                    self.settings_dirty = true;
                    self.status = format!("Imported TMOG settings ({outcome:?})");
                }
                Err(e) => self.status = format!("Import failed: {e}"),
            }
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
