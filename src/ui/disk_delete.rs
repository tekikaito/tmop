//! Disk Space deletion UI: row selection, context menu, the confirm dialog, the worker results
//! and the "Restart as administrator" panel. The rules live in [`crate::pro::diskdelete`].

use super::pro_pages::confirm_row;
use super::theme::{self, Palette};
use super::widgets::{self, draw_icon, Button, ButtonKind, ButtonSize, Icon};
use super::{kit, App};
use crate::pro::diskdelete::{self, Assessed, ItemMeta, Mode, Report, Rules, Target, Verdict};
use crate::pro::diskspace::Tree;
use crate::pro::Loader;
use crate::process::{format_bytes, ProcessRow};
use eframe::egui::{self, pos2, vec2, Align2, Rect, RichText, Sense, Stroke};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Inventory and system paths older than this are rebuilt on the next request.
const RULES_MAX_AGE: Duration = Duration::from_secs(120);
/// Height of the strip kept free at the bottom of the side panel for the floating "n selected" bar.
pub const BAR_H: f32 = 66.0;

/// Selected rows of the current side-panel list (positions in the list as drawn).
#[derive(Default)]
pub struct Selection {
    /// Identifies list + sort + scan version + folder; any change clears the selection.
    pub view: u64,
    pub cursor: u32,
    pub rows: HashSet<usize>,
    pub anchor: Option<usize>,
}

impl Selection {
    /// Clears the selection when the list, folder or scan version changed.
    pub fn reset(&mut self, view: u64, cursor: u32) {
        if (self.view, self.cursor) != (view, cursor) {
            *self = Selection { view, cursor, ..Default::default() };
        }
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.anchor = None;
    }

    pub fn select_only(&mut self, i: usize) {
        self.rows.clear();
        self.rows.insert(i);
        self.anchor = Some(i);
    }

    /// Explorer rules. Returns true for a plain click (which keeps its navigation behaviour);
    /// Ctrl and Shift clicks only change the selection.
    pub fn click(&mut self, i: usize, ctrl: bool, shift: bool) -> bool {
        if ctrl {
            if !self.rows.remove(&i) {
                self.rows.insert(i);
            }
            self.anchor = Some(i);
            false
        } else if let (true, Some(a)) = (shift, self.anchor) {
            self.rows = (a.min(i)..=a.max(i)).collect();
            false
        } else {
            self.select_only(i);
            true
        }
    }

    /// Selected positions below `len`, ascending.
    pub fn sorted(&self, len: usize) -> Vec<usize> {
        let mut v: Vec<usize> = self.rows.iter().copied().filter(|&i| i < len).collect();
        v.sort_unstable();
        v
    }
}

pub struct PendingDelete {
    pub id: u64,
    pub targets: Vec<Target>,
    pub mode: Mode,
    pub ack: bool,
    /// False when the volume has no Recycle Bin (known once assessed).
    pub has_bin: bool,
    /// Every drive has a Recycle Bin size limit on record (otherwise large items may be deleted for good).
    pub bin_capped: bool,
    pub assessed: Option<Vec<Assessed>>,
    /// Scan tree version the targets were taken from.
    pub epoch: u64,
}

#[derive(Clone)]
pub struct AssessOut {
    pub id: u64,
    pub items: Vec<Assessed>,
    pub has_bin: bool,
    pub bin_capped: bool,
}

#[derive(Clone)]
pub struct JobOut {
    pub report: Report,
    /// The scan tree with the deleted items removed (None when the scan changed meanwhile).
    pub tree: Option<Arc<Tree>>,
    pub epoch: u64,
    /// Set for a Clean up.
    pub clean: Option<super::disk_cleanup::CleanDone>,
}

#[derive(Default)]
pub struct DiskDeleteUi {
    pub elevated: Option<bool>,
    pub relaunch: Loader<()>,
    pub rules: Loader<Arc<Rules>>,
    pub sel: Selection,
    pub pending: Option<PendingDelete>,
    pub assess: Loader<AssessOut>,
    pub job: Loader<JobOut>,
    /// Report with skipped items, shown after a job.
    pub result: Option<Report>,
    /// Bytes freed since the last scan started.
    pub freed: u64,
    next_id: u64,
    procs: Vec<(u32, String, Option<String>)>,
}

/// What a context menu asks for.
#[derive(Default)]
pub struct MenuOut {
    pub mode: Option<Mode>,
    pub target: Option<Target>,
    pub want_rules: bool,
}

// ---------------------------------------------------------------- pure helpers

/// (first line, destination line, permanent) of the confirm dialog.
pub fn summary_text(n: usize, size: u64, mode: Mode, has_bin: bool, capped: bool) -> (String, String, bool) {
    let first = format!("{n} item{} \u{2022} {}", if n == 1 { "" } else { "s" }, format_bytes(size as f64));
    match (mode, has_bin) {
        (Mode::Recycle, true) if !capped => (first, "Items go to the Recycle Bin, but large items may be deleted permanently by Windows.".into(), true),
        (Mode::Recycle, true) => (first, "Items go to the Recycle Bin and can be restored.".into(), false),
        (Mode::Recycle, false) => (first, "This volume has no Recycle Bin: items are deleted permanently.".into(), true),
        (Mode::Permanent, _) => (first, "Items are deleted permanently. This cannot be undone.".into(), true),
    }
}

/// The delete button is enabled: something is deletable and warnings are acknowledged.
pub fn can_confirm(assessed: &[Assessed], ack: bool) -> bool {
    let deletable = assessed.iter().any(|a| !matches!(a.verdict, Verdict::Blocked(_)));
    let warn = assessed.iter().any(|a| matches!(a.verdict, Verdict::Warn(_)));
    deletable && (!warn || ack)
}

/// The scan tree with `rep.deleted` removed. Files listed live have no index: they are looked
/// up by folder and name, or just subtracted when the scan never saw them.
pub fn apply_report(tree: &Tree, rep: &Report) -> Tree {
    let mut t = tree.clone();
    let (mut nodes, mut files) = diskdelete::tree_updates(&rep.deleted);
    for tg in rep.deleted.iter().filter(|x| x.node.is_none() && x.file.is_none()) {
        if tg.is_dir {
            // A folder deleted from outside the tree's own lists (Cleanup): find it by path.
            nodes.extend(t.find_node(&tg.path));
            continue;
        }
        let Some(dir) = tg.path.parent().and_then(|p| t.find_node(p)) else { continue };
        let name = tg.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        match t.files_in(dir).into_iter().find(|&i| t.file_name(&t.files[i]).to_lowercase() == name) {
            Some(i) => files.push(i),
            None => t.forget_file(dir, tg.size),
        }
    }
    t.remove(&nodes, &files);
    t
}

pub fn file_target(t: &Tree, idx: usize) -> Option<Target> {
    let f = t.files.get(idx)?;
    Some(Target { path: t.file_path(f), size: f.size, is_dir: false, node: None, file: Some(idx) })
}

/// Context menu of one item. Delete entries are greyed with the reason when the rules are
/// loaded and block the item; otherwise the dialog does the full check.
pub fn item_menu(ui: &mut egui::Ui, t: &Target, rules: Option<&Rules>, out: &mut MenuOut) {
    out.want_rules = true;
    if ui.button("Show in Explorer").clicked() {
        let _ = kit::reveal(&t.path);
        ui.close_menu();
    }
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(t.path.display().to_string());
        ui.close_menu();
    }
    ui.separator();
    let blocked = rules.and_then(|r| match diskdelete::classify(&t.path, ItemMeta { is_dir: t.is_dir, size: t.size, is_reparse: false }, r) {
        Verdict::Blocked(why) => Some(why),
        _ => None,
    });
    for (label, mode) in [("Move to Recycle Bin", Mode::Recycle), ("Delete permanently\u{2026}", Mode::Permanent)] {
        let r = ui.add_enabled(blocked.is_none(), egui::Button::new(label));
        let r = match &blocked {
            Some(why) => r.on_disabled_hover_text(why),
            None => r,
        };
        if r.clicked() {
            out.mode = Some(mode);
            out.target = Some(t.clone());
            ui.close_menu();
        }
    }
}

/// Highlight for a selected row: the table's selected-row wash with a 1 px accent inset.
pub fn paint_selected(p: &egui::Painter, r: Rect, pal: &Palette) {
    p.rect_filled(r, 4.0, pal.selected_row());
    p.rect_stroke(r.shrink(0.5), 4.0, Stroke::new(1.0_f32, pal.accent));
}

/// The warnings and the skipped items of a delete / clean up confirmation, in a scroll area:
/// each warning with its reasons, then "Will be skipped" with why. Nothing is drawn when both are
/// empty.
pub fn verdict_lists(ui: &mut egui::Ui, pal: &Palette, warns: &[&Assessed], blocked: &[&Assessed]) {
    if warns.is_empty() && blocked.is_empty() {
        return;
    }
    let st = theme::status(pal);
    egui::ScrollArea::vertical().max_height(220.0).auto_shrink([true, true]).show(ui, |ui| {
        for a in warns.iter().take(40) {
            let Verdict::Warn(reasons) = &a.verdict else { continue };
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                draw_icon(ui.painter(), r, Icon::Warning, st.yellow);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(RichText::new(a.target.path.display().to_string()).font(theme::mono(12.0)).color(pal.text)).truncate());
                    ui.add(egui::Label::new(RichText::new(reasons.join(" \u{2022} ")).font(theme::ui(12.0)).color(pal.text_dim)).wrap());
                });
            });
        }
        if warns.len() > 40 {
            ui.label(RichText::new(format!("+{} more with warnings", warns.len() - 40)).font(theme::ui(12.0)).color(pal.text_faint));
        }
        if !blocked.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("Will be skipped").font(theme::semibold(13.0)).color(pal.text));
            for a in blocked.iter().take(40) {
                let Verdict::Blocked(why) = &a.verdict else { continue };
                ui.horizontal_top(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    draw_icon(ui.painter(), r, Icon::Shield, pal.text_faint);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(a.target.path.display().to_string()).font(theme::mono(12.0)).color(pal.text_dim)).truncate());
                        ui.label(RichText::new(why).font(theme::ui(12.0)).color(pal.text_faint));
                    });
                });
            }
        }
    });
    ui.add_space(6.0);
}

/// Paths in an inset box (the first `max`, then "+n more").
pub fn path_box<'a>(ui: &mut egui::Ui, pal: &Palette, paths: impl ExactSizeIterator<Item = &'a std::path::Path>, max: usize) {
    let total = paths.len();
    egui::Frame::none().fill(pal.well()).stroke(Stroke::new(1.0_f32, pal.row_rule())).rounding(6.0).inner_margin(egui::Margin::symmetric(12.0, 8.0)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        for p in paths.take(max) {
            ui.add(egui::Label::new(RichText::new(p.display().to_string()).font(theme::mono(12.0)).color(pal.text_code())).truncate());
        }
        if total > max {
            ui.label(RichText::new(format!("+{} more", total - max)).font(theme::ui(12.0)).color(pal.text_faint));
        }
    });
}

// ---------------------------------------------------------------- state machine

impl DiskDeleteUi {
    /// Starts building the rules (installed apps, services, ...) unless they are fresh.
    pub fn ensure_rules(&mut self, rows: &[ProcessRow]) {
        if self.rules.stale(RULES_MAX_AGE) {
            let procs: Vec<_> = rows.iter().map(|r| (r.pid, r.name.clone(), r.path.clone())).collect();
            self.rules.start(move || Ok(Arc::new(diskdelete::build_rules(procs))));
        }
    }

    /// Opens the confirm dialog for `targets`.
    pub fn request(&mut self, targets: Vec<Target>, mode: Mode, epoch: u64, rows: &[ProcessRow]) {
        if targets.is_empty() || self.pending.is_some() || self.job.is_loading() {
            return;
        }
        self.next_id += 1;
        self.procs = rows.iter().map(|r| (r.pid, r.name.clone(), r.path.clone())).collect();
        self.pending = Some(PendingDelete { id: self.next_id, targets, mode, ack: false, has_bin: true, bin_capped: false, assessed: None, epoch });
    }

    /// Plain/Ctrl/Shift click on row `i`. True for a plain click.
    pub fn on_click(&mut self, ui: &egui::Ui, i: usize) -> bool {
        let m = ui.input(|x| x.modifiers);
        self.sel.click(i, m.ctrl || m.command, m.shift)
    }

    /// Context menu of row `i`. A right-clicked row outside the selection replaces it.
    pub fn row_menu(&mut self, resp: &egui::Response, i: usize, mk: &dyn Fn(usize) -> Option<Target>, rules: Option<&Rules>, out: &mut MenuOut) {
        if resp.secondary_clicked() && !self.sel.rows.contains(&i) {
            self.sel.select_only(i);
        }
        resp.context_menu(|ui| {
            if let Some(t) = mk(i) {
                item_menu(ui, &t, rules, out);
            }
        });
    }

    /// Bottom bar, Del / Shift+Del and menu requests for a list of `len` rows. `mk` and `size_of`
    /// describe row i; `menu` is what a context menu asked for this frame.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(&mut self, ui: &mut egui::Ui, pal: &Palette, bar: Rect, len: usize, size_of: &dyn Fn(usize) -> u64, mk: &dyn Fn(usize) -> Option<Target>, menu: Option<Mode>, epoch: u64, rows: &[ProcessRow]) {
        let picked = self.sel.sorted(len);
        let mut mode = menu;
        if !picked.is_empty() {
            let total: u64 = picked.iter().map(|&i| size_of(i)).sum();
            // The bar floats above the bottom of the reserved strip (see [`BAR_H`]) and stays out
            // of the way of a dialog that is open for this selection.
            if self.pending.is_none() && !self.job.is_loading() {
                kit::floating_bar(ui, bar.translate(vec2(0.0, 12.0)), "disk-selection", |ui| {
                    ui.label(RichText::new(format!("{} selected \u{2022} {}", picked.len(), format_bytes(total as f64))).font(theme::semibold(13.0)).color(pal.text));
                    ui.add_space(8.0);
                    if Button::new("Move to Recycle Bin").size(ButtonSize::Medium).show(ui, pal).clicked() {
                        mode = Some(Mode::Recycle);
                    }
                    if Button::danger("Delete permanently").size(ButtonSize::Medium).show(ui, pal).clicked() {
                        mode = Some(Mode::Permanent);
                    }
                });
            }
            if mode.is_none() && self.pending.is_none() && !ui.ctx().wants_keyboard_input() {
                let (del, shift) = ui.input(|i| (i.key_pressed(egui::Key::Delete), i.modifiers.shift));
                if del {
                    mode = Some(if shift { Mode::Permanent } else { Mode::Recycle });
                }
            }
        }
        if let (Some(m), false) = (mode, picked.is_empty()) {
            self.request(picked.iter().filter_map(|&i| mk(i)).collect(), m, epoch, rows);
        }
    }
}

// ---------------------------------------------------------------- App glue

impl App {
    /// Amber banner at the top of Disk Space while TMOP is not elevated.
    pub fn disk_admin_panel(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Palette) {
        let st = theme::status(pal);
        let p = ui.painter().clone();
        widgets::tinted_card(&p, rect, pal, st.warn);
        draw_icon(&p, Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), vec2(18.0, 18.0)), Icon::Alert, st.warn);
        let busy = self.pro.del.relaunch.is_loading();
        let btn_w = 210.0;
        let txt_w = (rect.width() - 46.0 - btn_w - 28.0).max(40.0);
        widgets::text_fit(&p, pos2(rect.left() + 46.0, rect.center().y), Align2::LEFT_CENTER, "Not running as administrator: slower scan, some folders unreadable, protected files can't be deleted.", theme::ui(13.0), st.yellow, txt_w);
        let btn = Rect::from_min_max(pos2(rect.right() - btn_w - 8.0, rect.top()), pos2(rect.right() - 8.0, rect.bottom()));
        let mut clicked = false;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(btn).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                clicked = Button::new("Restart as administrator").icon(Icon::Shield).size(ButtonSize::Small).show(ui, pal).clicked();
            });
        });
        if clicked {
            let root = self.pro.disk_tree.as_ref().map(|(t, _)| t.root.clone()).unwrap_or_else(|| PathBuf::from(self.pro.disk_root.trim()));
            self.pro.del.relaunch.start(move || diskdelete::relaunch_elevated(&root));
        }
    }

    /// Polls the delete workers and shows the dialogs. Called every frame.
    pub fn disk_delete_dialogs(&mut self, ctx: &egui::Context, pal: &Palette) {
        // --- elevation relaunch
        if self.pro.del.relaunch.poll() {
            match self.pro.del.relaunch.get() {
                Some(Ok(())) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Some(Err(e)) => self.status = e.clone(),
                None => {}
            }
        }
        // --- rules, assessment
        let d = &mut self.pro.del;
        d.rules.poll();
        if d.assess.poll() {
            if let (Some(out), Some(p)) = (d.assess.ok(), d.pending.as_mut()) {
                if out.id == p.id {
                    p.has_bin = out.has_bin;
                    p.bin_capped = out.bin_capped;
                    p.assessed = Some(out.items.clone());
                }
            }
        }
        if let Some(p) = &d.pending {
            if d.rules.stale(RULES_MAX_AGE) {
                let procs = d.procs.clone();
                d.rules.start(move || Ok(Arc::new(diskdelete::build_rules(procs))));
            } else if p.assessed.is_none() && !d.assess.is_loading() && !d.rules.is_loading() {
                if let Some(rules) = d.rules.ok().cloned() {
                    let (targets, id) = (p.targets.clone(), p.id);
                    d.assess.start(move || {
                        let mut roots: Vec<std::path::Component> = Vec::new();
                        let mut has_bin = true;
                        let mut bin_capped = true;
                        for t in &targets {
                            let root = t.path.components().next();
                            if let Some(r) = root {
                                if !roots.contains(&r) {
                                    roots.push(r);
                                    has_bin &= diskdelete::has_recycle_bin(&t.path);
                                    bin_capped &= diskdelete::recycle_limit_known(&t.path);
                                }
                            }
                        }
                        Ok(AssessOut { id, items: diskdelete::assess(targets, &rules), has_bin, bin_capped })
                    });
                }
            }
        }
        if d.assess.is_loading() || d.rules.is_loading() || d.job.is_loading() || d.relaunch.is_loading() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // --- finished job
        if self.pro.del.job.poll() {
            match self.pro.del.job.ok().cloned() {
                Some(out) => self.disk_job_done(out),
                None => self.status = self.pro.del.job.error().unwrap_or("delete failed").to_string(),
            }
        }
        self.clean_dialogs(ctx, pal);
        self.disk_confirm_window(ctx, pal);
        self.disk_progress_and_result_windows(ctx, pal);
    }

    fn disk_job_done(&mut self, out: JobOut) {
        let rep = out.report;
        if let Some(t) = out.tree {
            if let Some((_, g)) = &self.pro.disk_tree {
                if self.pro.disk_epoch() == out.epoch {
                    self.pro.disk_tree = Some((t, *g));
                    self.pro.disk_edits += 1;
                }
            }
        }
        let d = &mut self.pro.del;
        d.freed += rep.freed;
        d.sel.clear();
        match out.clean {
            Some(done) => self.pro.clean.after_clean(&rep, done),
            None => self.pro.clean.started = false, // Cleanup measures again
        }
        self.status = format!("{} freed", format_bytes(rep.freed as f64));
        self.pro.disk_files_for = None;
        self.pro.volumes.start(|| Ok(crate::pro::diskspace::volumes()));
        if !rep.skipped.is_empty() {
            self.pro.del.result = Some(rep);
        }
    }

    fn disk_confirm_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(p) = self.pro.del.pending.as_mut() else { return };
        let st = theme::status(pal);
        let assessed = p.assessed.as_deref();
        let all: Vec<&Target> = match assessed {
            Some(a) => a.iter().filter(|x| !matches!(x.verdict, Verdict::Blocked(_))).map(|x| &x.target).collect(),
            None => p.targets.iter().collect(),
        };
        let size: u64 = all.iter().map(|t| t.size).sum();
        let (_, dest, permanent) = summary_text(all.len(), size, p.mode, p.has_bin, p.bin_capped);
        let warns: Vec<&Assessed> = assessed.map(|a| a.iter().filter(|x| matches!(x.verdict, Verdict::Warn(_))).collect()).unwrap_or_default();
        let blocked: Vec<&Assessed> = assessed.map(|a| a.iter().filter(|x| matches!(x.verdict, Verdict::Blocked(_))).collect()).unwrap_or_default();
        let skipped_in_use = assessed.map(diskdelete::in_use_skipped).unwrap_or(0);
        let mut ack = p.ack;
        let ok_to_go = assessed.map(|a| can_confirm(a, ack)).unwrap_or(false);
        let mut answer = None;
        let n = all.len();
        let items = format!("{n} item{}", if n == 1 { "" } else { "s" });
        let title = if permanent { format!("Delete **{items}** permanently?") } else { format!("Move **{items}** to the Recycle Bin?") };
        let icon = if permanent { (Icon::Octagon, st.danger) } else { (Icon::Disk, st.info) };
        let goes_to = if permanent { "Deleted for good" } else { "Recycle Bin" };
        let warn_count = if assessed.is_none() { "Checking\u{2026}".to_string() } else if warns.is_empty() { "None".to_string() } else { warns.len().to_string() };
        kit::dialog(ctx, "disk-delete", |ui| {
            kit::dialog_header(ui, pal, Some(icon), &title, &dest);
            ui.add_space(12.0);
            kit::fact_box(
                ui,
                pal,
                &[("Items", &n.to_string(), None), ("Size", &format_bytes(size as f64), None), ("Goes to", goes_to, Some(if permanent { st.danger_text } else { st.ok })), ("Warnings", &warn_count, (!warns.is_empty()).then_some(st.yellow))],
            );
            ui.add_space(10.0);
            path_box(ui, pal, all.iter().map(|t| t.path.as_path()), 5);
            ui.add_space(8.0);
            if assessed.is_none() {
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
            answer = confirm_row(ui, pal, "Cancel", if permanent { "Delete permanently" } else { "Move to Recycle Bin" }, if permanent { ButtonKind::DangerFill } else { ButtonKind::Primary }, ok_to_go);
        });
        p.ack = ack;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(false);
        }
        match answer {
            Some(true) => self.disk_start_job(),
            Some(false) => self.pro.del.pending = None,
            None => {}
        }
    }

    /// Hands the deletable targets to the worker thread.
    fn disk_start_job(&mut self) {
        if self.pro.del.job.is_loading() {
            self.status = "A delete is already running; try again when it has finished".into();
            return;
        }
        let Some(p) = self.pro.del.pending.take() else { return };
        let Some(items) = p.assessed else { return };
        let mode = if p.mode == Mode::Recycle && !p.has_bin { Mode::Permanent } else { p.mode };
        let targets: Vec<Target> = items.into_iter().filter(|a| !matches!(a.verdict, Verdict::Blocked(_))).map(|a| a.target).collect();
        let epoch = p.epoch;
        let rules = self.pro.del.rules.ok().cloned();
        let tree = self.pro.disk_tree.as_ref().filter(|_| self.pro.disk_epoch() == epoch).map(|(t, _)| t.clone());
        self.pro.del.job.start(move || {
            let report = diskdelete::run(targets, mode, rules.as_deref());
            let tree = tree.map(|t| Arc::new(apply_report(&t, &report)));
            Ok(JobOut { report, tree, epoch, clean: None })
        });
    }

    fn disk_progress_and_result_windows(&mut self, ctx: &egui::Context, pal: &Palette) {
        let st = theme::status(pal);
        if self.pro.del.job.is_loading() {
            kit::dialog(ctx, "disk-deleting", |ui| {
                kit::dialog_header(ui, pal, None, "**Deleting**\u{2026}", "");
                ui.add_space(6.0);
                ui.spinner();
            });
            return;
        }
        let Some(rep) = &self.pro.del.result else { return };
        let mut close = false;
        kit::dialog(ctx, "disk-skipped", |ui| {
            kit::dialog_header(ui, pal, Some((Icon::Alert, st.warn)), "Some items were **skipped**", &format!("{} deleted, {} skipped ({} freed)", rep.deleted.len(), rep.skipped.len(), format_bytes(rep.freed as f64)));
            ui.add_space(10.0);
            egui::ScrollArea::vertical().max_height(300.0).auto_shrink([true, true]).show(ui, |ui| {
                for (t, why) in &rep.skipped {
                    ui.add(egui::Label::new(RichText::new(t.path.display().to_string()).font(theme::mono(12.0)).color(pal.text)).truncate());
                    ui.label(RichText::new(why).font(theme::ui(12.0)).color(pal.text_dim));
                    ui.add_space(2.0);
                }
            });
            ui.add_space(8.0);
            close = kit::dialog_buttons(ui, pal, &[kit::DialogButton::Secondary("Close")]).is_some();
        });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pro.del.result = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::diskspace::{FileRec, Names, Node, ScanMethod, NO_PARENT};

    fn asm(path: &str, verdict: Verdict) -> Assessed {
        Assessed { target: Target { path: PathBuf::from(path), size: 1, is_dir: false, node: None, file: None }, verdict }
    }

    #[test]
    fn selection_click_rules() {
        let mut s = Selection::default();
        assert!(s.click(3, false, false));
        assert_eq!((s.rows.len(), s.anchor), (1, Some(3)));
        assert!(!s.click(5, true, false));
        assert_eq!(s.sorted(10), vec![3, 5]);
        assert!(!s.click(3, true, false));
        assert_eq!(s.sorted(10), vec![5]);
        // Shift selects anchor..=i in either direction and keeps the anchor.
        s.click(5, false, false);
        assert!(!s.click(8, false, true));
        assert_eq!(s.sorted(20), vec![5, 6, 7, 8]);
        assert!(!s.click(2, false, true));
        assert_eq!(s.sorted(20), vec![2, 3, 4, 5]);
        // Shift without an anchor behaves like a plain click.
        let mut s = Selection::default();
        assert!(s.click(4, false, true));
        assert_eq!(s.sorted(20), vec![4]);
        assert_eq!(s.sorted(4), Vec::<usize>::new());
    }

    #[test]
    fn selection_resets_on_navigation() {
        let mut s = Selection::default();
        s.reset(1, 0);
        s.click(2, false, false);
        s.reset(1, 0);
        assert_eq!(s.rows.len(), 1);
        s.reset(2, 0);
        assert!(s.rows.is_empty() && s.anchor.is_none());
        s.click(2, false, false);
        s.reset(2, 7);
        assert!(s.rows.is_empty());
    }

    #[test]
    fn dialog_summary_text() {
        let (a, b, perm) = summary_text(1, 1024, Mode::Recycle, true, true);
        assert!(a.starts_with("1 item ") && b.contains("Recycle Bin") && !perm);
        let (a, b, perm) = summary_text(3, 0, Mode::Recycle, false, true);
        assert!(a.starts_with("3 items") && b.contains("no Recycle Bin") && perm);
        let (_, b, perm) = summary_text(2, 0, Mode::Permanent, true, true);
        assert!(b.contains("cannot be undone") && perm);
        let (_, b, _) = summary_text(1, 0, Mode::Recycle, true, false);
        assert!(b.contains("may be deleted permanently") && !b.contains("can be restored"));
    }

    #[test]
    fn confirm_gating() {
        let blocked = asm("c:\\a", Verdict::Blocked("x".into()));
        let warn = asm("c:\\b", Verdict::Warn(vec!["y".into()]));
        let ok = asm("c:\\c", Verdict::Allowed);
        assert!(!can_confirm(&[], true));
        assert!(!can_confirm(std::slice::from_ref(&blocked), true));
        assert!(can_confirm(&[ok.clone(), blocked.clone()], false));
        assert!(!can_confirm(&[ok.clone(), warn.clone()], false));
        assert!(can_confirm(&[ok, warn], true));
    }

    #[test]
    fn report_updates_tree() {
        let mut names = Names::default();
        let mut files = Vec::new();
        for (dir, name, size) in [(0u32, "r.txt", 1u64), (1, "a.bin", 10), (1, "b.bin", 100)] {
            let (o, l) = names.push(name);
            files.push(FileRec { size, mtime: 0, dir, name_off: o, name_len: l, flags: 0 });
        }
        let mk = |name: &str, parent: u32, size: u64, own: u64, files: u64, children: Vec<u32>| Node { name: name.into(), parent, size, own, files, children, ..Default::default() };
        let nodes = vec![mk("r", NO_PARENT, 111, 1, 3, vec![1]), mk("a", 0, 110, 110, 2, vec![])];
        let t = Tree { root: PathBuf::from("r"), nodes, files, names, extensions: vec![], method: ScanMethod::Walker };
        let rep = Report { deleted: vec![file_target(&t, 2).unwrap()], freed: 100, ..Default::default() };
        let t2 = apply_report(&t, &rep);
        assert_eq!((t2.nodes[0].size, t2.nodes[1].size, t2.files.len()), (11, 10, 2));
        assert_eq!(t.files.len(), 3, "the input tree is untouched");
        let rep = Report { deleted: vec![crate::pro::diskdelete::folder_target(&t, 1)], ..Default::default() };
        let t3 = apply_report(&t, &rep);
        assert!(t3.nodes[1].removed && t3.nodes[0].size == 1 && t3.files.len() == 1);
    }
}
