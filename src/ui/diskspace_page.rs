//! Disk Space page: scan, folder map and listings.

use super::kit::{self, empty_state, header_toolbar, hsplit, titled_card, vsplit, GAP, MARGIN};
use super::table::cmp_str;
use super::theme::{self, mix, with_alpha, Palette};
use super::widgets::{self, card, Icon};
use super::{App, FrameData};
use crate::pro::diskdelete::{self, Target};
use crate::pro::diskspace::{self, Tree, Volume};
use crate::process::format_bytes;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Rect, RichText, Sense, Stroke};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use super::pro_pages::{FileCol, SubKey};

const DISK_ROW_H: f32 = 24.0;
const DISK_BIG_ROW_H: f32 = 40.0;
const DISK_DATE_W: f32 = 78.0;
/// Share of the Name+Folder width given to the name when the Folder column shows.
const FOLDER_SPLIT: f32 = 0.55;
const DISK_NARROW_W: f32 = 330.0;
const DISK_TINY_W: f32 = 240.0;

impl App {
    pub fn diskspace_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Disk Space", Icon::Disk);
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
            self.pro.disk_root = std::env::var("SystemDrive").map(|d| format!("{d}\\")).unwrap_or_else(|_| "C:\\".into());
        }
        let mut start_scan: Option<String> = None;
        let mut stop = false;
        let mut mode_click = None;
        header_toolbar(ui, body, 760.0, |ui| {
            if progress.running {
                stop = widgets::button(ui, "Stop scan", &pal, Some(Icon::Stop)).clicked();
            } else if widgets::button(ui, "Scan", &pal, Some(Icon::Play)).clicked() {
                start_scan = Some(self.pro.disk_root.clone());
            }
            let r = widgets::search_box(ui, &mut self.pro.disk_root, "Folder or drive to scan", 300.0, &pal);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                start_scan = Some(self.pro.disk_root.clone());
            }
            mode_click = super::disk_cleanup::segmented(ui, &["Analyze", "Clean up"], self.pro.disk_mode as usize, &pal);
        });
        match mode_click {
            Some(1) => {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Cleanup;
                self.pro.clean.started = false;
            }
            Some(_) => self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze,
            None => {}
        }
        let area = body.shrink(MARGIN);
        let elevated = *self.pro.del.elevated.get_or_insert_with(crate::pro::diskdelete::is_elevated);
        let rows = if elevated {
            vsplit(area, &[112.0, 30.0, -1.0], GAP)
        } else {
            let r = vsplit(area, &[36.0, 112.0, 30.0, -1.0], GAP);
            self.disk_admin_panel(ui, r[0], &pal);
            r[1..].to_vec()
        };
        let p = ui.painter().clone();
        // Volume cards.
        let vols: Vec<Volume> = self.pro.volumes.ok().cloned().unwrap_or_default();
        if !vols.is_empty() {
            let shown = vols.len().min(5);
            let strip = Rect::from_min_size(rows[0].min, vec2(rows[0].width().min(shown as f32 * 440.0), rows[0].height()));
            let rects = hsplit(strip, &vec![1.0; shown], GAP);
            for (r, v) in rects.iter().zip(vols.iter()) {
                let frac = v.used_fraction();
                let c = if frac > 0.9 { theme::RED } else if frac > 0.75 { theme::ORANGE } else { theme::GREEN };
                let title = format!("{} {}", v.root.trim_end_matches('\\'), if v.label.is_empty() { v.kind } else { v.label.as_str() });
                kit::stat_card(&p, *r, &pal, fd.bloom, &title, &format!("{} free", format_bytes(v.free as f64)), &format!("{} of {} used \u{2022} {}", format_bytes(v.used() as f64), format_bytes(v.total as f64), v.fs), c, Some(frac));
                let resp = ui.interact(*r, ui.id().with(("vol", &v.root)), Sense::click());
                if resp.clicked() && !progress.running {
                    self.pro.disk_root = v.root.clone();
                    start_scan = Some(v.root.clone());
                }
                resp.on_hover_text(format!("Scan {}", v.root));
            }
        }
        // Progress / breadcrumb line.
        let line = rows[1];
        let tree = self.pro.disk_tree.as_ref().map(|(t, _)| t.clone());
        if progress.running {
            let txt = format!("Scanning\u{2026} {} files, {} folders, {} \u{2022} {:.0} s \u{2022} {}", progress.files, progress.dirs, format_bytes(progress.bytes as f64), progress.elapsed.as_secs_f32(), progress.current);
            let clip = p.with_clip_rect(line);
            clip.text(pos2(line.left() + 2.0, line.center().y), Align2::LEFT_CENTER, &txt, theme::ui(12.5), pal.text_dim);
            let bar = Rect::from_min_max(pos2(line.left(), line.bottom() - 3.0), pos2(line.right(), line.bottom()));
            let t = (fd.time as f32 * 0.6).fract();
            p.rect_filled(bar, 1.0, pal.grid);
            p.rect_filled(Rect::from_min_max(pos2(bar.left() + bar.width() * t * 0.8, bar.top()), pos2(bar.left() + bar.width() * (t * 0.8 + 0.2), bar.bottom())), 1.0, theme::BLUE);
        } else if let Some(t) = &tree {
            let mut x = line.left() + 2.0;
            let mut go = None;
            for (k, idx) in t.ancestry(self.pro.disk_cursor).into_iter().enumerate() {
                if k > 0 {
                    let r = p.text(pos2(x, line.center().y), Align2::LEFT_CENTER, "\u{203a}", theme::ui(14.0), pal.text_faint);
                    x = r.right() + 6.0;
                }
                let name = &t.nodes[idx as usize].name;
                let r = p.text(pos2(x, line.center().y), Align2::LEFT_CENTER, name, theme::semibold(13.0), if idx == self.pro.disk_cursor { pal.text } else { pal.accent });
                if ui.interact(r, ui.id().with(("crumb", idx)), Sense::click()).clicked() {
                    go = Some(idx);
                }
                x = r.right() + 6.0;
            }
            let n = &t.nodes[self.pro.disk_cursor as usize];
            let mut summary = format!("{} in {} files", format_bytes(n.size as f64), n.files);
            if self.pro.del.freed > 0 {
                summary.push_str(&format!(" \u{2022} {} freed", format_bytes(self.pro.del.freed as f64)));
            }
            if progress.cancelled {
                summary.push_str(" \u{2022} partial (scan stopped)");
            } else if progress.finished {
                summary.push_str(&format!(" \u{2022} scanned in {:.1} s ({}), {} folders unreadable", progress.elapsed.as_secs_f32(), progress.method.label(), progress.denied));
                if let Some(why) = &progress.fell_back {
                    summary.push_str(&format!(" \u{2022} MFT unavailable: {why}, used parallel walker"));
                }
            }
            p.text(pos2(line.right(), line.center().y), Align2::RIGHT_CENTER, &summary, theme::ui(12.5), pal.text_dim);
            if let Some(g) = go {
                self.pro.disk_cursor = g;
            }
        }
        // Treemap + side list.
        let main = hsplit(rows[2], &[3.0, 2.0], GAP);
        if self.pro.disk_mode == super::disk_cleanup::DiskMode::Cleanup {
            if self.cleanup_page(ui, rows[2], fd, tree.as_ref(), progress.running, elevated) && !progress.running {
                self.pro.disk_mode = super::disk_cleanup::DiskMode::Analyze;
                start_scan = Some(self.pro.disk_root.clone());
            }
        } else {
            match &tree {
                None => {
                    card(&p, rows[2], &pal);
                    empty_state(&p, rows[2], &pal, if progress.running { "Building the folder map\u{2026}" } else { "Pick a drive above or enter a folder, then Scan" });
                }
                Some(t) => {
                    let cursor = self.pro.disk_cursor;
                    if let Some(next) = self.treemap(ui, main[0], t, cursor, fd) {
                        self.pro.disk_cursor = next;
                    }
                    self.disk_side(ui, main[1], t, fd);
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
            egui::Window::new("Stop scan").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ui.ctx(), |ui| {
                ui.label("Stop the Disk Space scan? The results so far are kept.");
                ui.checkbox(&mut dont_ask, "Don't ask again");
                ui.horizontal(|ui| {
                    if widgets::button(ui, "Stop", &pal, None).clicked() {
                        answer = Some(true);
                    }
                    if widgets::button(ui, "Keep scanning", &pal, None).clicked() {
                        answer = Some(false);
                    }
                });
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

    fn treemap(&mut self, ui: &mut egui::Ui, rect: Rect, t: &Tree, cursor: u32, fd: &FrameData<'_>) -> Option<u32> {
        let pal = fd.pal;
        let p = ui.painter().clone();
        let node = &t.nodes[cursor as usize];
        let content = titled_card(&p, rect, &pal, fd.bloom, &format!("Folder map {}", node.name), &format_bytes(node.size as f64));
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
            empty_state(&p, content, &pal, "This folder is empty");
            return None;
        }
        let rects = diskspace::squarify(&sizes, content.left(), content.top(), content.width(), content.height());
        let mut go = None;
        let hover = ui.ctx().pointer_hover_pos();
        // Registered first so the tiles sit on top; right-click on empty map goes up one level.
        let bg = ui.interact(content, ui.id().with("tm-bg"), Sense::click());
        let mut tiles: Vec<Rect> = Vec::new();
        let rules = self.pro.del.rules.ok().cloned();
        let mut menu = super::disk_delete::MenuOut::default();
        for (i, (x, y, w, h)) in rects.iter().enumerate() {
            let r = Rect::from_min_size(pos2(*x, *y), vec2(*w, *h)).shrink(1.0);
            if r.width() < 1.0 || r.height() < 1.0 {
                continue;
            }
            tiles.push(r);
            let is_rest = i >= kids.len();
            let c = if is_rest { pal.text_faint } else { kit::category_color(i) };
            let hovered = hover.map(|h| r.contains(h)).unwrap_or(false);
            let fill = mix(pal.card, c, if hovered { 0.42 } else { 0.24 });
            p.rect_filled(r, 3.0, fill);
            if fd.bloom > 0.0 && hovered {
                p.rect_stroke(r.expand(1.5), 4.0, Stroke::new(3.0_f32, with_alpha(c, 0.25 * fd.bloom)));
            }
            p.rect_stroke(r, 3.0, Stroke::new(1.0_f32, with_alpha(c, 0.85)));
            let is_more = is_rest && more > 0 && i == kids.len();
            let (name, size) = if is_more { ("More folders".to_string(), more) } else if is_rest { ("Files here".to_string(), rest) } else { let n = &t.nodes[kids[i] as usize]; (n.name.clone(), n.size) };
            if r.width() > 46.0 && r.height() > 22.0 {
                let clip = p.with_clip_rect(r.shrink(4.0));
                clip.text(r.left_top() + vec2(6.0, 5.0), Align2::LEFT_TOP, &name, theme::semibold(12.5), pal.text);
                if r.height() > 40.0 {
                    clip.text(r.left_top() + vec2(6.0, 22.0), Align2::LEFT_TOP, format_bytes(size as f64), theme::ui(11.5), lighten_text(c));
                }
            }
            if !is_rest {
                let resp = ui.interact(r, ui.id().with(("tm", kids[i])), Sense::click());
                if resp.clicked() && !t.nodes[kids[i] as usize].children.is_empty() {
                    go = Some(kids[i]);
                }
                resp.context_menu(|ui| super::disk_delete::item_menu(ui, &diskdelete::folder_target(t, kids[i]), rules.as_deref(), &mut menu));
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
        let p = ui.painter().clone();
        card(&p, rect, &pal);
        let inner = rect.shrink(8.0);
        // Tabs.
        let tabs = ["Folders", "Files", "Largest files", "File types"];
        let mut x = inner.left();
        for (i, name) in tabs.iter().enumerate() {
            let r = Rect::from_min_size(pos2(x, inner.top()), vec2(widgets::text_width(&p, name, theme::ui(13.0)) + 24.0, 28.0));
            let resp = ui.interact(r, ui.id().with(("disk-tab", i)), Sense::click());
            let sel = self.pro.disk_tab == i;
            if sel {
                p.rect_filled(r, 4.0, pal.selection);
            } else if resp.hovered() {
                p.rect_filled(r, 4.0, pal.hover);
            }
            p.text(r.center(), Align2::CENTER_CENTER, *name, theme::ui(13.0), if sel { pal.on_selection() } else { pal.text });
            if resp.clicked() {
                if i == 1 && self.pro.disk_tab != 1 {
                    self.pro.disk_files_for = None; // re-list on entering the Files tab
                }
                self.pro.disk_tab = i;
            }
            x = r.right() + 6.0;
        }
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
        let list = Rect::from_min_max(pos2(inner.left(), inner.top() + 38.0), pos2(inner.right(), inner.bottom() - bar_h));
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
                    ui.label(RichText::new("Include subfolders").font(theme::ui(12.5)).color(pal.text));
                    ui.add_space(4.0);
                    widgets::toggle(ui, &mut self.pro.disk_files_sub, &pal);
                });
                let hdr = Rect::from_min_size(pos2(list.left(), tgl.bottom()), vec2(list.width(), 22.0));
                let body = Rect::from_min_max(pos2(list.left(), hdr.bottom()), list.max);
                let date_w = if tiny { 0.0 } else { DISK_DATE_W };
                let cols = [(FileCol::Name, "Name", Align2::LEFT_CENTER, hdr.left() + 4.0), (FileCol::Size, "Size", Align2::RIGHT_CENTER, hdr.right() - 4.0 - date_w), (FileCol::Modified, "Modified", Align2::RIGHT_CENTER, hdr.right() - 4.0)];
                for (k, (c, label, anchor, x)) in cols.iter().enumerate() {
                    if *c == FileCol::Modified && tiny {
                        continue;
                    }
                    let active = *c == self.pro.disk_files_sort;
                    let arrow = if !active { "" } else if self.pro.disk_files_asc { "  \u{25b2}" } else { "  \u{25bc}" };
                    let r = p.text(pos2(*x, hdr.center().y), *anchor, format!("{label}{arrow}"), theme::ui(12.0), if active { pal.text } else { pal.text_dim });
                    if ui.interact(r.expand2(vec2(4.0, 3.0)), ui.id().with(("files-hdr", k)), Sense::click()).clicked() {
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
                    p.text(pos2(folder_x(hdr, true, !tiny), hdr.center().y), Align2::LEFT_CENTER, "Folder", theme::ui(12.0), pal.text_dim);
                }
                p.line_segment([pos2(hdr.left(), hdr.bottom()), pos2(hdr.right(), hdr.bottom())], Stroke::new(1.0_f32, pal.panel_stroke));
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
                            disk_row(ui.painter(), r, &pal, t.file_name(f), f.hidden(), f.size, f.size as f32 / total, theme::BLUE, !narrow, date.as_deref(), (!narrow).then_some(folder));
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
                                disk_row(ui.painter(), r, &pal, &f.name, f.hidden, f.size, f.size as f32 / total, theme::BLUE, !narrow, date.as_deref(), None);
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
                    let line1 = Rect::from_min_size(r.min, vec2(r.width(), 22.0));
                    let date = date_of(f.modified());
                    if del.sel.rows.contains(&i) {
                        super::disk_delete::paint_selected(ui.painter(), r, &pal);
                    }
                    disk_row(ui.painter(), line1, &pal, name, false, f.size, f.size as f32 / total, theme::ORANGE, !narrow, date.as_deref(), None);
                    widgets::text_fit(ui.painter(), pos2(r.left() + 4.0, r.bottom() - 9.0), Align2::LEFT_CENTER, &folder.display().to_string(), theme::ui(11.0), pal.text_dim, r.width() - 8.0);
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
                    ui.painter().rect_filled(r, 3.0, with_alpha(pal.text, 0.05));
                }
                row(ui, i, r, resp);
            }
        });
    });
}

/// One list row: name, thin % bar, size and optional date (right-aligned). `pct` prints the
/// percentage text (dropped on narrow panels); `date` is skipped when `None`.
#[allow(clippy::too_many_arguments)]
pub(super) fn disk_row(p: &egui::Painter, r: Rect, pal: &Palette, name: &str, dim: bool, size: u64, frac: f32, color: Color32, pct: bool, date: Option<&str>, folder: Option<&str>) {
    let bar = Rect::from_min_max(pos2(r.left(), r.bottom() - 4.0), pos2(r.left() + r.width() * frac.clamp(0.0, 1.0), r.bottom() - 1.0));
    p.rect_filled(bar, 1.0, with_alpha(color, 0.75));
    let y = r.center().y - 2.0;
    let mut x = r.right() - 4.0;
    if let Some(d) = date {
        p.text(pos2(x, y), Align2::RIGHT_CENTER, d, theme::ui(11.5), pal.text_dim);
        x -= DISK_DATE_W;
    }
    if pct {
        p.text(pos2(x, y), Align2::RIGHT_CENTER, format!("{:.1}%", frac.clamp(0.0, 1.0) * 100.0), theme::ui(12.0), pal.text_dim);
        x -= 56.0;
    }
    p.text(pos2(x, y), Align2::RIGHT_CENTER, format_bytes(size as f64), theme::ui(12.0), pal.text);
    x -= 70.0;
    let avail = x - r.left() - 8.0;
    let name_w = if folder.is_some() { avail * FOLDER_SPLIT } else { avail };
    widgets::text_fit(p, pos2(r.left() + 4.0, y), Align2::LEFT_CENTER, name, theme::ui(12.5), if dim { pal.text_faint } else { pal.text }, name_w);
    if let Some(f) = folder {
        widgets::text_fit(p, pos2(r.left() + 4.0 + name_w + 8.0, y), Align2::LEFT_CENTER, f, theme::ui(11.5), pal.text_dim, avail - name_w - 8.0);
    }
}

/// Left edge of the Folder column in a row or header `r`, mirroring [`disk_row`].
fn folder_x(r: Rect, pct: bool, date: bool) -> f32 {
    let x = r.right() - 4.0 - if date { DISK_DATE_W } else { 0.0 } - if pct { 56.0 } else { 0.0 } - 70.0;
    r.left() + 4.0 + (x - r.left() - 8.0) * FOLDER_SPLIT + 8.0
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

fn lighten_text(c: Color32) -> Color32 {
    theme::lighten(c, 0.35)
}
