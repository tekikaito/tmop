//! Disk Space "Clean up" view: the category list, its footer and the confirm dialog. The
//! categories and their measuring live in [`crate::pro::cleanup`]; deletion goes through the same
//! classifier, worker, log and skipped list as the manual delete in [`super::disk_delete`].

use super::disk_delete::{apply_report, verdict_lists, JobOut};
use super::diskspace_page::{disk_list, disk_row};
use super::kit::{self, section_card, stat_grid, StatTile};
use super::pro_pages::{confirm_row, list_row, RowLook};
use super::theme::{self, Palette};
use super::widgets::{self, tracked_text, Button, ButtonKind, ButtonSize, Icon};
use super::{App, FrameData};
use crate::pro::cleanup::{self, Category, Measured};
use crate::pro::diskdelete::{self, Assessed, Mode, Report, SystemPaths, Target, Verdict};
use crate::pro::diskspace::Tree;
use crate::pro::volcache::{self, Handler};
use crate::pro::{trash, Loader};
use crate::platform::{elevation_word, os_name, trash_word};
use crate::process::{format_bytes, ProcessRow};
use eframe::egui::{self, pos2, vec2, Align2, Rect, RichText, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const ROW_H: f32 = 56.0;
const LIST_ROW_H: f32 = 22.0;
const RULES_MAX_AGE: Duration = Duration::from_secs(120);

/// Analyze (treemap and lists) or Clean up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiskMode {
    #[default]
    Analyze,
    Cleanup,
}

/// Result of the background measuring of the fixed locations.
pub struct Fixed {
    pub sys: SystemPaths,
    pub measured: Vec<Measured>,
}

/// One checked category going into the dialog.
#[derive(Clone)]
pub struct CatSel {
    pub cat: Category,
    pub targets: Vec<Target>,
}

#[derive(Clone)]
pub struct CleanAssess {
    pub id: u64,
    /// Assessed items per `CleanPending::cats` entry, same order. Items of the Recycle Bin / Trash
    /// categories on a volume without one are Blocked with the reason (skipped, never deleted
    /// permanently instead).
    pub items: Vec<Vec<Assessed>>,
    /// Every bin those items go to has a size limit on record (else large items may be deleted for good).
    pub bin_capped: bool,
}

pub struct CleanPending {
    pub id: u64,
    pub cats: Vec<CatSel>,
    /// Checked Windows Disk Cleanup handlers (purged by Windows, not through the classifier).
    pub vols: Vec<Handler>,
    pub drive: String,
    /// Bytes in the Recycle Bin when "Empty Recycle Bin" is checked.
    pub bin_bytes: Option<u64>,
    pub ack: bool,
    pub assessed: Option<CleanAssess>,
    /// Scan tree version the tree-based items were taken from.
    pub epoch: u64,
    procs: Vec<(u32, String, Option<String>)>,
}

#[derive(Default)]
pub struct CleanupUi {
    pub fixed: Loader<Fixed>,
    /// The fixed locations were measured (cleared to measure again).
    pub started: bool,
    /// Old Downloads and developer artifacts, with the scan tree version they were measured for.
    pub tree_m: Loader<(u64, Vec<Measured>)>,
    pub tree_for: Option<u64>,
    pub checked: HashMap<Category, bool>,
    pub expanded: HashSet<Category>,
    pub pending: Option<CleanPending>,
    pub assess: Loader<CleanAssess>,
    /// Windows Disk Cleanup handlers of the system drive (helper process), measured with `fixed`.
    pub vols: Loader<Vec<Handler>>,
    pub vol_checked: HashMap<String, bool>,
    /// The handlers must be queried again; done as soon as no query is running. The previous answer stays
    /// in place meanwhile (the Loader keeps its value), so covered categories do not reappear.
    pub vols_dirty: bool,
    /// Set after a Clean up: measure again only the handlers it purged (the rest did not change); a full
    /// query takes seconds and keeps "Clean up selected" disabled meanwhile.
    pub vols_only: Option<Vec<String>>,
    /// Handlers the running partial query measures again (see `poll_vols`).
    vols_checking: Vec<String>,
    next_id: u64,
}

/// What a finished Clean up hands back to the page.
#[derive(Clone)]
pub struct CleanDone {
    /// Handlers to query again: the purged ones plus the Recycle Bin (recycled items land there).
    pub requery: Vec<String>,
    /// The Recycle Bin read after the job (on the worker thread; it can be slow).
    pub recycle_bin: Measured,
}

impl CleanupUi {
    /// After a Clean up: drop what it deleted from the fixed locations instead of measuring them all again
    /// (seconds), take the Recycle Bin read by the job, and query only the handlers it changed (only with
    /// `volcache`: elsewhere no handler query runs that would clear the flag again).
    pub fn after_clean(&mut self, report: &Report, done: CleanDone, volcache: bool) {
        let gone: HashSet<&std::path::Path> = report.deleted.iter().map(|t| t.path.as_path()).collect();
        let loading = self.fixed.is_loading();
        match self.fixed.ok_mut() {
            // A measurement still running (Refresh during the job) may predate the deletes: measure again after it.
            Some(_) if loading => self.started = false,
            Some(f) => {
                for m in &mut f.measured {
                    if m.cat == Category::RecycleBin {
                        *m = done.recycle_bin.clone();
                    } else {
                        m.forget(&gone);
                    }
                }
            }
            None => self.started = false,
        }
        if volcache {
            self.vols_only = Some(done.requery);
            self.vols_dirty = true;
        }
    }

    /// A measurement or handler query is due or running, so a Clean up would act on stale sizes.
    pub fn measuring(&self, volcache: bool) -> bool {
        (volcache && self.vols_dirty) || self.vols.is_loading() || self.fixed.is_loading()
    }

    /// Polls the handler query. When a partial one leaves a handler that covers a category failed or missing,
    /// that category shows again with sizes from before the purge, so the fixed locations are measured again.
    pub fn poll_vols(&mut self) {
        if !self.vols.poll() {
            return;
        }
        let checked = std::mem::take(&mut self.vols_checking);
        let all = self.vols.ok().map(Vec::as_slice).unwrap_or_default();
        let failed = |id: &String| !volcache::covers(id).is_empty() && all.iter().find(|h| h.id.eq_ignore_ascii_case(id)).is_none_or(|h| h.error.is_some());
        if checked.iter().any(failed) {
            self.started = false;
        }
    }

    pub fn is_checked(&self, c: Category) -> bool {
        self.checked.get(&c).copied().unwrap_or(c.preselected())
    }

    pub fn is_vol_checked(&self, id: &str) -> bool {
        self.vol_checked.get(id).copied().unwrap_or_else(|| volcache::preselected(id))
    }

    /// Categories to show: all, minus those a working Windows handler replaces. When the helper failed
    /// (or has not answered yet) nothing is hidden, so nothing is lost.
    pub fn visible(&self, elevated: bool) -> Vec<Category> {
        let hidden = self.vols.ok().map(|v| volcache::hidden_categories(v, elevated)).unwrap_or_default();
        Category::all().iter().copied().filter(|c| c.supported() && !hidden.contains(c)).collect()
    }

    /// `cat` cannot be selected: root-only rows always, administrator-only ones unless elevated.
    pub fn locked(cat: Category, elevated: bool) -> bool {
        cat.read_only() || (cat.needs_admin() && !elevated)
    }

    /// Handlers shown in the "Windows Disk Cleanup" group.
    pub fn vol_list(&self, elevated: bool) -> Vec<Handler> {
        self.vols.ok().map(|v| volcache::listed(v, elevated)).unwrap_or_default()
    }

    /// Checked handlers a cleanup would purge (administrator-only ones only when elevated).
    pub fn selected_vols(&self, elevated: bool) -> Vec<Handler> {
        self.vol_list(elevated).into_iter().filter(|h| h.size > 0 && (elevated || !h.needs_admin) && self.is_vol_checked(&h.id)).collect()
    }

    /// Measurement of `cat`; the scan-based ones only when they match scan version `epoch`.
    pub fn measured(&self, cat: Category, epoch: u64) -> Option<&Measured> {
        if cat.needs_scan() {
            self.tree_m.ok().filter(|(e, _)| *e == epoch).and_then(|(_, v)| v.iter().find(|m| m.cat == cat))
        } else {
            self.fixed.ok().and_then(|f| f.measured.iter().find(|m| m.cat == cat))
        }
    }
}

// ---------------------------------------------------------------- pure helpers

/// (bytes, items) of what a cleanup would take from `m`; items needing administrator are left out
/// unless elevated.
pub fn selectable(m: &Measured, elevated: bool) -> (u64, u64) {
    if m.cat == Category::RecycleBin {
        return (m.bytes, m.count);
    }
    m.items.iter().filter(|i| elevated || !i.needs_admin).fold((0, 0), |(b, n), i| (b + i.size, n + 1))
}

/// Destination of a Windows Disk Cleanup handler in the confirm dialog.
pub const VOL_DEST: &str = "removed by Windows Disk Cleanup (not recoverable)";

/// (destination text, permanent) of a category in the confirm dialog. `capped`: the Recycle Bin has a size
/// limit on record, otherwise Windows may delete large items for good and the text must not promise a restore.
/// Items without a bin are not counted here: they are skipped (see [`CleanAssess::items`]).
pub fn dest_text(cat: Category, capped: bool) -> (String, bool) {
    match (cat, cat.mode()) {
        (Category::RecycleBin, _) if cfg!(windows) => ("emptied by Windows, permanent".into(), true),
        (Category::RecycleBin, _) => ("emptied, permanent".into(), true),
        (_, Mode::Recycle) if !capped => (format!("moved to the {}; large items may be deleted permanently by {}", trash_word(), os_name()), true),
        (_, Mode::Recycle) => (format!("moved to the {}", trash_word()), false),
        _ => ("deleted permanently".into(), true),
    }
}

/// Footer summary: total bytes and number of categories that have something to clean.
pub fn footer_totals(ms: &[(&Measured, bool)], elevated: bool) -> (u64, usize) {
    let mut total = 0;
    let mut n = 0;
    for (m, checked) in ms {
        let (b, c) = selectable(m, elevated);
        if *checked && (c > 0 || b > 0) {
            total += b;
            n += 1;
        }
    }
    (total, n)
}


fn to_target(i: &cleanup::CleanItem) -> Target {
    Target { path: i.path.clone(), size: i.size, is_dir: i.is_dir, node: i.node, file: i.file }
}

/// Drops the targets that another selected folder already holds (a cache folder that also has a
/// `node_modules` inside, the same folder in two categories), so nothing is counted or deleted
/// twice. The first category to list a path keeps it.
pub fn drop_nested(cats: &mut Vec<CatSel>) {
    let dirs: HashSet<PathBuf> = cats.iter().flat_map(|c| &c.targets).filter(|t| t.is_dir).map(|t| t.path.clone()).collect();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for c in cats.iter_mut() {
        c.targets.retain(|t| t.path.ancestors().skip(1).all(|a| !dirs.contains(a)) && seen.insert(t.path.clone()));
    }
    cats.retain(|c| !c.targets.is_empty());
}

// ---------------------------------------------------------------- page

impl App {
    /// The Clean up view in `rect`. True when the user asked for a scan from the "Run a scan first" state.
    pub fn cleanup_page(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>, tree: Option<&Arc<Tree>>, scanning: bool, elevated: bool) -> bool {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let epoch = self.pro.disk_epoch();
        // Off Windows: no Windows Disk Cleanup handlers, and measuring only until deletes are safe.
        let (volcache_on, delete_safe) = (self.caps.volcache, self.pro.del.delete_safe);
        {
            let c = &mut self.pro.clean;
            c.fixed.poll();
            c.tree_m.poll();
            c.poll_vols();
            if !c.started && !c.fixed.is_loading() {
                c.started = true;
                // Windows Disk Cleanup sizes (helper process), cached until the next Refresh or delete.
                c.vols_dirty = volcache_on;
                c.fixed.start(|| {
                    let sys = diskdelete::system_paths();
                    let mut measured = cleanup::measure_fixed(&sys, SystemTime::now());
                    measured.push(cleanup::measure_recycle_bin());
                    Ok(Fixed { sys, measured })
                });
            }
            if c.vols_dirty && !c.vols.is_loading() && volcache_on {
                c.vols_dirty = false;
                match (c.vols_only.take(), c.vols.ok().cloned()) {
                    (Some(ids), _) if ids.is_empty() => {}
                    (Some(ids), Some(prev)) => c.vols.start({
                        c.vols_checking = ids.clone();
                        move || {
                        let drive = volcache::system_drive();
                        // Should the partial query fail, measure everything rather than keep stale sizes.
                        volcache::query_ids(&drive, &ids).map(|fresh| volcache::merge(prev, fresh)).or_else(|_| volcache::query(&drive))
                        }
                    }),
                    _ => c.vols.start(|| volcache::query(&volcache::system_drive())),
                }
            }
            match tree {
                None => c.tree_for = None,
                Some(t) if !scanning && c.tree_for != Some(epoch) && !c.tree_m.is_loading() => {
                    if let Some(f) = c.fixed.ok() {
                        c.tree_for = Some(epoch);
                        let (t, sys) = (t.clone(), f.sys.clone());
                        c.tree_m.start(move || Ok((epoch, cleanup::measure_from_tree(&t, &sys, SystemTime::now()))));
                    }
                }
                _ => {}
            }
            if c.fixed.is_loading() || c.tree_m.is_loading() || c.vols.is_loading() {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }

        let st = theme::status(&pal);
        let busy = self.pro.clean.fixed.is_loading() || self.pro.clean.tree_m.is_loading() || self.pro.clean.vols.is_loading();
        let content = section_card(&p, rect, &pal, "Clean up", "");
        // Refresh at the right end of the heading row; while measuring, a note beside the title.
        let mut refresh = false;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(rect.right() - 22.0 - 120.0, rect.top() + 10.0), vec2(120.0, 28.0))).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            refresh = Button::new("Refresh").size(ButtonSize::Compact).show(ui, &pal).clicked();
        });
        if busy {
            p.text(pos2(rect.left() + 22.0 + 90.0, rect.top() + 24.0), Align2::LEFT_CENTER, "Measuring\u{2026}", theme::ui(12.0), pal.text_dim);
        }

        let mut toggles: Vec<(Category, bool)> = Vec::new();
        let mut expands: Vec<Category> = Vec::new();
        let mut vol_toggles: Vec<(String, bool)> = Vec::new();
        let mut want_scan = false;
        let c = &self.pro.clean;
        let visible = c.visible(elevated);
        let vol_rows = c.vol_list(elevated);
        let needs = format!("needs {}", elevation_word());

        // What is selected and what there is in total.
        let ms: Vec<(&Measured, bool)> = visible
            .iter()
            .filter(|&&cat| !CleanupUi::locked(cat, elevated))
            .filter_map(|&cat| c.measured(cat, epoch).map(|m| (m, c.is_checked(cat))))
            .collect();
        let (mut total, mut n) = footer_totals(&ms, elevated);
        let sel_vols = c.selected_vols(elevated);
        total += sel_vols.iter().map(|h| h.size).sum::<u64>();
        n += sel_vols.len();
        let everything: Vec<(&Measured, bool)> = ms.iter().map(|(m, _)| (*m, true)).collect();
        let (mut found, mut found_n) = footer_totals(&everything, elevated);
        for h in vol_rows.iter().filter(|h| h.size > 0 && (elevated || !h.needs_admin)) {
            found += h.size;
            found_n += 1;
        }
        let plural = |n: usize| if n == 1 { "category" } else { "categories" };
        let mut selected = StatTile::new("Selected", format_bytes(total as f64), if n == 0 { "Nothing selected".to_string() } else { format!("{n} {}", plural(n)) });
        if found > 0 {
            selected = selected.bar((total as f32 / found as f32).min(1.0), pal.accent);
        }
        let tiles = [selected, StatTile::new("Found in total", format_bytes(found as f64), format!("across {found_n} {}", plural(found_n)))];
        let tiles_h = stat_grid(&p, content, &pal, &tiles, 190.0);
        let foot = Rect::from_min_max(pos2(content.left(), content.bottom() - 40.0), content.max);
        let list = Rect::from_min_max(pos2(content.left(), content.top() + tiles_h + 14.0), pos2(content.right(), foot.top() - 10.0));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
                // Windows Disk Cleanup: the handlers Windows' own Disk Cleanup offers. They replace TMOP's
                // temp, thumbnail, Delivery Optimization, Recycle Bin and error report categories.
                if volcache_on {
                    let (hr, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    let p = ui.painter();
                    tracked_text(p, pos2(hr.left() + 2.0, hr.center().y), Align2::LEFT_CENTER, "WINDOWS DISK CLEANUP", theme::semibold(11.0), pal.text_faint, 0.14);
                    let (msg, col) = if c.vols.is_loading() {
                        ("Measuring\u{2026}".to_string(), pal.text_dim)
                    } else if let Some(e) = c.vols.error() {
                        (format!("Not available: {e}"), st.warn)
                    } else if vol_rows.is_empty() {
                        ("Nothing to clean".to_string(), pal.text_dim)
                    } else {
                        (String::new(), pal.text_dim)
                    };
                    widgets::text_fit(p, pos2(hr.right() - 4.0, hr.center().y), Align2::RIGHT_CENTER, &msg, theme::ui(12.0), col, (hr.width() - 190.0).max(60.0));
                    if let Some(e) = c.vols.error() {
                        hresp.on_hover_text(format!("{e}\nTMOP's own categories are shown instead."));
                    }
                }
                for h in &vol_rows {
                    let locked = h.needs_admin && !elevated;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
                    let resp = if h.description.is_empty() { resp } else { resp.on_hover_text(h.description.as_str()) };
                    list_row(ui.painter(), r, &pal, RowLook::Plain, resp.hovered());
                    let mut v = c.is_vol_checked(&h.id) && !locked;
                    let cb = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(20.0, 20.0));
                    if ui.add_enabled_ui(!locked, |ui| ui.put(cb, egui::Checkbox::without_text(&mut v)).changed()).inner {
                        vol_toggles.push((h.id.clone(), v));
                    }
                    let p = ui.painter();
                    let dim = if locked { pal.text_faint } else { pal.text };
                    let left = r.left() + 46.0;
                    let right_w = 190.0;
                    let text_w = (r.width() - 46.0 - right_w - 34.0).max(60.0);
                    widgets::text_fit(p, pos2(left, r.top() + 18.0), Align2::LEFT_CENTER, &h.name, theme::semibold(13.0), dim, text_w);
                    let detail = h.description.split_whitespace().collect::<Vec<_>>().join(" ");
                    widgets::text_fit(p, pos2(left, r.top() + 38.0), Align2::LEFT_CENTER, &detail, theme::ui(12.0), pal.text_dim, text_w);
                    let rx = r.right() - 34.0;
                    if locked {
                        widgets::text_fit(p, pos2(rx, r.top() + 18.0), Align2::RIGHT_CENTER, "\u{2014}", theme::mono_medium(13.0), dim, right_w);
                        widgets::text_fit(p, pos2(rx, r.top() + 38.0), Align2::RIGHT_CENTER, "needs administrator", theme::ui(12.0), st.warn, right_w);
                    } else {
                        widgets::text_fit(p, pos2(rx, r.top() + 28.0), Align2::RIGHT_CENTER, &format_bytes(h.size as f64), theme::mono_medium(13.0), dim, right_w);
                    }
                }
                if !vol_rows.is_empty() || c.vols.is_loading() || c.vols.error().is_some() {
                    ui.add_space(4.0);
                }
                for cat in visible.iter().copied() {
                    let m = c.measured(cat, epoch);
                    let locked = CleanupUi::locked(cat, elevated);
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                    let can_expand = m.is_some_and(|m| !m.items.is_empty());
                    if resp.clicked() && can_expand {
                        expands.push(cat);
                    }
                    list_row(ui.painter(), r, &pal, RowLook::Plain, resp.hovered());
                    if cat.read_only() {
                        resp.on_hover_text(format!("Only {} can clean this up; TMOP shows its size and leaves it alone", elevation_word()));
                    }
                    let mut v = c.is_checked(cat) && !locked;
                    let cb = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(20.0, 20.0));
                    if ui.add_enabled_ui(!locked, |ui| ui.put(cb, egui::Checkbox::without_text(&mut v)).changed()).inner {
                        toggles.push((cat, v));
                    }
                    let p = ui.painter();
                    let dim = if locked { pal.text_faint } else { pal.text };
                    let left = r.left() + 46.0;
                    let right_w = 190.0;
                    let text_w = (r.width() - 46.0 - right_w - 34.0).max(60.0);
                    widgets::text_fit(p, pos2(left, r.top() + 18.0), Align2::LEFT_CENTER, cat.title(), theme::semibold(13.0), dim, text_w);
                    widgets::text_fit(p, pos2(left, r.top() + 38.0), Align2::LEFT_CENTER, cat.detail(), theme::ui(12.0), pal.text_dim, text_w);
                    // Right block: size and item count, or why there is none.
                    let rx = r.right() - 34.0;
                    let no_scan = cat.needs_scan() && tree.is_none();
                    let (top, sub, sub_col) = if no_scan {
                        ("Run a scan first".to_string(), String::new(), pal.text_dim)
                    } else if cat.needs_scan() && scanning {
                        ("Scanning\u{2026}".to_string(), String::new(), pal.text_dim)
                    } else if let Some(m) = m {
                        if let Some(e) = &m.error {
                            (e.clone(), String::new(), pal.text_dim)
                        } else {
                            let admin_items = m.items.iter().filter(|i| i.needs_admin).count();
                            let (sub, col) = if locked {
                                (needs.clone(), st.warn)
                            } else if admin_items > 0 && !elevated {
                                (format!("{} items \u{2022} {admin_items} need {}", m.count, elevation_word()), st.warn)
                            } else if cat == Category::RecycleBin {
                                (format!("{} items", m.count), pal.text_dim)
                            } else {
                                (format!("{} item{}", m.count, if m.count == 1 { "" } else { "s" }), pal.text_dim)
                            };
                            (format_bytes(m.bytes as f64), sub, col)
                        }
                    } else if locked {
                        ("\u{2014}".to_string(), needs.clone(), st.warn)
                    } else {
                        ("Measuring\u{2026}".to_string(), String::new(), pal.text_dim)
                    };
                    if no_scan && !scanning {
                        let br = Rect::from_min_size(pos2(rx - 92.0, r.center().y - 14.0), vec2(92.0, 28.0));
                        widgets::text_fit(p, pos2(br.left() - 10.0, r.center().y), Align2::RIGHT_CENTER, &top, theme::ui(12.0), pal.text_dim, 140.0);
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(br), |ui| {
                            if Button::new("Scan").icon(Icon::Play).size(ButtonSize::Compact).show(ui, &pal).clicked() {
                                want_scan = true;
                            }
                        });
                    } else {
                        let p = ui.painter();
                        let font = if sub.is_empty() { theme::ui(12.0) } else { theme::mono_medium(13.0) };
                        widgets::text_fit(p, pos2(rx, r.top() + if sub.is_empty() { 28.0 } else { 18.0 }), Align2::RIGHT_CENTER, &top, font, dim, right_w);
                        if !sub.is_empty() {
                            widgets::text_fit(p, pos2(rx, r.top() + 38.0), Align2::RIGHT_CENTER, &sub, theme::ui(12.0), sub_col, right_w);
                        }
                    }
                    if can_expand {
                        let open = c.expanded.contains(&cat);
                        widgets::draw_icon(ui.painter(), Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(12.0, 12.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, pal.text_faint);
                    }
                    if let (Some(m), true) = (m, c.expanded.contains(&cat)) {
                        let n = m.items.len();
                        let h = n.min(8) as f32 * LIST_ROW_H;
                        let (lr, _) = ui.allocate_exact_size(vec2((ui.available_width() - 24.0).max(40.0), h), Sense::hover());
                        let lr = lr.translate(vec2(24.0, 0.0));
                        let biggest = m.items.first().map(|i| i.size).unwrap_or(1).max(1) as f32;
                        disk_list(ui, lr, ("clean-list", cat), &pal, LIST_ROW_H, n, |ui, i, r, _| {
                            let it = &m.items[i];
                            let dim = it.needs_admin && !elevated;
                            disk_row(ui.painter(), r, &pal, &it.path.display().to_string(), dim, it.size, it.size as f32 / biggest, st.info, false, None, None);
                        });
                    }
                }
            });
        });

        // Footer: how the safety rules apply, and the action.
        p.hline(foot.x_range(), foot.top(), Stroke::new(1.0_f32, pal.row_rule()));
        let foot_text = if delete_safe { "Everything is checked against the same safety rules as manual deletes".to_string() } else { super::disk_delete::delete_off_reason() };
        widgets::text_fit(&p, pos2(foot.left(), foot.center().y + 2.0), Align2::LEFT_CENTER, &foot_text, theme::ui(12.0), pal.text_faint, (foot.width() - 220.0).max(40.0));
        let can_go = delete_safe && n > 0 && !busy && c.pending.is_none() && !self.pro.del.job.is_loading();
        let mut want_clean = false;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(foot.left(), foot.top() + 6.0), foot.max)).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.add_enabled_ui(can_go, |ui| {
                want_clean = Button::primary("Clean up selected\u{2026}").show(ui, &pal).clicked();
            });
        });

        let c = &mut self.pro.clean;
        for (cat, v) in toggles {
            c.checked.insert(cat, v);
        }
        for (id, v) in vol_toggles {
            c.vol_checked.insert(id, v);
        }
        for cat in expands {
            if !c.expanded.remove(&cat) {
                c.expanded.insert(cat);
            }
        }
        if refresh {
            c.started = false;
            c.vols_only = None;
            c.tree_for = None;
        }
        if want_clean {
            self.clean_request(fd.rows, elevated, epoch);
        }
        want_scan
    }

    /// Opens the confirm dialog for the checked categories.
    fn clean_request(&mut self, rows: &[ProcessRow], elevated: bool, epoch: u64) {
        let c = &self.pro.clean;
        if !self.pro.del.delete_safe || c.pending.is_some() || self.pro.del.pending.is_some() || self.pro.del.job.is_loading() || c.measuring(self.caps.volcache) {
            return;
        }
        let mut cats = Vec::new();
        let mut bin_bytes = None;
        for cat in c.visible(elevated) {
            if !c.is_checked(cat) || CleanupUi::locked(cat, elevated) {
                continue;
            }
            let Some(m) = c.measured(cat, epoch) else { continue };
            if cat == Category::RecycleBin {
                if m.bytes > 0 || m.count > 0 {
                    bin_bytes = Some(m.bytes);
                }
                continue;
            }
            let targets: Vec<Target> = m.items.iter().filter(|i| elevated || !i.needs_admin).map(to_target).collect();
            if !targets.is_empty() {
                cats.push(CatSel { cat, targets });
            }
        }
        drop_nested(&mut cats);
        let vols = c.selected_vols(elevated);
        if cats.is_empty() && bin_bytes.is_none() && vols.is_empty() {
            return;
        }
        let procs = rows.iter().map(|r| (r.pid, r.name.clone(), r.path.clone())).collect();
        let c = &mut self.pro.clean;
        c.next_id += 1;
        c.pending = Some(CleanPending { id: c.next_id, cats, vols, drive: volcache::system_drive(), bin_bytes, ack: false, assessed: None, epoch, procs });
        self.pro.del.ensure_rules(rows);
    }

    /// Polls the cleanup workers and shows the confirm dialog. Called every frame.
    pub fn clean_dialogs(&mut self, ctx: &egui::Context, pal: &Palette) {
        let (c, d) = (&mut self.pro.clean, &mut self.pro.del);
        c.fixed.poll();
        c.tree_m.poll();
        c.poll_vols();
        if c.assess.poll() {
            if let (Some(out), Some(p)) = (c.assess.ok().cloned(), c.pending.as_mut()) {
                if out.id == p.id {
                    p.assessed = Some(out);
                }
            }
        }
        if let Some(p) = &c.pending {
            if p.assessed.is_none() && !c.assess.is_loading() {
                if d.rules.stale(RULES_MAX_AGE) {
                    let procs = p.procs.clone();
                    d.rules.start(move || Ok(Arc::new(diskdelete::build_rules(procs))));
                } else if let Some(rules) = d.rules.ok().cloned() {
                    let id = p.id;
                    let cats: Vec<CatSel> = p.cats.clone();
                    c.assess.start(move || {
                        let counts: Vec<(usize, Mode)> = cats.iter().map(|cs| (cs.targets.len(), cs.cat.mode())).collect();
                        let flat: Vec<Target> = cats.into_iter().flat_map(|cs| cs.targets).collect();
                        let mut done = diskdelete::assess(flat, &rules).into_iter();
                        let mut items: Vec<Vec<Assessed>> = counts.iter().map(|&(n, _)| done.by_ref().take(n).collect()).collect();
                        // Bin categories: what has no bin on its volume is skipped with the reason.
                        let (mut vols, mut bin_capped) = (trash::Volumes::default(), true);
                        for (list, _) in items.iter_mut().zip(&counts).filter(|(_, (_, m))| *m == Mode::Recycle) {
                            bin_capped &= diskdelete::block_untrashable(list, &mut vols).1;
                        }
                        Ok(CleanAssess { id, items, bin_capped })
                    });
                }
            }
            if c.assess.is_loading() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        self.clean_confirm_window(ctx, pal);
    }

    fn clean_confirm_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(p) = self.pro.clean.pending.as_mut() else { return };
        let st = theme::status(pal);
        let a = p.assessed.as_ref();
        let capped = a.map(|a| a.bin_capped).unwrap_or(false);
        let vols = p.vols.clone();
        let live = |v: &Verdict| !matches!(v, Verdict::Blocked(_));
        // (category, items, bytes) of what will actually be deleted.
        let mut lines: Vec<(Category, u64, u64)> = Vec::new();
        for (i, cs) in p.cats.iter().enumerate() {
            let (n, bytes) = match a {
                Some(a) => a.items[i].iter().filter(|x| live(&x.verdict)).fold((0u64, 0u64), |(n, b), x| (n + 1, b + x.target.size)),
                None => (cs.targets.len() as u64, cs.targets.iter().map(|t| t.size).sum()),
            };
            if n > 0 {
                lines.push((cs.cat, n, bytes));
            }
        }
        if let Some(b) = p.bin_bytes {
            lines.push((Category::RecycleBin, 0, b));
        }
        let all: Vec<&Assessed> = a.map(|a| a.items.iter().flatten().collect()).unwrap_or_default();
        let warns: Vec<&Assessed> = all.iter().copied().filter(|x| matches!(x.verdict, Verdict::Warn(_))).collect();
        let blocked: Vec<&Assessed> = all.iter().copied().filter(|x| !live(&x.verdict)).collect();
        let deletable = !vols.is_empty() || lines.iter().any(|l| l.1 > 0 || l.0 == Category::RecycleBin);
        let total: u64 = lines.iter().map(|l| l.2).sum::<u64>() + vols.iter().map(|h| h.size).sum::<u64>();
        let count = lines.len() + vols.len();
        let skipped_in_use = diskdelete::in_use_skipped(all.iter().copied());
        let mut ack = p.ack;
        let ok_to_go = a.is_some() && deletable && (warns.is_empty() || ack);
        let mut answer = None;
        let title = format!("Clean up **{}** in {count} categor{}?", format_bytes(total as f64), if count == 1 { "y" } else { "ies" });
        kit::dialog(ctx, "disk-cleanup", |ui| {
            kit::dialog_header(ui, pal, Some((Icon::Disk, st.warn)), &title, "These locations are checked against the same safety rules as manual deletes.");
            ui.add_space(10.0);
            // One line per category: what it is, how much, and where the items go.
            let line = |ui: &mut egui::Ui, name: &str, amount: String, dest: &str, permanent: bool| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).font(theme::semibold(13.0)).color(pal.text));
                    ui.label(RichText::new(amount).font(theme::mono(12.0)).color(pal.text_faint));
                });
                ui.label(RichText::new(dest).font(theme::ui(12.0)).color(if permanent { st.danger_text } else { pal.text_dim }));
                ui.add_space(4.0);
            };
            for (cat, n, bytes) in &lines {
                let (dest, permanent) = dest_text(*cat, capped);
                let count = if *cat == Category::RecycleBin { String::new() } else { format!("{n} item{} \u{2022} ", if *n == 1 { "" } else { "s" }) };
                line(ui, cat.title(), format!("{count}{}", format_bytes(*bytes as f64)), &dest, permanent);
            }
            for h in &vols {
                line(ui, h.name.as_str(), format_bytes(h.size as f64), VOL_DEST, true);
            }
            if a.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking safety\u{2026}").color(pal.text_dim));
                });
            }
            if skipped_in_use > 0 {
                ui.label(RichText::new(format!("In-use check skipped for {skipped_in_use} files (more than {} selected)", diskdelete::IN_USE_CAP)).font(theme::ui(12.0)).color(st.yellow));
                ui.add_space(4.0);
            }
            verdict_lists(ui, pal, &warns, &blocked);
            if !warns.is_empty() {
                ui.checkbox(&mut ack, "I know what I'm doing");
                ui.add_space(4.0);
            }
            answer = confirm_row(ui, pal, "Cancel", "Clean up", ButtonKind::DangerFill, ok_to_go);
        });
        p.ack = ack;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
        match answer {
            Some(true) => self.clean_start_job(),
            Some(false) => self.pro.clean.pending = None,
            None => {}
        }
    }

    /// Hands the deletable items of every category to the worker thread (the manual-delete job slot).
    fn clean_start_job(&mut self) {
        if !self.pro.del.delete_safe {
            self.pro.clean.pending = None;
            return;
        }
        if self.pro.del.job.is_loading() {
            self.status = "A delete is already running; try again when it has finished".into();
            return;
        }
        let Some(p) = self.pro.clean.pending.take() else { return };
        let Some(a) = p.assessed else { return };
        let groups: Vec<(Mode, Vec<Target>)> = p
            .cats
            .iter()
            .zip(a.items)
            .map(|(cs, items)| {
                // Never downgraded: bin items without a bin were blocked by the assessment.
                (cs.cat.mode(), items.into_iter().filter(|x| !matches!(x.verdict, Verdict::Blocked(_))).map(|x| x.target).collect::<Vec<_>>())
            })
            .filter(|g| !g.1.is_empty())
            .collect();
        let bin = p.bin_bytes;
        let (vols, drive) = (p.vols, p.drive);
        let mut requery: Vec<String> = vols.iter().map(|v| v.id.clone()).collect();
        if !requery.iter().any(|i| i.eq_ignore_ascii_case("Recycle Bin")) {
            requery.push("Recycle Bin".into()); // recycled items land there; one handler is cheap to query
        }
        let epoch = p.epoch;
        let rules = self.pro.del.rules.ok().cloned();
        let tree = self.pro.disk_tree.as_ref().filter(|_| self.pro.disk_epoch() == epoch).map(|(t, _)| t.clone());
        self.pro.del.job.start(move || {
            let mut report = Report { mode: Mode::Permanent, ..Default::default() };
            for (mode, targets) in groups {
                let r = diskdelete::run(targets, mode, rules.as_deref());
                report.deleted.extend(r.deleted);
                report.skipped.extend(r.skipped);
                report.freed += r.freed;
            }
            if let Some(bytes) = bin {
                let res = trash::empty();
                log_entry(trash_word(), bytes, &res);
                match res {
                    Ok(()) => report.freed += bytes,
                    Err(e) => report.skipped.push((Target { path: PathBuf::from(trash_word()), size: bytes, is_dir: false, node: None, file: None }, e)),
                }
            }
            purge_vols(&vols, &drive, &mut report);
            let tree = tree.map(|t| Arc::new(apply_report(&t, &report)));
            let clean = Some(CleanDone { requery, recycle_bin: cleanup::measure_recycle_bin() });
            Ok(JobOut { report, tree, epoch, clean })
        });
    }
}

/// One `deletions.log` line for a purge by Windows (`label` is the Recycle Bin or `cleanmgr:<handler id>`).
fn log_entry(label: &str, bytes: u64, res: &Result<(), String>) {
    use std::io::Write;
    let path = diskdelete::default_log();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let line = diskdelete::log_line(&crate::pro::display_stamp(), Mode::Permanent, bytes, res, std::path::Path::new(label));
        let _ = writeln!(f, "{line}");
    }
}

/// Purges exactly the checked Windows Disk Cleanup handlers through the helper process, logs each one
/// (`cleanmgr:<id>`, size before, result) and adds the outcome to `report`. They are not in `report.deleted`
/// (no paths): the scan tree is not touched.
fn purge_vols(vols: &[Handler], drive: &str, report: &mut Report) {
    if vols.is_empty() {
        return;
    }
    let ids: Vec<String> = vols.iter().map(|v| v.id.clone()).collect();
    // The purge can run for a long time in a detached helper: record the intent first so it is never unlogged.
    for v in vols {
        log_entry(&format!("cleanmgr:{}", v.id), v.size, &Err("started".into()));
    }
    let (done, failure) = volcache::purge(drive, &ids);
    for v in vols {
        let d = done.iter().find(|d| d.id.eq_ignore_ascii_case(&v.id));
        // What the helper measured at purge time (0 = nothing was left); the queried size only when it did not answer.
        let before = match d {
            Some(d) if d.error.is_none() => d.size,
            _ => v.size,
        };
        let res: Result<(), String> = match d {
            Some(d) => d.error.clone().map_or(Ok(()), Err),
            None => Err(failure.clone().unwrap_or_else(|| "not run".into())),
        };
        log_entry(&format!("cleanmgr:{}", v.id), before, &res);
        match res {
            Ok(()) => report.freed += before,
            Err(e) => report.skipped.push((Target { path: PathBuf::from(format!("Windows Disk Cleanup: {}", v.name)), size: before, is_dir: false, node: None, file: None }, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::cleanup::CleanItem;

    fn item(size: u64, admin: bool) -> CleanItem {
        CleanItem { path: PathBuf::from("x"), size, is_dir: false, needs_admin: admin, node: None, file: None }
    }

    fn measured(cat: Category, items: Vec<CleanItem>) -> Measured {
        Measured { cat, bytes: items.iter().map(|i| i.size).sum(), count: items.len() as u64, items, error: None }
    }

    #[test]
    fn selectable_skips_admin_items_unless_elevated() {
        let m = measured(Category::CrashDumps, vec![item(10, false), item(100, true)]);
        assert_eq!(selectable(&m, false), (10, 1));
        assert_eq!(selectable(&m, true), (110, 2));
        let bin = Measured { cat: Category::RecycleBin, items: vec![], bytes: 5, count: 2, error: None };
        assert_eq!(selectable(&bin, false), (5, 2));
    }

    #[test]
    fn footer_counts_checked_categories_with_content() {
        let a = measured(Category::UserTemp, vec![item(10, false)]);
        let b = measured(Category::BrowserCaches, vec![item(20, false)]);
        let empty = measured(Category::ThumbnailCache, vec![]);
        assert_eq!(footer_totals(&[(&a, true), (&b, false), (&empty, true)], true), (10, 1));
        assert_eq!(footer_totals(&[], true), (0, 0));
    }

    #[test]
    fn destinations_per_category() {
        assert_eq!(dest_text(Category::UserTemp, true), ("deleted permanently".to_string(), true));
        assert_eq!(dest_text(Category::DevArtifacts, true), (format!("moved to the {}", trash_word()), false));
        assert!(dest_text(Category::RecycleBin, true).1);
        if cfg!(windows) {
            assert_eq!(dest_text(Category::DevArtifacts, true).0, "moved to the Recycle Bin");
        }
        // No size limit on record for the volume: no promise of a restore.
        let (t, perm) = dest_text(Category::DevArtifacts, false);
        assert!(t.contains("large items may be deleted permanently") && perm);
    }

    #[test]
    fn bin_items_without_a_bin_are_skipped_not_deleted_permanently() {
        let mk = |p: &str| Assessed { target: Target { path: PathBuf::from(p), size: 1, is_dir: false, node: None, file: None }, verdict: Verdict::Allowed };
        let mut items = vec![mk("x"), Assessed { verdict: Verdict::Blocked("protected".into()), ..mk("y") }];
        let mut vols = trash::Volumes::default();
        let (n, _) = diskdelete::block_untrashable(&mut items, &mut vols);
        // Whatever this host says about the bin of "x", a missing one blocks it with the reason.
        match &items[0].verdict {
            Verdict::Blocked(why) => assert!(n == 1 && why.contains(trash_word()), "{why}"),
            v => assert!(n == 0 && *v == Verdict::Allowed),
        }
        assert_eq!(items[1].verdict, Verdict::Blocked("protected".into()), "already blocked items keep their reason");
    }

    #[test]
    fn root_only_rows_are_never_selectable() {
        for elevated in [false, true] {
            for cat in [Category::SystemJournal, Category::PackageCache, Category::SystemCaches] {
                assert!(CleanupUi::locked(cat, elevated), "{cat:?} even as {}", elevation_word());
            }
            assert_eq!(CleanupUi::locked(Category::WindowsTemp, elevated), !elevated);
            assert!(!CleanupUi::locked(Category::UserCache, elevated) && !CleanupUi::locked(Category::UserTemp, elevated));
        }
        // Checking one by hand does not change that.
        let mut c = CleanupUi::default();
        c.checked.insert(Category::PackageCache, true);
        assert!(c.is_checked(Category::PackageCache) && CleanupUi::locked(Category::PackageCache, true));
    }

    #[test]
    fn nested_targets_are_cleaned_once() {
        let base = std::env::temp_dir().join("tmop-nested");
        let t = |rel: &str, is_dir: bool| Target { path: rel.split('/').fold(base.clone(), |p, c| p.join(c)), size: 1, is_dir, node: None, file: None };
        let mut cats = vec![
            CatSel { cat: Category::UserCache, targets: vec![t("cache/tool", true), t("cache/lock", false)] },
            CatSel { cat: Category::DevArtifacts, targets: vec![t("cache/tool/node_modules", true), t("proj/node_modules", true), t("cache/lock", false)] },
            CatSel { cat: Category::OldDownloads, targets: vec![t("cache/tool/x.zip", false)] },
        ];
        drop_nested(&mut cats);
        let left: Vec<(Category, Vec<PathBuf>)> = cats.iter().map(|c| (c.cat, c.targets.iter().map(|x| x.path.clone()).collect())).collect();
        assert_eq!(
            left,
            vec![(Category::UserCache, vec![t("cache/tool", true).path, t("cache/lock", false).path]), (Category::DevArtifacts, vec![t("proj/node_modules", true).path])],
            "a category left empty is dropped"
        );
        // A file named like a folder prefix is not a parent.
        let mut cats = vec![CatSel { cat: Category::UserCache, targets: vec![t("a", false), t("ab", true), t("a/b", false)] }];
        drop_nested(&mut cats);
        assert_eq!(cats[0].targets.len(), 3);
    }

    #[test]
    fn checked_defaults_to_preselection() {
        let mut c = CleanupUi::default();
        assert!(c.is_checked(Category::UserTemp) && !c.is_checked(Category::DevArtifacts));
        c.checked.insert(Category::UserTemp, false);
        c.checked.insert(Category::DevArtifacts, true);
        assert!(!c.is_checked(Category::UserTemp) && c.is_checked(Category::DevArtifacts));
    }

    fn with_vols(r: Result<Vec<Handler>, String>) -> CleanupUi {
        let mut c = CleanupUi::default();
        c.vols.start(move || r);
        while !c.vols.poll() {
            std::thread::sleep(Duration::from_millis(1));
        }
        c
    }

    fn handler(id: &str, size: u64, admin: bool) -> Handler {
        Handler { id: id.into(), name: id.into(), size, needs_admin: admin, ..Default::default() }
    }

    #[test]
    fn windows_handlers_replace_overlapping_categories() {
        let c = with_vols(Ok(vec![handler("Temporary Files", 10, false), handler("Recycle Bin", 5, false), handler("Thumbnail Cache", 0, false)]));
        let vis = c.visible(true);
        for hidden in [Category::UserTemp, Category::WindowsTemp, Category::RecycleBin, Category::ThumbnailCache] {
            assert!(!vis.contains(&hidden), "{hidden:?}");
        }
        let listed = [Category::BrowserCaches, Category::UpdateDownloads, Category::CrashDumps, Category::OldDownloads, Category::DevArtifacts, Category::WerReports];
        for kept in listed.into_iter().filter(|c| Category::all().contains(c)) {
            assert!(vis.contains(&kept), "{kept:?}");
        }
        // The helper failed: nothing is hidden.
        let failed = with_vols(Err("timed out".into()));
        let all: Vec<Category> = Category::all().iter().copied().filter(|c| c.supported()).collect();
        assert_eq!(failed.visible(true), all);
        assert_eq!(CleanupUi::default().visible(true), all);
    }

    fn done(ids: &[&str], bin: u64) -> CleanDone {
        CleanDone { requery: ids.iter().map(|s| s.to_string()).collect(), recycle_bin: Measured { bytes: bin, ..measured(Category::RecycleBin, vec![]) } }
    }

    fn with_fixed(c: &mut CleanupUi, measured: Vec<Measured>) {
        c.fixed.start(move || Ok(Fixed { sys: Default::default(), measured }));
        while !c.fixed.poll() {
            std::thread::sleep(Duration::from_millis(1));
        }
        c.started = true;
    }

    #[test]
    fn after_clean_forgets_deleted_items_and_requeries_only_changed_handlers() {
        let mut c = with_vols(Ok(vec![handler("Temporary Files", 10, false)]));
        let (a, b) = (CleanItem { path: PathBuf::from("a"), ..item(5, false) }, CleanItem { path: PathBuf::from("b"), ..item(7, false) });
        with_fixed(&mut c, vec![measured(Category::BrowserCaches, vec![a, b]), measured(Category::RecycleBin, vec![])]);
        let gone = Target { path: PathBuf::from("a"), size: 5, is_dir: false, node: None, file: None };
        c.after_clean(&Report { deleted: vec![gone], ..Default::default() }, done(&["Thumbnail Cache", "Recycle Bin"], 42), true);
        let m = c.measured(Category::BrowserCaches, 0).unwrap();
        assert_eq!((m.bytes, m.count), (7, 1));
        assert_eq!(c.measured(Category::RecycleBin, 0).unwrap().bytes, 42);
        assert!(c.started, "fixed locations are not measured again");
        assert!(c.vols_dirty && c.vols_only == Some(vec!["Thumbnail Cache".to_string(), "Recycle Bin".to_string()]));
        assert!(c.measuring(true), "the next Clean up waits for the handler query");
    }

    #[test]
    fn after_clean_without_volcache_does_not_block_the_next_clean_up() {
        let mut c = CleanupUi::default();
        with_fixed(&mut c, vec![measured(Category::RecycleBin, vec![])]);
        c.after_clean(&Report::default(), done(&["Recycle Bin"], 0), false);
        assert!(!c.vols_dirty && c.vols_only.is_none());
        assert!(!c.measuring(false));
        // A stale flag never blocks where no handler query would clear it.
        c.vols_dirty = true;
        assert!(!c.measuring(false) && c.measuring(true));
    }

    #[test]
    fn after_clean_during_a_running_measurement_measures_again() {
        let mut c = CleanupUi::default();
        with_fixed(&mut c, vec![]);
        c.fixed.start(|| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(Fixed { sys: Default::default(), measured: vec![] })
        });
        c.after_clean(&Report::default(), done(&["Recycle Bin"], 0), true);
        assert!(!c.started);
    }

    #[test]
    fn failed_partial_requery_of_a_covering_handler_measures_again() {
        for (answer, again) in [(handler("Temporary Files", 0, false), false), (Handler { error: Some("timed out".into()), ..handler("Temporary Files", 0, false) }, true)] {
            let mut c = with_vols(Ok(vec![handler("Temporary Files", 10, false)]));
            c.started = true;
            c.vols_checking = vec!["Temporary Files".into()];
            c.vols.start(move || Ok(vec![answer]));
            while c.vols.is_loading() {
                c.poll_vols();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(!c.started, again);
        }
    }

    #[test]
    fn requery_keeps_covered_categories_hidden() {
        let mut c = with_vols(Ok(vec![handler("Temporary Files", 10, false)]));
        c.vols.start(|| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(vec![handler("Temporary Files", 0, false)])
        });
        assert!(c.vols.is_loading());
        assert!(!c.visible(true).contains(&Category::UserTemp));
        // A needs-admin handler covers nothing when not elevated.
        let adm = with_vols(Ok(vec![handler("Temporary Files", 10, true)]));
        // (User temp is a Windows category.)
        assert_eq!(adm.visible(false).contains(&Category::UserTemp), Category::all().contains(&Category::UserTemp));
        assert!(!adm.visible(true).contains(&Category::UserTemp));
    }

    #[test]
    fn windows_handler_selection() {
        let mut c = with_vols(Ok(vec![
            handler("Temporary Files", 10, false),
            handler("Recycle Bin", 20, false),
            handler("Update Cleanup", 0, true),
            handler("D3D Shader Cache", 30, false),
            handler("DownloadsFolder", 99, false),
        ]));
        let ids = |v: Vec<Handler>| v.into_iter().map(|h| h.id).collect::<Vec<_>>();
        // Only the pre-selected ones; never Recycle Bin, Update Cleanup or DownloadsFolder.
        assert_eq!(ids(c.selected_vols(false)), vec!["D3D Shader Cache", "Temporary Files"]);
        c.vol_checked.insert("Recycle Bin".into(), true);
        c.vol_checked.insert("Update Cleanup".into(), true);
        c.vol_checked.insert("DownloadsFolder".into(), true);
        // Administrator-only handlers are listed (greyed) but not selectable until elevated.
        assert_eq!(ids(c.vol_list(false)), vec!["D3D Shader Cache", "Recycle Bin", "Temporary Files", "Update Cleanup"]);
        assert_eq!(ids(c.selected_vols(false)), vec!["D3D Shader Cache", "Recycle Bin", "Temporary Files"]);
        assert!(!ids(c.vol_list(true)).contains(&"DownloadsFolder".to_string()));
        c.vol_checked.insert("Temporary Files".into(), false);
        assert_eq!(ids(c.selected_vols(true)), vec!["D3D Shader Cache", "Recycle Bin"]);
    }
}
