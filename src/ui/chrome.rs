//! Window chrome: the header bar, the navigation rail, the status bar and the page title block.

use super::theme::{self, Palette};
use super::widgets::{self, draw_icon, Icon};
use super::{fmt_clock, fmt_hms, kit, App, Page, HEADER_H, RAIL_W, STATUS_H, TOP_BAR_H};
use crate::metrics::Snapshot;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Id, Key, Rect, Response, Sense, Stroke, ViewportCommand};
use std::time::{Duration, Instant};

/// Window width below which the header drops the live readouts.
const READOUTS_MIN_W: f32 = 1100.0;
const RAIL_ITEM: (f32, f32) = (60.0, 46.0);
const RAIL_GAP: f32 = 2.0;
const RAIL_MIN_STEP: f32 = 38.0;

/// "HOST · user" from the environment (Windows: `COMPUTERNAME`, `USERNAME`; elsewhere
/// `HOSTNAME`, `USER`); empty parts are skipped.
pub(super) fn machine_line() -> String {
    let var = |names: &[&str]| names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()));
    join_machine(var(&["COMPUTERNAME", "HOSTNAME"]), var(&["USERNAME", "USER"]))
}

fn join_machine(host: Option<String>, user: Option<String>) -> String {
    [host, user].into_iter().flatten().collect::<Vec<_>>().join(" \u{b7} ")
}

/// A 40 x 40 icon button of the header: faint icon, bright on hover, `selection` fill when `on`.
fn icon_button(ui: &mut egui::Ui, rect: Rect, id: &str, icon: Icon, pal: &Palette, on: bool, tip: &str) -> Response {
    let resp = ui.interact(rect, ui.id().with(id), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 6.0, pal.selection);
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, pal.raise(0.06));
    }
    let color = if on { pal.accent } else if resp.hovered() { pal.text } else { pal.text_faint };
    draw_icon(p, Rect::from_center_size(rect.center(), vec2(18.0, 18.0)), icon, color);
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl App {
    /// The 64 px header bar: logo and machine name, global search, live readouts, and the pin,
    /// theme and overflow buttons.
    pub(super) fn header_bar(&mut self, ctx: &egui::Context, pal: &Palette, snap: &Snapshot) {
        let focus = std::mem::take(&mut self.focus_search);
        let frame = egui::Frame::none();
        egui::TopBottomPanel::top("header").exact_height(TOP_BAR_H).frame(frame).show_separator_line(false).show(ctx, |ui| {
            let full = ui.max_rect();
            let p = ui.painter().clone();
            let cy = full.top() + 16.0 + 20.0;
            // Left: mark, wordmark, machine.
            let mut left = full.left() + 28.0;
            widgets::logo(&p, pos2(left + 14.0, cy), 28.0, pal.accent);
            left += 28.0 + 12.0;
            left = widgets::tracked_text(&p, pos2(left, cy), Align2::LEFT_CENTER, "TMOP", theme::semibold(15.0), pal.text, 0.18).right() + 12.0;
            let machine_w = if self.machine.is_empty() { 0.0 } else { widgets::text_width(&p, &self.machine, theme::ui(12.0)) };
            // Right: buttons, then the readouts.
            let mut right = full.right() - 28.0;
            let button = |x: f32| Rect::from_min_size(pos2(x, cy - 20.0), vec2(40.0, 40.0));
            let dots_rect = button(right - 40.0);
            let theme_rect = button(right - 84.0);
            let pin_rect = button(right - 128.0);
            right -= 128.0 + 14.0;
            let mut readouts: Vec<(&str, String)> = vec![("CPU", format!("{:.0}%", self.smooth.cpu)), ("RAM", format!("{:.0}%", self.smooth.memory)), ("DISK", format!("{:.0}%", self.smooth.disk))];
            if snap.gpu.available {
                readouts.push(("GPU", format!("{:.0}%", self.smooth.gpu)));
            }
            let show_readouts = full.width() >= READOUTS_MIN_W;
            if show_readouts {
                let mut x = right;
                for (label, value) in readouts.iter().rev() {
                    let vw = widgets::text_width(&p, value, theme::mono_medium(12.0));
                    x -= vw;
                    p.text(pos2(x, cy), Align2::LEFT_CENTER, value, theme::mono_medium(12.0), pal.text);
                    let lw = widgets::text_width(&p, label, theme::mono(12.0)) + 6.0;
                    x -= lw;
                    p.text(pos2(x, cy), Align2::LEFT_CENTER, label, theme::mono(12.0), pal.text_faint);
                    x -= 14.0;
                }
                right = x;
            }
            // The machine line yields first when space gets short, then the search narrows.
            let mut avail = right - 20.0 - (left + machine_w + 20.0);
            let show_machine = machine_w > 0.0 && avail >= 260.0;
            if show_machine {
                p.text(pos2(left, cy), Align2::LEFT_CENTER, &self.machine, theme::ui(12.0), pal.text_faint);
                left += machine_w + 20.0;
            } else {
                left += 8.0;
                avail = right - 20.0 - left;
            }
            let w = avail.clamp(0.0, 520.0);
            if w >= 140.0 {
                let search = Rect::from_min_size(pos2(left + (avail - w) / 2.0, cy - 20.0), vec2(w, 40.0));
                p.rect_filled(search, 6.0, pal.control);
                p.rect_stroke(search, 6.0, Stroke::new(1.0_f32, pal.control_stroke));
                draw_icon(&p, Rect::from_center_size(pos2(search.left() + 22.0, cy), vec2(16.0, 16.0)), Icon::Search, pal.text_faint);
                let hint_w = if w >= 300.0 { widgets::key_hint(&p, pos2(search.right() - 14.0 - widgets::text_width(&p, "Ctrl K", theme::mono(11.0)) - 12.0, cy), "Ctrl K", pal).width() + 14.0 } else { 0.0 };
                let edit = Rect::from_min_max(pos2(search.left() + 38.0, cy - 12.0), pos2(search.right() - 12.0 - hint_w, cy + 12.0));
                let id = Id::new("global-search");
                let resp = ui.put(
                    edit,
                    egui::TextEdit::singleline(&mut self.filter)
                        .id(id)
                        .hint_text(egui::RichText::new("Search processes by name, PID, user, description").color(pal.text_faint))
                        .frame(false)
                        .font(theme::ui(14.0))
                        .vertical_align(egui::Align::Center)
                        .desired_width(edit.width()),
                );
                if focus {
                    resp.request_focus();
                }
                if resp.changed() && self.page != Page::Processes {
                    self.page = Page::Processes;
                }
            }
            // Pin: always on top.
            let on_top = self.settings.window.always_on_top;
            if icon_button(ui, pin_rect, "hdr-pin", Icon::Pin, pal, on_top, if on_top { "Always on top: on" } else { "Always on top" }).clicked() {
                self.settings.window.always_on_top = !on_top;
                self.settings_dirty = true;
            }
            // Theme: the other polarity.
            let (icon, polarity) = if pal.dark { (Icon::Moon, "light") } else { (Icon::Sun, "dark") };
            // The button always lands on the built-in theme of the other polarity.
            let tip = match self.settings.appearance.theme.as_str() {
                "dark" | "light" => format!("Switch to {polarity} theme"),
                _ => format!("Switch to the built-in {polarity} theme (your palette stays available in Settings)"),
            };
            if icon_button(ui, theme_rect, "hdr-theme", icon, pal, false, &tip).clicked() {
                self.settings.appearance.theme = if pal.dark { "light" } else { "dark" }.into();
                self.settings_dirty = true;
                self.palette_ui.note_theme_change(&self.palettes);
            }
            // Overflow menu.
            let dots = icon_button(ui, dots_rect, "hdr-dots", Icon::Dots, pal, false, "More");
            let menu_id = ui.id().with("hdr-menu");
            if dots.clicked() {
                ui.memory_mut(|m| m.toggle_popup(menu_id));
            }
            let mut close = false;
            egui::popup_below_widget(ui, menu_id, &dots, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(240.0);
                if kit::menu_item(ui, "Refresh now", Some("F5"), false) {
                    self.sampler.refresh();
                    close = true;
                }
                close |= self.replace_menu_item(ui);
                let label = if self.popout_open { "Popout: On" } else { "Popout: Off" };
                if kit::menu_item(ui, label, None, false) {
                    let open = !self.popout_open;
                    self.set_popout(open);
                    close = true;
                }
                ui.separator();
                ui.add_space(2.0);
                // A top-down, unjustified layout: the popup's own would spread the words of these lines.
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    let line = |ui: &mut egui::Ui, text: egui::RichText| ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
                    line(ui, egui::RichText::new(format!("{} ({})", crate::APP_NAME, crate::APP_LONG_NAME)).font(theme::semibold(13.0)).color(pal.text));
                    line(ui, egui::RichText::new("A skinnable system monitor for Windows.").font(theme::ui(12.0)).color(pal.text_dim));
                    line(ui, egui::RichText::new("No network access, no licensing, no telemetry.").font(theme::ui(12.0)).color(pal.text_dim));
                });
                ui.separator();
                if kit::menu_item(ui, "Exit", None, false) {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
            if close {
                ui.memory_mut(|m| m.close_popup());
            }
        });
    }

    /// One rail entry (60 x 46): icon above a tracked 10 px label. Returns the response.
    #[allow(clippy::too_many_arguments)]
    fn rail_item(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, hit: Rect, id: &str, icon: Icon, label: &str, active: bool, pal: &Palette) -> Response {
        let resp = ui.interact(hit, ui.id().with(id), Sense::click());
        if active {
            p.rect_filled(rect, 8.0, pal.selection);
            p.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.top() + 5.0), pos2(rect.left() + 2.0, rect.bottom() - 5.0)), 1.0, pal.accent);
        } else if resp.hovered() {
            p.rect_filled(rect, 8.0, pal.hover);
        }
        let color = if active {
            pal.accent
        } else if resp.hovered() {
            pal.text_dim
        } else {
            pal.text_rail()
        };
        // Squeezed entries (a short window) get a smaller icon so the label still fits below it.
        let glyph = if rect.height() >= 44.0 { 22.0 } else { 20.0 };
        draw_icon(p, Rect::from_center_size(pos2(rect.center().x, rect.top() + 3.5 + glyph / 2.0), vec2(glyph, glyph)), icon, color);
        let font = if active { theme::semibold(10.0) } else { theme::ui(10.0) };
        widgets::tracked_text(p, pos2(rect.center().x, rect.bottom() - 4.0), Align2::CENTER_BOTTOM, label, font, color, 0.04);
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    /// The 84 px navigation rail: every page, then Settings and the popout toggle at the bottom.
    pub(super) fn rail(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::left("rail").exact_width(RAIL_W).resizable(false).frame(egui::Frame::none()).show_separator_line(false).show(ctx, |ui| {
            let full = ui.max_rect();
            let p = ui.painter().clone();
            p.line_segment([pos2(full.right() - 0.5, full.top()), pos2(full.right() - 0.5, full.bottom())], Stroke::new(1.0_f32, pal.sidebar_rule));
            let x = full.left() + (RAIL_W - RAIL_ITEM.0) / 2.0 - 0.5;
            let pages = self.visible_pages();
            // Every entry gets the same pitch: the full size when the window is tall enough, squeezed down to
            // RAIL_MIN_STEP when it isn't; below that the page list scrolls.
            let fit = (full.height() - 12.0 - 16.0 - 8.0) / (pages.len() + 2) as f32;
            let step = fit.clamp(RAIL_MIN_STEP, RAIL_ITEM.1 + RAIL_GAP);
            let item_h = step - RAIL_GAP;
            let bottom_top = full.bottom() - 16.0 - 2.0 * step + RAIL_GAP;
            let nav_rect = Rect::from_min_max(pos2(full.left(), full.top() + 12.0), pos2(full.right(), (bottom_top - 8.0).max(full.top() + 12.0)));
            let nav_h = pages.len() as f32 * step;
            let scroll_id = ui.id().with("nav-scroll");
            let max_scroll = (nav_h - nav_rect.height()).max(0.0);
            let mut scroll = ui.data(|d| d.get_temp::<f32>(scroll_id)).unwrap_or(0.0);
            if max_scroll > 0.0 && ui.rect_contains_pointer(nav_rect) {
                scroll -= ui.input(|i| i.smooth_scroll_delta.y);
            }
            // A page picked elsewhere (shortcut, link) is scrolled into view.
            let seen_id = ui.id().with("nav-seen");
            if ui.data(|d| d.get_temp::<Page>(seen_id)) != Some(self.page) {
                ui.data_mut(|d| d.insert_temp(seen_id, self.page));
                if let Some(i) = pages.iter().position(|&q| q == self.page) {
                    let (top, bottom) = (i as f32 * step, i as f32 * step + item_h);
                    scroll = scroll.min(top).max(bottom - nav_rect.height());
                }
            }
            scroll = scroll.clamp(0.0, max_scroll);
            ui.data_mut(|d| d.insert_temp(scroll_id, scroll));
            let clip = p.with_clip_rect(nav_rect);
            let mut y = nav_rect.top() - scroll;
            for page in pages {
                let rect = Rect::from_min_size(pos2(x, y), vec2(RAIL_ITEM.0, item_h));
                y += step;
                let hit = rect.intersect(nav_rect);
                if !hit.is_positive() {
                    continue;
                }
                let resp = Self::rail_item(ui, &clip, rect, hit, page.id(), page.icon(), page.rail_label(), self.page == page, pal);
                let tip = if page.shortcut().is_empty() { page.title().to_string() } else { format!("{}  {}", page.title(), page.shortcut()) };
                if resp.on_hover_text(tip).clicked() {
                    self.page = page;
                }
            }
            let settings = Rect::from_min_size(pos2(x, bottom_top), vec2(RAIL_ITEM.0, item_h));
            let resp = Self::rail_item(ui, &p, settings, settings, "nav-settings", Icon::Gear, "Settings", self.page == Page::Settings, pal);
            if resp.on_hover_text(format!("Settings  {}", Page::Settings.shortcut())).clicked() {
                self.page = Page::Settings;
            }
            let popout = Rect::from_min_size(pos2(x, bottom_top + step), vec2(RAIL_ITEM.0, item_h));
            let resp = Self::rail_item(ui, &p, popout, popout, "nav-popout", Icon::Popout, "Popout", self.popout_open, pal);
            if resp.on_hover_text(if self.popout_open { "Close the popout" } else { "Open the popout" }).clicked() {
                let open = !self.popout_open;
                self.set_popout(open);
            }
        });
    }

    pub(super) fn status_bar(&mut self, ctx: &egui::Context, pal: &Palette, snap: &Snapshot) {
        // Transient messages fade out after a while; the provider line stays.
        if self.status != self.status_seen.0 {
            self.status_seen = (self.status.clone(), Instant::now());
        }
        if self.ip.1.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true) {
            self.ip = (crate::pro::connections::primary_ipv4().map(|a| a.to_string()), Some(Instant::now()));
        }
        let st = theme::status(pal);
        egui::TopBottomPanel::bottom("status").exact_height(STATUS_H).frame(egui::Frame::none()).show_separator_line(false).show(ctx, |ui| {
            let r = ui.max_rect();
            let p = ui.painter();
            p.line_segment([pos2(r.left(), r.top() + 0.5), pos2(r.right(), r.top() + 0.5)], Stroke::new(1.0_f32, pal.sidebar_rule));
            let healthy = snap.errors.is_empty();
            let dot = if healthy { st.ok } else { st.warn };
            let mut x = r.left() + 16.0;
            p.circle_filled(pos2(x, r.center().y), 3.0, dot);
            x += 10.0;
            let mut parts: Vec<(String, Color32)> = vec![
                (if healthy { "Native providers healthy".to_string() } else { snap.errors[0].clone() }, pal.text_faint),
                (format!("{} processes", snap.process_count), pal.text_faint),
            ];
            if let Some(ip) = &self.ip.0 {
                parts.push((format!("IP {ip}"), pal.accent));
            }
            parts.push((format!("Generation {}", snap.seq), pal.text_faint));
            if self.settings.window.uptime_visible {
                parts.push((format!("Up {}", fmt_hms(snap.uptime)), pal.text_faint));
            }
            if !self.status_seen.0.is_empty() && self.status_seen.1.elapsed() < Duration::from_secs(10) {
                parts.push((self.status_seen.0.clone(), pal.text));
            }
            let right_reserved = 300.0;
            let clip = p.with_clip_rect(Rect::from_min_max(r.min, pos2(r.right() - right_reserved, r.bottom())));
            for (i, (s, c)) in parts.iter().enumerate() {
                if i > 0 {
                    clip.text(pos2(x, r.center().y), Align2::LEFT_CENTER, "\u{b7}", theme::mono(11.0), pal.text_faint);
                    x += 12.0;
                }
                let rr = clip.text(pos2(x, r.center().y), Align2::LEFT_CENTER, s, theme::mono(11.0), *c);
                x += rr.width() + 10.0;
            }
            // Recorder transport and clock.
            let rec = self.recorder.is_recording();
            let clock = match self.recorder.elapsed() {
                Some(d) => fmt_clock(d),
                None => "00:00:00.00".to_string(),
            };
            let h = 15.0;
            let cw = widgets::seven_segment_width(&clock, h);
            let lcd = Rect::from_min_size(pos2(r.right() - 14.0 - cw - 14.0, r.center().y - 11.0), vec2(cw + 14.0, 22.0));
            let lcd_pal = pal.lcd();
            p.rect_filled(lcd, 3.0, lcd_pal.bg);
            p.rect_stroke(lcd, 3.0, Stroke::new(1.0_f32, lcd_pal.rim));
            let lit = if rec { Color32::from_rgb(0xFF, 0x6A, 0x6A) } else { lcd_pal.lit };
            widgets::seven_segment(p, pos2(lcd.left() + 7.0, lcd.top() + 3.5), &clock, h, lit, lcd_pal.unlit, if rec { self.bloom() } else { 0.0 });
            let lap = pos2(lcd.left() - 18.0, r.center().y);
            p.circle_filled(lap, 3.5, pal.control_stroke);
            let stop = Rect::from_center_size(pos2(lap.x - 34.0, r.center().y), vec2(24.0, 20.0));
            let stop_resp = ui.interact(stop, ui.id().with("status-stop"), Sense::click());
            let p = ui.painter();
            p.rect_filled(stop, 3.0, if stop_resp.hovered() && rec { pal.hover } else { pal.control });
            p.rect_filled(Rect::from_center_size(stop.center(), vec2(9.0, 9.0)), 1.0, if rec { pal.text } else { pal.control_stroke });
            p.line_segment([pos2(stop.right() + 9.5, r.top() + 6.0), pos2(stop.right() + 9.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            let ball = pos2(stop.left() - 26.0, r.center().y);
            let rec_resp = ui.interact(Rect::from_center_size(ball, vec2(22.0, 22.0)), ui.id().with("status-rec"), Sense::click());
            let p = ui.painter();
            let base = if rec { Color32::from_rgb(0xE0, 0x20, 0x20) } else { Color32::from_rgb(0x6A, 0x18, 0x18) };
            if rec && self.bloom() > 0.0 {
                p.circle_filled(ball, 13.0, theme::with_alpha(base, 0.22 * self.bloom()));
            }
            p.circle_filled(ball, 8.0, theme::dim(base, 0.75));
            p.circle_filled(ball + vec2(-1.0, -1.0), 6.5, base);
            p.circle_filled(ball + vec2(-2.5, -2.5), 2.5, theme::with_alpha(Color32::WHITE, if rec { 0.45 } else { 0.18 }));
            if rec_resp.hovered() {
                p.circle_stroke(ball, 9.5, Stroke::new(1.0_f32, pal.text_dim));
            }
            p.line_segment([pos2(ball.x - 17.5, r.top() + 6.0), pos2(ball.x - 17.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            p.line_segment([pos2(ball.x + 17.5, r.top() + 6.0), pos2(ball.x + 17.5, r.bottom() - 6.0)], Stroke::new(1.0_f32, pal.panel_stroke));
            if rec_resp.clicked() && !rec {
                if let Err(e) = self.recorder.start(None) {
                    self.status = format!("Recording: {e}");
                }
            }
            if stop_resp.clicked() && rec {
                if let Some(path) = self.recorder.stop() {
                    self.status = format!("Recording saved to {}", path.display());
                }
            }
            rec_resp.on_hover_text(if rec { "Recording" } else { "Start recording to the recordings folder" });
            stop_resp.on_hover_text("Stop recording");
        });
    }

    /// Page title row inside the content area (the title only; `icon` is not drawn any more).
    /// Returns the rect left for the page body.
    pub fn page_header(&self, ui: &mut egui::Ui, pal: &Palette, title: &str, _icon: Icon) -> Rect {
        self.page_title(ui, pal, title, "")
    }

    /// Page title block: the title in Light 30 and, below it, a mono 12 `subtitle` (live facts such
    /// as "312 services . Ctrl+8"). Returns the rect left for the page body.
    pub fn page_title(&self, ui: &mut egui::Ui, pal: &Palette, title: &str, subtitle: &str) -> Rect {
        let full = ui.max_rect();
        let p = ui.painter();
        let x = full.left() + 12.0;
        if subtitle.is_empty() {
            p.text(pos2(x, full.top() + HEADER_H / 2.0), Align2::LEFT_CENTER, title, theme::light(30.0), pal.text);
        } else {
            p.text(pos2(x, full.top() + 1.0), Align2::LEFT_TOP, title, theme::light(30.0), pal.text);
            widgets::text_fit(p, pos2(x + 1.0, full.top() + 44.0), Align2::LEFT_TOP, subtitle, theme::mono(12.0), pal.text_faint, (full.width() - 400.0).max(200.0));
        }
        Rect::from_min_max(pos2(full.left(), full.top() + HEADER_H), full.max)
    }

    /// Ctrl+K focuses the header search on the next frame.
    pub(super) fn search_shortcut(ctx: &egui::Context) -> bool {
        ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(Key::K))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_line_skips_empty_parts() {
        assert_eq!(join_machine(Some("PC-1".into()), Some("alex".into())), "PC-1 \u{b7} alex");
        assert_eq!(join_machine(Some("PC-1".into()), None), "PC-1");
        assert_eq!(join_machine(None, Some("alex".into())), "alex");
        assert_eq!(join_machine(None, None), "");
    }
}
