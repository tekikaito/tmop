//! Disk Space page: scan, folder map and listings.

use super::hardware::{self, Hardware};
use super::kit::{self, empty_state, hsplit, section_card};
use super::pro_pages::{confirm_row, mono_input, page_layout, toolbar, FileCol, SubKey};
use super::table::cmp_str;
use super::theme::{self, with_alpha, Palette};
use super::widgets::{self, tracked_text, Button, ButtonKind, Icon};
use super::{App, FrameData};
use crate::pro::diskdelete::{self, Target};
use crate::pro::diskspace::{self, ScanProgress, Tree, Volume};
use crate::process::format_bytes;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense, Stroke};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const DISK_ROW_H: f32 = 32.0;
const DISK_BIG_ROW_H: f32 = 40.0;
/// Widths of the right-hand columns of a list row: date, percentage, size.
const DISK_DATE_W: f32 = 96.0;
const DISK_PCT_W: f32 = 52.0;
const DISK_SIZE_W: f32 = 74.0;
/// Share of the Name+Folder width given to the name when the Folder column shows.
const FOLDER_SPLIT: f32 = 0.55;
const DISK_NARROW_W: f32 = 330.0;
const DISK_TINY_W: f32 = 240.0;
/// Width the toolbar needs: path field, Scan and the Analyze / Clean up switch.
const TOOLBAR_W: f32 = 600.0;
const VOLUME_H: f32 = 76.0;
const VOLUME_MIN_W: f32 = 250.0;
const BANNER_H: f32 = 44.0;

impl App {
    pub fn diskspace_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        self.pro.volumes.poll();
        if self.pro.volumes.stale(Duration::from_secs(30)) {
            self.pro.volumes.start(|| Ok(diskspace::volumes()));
        }
        let progress = self.pro.scanner.progress();
        let (tree, gen) = self.pro.scanner.tree();
        if let Some(t) = tree {
            if self.pro.disk_tree.as_ref().map(|(_, g)| *g != gen).unwrap_or(true) {
                if (self.pro.disk_cursor as usize) >= t.nodes.len() {
                    self.pro.disk_cursor = 0;
                }
                self.pro.disk_tree = Some((t, gen));
            }
        }
        if let Some((t, _)) = &self.pro.disk_tree {
            if !t.nodes.get(self.pro.disk_cursor as usize).is_some_and(|n| !n.removed) {
                self.pro.disk_cursor = 0;
            }
        }
        if self.pro.disk_root.is_empty() {
            self.pro.disk_root = diskspace::default_root().display().to_string();
        }
        let tree = self.pro.disk_tree.as_ref().map(|(t, _)| t.clone());
        let body = self.page_title(ui, &pal, "Disk Space", &scan_subtitle(&progress, tree.as_deref()));
        let (tools, area) = page_layout(body, TOOLBAR_W);
        let mut start_scan: Option<String> = None;
        let mut stop = false;
        let mut mode_click = None;
        toolbar(ui, tools, |ui| {
            // Clean up wherever this OS has categories; its actions stay off while deleting is (delete_safe).
            if !crate::pro::cleanup::Category::all().is_empty() {
                mode_click = widgets::segmented(ui, &["Analyze", "Clean up"], self.pro.disk_mode as usize, &pal, false);
            }
            if progress.running {
                stop = Button::danger("Stop scan").icon(Icon::Stop).show(ui, &pal).clicked();
            } else if Button::primary("Scan").icon(Icon::Play).show(ui, &pal).clicked() {
                start_scan = Some(self.pro.disk_root.clone());
            }
            let r = mono_input(ui, &mut self.pro.disk_root, "Folder or drive to scan", 260.0, &pal);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                start_scan = Some(self.pro.disk_root.clone());
            }
        });
        match mode_click {
            Some(1) => {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Cleanup;
                self.pro.clean.started = false;
            }
            Some(_) => self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze,
            None => {}
        }
        let elevated = *self.pro.del.elevated.get_or_insert_with(crate::platform::is_elevated);
        let p = ui.painter().clone();
        let mut y = area.top();
        if !elevated && self.caps.elevate_relaunch {
            let banner = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), BANNER_H));
            self.disk_admin_panel(ui, banner, &pal);
            y = banner.bottom() + 14.0;
        }
        // Volume cards: as many columns as fit, at most two rows.
        let vols: Vec<Volume> = self.pro.volumes.ok().cloned().unwrap_or_default();
        if !vols.is_empty() {
            let cols = (((area.width() + 12.0) / (VOLUME_MIN_W + 12.0)).floor() as usize).clamp(1, vols.len());
            let shown = vols.len().min(cols * 2);
            let w = (area.width() - 12.0 * (cols - 1) as f32) / cols as f32;
            let scanned = self.pro.disk_tree.as_ref().map(|(t, _)| t.root.display().to_string()).unwrap_or_else(|| self.pro.disk_root.trim().to_string());
            let roots: Vec<&str> = vols.iter().map(|v| v.root.as_str()).collect();
            let active = diskspace::volume_index(&scanned, &roots);
            for (i, v) in vols.iter().take(shown).enumerate() {
                let r = Rect::from_min_size(pos2(area.left() + (i % cols) as f32 * (w + 12.0), y + (i / cols) as f32 * (VOLUME_H + 12.0)), vec2(w, VOLUME_H));
                let resp = ui.interact(r, ui.id().with(("vol", &v.root)), Sense::click());
                volume_card(&p, r, &pal, v, active == Some(i), resp.hovered());
                if resp.clicked() && !progress.running {
                    self.pro.disk_root = v.root.clone();
                    start_scan = Some(v.root.clone());
                }
                resp.on_hover_text(format!("Scan {}", v.root)).on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            let rows = shown.div_ceil(cols);
            y += rows as f32 * VOLUME_H + (rows - 1) as f32 * 12.0 + 14.0;
        }
        // Folder map and details, or the Clean up view.
        let main = Rect::from_min_max(pos2(area.left(), y), area.max);
        if self.pro.disk_mode == super::disk_cleanup::DiskMode::Cleanup {
            if self.cleanup_page(ui, main, fd, tree.as_ref(), progress.running, elevated) && !progress.running {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze;
                start_scan = Some(self.pro.disk_root.clone());
            }
        } else {
            match &tree {
                None => {
                    let content = section_card(&p, main, &pal, "Folder map", "");
                    if progress.running {
                        let c = content.center();
                        p.text(c - vec2(0.0, 22.0), Align2::CENTER_CENTER, "Building the folder map\u{2026}", theme::ui(13.0), pal.text_faint);
                        widgets::text_fit(&p, c + vec2(0.0, 24.0), Align2::CENTER_CENTER, &progress.current, theme::mono(11.0), pal.text_faint, content.width() * 0.8);
                        let track = Rect::from_center_size(c, vec2((content.width() * 0.5).min(360.0), 4.0));
                        p.rect_filled(track, 2.0, pal.raise(0.08));
                        let t = (fd.time as f32 * 0.6).fract();
                        let seg = Rect::from_min_max(pos2(track.left() + track.width() * t * 0.8, track.top()), pos2(track.left() + track.width() * (t * 0.8 + 0.2), track.bottom()));
                        p.rect_filled(seg, 2.0, st.info);
                    } else {
                        empty_state(&p, content, &pal, "Pick a drive above or enter a folder, then Scan");
                    }
                }
                Some(t) => {
                    let split = hsplit(main, &[3.0, 2.0], 14.0);
                    let cursor = self.pro.disk_cursor;
                    if let Some(next) = self.treemap(ui, split[0], t, cursor, &progress, fd) {
                        self.pro.disk_cursor = next;
                    }
                    self.disk_side(ui, split[1], t, fd);
                }
            }
        }
        if stop {
            if self.settings.general.disk_space_suppress_stop_confirm {
                self.pro.scanner.cancel();
            } else {
                self.pro.disk_confirm_stop = true;
            }
        }
        if self.pro.disk_confirm_stop {
            let mut answer = None;
            let mut dont_ask = false;
            kit::dialog(ui.ctx(), "stop-scan", |ui| {
                kit::dialog_header(ui, &pal, Some((Icon::Stop, st.danger)), "Stop the **Disk Space scan**?", "The results found so far are kept.");
                ui.add_space(8.0);
                ui.checkbox(&mut dont_ask, "Don't ask again");
                ui.add_space(8.0);
                answer = confirm_row(ui, &pal, "Keep scanning", "Stop scan", ButtonKind::DangerFill, true);
            });
            if dont_ask {
                self.settings.general.disk_space_suppress_stop_confirm = true;
                self.settings_dirty = true;
            }
            if let Some(a) = answer {
                self.pro.disk_confirm_stop = false;
                if a {
                    self.pro.scanner.cancel();
                }
            }
        }
        if let Some(root) = start_scan {
            let root = diskdelete::canonical_root(PathBuf::from(root.trim()));
            if root.is_dir() {
                self.pro.disk_cursor = 0;
                self.pro.disk_tree = None;
                self.pro.disk_largest = None;
                self.pro.disk_files_for = None;
                self.pro.del.sel = Default::default();
                self.pro.del.freed = 0;
                self.pro.scanner.start(root);
            } else {
                self.status = format!("{} is not a folder", root.display());
            }
        }
    }

    fn treemap(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Tree, cursor: u32, progress: &ScanProgress, fd: &FrameData<'_>) -> Option<u32> {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let node = &t.nodes[cursor as usize];
        let title = format!("Folder map \u{b7} {}", node.name);
        let content = section_card(&p, rect, &pal, &title, "");
        let mut go = breadcrumb(ui, rect, &pal, t, cursor, 22.0 + title.chars().count() as f32 * 9.2);
        // Hint (and how the scan went) under the map.
        let hint_y = content.bottom() - 6.0;
        let map = Rect::from_min_max(content.min, pos2(content.right(), content.bottom() - 24.0));
        let hint = p.text(pos2(content.left(), hint_y), Align2::LEFT_CENTER, "Click a folder to zoom in \u{b7} right-click for the folder menu", theme::ui(11.0), pal.text_faint);
        let mut notes: Vec<String> = Vec::new();
        if progress.cancelled {
            notes.push("partial (scan stopped)".into());
        } else if progress.finished {
            notes.push(format!("{} \u{b7} {:.1} s", progress.method.label(), progress.elapsed.as_secs_f32()));
            if progress.denied > 0 {
                notes.push(format!("{} folders unreadable", progress.denied));
            }
        }
        if self.pro.del.freed > 0 {
            notes.push(format!("{} freed", format_bytes(self.pro.del.freed as f64)));
        }
        if !notes.is_empty() {
            let r = widgets::text_fit(&p, pos2(content.right(), hint_y), Align2::RIGHT_CENTER, &notes.join(" \u{b7} "), theme::mono(11.0), pal.text_faint, (content.width() - hint.width() - 20.0).max(40.0));
            if let Some(why) = progress.fell_back.as_ref().filter(|_| self.caps.mft) {
                ui.interact(r, ui.id().with("scan-note"), Sense::hover()).on_hover_text(format!("MFT unavailable: {why}, used the parallel walker"));
            } else if let Some(hint) = denied_hint().filter(|_| progress.finished && progress.denied > 0) {
                ui.interact(r, ui.id().with("scan-note"), Sense::hover()).on_hover_text(hint);
            }
        }
        let kids: Vec<u32> = node.children.iter().copied().filter(|c| t.nodes[*c as usize].size > 0).take(60).collect();
        let mut sizes: Vec<f64> = kids.iter().map(|c| t.nodes[*c as usize].size as f64).collect();
        let shown: u64 = sizes.iter().sum::<f64>() as u64;
        let rest = node.own; // files directly here
        let more = node.size.saturating_sub(shown).saturating_sub(rest); // folders past the 60th
        if more > 0 {
            sizes.push(more as f64);
        }
        if rest > 0 {
            sizes.push(rest as f64);
        }
        if sizes.is_empty() {
            empty_state(&p, map, &pal, "This folder is empty");
            return go;
        }
        let total = node.size.max(1) as f64;
        let rects = diskspace::squarify(&sizes, map.left(), map.top(), map.width(), map.height());
        let hover = ui.ctx().pointer_hover_pos();
        // Registered first so the tiles sit on top; right-click on empty map goes up one level.
        let bg = ui.interact(map, ui.id().with("tm-bg"), Sense::click());
        let mut tiles: Vec<Rect> = Vec::new();
        let rules = self.pro.del.rules.ok().cloned();
        let mut menu = super::disk_delete::MenuOut::default();
        for (i, (x, y, w, h)) in rects.iter().enumerate() {
            let r = Rect::from_min_size(pos2(*x, *y), vec2(*w, *h)).shrink(1.5);
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            tiles.push(r);
            let is_rest = i >= kids.len();
            let hovered = hover.map(|h| r.contains(h)).unwrap_or(false);
            let (fill, stroke) = if is_rest {
                (pal.raise(if hovered { 0.10 } else { 0.06 }), pal.raise(0.12))
            } else {
                let c = theme::darken_for(&pal, kit::category_color(i));
                (with_alpha(c, if hovered { 0.30 } else { 0.20 }), with_alpha(c, 0.5))
            };
            p.rect_filled(r, 2.0, fill);
            p.rect_stroke(r, 2.0, Stroke::new(1.0_f32, stroke));
            let is_more = is_rest && more > 0 && i == kids.len();
            let (name, size) = if is_more { ("More folders".to_string(), more) } else if is_rest { ("Files here".to_string(), rest) } else { let n = &t.nodes[kids[i] as usize]; (n.name.clone(), n.size) };
            if r.width() > 46.0 && r.height() > 22.0 {
                let clip = p.with_clip_rect(r.shrink(2.0));
                let sub = format!("{} \u{b7} {}", format_bytes(size as f64), pct_text(size as f64 / total));
                widgets::text_fit(&clip, r.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, &name, theme::semibold(13.0), pal.text, r.width() - 20.0);
                if r.height() > 44.0 {
                    widgets::text_fit(&clip, pos2(r.left() + 10.0, r.bottom() - 8.0), Align2::LEFT_BOTTOM, &sub, theme::mono(11.0), pal.text_dim, r.width() - 20.0);
                } else if r.width() > 190.0 {
                    clip.text(pos2(r.right() - 10.0, r.top() + 8.0), Align2::RIGHT_TOP, &sub, theme::mono(11.0), pal.text_dim);
                }
            }
            if !is_rest {
                let resp = ui.interact(r, ui.id().with(("tm", kids[i])), Sense::click());
                if resp.clicked() && !t.nodes[kids[i] as usize].children.is_empty() {
                    go = Some(kids[i]);
                }
                resp.context_menu(|ui| super::disk_delete::item_menu(ui, &diskdelete::folder_target(t, kids[i]), rules.as_deref(), self.pro.del.delete_safe, &mut menu));
                resp.on_hover_text(format!("{}\n{}  \u{2022}  {} files", t.path_of(kids[i]).display(), format_bytes(size as f64), t.nodes[kids[i] as usize].files));
            } else if is_more {
                ui.interact(r, ui.id().with("tm-more"), Sense::hover()).on_hover_text(format!("{} in smaller folders not drawn here\nSee the Folders tab", format_bytes(more as f64)));
            } else {
                let resp = ui.interact(r, ui.id().with("tm-rest"), Sense::click());
                if resp.clicked() {
                    self.pro.disk_tab = 1;
                }
                resp.on_hover_text(format!("{} in files directly in this folder\nClick to list them", format_bytes(rest as f64)));
            }
        }
        if bg.secondary_clicked() && node.parent != diskspace::NO_PARENT && !hover.is_some_and(|h| tiles.iter().any(|r| r.contains(h))) {
            go = Some(node.parent);
        }
        if menu.want_rules {
            self.pro.del.ensure_rules(fd.rows);
        }
        if let (Some(mode), Some(target)) = (menu.mode, menu.target.take()) {
            let epoch = self.pro.disk_epoch();
            self.pro.del.request(vec![target], mode, epoch, fd.rows);
        }
        go
    }

    fn disk_side(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Tree, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        let p = ui.painter().clone();
        widgets::card(&p, rect, &pal);
        // Tabs along the top, the list in the rest of the card.
        let tabs = [("Folders", None), ("Files", None), ("Largest files", None), ("File types", None)];
        if let Some(i) = widgets::tab_bar(ui, Rect::from_min_size(pos2(rect.left(), rect.top() + 8.0), vec2(rect.width(), 36.0)), &pal, &tabs, self.pro.disk_tab, None) {
            if i == 1 && self.pro.disk_tab != 1 {
                self.pro.disk_files_for = None; // re-list on entering the Files tab
            }
            self.pro.disk_tab = i;
        }
        let inner = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top() + 8.0 + 36.0 + 6.0), pos2(rect.right() - 10.0, rect.bottom() - 8.0));
        // Selection is per list: a different tab, sort, subfolder mode, folder or tree version clears it.
        let epoch = self.pro.disk_epoch();
        let view = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.pro.disk_tab, self.pro.disk_files_sub, self.pro.disk_files_sort as u8, self.pro.disk_files_asc, epoch).hash(&mut h);
            h.finish()
        };
        self.pro.del.sel.reset(view, self.pro.disk_cursor);
        let bar_h = if self.pro.disk_tab < 3 && !self.pro.del.sel.rows.is_empty() { super::disk_delete::BAR_H } else { 0.0 };
        let bar = Rect::from_min_max(pos2(inner.left(), inner.bottom() - super::disk_delete::BAR_H), inner.max);
        // The Largest files tab says how many files it lists; the line gives way to the selection bar.
        let foot_h = if self.pro.disk_tab == 2 && bar_h == 0.0 { 24.0 } else { 0.0 };
        let list = Rect::from_min_max(inner.min, pos2(inner.right(), inner.bottom() - bar_h - foot_h));
        if foot_h > 0.0 {
            p.text(pos2(inner.left() + 4.0, inner.bottom() - 10.0), Align2::LEFT_CENTER, "Up to the 100 largest files in this folder and below", theme::ui(11.0), pal.text_faint);
        }
        let rules = self.pro.del.rules.ok().cloned();
        let rules = rules.as_deref();
        let del = &mut self.pro.del;
        let mut menu = super::disk_delete::MenuOut::default();
        let node = &t.nodes[self.pro.disk_cursor as usize];
        let total = node.size.max(1) as f32;
        let mut go = None;
        let mut reveal = None;
        let now = SystemTime::now();
        let narrow = list.width() < DISK_NARROW_W;
        let tiny = list.width() < DISK_TINY_W;
        let stamp = |m: Option<SystemTime>| m.map(diskspace::local_stamp).filter(|s| !s.is_empty()).unwrap_or_else(|| "\u{2014}".into());
        // Date cell text; a dash keeps the size column aligned when there is no timestamp.
        let date_of = |m: Option<SystemTime>| if tiny { None } else { Some(m.map(|m| diskspace::relative_time(now, m)).unwrap_or_else(|| "\u{2014}".into())) };
        match self.pro.disk_tab {
            0 => {
                let mk = |i: usize| Some(diskdelete::folder_target(t, node.children[i]));
                let size_of = |i: usize| t.nodes[node.children[i] as usize].size;
                disk_list(ui, list, ("disk-folders", self.pro.disk_cursor), &pal, DISK_ROW_H, node.children.len(), |ui, i, r, resp| {
                    let c = node.children[i];
                    let n = &t.nodes[c as usize];
                    if del.sel.rows.contains(&i) {
                        super::disk_delete::paint_selected(ui.painter(), r, &pal);
                    }
                    disk_row(ui.painter(), r, &pal, &n.name, false, n.size, n.size as f32 / total, kit::category_color(i), !narrow, None, None);
                    if resp.clicked() && del.on_click(ui, i) && !n.children.is_empty() {
                        go = Some(c);
                    }
                    del.row_menu(&resp, i, &mk, rules, &mut menu);
                });
                del.finish(ui, &pal, bar, node.children.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                if node.children.is_empty() {
                    empty_state(&p, list, &pal, "No sub-folders");
                }
            }
            1 => {
                let sub = self.pro.disk_files_sub;
                let cur = self.pro.disk_cursor;
                let gen = epoch;
                let (col, asc) = (self.pro.disk_files_sort, self.pro.disk_files_asc);
                let here = t.path_of(cur);
                let mut loading;
                let mut fresh = false;
                let mut order: &[u32] = &[];
                if sub {
                    // Every file below this folder, from the scan index, sorted on a worker thread.
                    if self.pro.disk_sub_order.poll() {
                        self.pro.disk_sub_ready = self.pro.disk_sub_key;
                    }
                    let want: SubKey = (gen, cur, col, asc);
                    if self.pro.disk_sub_ready != Some(want) && !self.pro.disk_sub_order.is_loading() {
                        if let Some((tree, _)) = &self.pro.disk_tree {
                            let tree = tree.clone();
                            self.pro.disk_sub_key = Some(want);
                            self.pro.disk_sub_order.start(move || Ok(sorted_files(&tree, cur, col, asc)));
                        }
                    }
                    // A newer scan generation may keep showing the previous order meanwhile.
                    let usable = self.pro.disk_sub_ready.map(|r| (r.1, r.2, r.3) == (cur, col, asc)).unwrap_or(false);
                    fresh = self.pro.disk_sub_ready == Some(want);
                    loading = !fresh;
                    if usable {
                        order = self.pro.disk_sub_order.ok().map(|o| o.0.as_slice()).unwrap_or(&[]);
                        loading = false;
                    }
                    if self.pro.disk_sub_order.is_loading() {
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                } else {
                    // Keep the listing current: one background read per visited folder.
                    if self.pro.disk_files.poll() {
                        self.pro.disk_files_order = None;
                    }
                    if self.pro.disk_files_for.as_deref() != Some(here.as_path()) && !self.pro.disk_files.is_loading() {
                        self.pro.disk_files_for = Some(here.clone());
                        self.pro.disk_files_order = None;
                        let h = here.clone();
                        self.pro.disk_files.start(move || diskspace::list_files(&h));
                    }
                    loading = self.pro.disk_files.is_loading() || self.pro.disk_files_for.as_deref() != Some(here.as_path());
                    if loading {
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                }
                // Include-subfolders switch, then the header strip with sortable columns.
                let tgl = Rect::from_min_size(list.min, vec2(list.width(), 24.0));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tgl).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                    ui.label(RichText::new("Include subfolders").font(theme::ui(12.0)).color(pal.text_dim));
                    ui.add_space(4.0);
                    widgets::toggle(ui, &mut self.pro.disk_files_sub, &pal);
                });
                let hdr = Rect::from_min_size(pos2(list.left(), tgl.bottom() + 2.0), vec2(list.width(), 28.0));
                let body = Rect::from_min_max(pos2(list.left(), hdr.bottom()), list.max);
                let (date_x, _, size_x, _) = row_cols(hdr, !narrow, !tiny);
                let cols = [(FileCol::Name, "Name", Align2::LEFT_CENTER, hdr.left() + 8.0), (FileCol::Size, "Size", Align2::RIGHT_CENTER, size_x), (FileCol::Modified, "Modified", Align2::RIGHT_CENTER, date_x)];
                for (k, (c, label, anchor, x)) in cols.iter().enumerate() {
                    if *c == FileCol::Modified && tiny {
                        continue;
                    }
                    let active = *c == self.pro.disk_files_sort;
                    let arrow = if !active { "" } else if self.pro.disk_files_asc { " \u{2191}" } else { " \u{2193}" };
                    let (font, color) = if active { (theme::semibold(11.0), pal.accent) } else { (theme::ui(11.0), pal.text_faint) };
                    let r = tracked_text(&p, pos2(*x, hdr.center().y), *anchor, &format!("{}{arrow}", label.to_uppercase()), font, color, 0.08);
                    if ui.interact(r.expand2(vec2(4.0, 3.0)), ui.id().with(("files-hdr", k)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        if active {
                            self.pro.disk_files_asc = !self.pro.disk_files_asc;
                        } else {
                            self.pro.disk_files_sort = *c;
                            self.pro.disk_files_asc = *c == FileCol::Name;
                        }
                        self.pro.disk_files_order = None;
                    }
                }
                if sub && !narrow {
                    tracked_text(&p, pos2(folder_x(hdr, true, !tiny), hdr.center().y), Align2::LEFT_CENTER, "FOLDER", theme::ui(11.0), pal.text_faint, 0.08);
                }
                p.hline(hdr.x_range(), hdr.bottom(), Stroke::new(1.0_f32, pal.row_rule()));
                if sub {
                    if loading {
                        empty_state(&p, body, &pal, &format!("Sorting {} files\u{2026}", t.nodes[cur as usize].files));
                    } else if order.is_empty() {
                        empty_state(&p, body, &pal, "No files in this folder or below");
                    } else {
                        let mk = |i: usize| if fresh { super::disk_delete::file_target(t, order[i] as usize) } else { None };
                        let size_of = |i: usize| t.files.get(order[i] as usize).map_or(0, |f| f.size);
                        disk_list(ui, body, ("disk-files-sub", cur), &pal, DISK_ROW_H, order.len(), |ui, i, r, resp| {
                            let Some(f) = t.files.get(order[i] as usize) else { return };
                            let rel = t.relative_dir(cur, f.dir);
                            let date = date_of(f.modified());
                            let folder = if rel.is_empty() { "." } else { rel.as_str() };
                            if del.sel.rows.contains(&i) {
                                super::disk_delete::paint_selected(ui.painter(), r, &pal);
                            }
                            disk_row(ui.painter(), r, &pal, t.file_name(f), f.hidden(), f.size, f.size as f32 / total, st.info, !narrow, date.as_deref(), (!narrow).then_some(folder));
                            if resp.clicked() && del.on_click(ui, i) {
                                reveal = Some(t.file_path(f));
                            }
                            del.row_menu(&resp, i, &mk, rules, &mut menu);
                            if resp.hovered() {
                                resp.on_hover_text(format!("{}\n{}\nModified {}", t.file_path(f).display(), format_bytes(f.size as f64), stamp(f.modified())));
                            }
                        });
                        del.finish(ui, &pal, bar, order.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                    }
                } else {
                    // Sorted order, rebuilt only when the data or sort changed.
                    if self.pro.disk_files_order.as_ref().map(|o| (o.0, o.1) != (col, asc)).unwrap_or(true) {
                        if let Some(files) = self.pro.disk_files.ok() {
                            let mut v: Vec<usize> = (0..files.len()).collect();
                            v.sort_by(|a, b| {
                                let (a, b) = (&files[*a], &files[*b]);
                                let o = match col {
                                    FileCol::Name => cmp_str(&a.name, &b.name),
                                    FileCol::Size => a.size.cmp(&b.size),
                                    FileCol::Modified => a.modified.cmp(&b.modified),
                                };
                                if asc { o } else { o.reverse() }
                            });
                            self.pro.disk_files_order = Some((col, asc, v));
                        }
                    }
                    let order: &[usize] = self.pro.disk_files_order.as_ref().map(|o| o.2.as_slice()).unwrap_or(&[]);
                    if loading {
                        empty_state(&p, body, &pal, "Reading folder\u{2026}");
                    } else if let Some(e) = self.pro.disk_files.error() {
                        empty_state(&p, body, &pal, e);
                    } else if let Some(files) = self.pro.disk_files.ok() {
                        if files.is_empty() {
                            empty_state(&p, body, &pal, "No files directly in this folder");
                        } else {
                            // Listed live: no file index; the tree update finds the record by name.
                            let mk = |i: usize| Some(Target { path: here.join(&files[order[i]].name), size: files[order[i]].size, is_dir: false, node: None, file: None });
                            let size_of = |i: usize| files[order[i]].size;
                            disk_list(ui, body, ("disk-files", cur), &pal, DISK_ROW_H, order.len(), |ui, i, r, resp| {
                                let f = &files[order[i]];
                                let date = date_of(f.modified);
                                if del.sel.rows.contains(&i) {
                                    super::disk_delete::paint_selected(ui.painter(), r, &pal);
                                }
                                disk_row(ui.painter(), r, &pal, &f.name, f.hidden, f.size, f.size as f32 / total, st.info, !narrow, date.as_deref(), None);
                                let path = here.join(&f.name);
                                if resp.clicked() && del.on_click(ui, i) {
                                    reveal = Some(path.clone());
                                }
                                del.row_menu(&resp, i, &mk, rules, &mut menu);
                                resp.on_hover_text(format!("{}\n{}\nModified {}", path.display(), format_bytes(f.size as f64), stamp(f.modified)));
                            });
                            del.finish(ui, &pal, bar, order.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
                        }
                    }
                }
            }
            2 => {
                let gen = epoch;
                let cursor = self.pro.disk_cursor;
                if self.pro.disk_largest.as_ref().map(|(g, c, _)| (*g, *c) != (gen, cursor)).unwrap_or(true) {
                    self.pro.disk_largest = Some((gen, cursor, t.largest_under(cursor, 100)));
                }
                let big: &[usize] = self.pro.disk_largest.as_ref().map(|l| l.2.as_slice()).unwrap_or(&[]);
                if big.is_empty() {
                    empty_state(&p, list, &pal, "No files in this folder");
                }
                let mk = |i: usize| super::disk_delete::file_target(t, big[i]);
                let size_of = |i: usize| t.files[big[i]].size;
                disk_list(ui, list, ("disk-largest", cursor), &pal, DISK_BIG_ROW_H, big.len(), |ui, i, r, resp| {
                    let f = &t.files[big[i]];
                    let folder = t.path_of(f.dir);
                    let name = t.file_name(f);
                    let line1 = Rect::from_min_size(r.min, vec2(r.width(), 26.0));
                    let date = date_of(f.modified());
                    if del.sel.rows.contains(&i) {
                        super::disk_delete::paint_selected(ui.painter(), r, &pal);
                    }
                    row_bar(ui.painter(), r, f.size as f32 / total, st.warn);
                    row_text(ui.painter(), line1, &pal, line1.center().y, name, false, f.size, f.size as f32 / total, !narrow, date.as_deref(), None);
                    widgets::text_fit(ui.painter(), pos2(r.left() + 8.0, r.bottom() - 13.0), Align2::LEFT_CENTER, &folder.display().to_string(), theme::mono(11.0), pal.text_faint, r.width() - 16.0);
                    let path = folder.join(name);
                    if resp.clicked() && del.on_click(ui, i) {
                        reveal = Some(path.clone());
                    }
                    del.row_menu(&resp, i, &mk, rules, &mut menu);
                    resp.on_hover_text(format!("{}\n{}\nModified {}", path.display(), format_bytes(f.size as f64), stamp(f.modified())));
                });
                del.finish(ui, &pal, bar, big.len(), &size_of, &mk, menu.mode, epoch, fd.rows);
            }
            _ => {
                let all: u64 = t.extensions.iter().map(|e| e.1).sum::<u64>().max(1);
                disk_list(ui, list, "disk-types", &pal, DISK_ROW_H, t.extensions.len(), |ui, i, r, _| {
                    let (ext, bytes, files) = &t.extensions[i];
                    disk_row(ui.painter(), r, &pal, &format!(".{ext}  ({files} files)"), false, *bytes, *bytes as f32 / all as f32, kit::category_color(i), !narrow, None, None);
                });
            }
        }
        if menu.want_rules {
            self.pro.del.ensure_rules(fd.rows);
        }
        if let Some(g) = go {
            self.pro.disk_cursor = g;
        }
        if let Some(path) = reveal {
            let _ = kit::reveal(&path);
        }
    }
}

/// Virtualised list in `rect`: `row(ui, i, r, resp)` draws item i into r (use `ui.painter()`).
/// `id` is salted with the folder so each one starts scrolled to the top.
pub(super) fn disk_list(ui: &mut egui::Ui, rect: Rect, id: impl std::hash::Hash + Copy, pal: &Palette, row_h: f32, n: usize, mut row: impl FnMut(&mut egui::Ui, usize, Rect, egui::Response)) {
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).id_salt(id), |ui| {
        ui.style_mut().spacing.item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let resp = ui.interact(r, ui.id().with(("row", i)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 4.0, pal.raise(0.04));
                }
                row(ui, i, r, resp);
            }
        });
    });
}

/// Right edges of the columns of a list row (or its header) `r`: date, percentage, size, and the
/// right edge of the name / folder area.
fn row_cols(r: Rect, pct: bool, date: bool) -> (f32, f32, f32, f32) {
    let date_x = r.right() - 8.0;
    let pct_x = date_x - if date { DISK_DATE_W } else { 0.0 };
    let size_x = pct_x - if pct { DISK_PCT_W } else { 0.0 };
    (date_x, pct_x, size_x, size_x - DISK_SIZE_W)
}

/// The 3 px share bar along the bottom of a list row, in the row's colour.
fn row_bar(p: &egui::Painter, r: Rect, frac: f32, color: Color32) {
    let w = (r.width() - 16.0) * frac.clamp(0.0, 1.0);
    if w >= 1.0 {
        p.rect_filled(Rect::from_min_size(pos2(r.left() + 8.0, r.bottom() - 5.0), vec2(w, 3.0)), 1.5, with_alpha(color, 0.75));
    }
}

/// The texts of a list row centred on `y`: name (and folder), then date, percentage and size
/// right-aligned in mono.
#[allow(clippy::too_many_arguments)]
fn row_text(p: &egui::Painter, r: Rect, pal: &Palette, y: f32, name: &str, dim: bool, size: u64, frac: f32, pct: bool, date: Option<&str>, folder: Option<&str>) {
    let (date_x, pct_x, size_x, name_right) = row_cols(r, pct, date.is_some());
    if let Some(d) = date {
        p.text(pos2(date_x, y), Align2::RIGHT_CENTER, d, theme::mono(11.0), pal.text_faint);
    }
    if pct {
        p.text(pos2(pct_x, y), Align2::RIGHT_CENTER, format!("{:.1}%", frac.clamp(0.0, 1.0) * 100.0), theme::mono(11.0), pal.text_faint);
    }
    p.text(pos2(size_x, y), Align2::RIGHT_CENTER, format_bytes(size as f64), theme::mono(12.0), pal.text_table());
    let avail = name_right - r.left() - 16.0;
    let name_w = if folder.is_some() { avail * FOLDER_SPLIT } else { avail };
    widgets::text_fit(p, pos2(r.left() + 8.0, y), Align2::LEFT_CENTER, name, theme::ui(13.0), if dim { pal.text_faint } else { pal.text }, name_w);
    if let Some(f) = folder {
        widgets::text_fit(p, pos2(r.left() + 8.0 + name_w + 8.0, y), Align2::LEFT_CENTER, f, theme::mono(11.0), pal.text_faint, avail - name_w - 8.0);
    }
}

/// One list row: name, size, share bar and optional date (right-aligned, mono). `pct` prints the
/// percentage text (dropped on narrow panels); `date` is skipped when `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn disk_row(p: &egui::Painter, r: Rect, pal: &Palette, name: &str, dim: bool, size: u64, frac: f32, color: Color32, pct: bool, date: Option<&str>, folder: Option<&str>) {
    row_bar(p, r, frac, color);
    row_text(p, r, pal, r.center().y - 2.0, name, dim, size, frac, pct, date, folder);
}

/// Left edge of the Folder column in a row or header `r`, mirroring [`disk_row`].
fn folder_x(r: Rect, pct: bool, date: bool) -> f32 {
    let (_, _, _, name_right) = row_cols(r, pct, date);
    r.left() + 8.0 + (name_right - r.left() - 16.0) * FOLDER_SPLIT + 8.0
}

/// The trail of folders from the scan root to `cursor` (mono 12, right-aligned in the heading of the
/// folder map card `rect`, after a title `title_w` wide), followed by the size of the current
/// folder. Earlier folders are clickable; leading ones are dropped for "\u{2026}" when the trail
/// does not fit. Returns the folder clicked.
fn breadcrumb(ui: &mut egui::Ui, rect: Rect, pal: &Palette, t: &Tree, cursor: u32, title_w: f32) -> Option<u32> {
    let p = ui.painter().clone();
    let font = theme::mono(12.0);
    let width = |s: &str| widgets::text_width(&p, s, font.clone());
    let (sep, ellipsis) = (" \u{203a} ", "\u{2026}");
    let tail = format!(" \u{b7} {}", format_bytes(t.nodes[cursor as usize].size as f64));
    let crumbs: Vec<(u32, &str)> = t.ancestry(cursor).into_iter().map(|i| (i, t.nodes[i as usize].name.as_str())).collect();
    let total = |from: usize| crumbs[from..].iter().map(|c| width(c.1)).sum::<f32>() + width(sep) * (crumbs.len() - from - 1) as f32 + width(&tail) + if from > 0 { width(ellipsis) + width(sep) } else { 0.0 };
    let avail = rect.width() - 44.0 - title_w;
    let mut first = 0;
    while first + 1 < crumbs.len() && total(first) > avail {
        first += 1;
    }
    let y = rect.top() + 24.0;
    let mut x = rect.right() - 22.0 - total(first);
    let mut go = None;
    let put = |x: &mut f32, s: &str, color: Color32| {
        let r = p.text(pos2(*x, y), Align2::LEFT_CENTER, s, font.clone(), color);
        *x = r.right();
        r
    };
    if first > 0 {
        put(&mut x, ellipsis, pal.text_faint);
        put(&mut x, sep, pal.text_faint);
    }
    for (k, (idx, name)) in crumbs.iter().enumerate().skip(first) {
        let last = k + 1 == crumbs.len();
        let at = x;
        let hit = Rect::from_min_size(pos2(at, y - 9.0), vec2(width(name), 18.0));
        let hovered = !last && ui.rect_contains_pointer(hit);
        let r = put(&mut x, name, if last { pal.text } else if hovered { pal.accent } else { pal.text_faint });
        if !last {
            if ui.interact(r.expand2(vec2(2.0, 4.0)), ui.id().with(("crumb", *idx)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                go = Some(*idx);
            }
            put(&mut x, sep, pal.text_faint);
        }
    }
    put(&mut x, &tail, pal.text);
    go
}

/// A volume card: drive icon, "C: label · size", used / total, a usage bar and the free space.
/// The scanned volume is washed in the accent colour, a nearly full one is red.
fn volume_card(p: &egui::Painter, r: Rect, pal: &Palette, v: &Volume, active: bool, hovered: bool) {
    let st = theme::status(pal);
    let frac = v.used_fraction();
    let full = frac > 0.9;
    p.rect_filled(r, 8.0, pal.card);
    if hovered {
        p.rect_filled(r, 8.0, pal.raise(0.03));
    }
    if active {
        p.rect_filled(r, 8.0, with_alpha(pal.accent, 0.08));
        p.rect_stroke(r, 8.0, Stroke::new(1.0_f32, with_alpha(pal.accent, 0.4)));
    } else {
        p.rect_stroke(r, 8.0, Stroke::new(1.0_f32, pal.card_stroke));
    }
    hardware::draw(p, Rect::from_min_size(pos2(r.left() + 14.0, r.center().y - 20.0), vec2(60.0, 40.0)), if v.kind == "Fixed" { Hardware::Disk } else { Hardware::Hdd }, pal.text_rail());
    let (x0, x1) = (r.left() + 88.0, r.right() - 16.0);
    let clip = p.with_clip_rect(r);
    // "C:" in Medium, then what it is and how big, with used / total at the right when there is room.
    let used = format!("{} / {}", format_bytes(v.used() as f64), format_bytes(v.total as f64));
    let root = clip.text(pos2(x0, r.top() + 22.0), Align2::LEFT_CENTER, v.root.trim_end_matches('\\'), theme::semibold(13.0), pal.text);
    let desc = format!(" {} \u{b7} {}", if v.label.is_empty() { v.kind } else { v.label.as_str() }, format_bytes(v.total as f64));
    let used_w = widgets::text_width(p, &used, theme::mono(11.0));
    let room = x1 - root.right();
    let show_used = room > widgets::text_width(p, &desc, theme::ui(13.0)).min(120.0) + used_w + 12.0;
    widgets::text_fit(&clip, pos2(root.right(), r.top() + 22.0), Align2::LEFT_CENTER, &desc, theme::ui(13.0), pal.text_faint, if show_used { room - used_w - 12.0 } else { room });
    if show_used {
        clip.text(pos2(x1, r.top() + 22.0), Align2::RIGHT_CENTER, &used, theme::mono(11.0), pal.text_table());
    }
    let color = if full { st.danger } else if active { pal.accent } else { theme::darken_for(pal, theme::INDIGO) };
    widgets::bar(&clip, Rect::from_min_size(pos2(x0, r.top() + 38.0), vec2(x1 - x0, 4.0)), frac, color);
    let free = format_bytes(v.free as f64);
    let (sub, sub_color) = if full {
        (format!("\u{25a0} {free} free \u{b7} almost full"), st.danger_text)
    } else if active {
        (format!("{free} free"), pal.text_faint)
    } else {
        (format!("{free} free \u{b7} click to scan"), pal.text_faint)
    };
    widgets::text_fit(&clip, pos2(x0, r.top() + 56.0), Align2::LEFT_CENTER, &sub, theme::ui(12.0), sub_color, x1 - x0);
}

/// Why folders were unreadable and what helps, off Windows (where the admin banner says it).
fn denied_hint() -> Option<&'static str> {
    match crate::platform::OS {
        crate::platform::Os::Windows => None,
        crate::platform::Os::MacOs => Some("macOS protects some folders (Mail, Messages, Safari, Trash). Give TMOP Full Disk Access in System Settings \u{203a} Privacy & Security to include them (after an update, remove TMOP there and add it again)."),
        _ => Some("Folders of other users and of the system need root. Run TMOP as root (sudo) to include them."),
    }
}

/// Page subtitle: what is being scanned, or what was scanned.
fn scan_subtitle(progress: &ScanProgress, tree: Option<&Tree>) -> String {
    let secs = progress.elapsed.as_secs_f32();
    if progress.running {
        format!("Scanning\u{2026} {} files \u{b7} {} folders \u{b7} {} \u{b7} {secs:.0} s", group_digits(progress.files), group_digits(progress.dirs), format_bytes(progress.bytes as f64))
    } else if let Some(t) = tree {
        let root = &t.nodes[0];
        format!("Scanned {} \u{b7} {} files \u{b7} {} folders \u{b7} {} \u{b7} {secs:.0} s \u{b7} Ctrl+0", t.root.display(), group_digits(root.files), group_digits(progress.dirs), format_bytes(root.size as f64))
    } else {
        "Pick a drive above or enter a folder, then Scan".into()
    }
}

/// 1204118 -> "1,204,118".
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    // Groups of three digits from the right.
    s.as_bytes().rchunks(3).rev().map(|g| String::from_utf8_lossy(g).into_owned()).collect::<Vec<_>>().join(",")
}

/// A share as a percentage: "42%", "<1%" below one percent.
fn pct_text(frac: f64) -> String {
    if frac < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", frac * 100.0)
    }
}

/// Case-insensitive name order without allocating.
fn cmp_name_nocase(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// File indices below `base` in the requested order, and the biggest file size.
fn sorted_files(t: &Tree, base: u32, col: FileCol, asc: bool) -> (Vec<u32>, u64) {
    let mut v: Vec<u32> = t.files_under(base).into_iter().map(|i| i as u32).collect();
    let biggest = v.iter().map(|i| t.files[*i as usize].size).max().unwrap_or(0);
    v.sort_by(|a, b| {
        let (a, b) = (&t.files[*a as usize], &t.files[*b as usize]);
        let o = match col {
            FileCol::Name => cmp_name_nocase(t.file_name(a), t.file_name(b)),
            FileCol::Size => a.size.cmp(&b.size),
            FileCol::Modified => a.mtime.cmp(&b.mtime),
        };
        if asc { o } else { o.reverse() }
    });
    (v, biggest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(add: impl Fn(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| add(ui));
            });
        }
    }

    /// Volume cards, list rows and the breadcrumb draw without panicking, narrow or wide.
    #[test]
    fn cards_rows_and_breadcrumb_draw() {
        use crate::pro::diskspace::{Names, Node, ScanMethod, NO_PARENT};
        let mk = |name: &str, parent: u32, children: Vec<u32>| Node { name: name.into(), parent, size: 2_000_000, children, ..Default::default() };
        let tree = Tree { root: PathBuf::from("C:\\"), nodes: vec![mk("C:\\", NO_PARENT, vec![1]), mk("Users", 0, vec![2]), mk("a-very-long-user-folder-name", 1, vec![])], files: vec![], names: Names::default(), extensions: vec![], method: ScanMethod::Walker };
        frame(|ui| {
            let (p, pal) = (ui.painter().clone(), theme::DARK);
            for (kind, active, free, w) in [("Fixed", true, 600, 430.0), ("Removable", false, 50, 250.0), ("Fixed", false, 0, 250.0)] {
                let v = Volume { root: "C:\\".into(), label: "Windows".into(), fs: "NTFS".into(), kind, total: 1000, free };
                volume_card(&p, Rect::from_min_size(pos2(10.0, 10.0), vec2(w, VOLUME_H)), &pal, &v, active, free == 600);
            }
            volume_card(&p, Rect::from_min_size(pos2(10.0, 100.0), vec2(300.0, VOLUME_H)), &pal, &Volume::default(), false, false);
            let row = Rect::from_min_size(pos2(10.0, 200.0), vec2(400.0, DISK_ROW_H));
            disk_row(&p, row, &pal, "notes.txt", true, 1024, 0.5, pal.accent, true, Some("3 weeks ago"), Some("Users\\alex"));
            disk_row(&p, row.translate(vec2(0.0, 40.0)), &pal, "x", false, 0, 0.0, pal.accent, false, None, None);
            for width in [600.0, 200.0] {
                let card = Rect::from_min_size(pos2(10.0, 300.0), vec2(width, 200.0));
                assert_eq!(breadcrumb(ui, card, &pal, &tree, 2, 100.0), None);
            }
        });
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!((group_digits(0), group_digits(999), group_digits(1000), group_digits(1_204_118)), ("0".into(), "999".into(), "1,000".into(), "1,204,118".into()));
        assert_eq!((pct_text(0.004), pct_text(0.42)), ("<1%".into(), "42%".into()));
    }

    /// The header columns line up with the row columns: dates at the right edge, then the
    /// percentage, then the size, and the folder column starts inside the name area.
    #[test]
    fn row_columns_mirror_the_header() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 32.0));
        let (date, pct, size, name) = row_cols(r, true, true);
        assert!(date > pct && pct > size && size > name);
        let (date2, pct2, size2, _) = row_cols(r, true, false);
        assert_eq!(pct2, date2, "without a date the percentage moves to the edge");
        assert!(size2 > size);
        let f = folder_x(r, true, true);
        assert!(f > r.left() + 8.0 && f < name);
    }

    #[test]
    fn subtitle_follows_the_scan_state() {
        let idle = ScanProgress::default();
        assert!(scan_subtitle(&idle, None).starts_with("Pick a drive"));
        let running = ScanProgress { running: true, files: 1200, dirs: 30, ..Default::default() };
        assert!(scan_subtitle(&running, None).starts_with("Scanning\u{2026} 1,200 files"));
    }
}
