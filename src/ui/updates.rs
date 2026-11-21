//! The update check in the UI: the Settings rows (General), the start-up check and the "new
//! version" dialog. The network and file work is in [`crate::update`].

use super::kit::{self, DialogButton};
use super::settings_page::{hint, note, toggle_row_hint};
use super::theme::{self, Palette};
use super::widgets::{self, Button, ButtonSize, Icon};
use super::App;
use crate::pro::Loader;
use crate::settings::General;
use crate::update::{self, Method, Release};
use eframe::egui::{self, RichText};
use super::widgets::ScrollArea029 as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TOGGLE_HINT: &str = "Off by default. When TMOP starts, and while it keeps running (also in the notification area), at most once a day it asks GitHub for the newest release and shows what's new. Nothing else is sent.";

#[derive(Default)]
pub struct UpdateUi {
    check: Loader<Option<Release>>,
    /// The running check was asked for in Settings: report "up to date" and errors, and offer
    /// even a skipped version.
    manual: bool,
    /// The result of the last check in this session, shown in Settings.
    result: Option<String>,
    /// The release the dialog offers.
    offer: Option<Release>,
    /// The Windows update in progress; on success the exe to start.
    install: Loader<PathBuf>,
    /// Download progress of `install` (bytes done, total).
    progress: Arc<Mutex<(u64, u64)>>,
    /// When [`UpdateUi::check_on_start`] last looked whether a check is due.
    looked_at: Option<Instant>,
}

/// How often a running TMOP (one that stays in the notification area for days) looks whether
/// the daily check is due.
pub const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);

/// Whether it is time to look again (never looked: yes).
fn look_due(looked_at: Option<Instant>, now: Instant) -> bool {
    looked_at.is_none_or(|t| now.saturating_duration_since(t) >= LOOK_EVERY)
}

impl UpdateUi {
    /// Starts a check of `general`'s update repository unless one is running.
    pub fn start_check(&mut self, general: &General, manual: bool) {
        if self.check.is_loading() {
            return;
        }
        self.manual = manual;
        let repo = update::repo(&general.update_repo).to_string();
        self.check.start(move || update::check(&repo));
    }

    /// The start-up check: only when switched on and a day has passed since the last one.
    pub fn check_on_start(&mut self, general: &General) {
        self.looked_at = Some(Instant::now());
        if general.check_for_updates && update::due(general.update_last_check, update::now_secs()) {
            self.start_check(general, false);
        }
    }

    /// While TMOP runs, shown or hidden in the notification area: once an hour it looks whether
    /// the daily check is due, so a TMOP that never restarts still checks once a day.
    pub fn check_while_running(&mut self, general: &General) {
        if look_due(self.looked_at, Instant::now()) {
            self.check_on_start(general);
        }
    }

    fn start_install(&mut self, release: Release) {
        let progress = self.progress.clone();
        *progress.lock().unwrap_or_else(|e| e.into_inner()) = (0, 0);
        self.install.start(move || install(&release, progress));
    }
}

#[cfg(windows)]
fn install(release: &Release, progress: Arc<Mutex<(u64, u64)>>) -> Result<PathBuf, String> {
    update::install(release, move |done, total| {
        if let Ok(mut p) = progress.lock() {
            *p = (done, total);
        }
    })
}

#[cfg(not(windows))]
fn install(_: &Release, _: Arc<Mutex<(u64, u64)>>) -> Result<PathBuf, String> {
    Err("TMOP updates itself only on Windows".into())
}

/// The rows in Settings → General. Returns true when a setting changed.
pub fn settings_rows(ui: &mut egui::Ui, pal: &Palette, general: &mut General, u: &mut UpdateUi) -> bool {
    let mut changed = toggle_row_hint(ui, pal, &mut general.check_for_updates, "Check for updates when TMOP starts", TOGGLE_HINT);
    if changed {
        u.check_on_start(general);
    }
    ui.add_space(4.0);
    let busy = u.check.is_loading();
    let mut now = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        ui.add_enabled_ui(!busy, |ui| now = Button::new("Check now").size(ButtonSize::Medium).icon(Icon::Refresh).show(ui, pal).clicked());
        let text = if busy { "Asking GitHub\u{2026}".to_string() } else { u.result.clone().unwrap_or_default() };
        if !text.is_empty() {
            note(ui, pal, &text);
        }
    });
    if now {
        u.start_check(general, true);
    }
    changed |= update_source_row(ui, pal, general);
    ui.add_space(8.0);
    changed
}

/// "Update source": the GitHub repository updates come from, for forks of TMOP.
fn update_source_row(ui: &mut egui::Ui, pal: &Palette, general: &mut General) -> bool {
    ui.add_space(6.0);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        ui.label(RichText::new("Update source").font(theme::ui(13.0)).color(pal.text));
        let edit = widgets::text_edit(egui::TextEdit::singleline(&mut general.update_repo).hint_text(update::RELEASE_REPO).desired_width(220.0 + 8.0).font(theme::mono(12.0)), true);
        changed = ui.add(edit).changed();
        if !general.update_repo.trim().is_empty() && Button::ghost("Reset").size(ButtonSize::Small).show(ui, pal).clicked() {
            general.update_repo.clear();
            changed = true;
        }
    });
    let entry = general.update_repo.trim();
    let text = if entry.is_empty() {
        format!("The GitHub repository (owner/name) TMOP takes updates from. Empty: {}, which this copy was released from. Only enter a fork you trust: TMOP installs what it publishes.", update::RELEASE_REPO)
    } else if update::valid_repo(entry) {
        format!("Updates come from github.com/{entry}. Only enter a fork you trust: TMOP installs what it publishes.")
    } else {
        format!("Not a GitHub repository (owner/name), so updates come from {}.", update::RELEASE_REPO)
    };
    hint(ui, pal, &text);
    if changed {
        // Another repository has other releases: the daily check starts over, and a version
        // skipped in the old one isn't hidden in the new one.
        general.update_last_check = 0;
        general.update_skipped.clear();
    }
    changed
}

/// Formats bytes as MB with one decimal.
fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

impl App {
    /// Per-frame work: finished checks and installs.
    pub fn updates_poll(&mut self, ctx: &egui::Context) {
        let u = &mut self.updates;
        if u.check.poll() {
            match u.check.get() {
                Some(Ok(found)) => {
                    self.settings.general.update_last_check = update::now_secs();
                    self.settings_dirty = true;
                    u.result = Some(match found {
                        Some(r) => format!("TMOP {} is available.", r.version),
                        None => format!("TMOP {} is the newest version.", update::current_version()),
                    });
                    if let Some(r) = found {
                        if (u.manual || r.tag != self.settings.general.update_skipped) && !u.install.is_loading() {
                            u.offer = Some(r.clone());
                            // A fresh offer, without an earlier attempt's error.
                            u.install = Loader::default();
                        }
                    }
                }
                Some(Err(e)) => u.result = Some(format!("Update check failed: {e}")),
                None => {}
            }
        }
        // A failed install stays in the dialog, which shows the error.
        let installed = if u.install.poll() { u.install.ok().cloned() } else { None };
        if let Some(exe) = installed {
            self.save_settings();
            match std::process::Command::new(&exe).arg(crate::tray::REPLACING_FLAG).spawn() {
                Ok(_) => self.quit(ctx),
                Err(e) => {
                    self.updates.offer = None;
                    self.status = format!("TMOP is updated; start it again to use the new version ({e})");
                }
            }
        }
        if self.updates.check.is_loading() || self.updates.install.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    /// The "new version" dialog.
    pub fn updates_dialog(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(release) = self.updates.offer.clone() else { return };
        let method = update::method(&release.repo);
        let installing = self.updates.install.is_loading();
        let error = self.updates.install.error().map(str::to_string);
        let (done, total) = *self.updates.progress.lock().unwrap_or_else(|e| e.into_inner());
        let notes = update::plain_notes(&release.notes);
        let accent = pal.accent;
        let buttons: Vec<DialogButton> = match &method {
            Method::InPlace => vec![DialogButton::Ghost("Skip this version"), DialogButton::Secondary("Later"), DialogButton::Primary(if error.is_some() { "Try again" } else { "Update now" })],
            Method::InstallCommand | Method::Homebrew | Method::Scoop => vec![DialogButton::Ghost("Skip this version"), DialogButton::Secondary("Later"), DialogButton::Primary("Copy command")],
        };
        let answer = kit::dialog(ctx, "update-available", |ui| {
            let body = format!("You have TMOP {}. What's new:", update::current_version());
            kit::dialog_header(ui, pal, Some((Icon::Refresh, accent)), &format!("TMOP **{}** is available", release.version), &body);
            ui.add_space(10.0);
            egui::Frame::new().fill(pal.card).corner_radius(6).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show_029(ui, [false, true], |ui| {
                    let text = if notes.is_empty() { "This release has no notes." } else { notes.as_str() };
                    ui.label(RichText::new(text).font(theme::ui(13.0)).color(pal.text_dim));
                });
            });
            ui.add_space(6.0);
            ui.hyperlink_to(RichText::new("Open the release page").font(theme::ui(13.0)), &release.page);
            if let Some(cmd) = method.command(&release.repo) {
                ui.add_space(8.0);
                ui.label(RichText::new(method.instructions()).font(theme::ui(13.0)).color(pal.text));
                ui.label(RichText::new(cmd).font(theme::mono(12.0)).color(pal.text));
            }
            if installing {
                ui.add_space(8.0);
                let text = if total > 0 { format!("Downloading TMOP {} ({} of {} MB)\u{2026}", release.version, mb(done), mb(total)) } else { format!("Downloading TMOP {}\u{2026}", release.version) };
                ui.label(RichText::new(text).font(theme::ui(13.0)).color(pal.text_dim));
            } else if let Some(e) = &error {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("The update failed: {e}")).font(theme::ui(13.0)).color(theme::status(pal).warn));
            }
            ui.add_space(12.0);
            ui.add_enabled_ui(!installing, |ui| kit::dialog_buttons(ui, pal, &buttons)).inner
        })
        .flatten()
        // Esc is "Later", unless the download is running (its buttons are off then, too).
        .or_else(|| (!installing && super::processes::key_once(ctx, egui::Key::Escape)).then_some(1));
        let Some(i) = answer else { return };
        match i {
            0 => {
                self.settings.general.update_skipped = release.tag.clone();
                self.settings_dirty = true;
                self.updates.offer = None;
            }
            1 => self.updates.offer = None,
            _ => match method {
                Method::InPlace => self.updates.start_install(release),
                Method::InstallCommand | Method::Homebrew | Method::Scoop => {
                    ctx.copy_text(method.command(&release.repo).unwrap_or_default());
                    self.status = "Update command copied: paste it in a terminal".into();
                    self.updates.offer = None;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn megabytes_have_one_decimal() {
        assert_eq!(mb(0), "0.0");
        assert_eq!(mb(15 * 1024 * 1024 + 512 * 1024), "15.5");
    }

    #[test]
    fn a_running_tmop_looks_once_an_hour() {
        let t0 = Instant::now();
        assert!(look_due(None, t0));
        assert!(!look_due(Some(t0), t0 + Duration::from_secs(59 * 60)));
        assert!(look_due(Some(t0), t0 + LOOK_EVERY));
        // With the switch off nothing is asked, also later.
        let mut u = UpdateUi::default();
        u.check_while_running(&General::default());
        assert!(u.looked_at.is_some() && !u.check.is_loading());
    }

    #[test]
    fn start_up_check_needs_the_switch() {
        let mut u = UpdateUi::default();
        u.check_on_start(&General::default());
        assert!(!u.check.is_loading(), "off by default: no request");
    }
}
