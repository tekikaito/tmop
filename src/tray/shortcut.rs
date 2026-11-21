//! The keyboard shortcut that brings TMOP to the front (`tray.shortcut`).
//!
//! * Stored as `Ctrl+Alt+T`: the modifiers in the order Ctrl, Alt, Shift, Win, then one key by
//!   its egui name (`T`, `F5`, `PageUp`, `Up`).
//! * Shown as `Ctrl + Alt + T`.
//! * A shortcut needs Ctrl, Alt or Win; Shift alone is not enough, since Shift plus a key is
//!   just typing.
//! * The key is the one the keyboard layout names (egui's logical key), which is what
//!   `RegisterHotKey` matches too.

use eframe::egui::{Key, Modifiers};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: Option<Key>,
}

/// What a key press in the capture field means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    /// A complete shortcut.
    Done(Shortcut),
    /// A key without Ctrl, Alt or Win.
    NeedsModifier,
    /// A key that cannot be part of a shortcut (a punctuation key, a media key).
    Unsupported,
    /// Escape on its own: stop capturing and keep the old shortcut.
    Cancel,
}

/// Keys a shortcut may end with: letters, digits, F1 to F24, and the navigation and editing keys.
/// Punctuation is left out: its egui name depends on the layout in ways `RegisterHotKey` does not.
pub fn usable(key: Key) -> bool {
    use Key::*;
    let name = key.name();
    let letter_or_digit = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphanumeric());
    let function = matches!(key, F1 | F2 | F3 | F4 | F5 | F6 | F7 | F8 | F9 | F10 | F11 | F12 | F13 | F14 | F15 | F16 | F17 | F18 | F19 | F20 | F21 | F22 | F23 | F24);
    let other = matches!(key, ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Escape | Tab | Backspace | Enter | Space | Insert | Delete | Home | End | PageUp | PageDown);
    letter_or_digit || function || other
}

impl Shortcut {
    /// Reads the stored form. Case and spaces around `+` do not matter; anything that is not a
    /// usable shortcut is `None`.
    pub fn parse(text: &str) -> Option<Shortcut> {
        let mut s = Shortcut::default();
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last()?;
        for m in mods {
            let flag = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut s.ctrl,
                "alt" => &mut s.alt,
                "shift" => &mut s.shift,
                "win" | "super" | "meta" => &mut s.win,
                _ => return None,
            };
            if *flag {
                return None;
            }
            *flag = true;
        }
        let key = Key::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(key))?;
        s.key = Some(key);
        s.is_valid().then_some(s)
    }

    /// Ctrl, Alt or Win, and a usable key.
    pub fn is_valid(&self) -> bool {
        (self.ctrl || self.alt || self.win) && self.key.is_some_and(usable)
    }

    fn parts(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift"), (self.win, "Win")] {
            if on {
                v.push(name);
            }
        }
        if let Some(k) = self.key {
            v.push(k.name());
        }
        v
    }

    /// The stored form, `Ctrl+Alt+T`.
    pub fn to_setting(&self) -> String {
        self.parts().join("+")
    }

    /// For people: `Ctrl + Alt + T`.
    pub fn label(&self) -> String {
        self.parts().join(" + ")
    }

    /// What a key press with `mods` held means in the capture field. `win`: a Windows key is
    /// down (egui does not report it on Windows).
    pub fn capture(key: Key, mods: Modifiers, win: bool) -> Capture {
        let s = Shortcut { ctrl: mods.ctrl, alt: mods.alt, shift: mods.shift, win: win || mods.mac_cmd, key: Some(key) };
        if key == Key::Escape && !(s.ctrl || s.alt || s.shift || s.win) {
            Capture::Cancel
        } else if !usable(key) {
            Capture::Unsupported
        } else if !s.is_valid() {
            Capture::NeedsModifier
        } else {
            Capture::Done(s)
        }
    }

    /// The form the `global-hotkey` crate parses (`control+alt+T`).
    pub fn hotkey_text(&self) -> String {
        let mut v = Vec::new();
        for (on, name) in [(self.ctrl, "control"), (self.alt, "alt"), (self.shift, "shift"), (self.win, "super")] {
            if on {
                v.push(name);
            }
        }
        v.push(self.key.map(|k| k.name()).unwrap_or(""));
        v.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        let s = Shortcut::parse("Ctrl+Alt+T").unwrap();
        assert_eq!((s.ctrl, s.alt, s.shift, s.win, s.key), (true, true, false, false, Some(Key::T)));
        assert_eq!(s.to_setting(), "Ctrl+Alt+T");
        assert_eq!(s.label(), "Ctrl + Alt + T");
        // Any order, case and spacing reads back in the canonical order.
        let s = Shortcut::parse(" shift + win + ctrl + f5 ").unwrap();
        assert_eq!(s.to_setting(), "Ctrl+Shift+Win+F5");
        assert_eq!(s.label(), "Ctrl + Shift + Win + F5");
        assert_eq!(Shortcut::parse("Alt+PageUp").unwrap().label(), "Alt + PageUp");
        assert_eq!(Shortcut::parse("Win+Up").unwrap().key, Some(Key::ArrowUp));
        assert_eq!(Shortcut::parse("Ctrl+Alt+0").unwrap().key, Some(Key::Num0));
        for s in ["Ctrl+Alt+T", "Win+F24", "Ctrl+Shift+Escape", "Alt+Space"] {
            assert_eq!(Shortcut::parse(s).unwrap().to_setting(), s, "round trip");
        }
    }

    #[test]
    fn rejects_what_is_not_a_shortcut() {
        for bad in ["", "T", "Shift+T", "Ctrl", "Ctrl+Alt", "Ctrl+Ctrl+T", "Ctrl+T+Alt", "Hyper+T", "Ctrl+Comma", "Ctrl+Alt+Copy", "Ctrl++"] {
            assert_eq!(Shortcut::parse(bad), None, "{bad:?}");
        }
        assert!(!Shortcut::default().is_valid());
    }

    #[test]
    fn captures_key_presses() {
        let ctrl_alt = Modifiers { ctrl: true, alt: true, ..Modifiers::NONE };
        assert_eq!(Shortcut::capture(Key::T, ctrl_alt, false), Capture::Done(Shortcut::parse("Ctrl+Alt+T").unwrap()));
        assert_eq!(Shortcut::capture(Key::T, Modifiers::NONE, true), Capture::Done(Shortcut::parse("Win+T").unwrap()));
        assert_eq!(Shortcut::capture(Key::T, Modifiers::NONE, false), Capture::NeedsModifier);
        assert_eq!(Shortcut::capture(Key::T, Modifiers::SHIFT, false), Capture::NeedsModifier);
        assert_eq!(Shortcut::capture(Key::Comma, Modifiers::CTRL, false), Capture::Unsupported);
        assert_eq!(Shortcut::capture(Key::Escape, Modifiers::NONE, false), Capture::Cancel);
        // Escape with modifiers is an ordinary key: the system decides whether it can be had.
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        assert_eq!(Shortcut::capture(Key::Escape, ctrl_shift, false), Capture::Done(Shortcut::parse("Ctrl+Shift+Escape").unwrap()));
    }

    #[test]
    fn hotkey_text_is_what_global_hotkey_reads() {
        assert_eq!(Shortcut::parse("Ctrl+Alt+T").unwrap().hotkey_text(), "control+alt+T");
        assert_eq!(Shortcut::parse("Shift+Win+F5").unwrap().hotkey_text(), "shift+super+F5");
        #[cfg(windows)]
        for s in ["Ctrl+Alt+T", "Win+F24", "Ctrl+Shift+Escape", "Alt+Space", "Ctrl+Alt+0", "Alt+Up", "Ctrl+PageDown", "Ctrl+Alt+Backspace"] {
            let text = Shortcut::parse(s).unwrap().hotkey_text();
            assert!(text.parse::<global_hotkey::hotkey::HotKey>().is_ok(), "{s} -> {text}");
        }
    }
}
