//! Shortening text to a width at a place that keeps it readable: a subtitle drops its trailing
//! parts, a path keeps its end. The decisions are pure (they take a `measure` function); the
//! `text_*` helpers at the bottom draw the result.

use super::widgets;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect};

/// Separator of the parts of a subtitle.
pub const PART_SEP: &str = " \u{b7} ";

/// `text` cut at [`PART_SEP`] boundaries: trailing parts are dropped until it is at most `max_w`
/// wide by `measure`. The first part always stays (the caller's ellipsis handles a first part that
/// is itself too wide), and so does a text without separators.
pub fn elide_parts(text: &str, max_w: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let mut cuts: Vec<usize> = text.match_indices(PART_SEP).map(|(i, _)| i).collect();
    while let Some(end) = cuts.pop() {
        let head = &text[..end];
        if measure(head) <= max_w || cuts.is_empty() {
            return head.to_string();
        }
    }
    text.to_string()
}

/// `text` with its middle replaced by an ellipsis so that start and end both stay visible
/// (paths: the drive or root and the file name). The end gets the larger share.
pub fn elide_middle(text: &str, max_w: f32, measure: &dyn Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |keep: usize| -> String {
        let head = keep / 3;
        let tail = keep - head;
        let mut s: String = chars[..head].iter().collect();
        s.push('\u{2026}');
        s.extend(&chars[chars.len() - tail..]);
        s
    };
    // The widest candidate that fits (width grows with the characters kept).
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&build(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

/// Single-line text at `pos` that drops trailing [`PART_SEP`] parts to fit `max_w`.
pub fn text_fit_parts(p: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32, max_w: f32) -> Rect {
    let shown = elide_parts(text, max_w, &|s| widgets::text_width(p, s, font.clone()));
    widgets::text_fit(p, pos, anchor, &shown, font, color, max_w)
}

/// Single-line path at `pos` shortened in the middle to fit `max_w`.
pub fn text_fit_path(p: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32, max_w: f32) -> Rect {
    let shown = elide_middle(text, max_w, &|s| widgets::text_width(p, s, font.clone()));
    widgets::text_fit(p, pos, anchor, &shown, font, color, max_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unit per character.
    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn parts_that_fit_stay() {
        assert_eq!(elide_parts("a \u{b7} b", 20.0, &chars), "a \u{b7} b");
    }

    #[test]
    fn trailing_parts_go_first() {
        let t = "live \u{b7} 1 s \u{b7} 40 apps \u{b7} 150 processes \u{b7} sorted by CPU";
        assert_eq!(elide_parts(t, 33.0, &chars), "live \u{b7} 1 s \u{b7} 40 apps");
        assert_eq!(elide_parts(t, 14.0, &chars), "live \u{b7} 1 s");
        // Never cut inside a part.
        assert_eq!(elide_parts("Drivers \u{b7} 71 entries", 15.0, &chars), "Drivers");
    }

    #[test]
    fn the_first_part_always_stays() {
        assert_eq!(elide_parts("a very long first part \u{b7} b", 5.0, &chars), "a very long first part");
        assert_eq!(elide_parts("no separator at all", 5.0, &chars), "no separator at all");
    }

    #[test]
    fn leading_blanks_belong_to_the_first_part() {
        assert_eq!(elide_parts("   Connected \u{b7} Docker 29 (linux/x86_64)", 14.0, &chars), "   Connected");
    }

    #[test]
    fn middle_elision_keeps_start_and_end() {
        let path = "C:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\tmop-screenshot\\TMOP\\recordings";
        let s = elide_middle(path, 30.0, &chars);
        assert!(chars(&s) <= 30.0);
        assert!(s.contains('\u{2026}'));
        assert!(s.starts_with("C:\\"));
        assert!(s.ends_with("recordings"));
        // The end gets the larger share.
        assert!(s.find('\u{2026}').unwrap() < s.len() / 2);
    }

    #[test]
    fn middle_elision_leaves_fitting_and_tiny_widths_alone() {
        assert_eq!(elide_middle("/tmp/x", 10.0, &chars), "/tmp/x");
        let s = elide_middle("/a/long/path/name", 1.0, &chars);
        assert_eq!(s, "\u{2026}");
        assert_eq!(elide_middle("", 0.0, &chars), "");
    }
}
