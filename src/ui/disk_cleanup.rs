//! Disk Space "Clean up" view: the category list, its footer and the confirm dialog. The
//! categories and their measuring live in [`crate::pro::cleanup`]; deletion goes through the same
//! classifier, worker, log and skipped list as the manual delete in [`super::disk_delete`].

use super::disk_delete::{apply_report, danger_button, JobOut};
use super::pro_pages::{disk_list, disk_row};
use super::theme::{self, with_alpha, Palette};
use super::widgets::{self, card, draw_icon, Icon};
use super::{App, FrameData};
use crate::pro::cleanup::{self, Category, Measured};
use crate::pro::diskdelete::{self, Assessed, Mode, Report, SystemPaths, Target, Verdict};
use crate::pro::diskspace::Tree;
use crate::pro::Loader;
use crate::process::{format_bytes, ProcessRow};
use eframe::egui::{self, pos2, vec2, Align2, Rect, RichText, Sense, Stroke};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const ROW_H: f32 = 58.0;
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
    /// Assessed items per `CleanPending::cats` entry, same order.
    pub items: Vec<Vec<Assessed>>,
    /// Every drive of the Recycle Bin categories has a Recycle Bin.
    pub bin_ok: bool,
}

pub struct CleanPending {
    pub id: u64,
    pub cats: Vec<CatSel>,
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
    next_id: u64,
}

impl CleanupUi {
    pub fn is_checked(&self, c: Category) -> bool {
        self.checked.get(&c).copied().unwrap_or(c.preselected())
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

/// (destination text, permanent) of a category in the confirm dialog.
pub fn dest_text(cat: Category, bin_ok: bool) -> (&'static str, bool) {
    match (cat, cat.mode()) {
        (Category::RecycleBin, _) => ("emptied by Windows, permanent", true),
        (_, Mode::Recycle) if bin_ok => ("moved to the Recycle Bin", false),
        (_, Mode::Recycle) => ("deleted permanently (no Recycle Bin on this volume)", true),
        _ => ("deleted permanently", true),
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

/// Two-option control that draws itself in one rectangle (layout direction does not matter).
/// Returns the index clicked.
pub fn segmented(ui: &mut egui::Ui, labels: &[&str], selected: usize, pal: &Palette) -> Option<usize> {
    let font = theme::ui(13.0);
    let widths: Vec<f32> = labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), pal.text).size().x + 24.0).collect();
    let (rect, _) = ui.allocate_exact_size(vec2(widths.iter().sum(), 30.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, pal.control);
    let mut x = rect.left();
    let mut clicked = None;
    for (i, (l, w)) in labels.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, rect.height()));
        let resp = ui.interact(r, ui.id().with(("segment", i)), Sense::click());
        let on = i == selected;
        if on {
            p.rect_filled(r.shrink(2.0), 3.0, pal.selection);
        } else if resp.hovered() {
            p.rect_filled(r.shrink(2.0), 3.0, with_alpha(pal.text, 0.06));
        }
        p.text(r.center(), Align2::CENTER_CENTER, l, font.clone(), if on { pal.text } else { pal.text_dim });
        if resp.clicked() && !on {
            clicked = Some(i);
        }
        x += w;
    }
    p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, pal.control_stroke));
    clicked
}

fn to_target(i: &cleanup::CleanItem) -> Target {
    Target { path: i.path.clone(), size: i.size, is_dir: i.is_dir, node: i.node, file: i.file }
}

// ---------------------------------------------------------------- page

impl App {
    /// The Clean up view in `rect`. True when the user asked for a scan from the "Run a scan first" state.
    pub fn cleanup_page(&mut self, ui: &mut egui::Ui, rect: Rect, fd: &FrameData<'_>, tree: Option<&Arc<Tree>>, scanning: bool, elevated: bool) -> bool {
        let pal = fd.pal;
        let p = ui.painter().clone();
        card(&p, rect, &pal);
        let epoch = self.pro.disk_epoch();
        {
            let c = &mut self.pro.clean;
            c.fixed.poll();
            c.tree_m.poll();
            if !c.started && !c.fixed.is_loading() {
                c.started = true;
                c.fixed.start(|| {
                    let sys = diskdelete::system_paths();
                    let mut measured = cleanup::measure_fixed(&sys, SystemTime::now());
                    measured.push(cleanup::measure_recycle_bin());
                    Ok(Fixed { sys, measured })
                });
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
            if c.fixed.is_loading() || c.tree_m.is_loading() {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }

        let inner = rect.shrink(14.0);
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), 30.0));
        let foot = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 40.0), inner.max);
        let list = Rect::from_min_max(pos2(inner.left(), head.bottom() + 8.0), pos2(inner.right(), foot.top() - 8.0));
        p.text(head.left_center(), Align2::LEFT_CENTER, "Clean up", theme::semibold(15.0), pal.text);
        let mut refresh = false;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, None).clicked();
        });
        let busy = self.pro.clean.fixed.is_loading() || self.pro.clean.tree_m.is_loading();
        let hint = if busy { "Measuring\u{2026}" } else { "Everything is checked against the same safety rules as manual deletes" };
        widgets::text_fit(&p, pos2(head.left() + 80.0, head.center().y + 1.0), Align2::LEFT_CENTER, hint, theme::ui(12.5), pal.text_dim, (head.width() - 180.0).max(40.0));

        let mut toggles: Vec<(Category, bool)> = Vec::new();
        let mut expands: Vec<Category> = Vec::new();
        let mut want_scan = false;
        let c = &self.pro.clean;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
                for cat in Category::ALL {
                    let m = c.measured(cat, epoch);
                    let locked = cat.needs_admin() && !elevated;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                    let can_expand = m.is_some_and(|m| !m.items.is_empty());
                    if resp.clicked() && can_expand {
                        expands.push(cat);
                    }
                    let p = ui.painter();
                    p.rect_filled(r, 6.0, with_alpha(pal.text, if resp.hovered() { 0.07 } else { 0.04 }));
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
                    widgets::text_fit(p, pos2(left, r.top() + 18.0), Align2::LEFT_CENTER, cat.title(), theme::semibold(13.5), dim, text_w);
                    widgets::text_fit(p, pos2(left, r.top() + 39.0), Align2::LEFT_CENTER, cat.detail(), theme::ui(12.0), pal.text_dim, text_w);
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
                                ("needs administrator".to_string(), pal.warn)
                            } else if admin_items > 0 && !elevated {
                                (format!("{} items \u{2022} {admin_items} need administrator", m.count), pal.warn)
                            } else if cat == Category::RecycleBin {
                                (format!("{} items", m.count), pal.text_dim)
                            } else {
                                (format!("{} item{}", m.count, if m.count == 1 { "" } else { "s" }), pal.text_dim)
                            };
                            (format_bytes(m.bytes as f64), sub, col)
                        }
                    } else if locked {
                        ("\u{2014}".to_string(), "needs administrator".to_string(), pal.warn)
                    } else {
                        ("Measuring\u{2026}".to_string(), String::new(), pal.text_dim)
                    };
                    if no_scan && !scanning {
                        let br = Rect::from_min_size(pos2(rx - 92.0, r.center().y - 15.0), vec2(92.0, 30.0));
                        widgets::text_fit(p, pos2(br.left() - 10.0, r.center().y), Align2::RIGHT_CENTER, &top, theme::ui(12.5), pal.text_dim, 140.0);
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(br), |ui| {
                            if widgets::button(ui, "Scan", &pal, Some(Icon::Play)).clicked() {
                                want_scan = true;
                            }
                        });
                    } else {
                        let p = ui.painter();
                        let font = if sub.is_empty() { theme::ui(12.5) } else { theme::semibold(13.5) };
                        widgets::text_fit(p, pos2(rx, r.top() + if sub.is_empty() { 29.0 } else { 18.0 }), Align2::RIGHT_CENTER, &top, font, dim, right_w);
                        if !sub.is_empty() {
                            widgets::text_fit(p, pos2(rx, r.top() + 39.0), Align2::RIGHT_CENTER, &sub, theme::ui(12.0), sub_col, right_w);
                        }
                    }
                    if can_expand {
                        let open = c.expanded.contains(&cat);
                        ui.painter().text(pos2(r.right() - 16.0, r.center().y), Align2::CENTER_CENTER, if open { "\u{25be}" } else { "\u{25b8}" }, theme::ui(14.0), pal.text_dim);
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
                            disk_row(ui.painter(), r, &pal, &it.path.display().to_string(), dim, it.size, it.size as f32 / biggest, theme::BLUE, false, None, None);
                        });
                    }
                }
            });
        });

        // Footer.
        let ms: Vec<(&Measured, bool)> = Category::ALL
            .iter()
            .filter(|cat| !(cat.needs_admin() && !elevated))
            .filter_map(|&cat| c.measured(cat, epoch).map(|m| (m, c.is_checked(cat))))
            .collect();
        let (total, n) = footer_totals(&ms, elevated);
        let text = if n == 0 { "Nothing selected".to_string() } else { format!("{} in {n} categor{} selected", format_bytes(total as f64), if n == 1 { "y" } else { "ies" }) };
        p.line_segment([foot.left_top(), foot.right_top()], Stroke::new(1.0_f32, pal.panel_stroke));
        p.text(pos2(foot.left() + 4.0, foot.center().y + 1.0), Align2::LEFT_CENTER, text, theme::semibold(13.0), pal.text);
        let can_go = n > 0 && c.pending.is_none() && !self.pro.del.job.is_loading();
        let mut want_clean = false;
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(foot.shrink2(vec2(0.0, 4.0))).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            ui.add_enabled_ui(can_go, |ui| {
                want_clean = widgets::button(ui, "Clean up selected\u{2026}", &pal, None).clicked();
            });
        });

        let c = &mut self.pro.clean;
        for (cat, v) in toggles {
            c.checked.insert(cat, v);
        }
        for cat in expands {
            if !c.expanded.remove(&cat) {
                c.expanded.insert(cat);
            }
        }
        if refresh {
            c.started = false;
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
        if c.pending.is_some() || self.pro.del.pending.is_some() || self.pro.del.job.is_loading() {
            return;
        }
        let mut cats = Vec::new();
        let mut bin_bytes = None;
        for cat in Category::ALL {
            if !c.is_checked(cat) || (cat.needs_admin() && !elevated) {
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
        if cats.is_empty() && bin_bytes.is_none() {
            return;
        }
        let procs = rows.iter().map(|r| (r.pid, r.name.clone(), r.path.clone())).collect();
        let c = &mut self.pro.clean;
        c.next_id += 1;
        c.pending = Some(CleanPending { id: c.next_id, cats, bin_bytes, ack: false, assessed: None, epoch, procs });
        self.pro.del.ensure_rules(rows);
    }

    /// Polls the cleanup workers and shows the confirm dialog. Called every frame.
    pub fn clean_dialogs(&mut self, ctx: &egui::Context, pal: &Palette) {
        let (c, d) = (&mut self.pro.clean, &mut self.pro.del);
        c.fixed.poll();
        c.tree_m.poll();
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
                        let mut bin_ok = true;
                        let mut roots: Vec<std::path::Component> = Vec::new();
                        for cs in cats.iter().filter(|cs| cs.cat.mode() == Mode::Recycle) {
                            for t in &cs.targets {
                                if let Some(r) = t.path.components().next() {
                                    if !roots.contains(&r) {
                                        roots.push(r);
                                        bin_ok &= diskdelete::has_recycle_bin(&t.path);
                                    }
                                }
                            }
                        }
                        let counts: Vec<usize> = cats.iter().map(|cs| cs.targets.len()).collect();
                        let flat: Vec<Target> = cats.into_iter().flat_map(|cs| cs.targets).collect();
                        let mut done = diskdelete::assess(flat, &rules).into_iter();
                        let items = counts.iter().map(|&n| done.by_ref().take(n).collect()).collect();
                        Ok(CleanAssess { id, items, bin_ok })
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
        let a = p.assessed.as_ref();
        let bin_ok = a.map(|a| a.bin_ok).unwrap_or(true);
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
        let deletable = lines.iter().any(|l| l.1 > 0 || l.0 == Category::RecycleBin);
        let total: u64 = lines.iter().map(|l| l.2).sum();
        let skipped_in_use = diskdelete::in_use_skipped(all.iter().copied());
        let mut ack = p.ack;
        let ok_to_go = a.is_some() && deletable && (warns.is_empty() || ack);
        let mut answer = None;
        egui::Window::new("Clean up").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).frame(egui::Frame::window(&ctx.style()).fill(pal.panel)).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(format!("{} \u{2022} {} categor{}", format_bytes(total as f64), lines.len(), if lines.len() == 1 { "y" } else { "ies" })).font(theme::semibold(14.0)).color(pal.text));
            ui.add_space(6.0);
            for (cat, n, bytes) in &lines {
                let (dest, permanent) = dest_text(*cat, bin_ok);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(cat.title()).font(theme::semibold(12.5)).color(pal.text));
                    let count = if *cat == Category::RecycleBin { String::new() } else { format!("{n} item{} \u{2022} ", if *n == 1 { "" } else { "s" }) };
                    ui.label(RichText::new(format!("{count}{}", format_bytes(*bytes as f64))).font(theme::ui(12.0)).color(pal.text_dim));
                });
                ui.label(RichText::new(dest).font(theme::ui(11.5)).color(if permanent { theme::RED } else { pal.text_dim }));
                ui.add_space(3.0);
            }
            if a.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking safety\u{2026}").color(pal.text_dim));
                });
            }
            if skipped_in_use > 0 {
                ui.label(RichText::new(format!("In-use check skipped for {skipped_in_use} files (more than {} selected)", diskdelete::IN_USE_CAP)).font(theme::ui(12.0)).color(theme::YELLOW));
                ui.add_space(4.0);
            }
            if !warns.is_empty() || !blocked.is_empty() {
                egui::ScrollArea::vertical().max_height(220.0).auto_shrink([true, true]).show(ui, |ui| {
                    for x in warns.iter().take(40) {
                        let Verdict::Warn(reasons) = &x.verdict else { continue };
                        ui.horizontal_top(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            draw_icon(ui.painter(), r, Icon::Warning, theme::YELLOW);
                            ui.vertical(|ui| {
                                ui.add(egui::Label::new(RichText::new(x.target.path.display().to_string()).font(theme::ui(12.0)).color(pal.text)).truncate());
                                ui.add(egui::Label::new(RichText::new(reasons.join(" \u{2022} ")).font(theme::ui(11.5)).color(pal.text_dim)).wrap());
                            });
                        });
                    }
                    if warns.len() > 40 {
                        ui.label(RichText::new(format!("+{} more with warnings", warns.len() - 40)).font(theme::ui(12.0)).color(pal.text_faint));
                    }
                    if !blocked.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new("Will be skipped").font(theme::semibold(12.5)).color(pal.text));
                        for x in blocked.iter().take(40) {
                            let Verdict::Blocked(why) = &x.verdict else { continue };
                            ui.horizontal_top(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                draw_icon(ui.painter(), r, Icon::Shield, pal.text_faint);
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(RichText::new(x.target.path.display().to_string()).font(theme::ui(12.0)).color(pal.text_dim)).truncate());
                                    ui.label(RichText::new(why).font(theme::ui(11.5)).color(pal.text_faint));
                                });
                            });
                        }
                    }
                });
                ui.add_space(6.0);
            }
            if !warns.is_empty() {
                ui.checkbox(&mut ack, "I know what I'm doing");
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(ok_to_go, |ui| {
                    if danger_button(ui, "Clean up", pal).clicked() {
                        answer = Some(true);
                    }
                });
                if widgets::button(ui, "Cancel", pal, None).clicked() {
                    answer = Some(false);
                }
            });
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
                let mode = if cs.cat.mode() == Mode::Recycle && !a.bin_ok { Mode::Permanent } else { cs.cat.mode() };
                (mode, items.into_iter().filter(|x| !matches!(x.verdict, Verdict::Blocked(_))).map(|x| x.target).collect::<Vec<_>>())
            })
            .filter(|g| !g.1.is_empty())
            .collect();
        let bin = p.bin_bytes;
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
                let res = cleanup::empty_recycle_bin();
                log_bin(bytes, &res);
                match res {
                    Ok(()) => report.freed += bytes,
                    Err(e) => report.skipped.push((Target { path: PathBuf::from("Recycle Bin"), size: bytes, is_dir: false, node: None, file: None }, e)),
                }
            }
            let tree = tree.map(|t| Arc::new(apply_report(&t, &report)));
            Ok(JobOut { report, tree, epoch })
        });
    }
}

/// One `deletions.log` line for emptying the Recycle Bin.
fn log_bin(bytes: u64, res: &Result<(), String>) {
    use std::io::Write;
    let path = diskdelete::default_log();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let line = diskdelete::log_line(&crate::pro::display_stamp(), Mode::Permanent, bytes, res, std::path::Path::new("Recycle Bin"));
        let _ = writeln!(f, "{line}");
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
        assert_eq!(dest_text(Category::UserTemp, true), ("deleted permanently", true));
        assert_eq!(dest_text(Category::DevArtifacts, true), ("moved to the Recycle Bin", false));
        assert!(dest_text(Category::OldDownloads, false).0.contains("no Recycle Bin"));
        assert!(dest_text(Category::RecycleBin, true).1);
    }

    #[test]
    fn checked_defaults_to_preselection() {
        let mut c = CleanupUi::default();
        assert!(c.is_checked(Category::UserTemp) && !c.is_checked(Category::DevArtifacts));
        c.checked.insert(Category::UserTemp, false);
        c.checked.insert(Category::DevArtifacts, true);
        assert!(!c.is_checked(Category::UserTemp) && c.is_checked(Category::DevArtifacts));
    }
}
