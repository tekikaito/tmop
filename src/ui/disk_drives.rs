//! The disk context menu: the disks grouped, or each one on its own.

use super::{App, FrameData};
use crate::metrics::DiskDrive;
use crate::process::{format_bytes, format_rate};
use eframe::egui;

/// The menu is only there to be offered with more than one disk: one disk is its own group.
pub fn offers_menu(fd: &FrameData<'_>) -> bool {
    fd.snap.disk.drives.len() >= 2
}

/// Fills a context menu of the disk section. Returns whether the disks are to be shown
/// separately, once the entry is clicked.
pub fn disk_menu(ui: &mut egui::Ui, fd: &FrameData<'_>) -> Option<bool> {
    let label = if fd.disks_separated { "Show disks grouped" } else { "Show disks separately" };
    if ui.button(super::theme::button_text(label)).clicked() {
        ui.close();
        return Some(!fd.disks_separated);
    }
    None
}

/// The line under a disk's title: its model, when the system names one.
pub fn drive_subtitle(d: &DiskDrive) -> &str {
    if d.model.is_empty() {
        "Active time of this disk"
    } else {
        &d.model
    }
}

/// "used / total" of a disk's volumes, written as the Memory card writes it.
pub fn space_text((used, total): (u64, u64)) -> String {
    format!("{} / {}", format_bytes(used as f64), format_bytes(total as f64))
}

/// The volumes of all the disks together (used, total), `None` when no disk says.
pub fn total_space(drives: &[DiskDrive]) -> Option<(u64, u64)> {
    drives.iter().filter_map(|d| d.space).reduce(|a, b| (a.0.saturating_add(b.0), a.1.saturating_add(b.1)))
}

/// The line under a disk's Summary card: how full its volumes are, as the Memory card shows its
/// figure (the rates are on the Performance page), else its rates; then what tells it from the
/// other disks. Drive letters go first, as they are short and the line is cut where the card
/// ends; a model goes last.
pub fn drive_footer(d: &DiskDrive) -> String {
    let rates = format!("R {} \u{b7} W {}", format_rate(d.read_bps), format_rate(d.write_bps));
    if let Some(space) = d.space {
        let space = space_text(space);
        return if !d.volumes.is_empty() {
            format!("{space} \u{b7} {}", d.volumes)
        } else if !d.model.is_empty() {
            format!("{space} \u{b7} {}", d.model)
        } else {
            space
        };
    }
    if !d.volumes.is_empty() {
        format!("{} \u{b7} {rates}", d.volumes)
    } else if !d.model.is_empty() {
        format!("{rates} \u{b7} {}", d.model)
    } else {
        rates
    }
}

/// The longest text a popout tile shows beside its label without touching it.
const TILE_CHARS: usize = 10;

/// Every disk's active time in one short text, for the popout tile the disks share: "12% / 3%"
/// as separated GPUs are written, "4/23/0%" when that is too long for a tile (three disks and
/// more), and the busiest disk alone ("max 23%") when even that is.
pub fn active_times(drives: &[DiskDrive]) -> String {
    let join = |unit: &str, sep: &str| drives.iter().map(|d| format!("{:.0}{unit}", d.active_pct)).collect::<Vec<_>>().join(sep);
    let spaced = join("%", " / ");
    if spaced.len() <= TILE_CHARS {
        return spaced;
    }
    let tight = join("", "/") + "%";
    if tight.len() <= TILE_CHARS {
        return tight;
    }
    format!("max {:.0}%", drives.iter().map(|d| d.active_pct).fold(0.0, f32::max))
}

impl App {
    pub fn apply_disk_menu(&mut self, separated: bool) {
        self.settings.window.disks_separated = separated;
        self.settings_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disks_share_one_text_and_name_their_model() {
        let d = |active: f32, model: &str| DiskDrive { active_pct: active, model: model.into(), ..Default::default() };
        let drives = [d(84.6, "Samsung SSD 990 PRO 2TB"), d(0.4, ""), d(12.0, "")];
        // Two disks as two GPUs; more, or two at full load, without the spaces; many, the busiest.
        assert_eq!(active_times(&drives[..2]), "85% / 0%");
        assert_eq!(active_times(&[d(100.0, ""), d(23.4, "")]), "100% / 23%");
        assert_eq!(active_times(&[d(100.0, ""), d(100.0, "")]), "100/100%");
        assert_eq!(active_times(&drives), "85/0/12%");
        assert_eq!(active_times(&[d(4.0, ""), d(23.0, ""), d(0.0, ""), d(12.0, "")]), "4/23/0/12%");
        assert_eq!(active_times(&[d(4.0, ""), d(23.0, ""), d(0.0, ""), d(12.0, ""), d(7.0, "")]), "max 23%");
        assert_eq!(active_times(&drives[..1]), "85%");
        assert_eq!(drive_subtitle(&drives[0]), "Samsung SSD 990 PRO 2TB");
        assert_eq!(drive_subtitle(&drives[1]), "Active time of this disk");
        let rates = format!("R {} \u{b7} W {}", format_rate(0.0), format_rate(0.0));
        assert_eq!(drive_footer(&drives[0]), format!("{rates} \u{b7} Samsung SSD 990 PRO 2TB"));
        assert_eq!(drive_footer(&DiskDrive { volumes: "C: D:".into(), ..drives[0].clone() }), format!("C: D: \u{b7} {rates}"));
        assert_eq!(drive_footer(&drives[1]), rates);
        let gb = 1024u64.pow(3);
        let full = DiskDrive { space: Some((gb, 4 * gb)), volumes: "C:".into(), ..drives[0].clone() };
        assert_eq!(drive_footer(&full), "1.0 GB / 4.0 GB \u{b7} C:");
        assert_eq!(drive_footer(&DiskDrive { volumes: String::new(), ..full.clone() }), "1.0 GB / 4.0 GB \u{b7} Samsung SSD 990 PRO 2TB");
        assert_eq!(drive_footer(&DiskDrive { volumes: String::new(), model: String::new(), ..full.clone() }), "1.0 GB / 4.0 GB");
        let other = DiskDrive { space: Some((gb, 2 * gb)), ..Default::default() };
        assert_eq!(total_space(&[full, other.clone(), drives[1].clone()]), Some((2 * gb, 6 * gb)));
        assert_eq!(total_space(&drives[1..]), None);
    }
}
