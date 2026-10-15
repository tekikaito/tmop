//! Encoding of keyboard and paste input as terminal bytes, following xterm conventions.

use eframe::egui::{Key, Modifiers};

const ESC: u8 = 0x1b;

/// Terminal modes that influence key encoding.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyModes {
    pub app_cursor: bool,
    pub app_keypad: bool,
}

/// Bytes for a key press, or `None` when the key produces no sequence.
/// Printable characters arrive separately as text and are encoded with [`encode_text`].
pub fn encode_key(key: Key, mods: Modifiers, modes: KeyModes) -> Option<Vec<u8>> {
    let ctrl = mods.ctrl || mods.command;
    let alt = mods.alt;
    let param = modifier_param(mods.shift, alt, ctrl);

    let bytes = match key {
        Key::ArrowUp => cursor_key(b'A', param, modes.app_cursor),
        Key::ArrowDown => cursor_key(b'B', param, modes.app_cursor),
        Key::ArrowRight => cursor_key(b'C', param, modes.app_cursor),
        Key::ArrowLeft => cursor_key(b'D', param, modes.app_cursor),
        Key::Home => cursor_key(b'H', param, modes.app_cursor),
        Key::End => cursor_key(b'F', param, modes.app_cursor),
        Key::Insert => tilde_key(2, param),
        Key::Delete => tilde_key(3, param),
        Key::PageUp => tilde_key(5, param),
        Key::PageDown => tilde_key(6, param),
        Key::F1 => ss3_key(b'P', param),
        Key::F2 => ss3_key(b'Q', param),
        Key::F3 => ss3_key(b'R', param),
        Key::F4 => ss3_key(b'S', param),
        Key::F5 => tilde_key(15, param),
        Key::F6 => tilde_key(17, param),
        Key::F7 => tilde_key(18, param),
        Key::F8 => tilde_key(19, param),
        Key::F9 => tilde_key(20, param),
        Key::F10 => tilde_key(21, param),
        Key::F11 => tilde_key(23, param),
        Key::F12 => tilde_key(24, param),
        Key::Enter => return Some(with_alt(alt, b"\r")),
        Key::Tab if mods.shift => return Some(with_alt(alt, b"\x1b[Z")),
        Key::Tab => return Some(with_alt(alt, b"\t")),
        Key::Backspace if ctrl => return Some(with_alt(alt, &[0x08])),
        Key::Backspace => return Some(with_alt(alt, &[0x7f])),
        Key::Escape => return Some(with_alt(alt, &[ESC])),
        Key::Space if ctrl => return Some(with_alt(alt, &[0x00])),
        Key::OpenBracket if ctrl => return Some(with_alt(alt, &[0x1b])),
        Key::Backslash if ctrl => return Some(with_alt(alt, &[0x1c])),
        Key::CloseBracket if ctrl => return Some(with_alt(alt, &[0x1d])),
        _ => return letter_key(key, mods.shift, alt, ctrl),
    };
    Some(bytes)
}

/// Bytes for typed text (egui `Event::Text`). Alt prefixes ESC.
pub fn encode_text(text: &str, mods: Modifiers) -> Vec<u8> {
    with_alt(mods.alt, text.as_bytes())
}

/// Bytes for pasted text. Line breaks become CR. With `bracketed`, the text is wrapped
/// in the bracketed paste markers and any ESC bytes are removed so the paste cannot end early.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\n', "\r");
    if bracketed {
        let body = text.replace('\x1b', "");
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl)
}

fn with_alt(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(ESC);
    }
    out.extend_from_slice(bytes);
    out
}

/// `CSI 1;m X` with modifiers, `SS3 X` in application cursor mode, `CSI X` otherwise.
fn cursor_key(letter: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", letter as char).into_bytes()
    } else if app_cursor {
        vec![ESC, b'O', letter]
    } else {
        vec![ESC, b'[', letter]
    }
}

/// `SS3 X`, or `CSI 1;m X` with modifiers.
fn ss3_key(letter: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{}", letter as char).into_bytes()
    } else {
        vec![ESC, b'O', letter]
    }
}

/// `CSI n ~`, or `CSI n;m ~` with modifiers.
fn tilde_key(code: u8, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[{code};{param}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// Ctrl and Alt combinations of the letter keys. Plain letters arrive as text.
fn letter_key(key: Key, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    let name = key.name();
    let letter = match name.as_bytes() {
        [c @ b'A'..=b'Z'] => *c,
        _ => return None,
    };
    if ctrl {
        return Some(with_alt(alt, &[letter - b'A' + 1]));
    }
    if alt {
        let typed = if shift { letter } else { letter.to_ascii_lowercase() };
        return Some(vec![ESC, typed]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Modifiers {
        Modifiers::NONE
    }

    fn ctrl() -> Modifiers {
        Modifiers { ctrl: true, ..Modifiers::NONE }
    }

    fn shift() -> Modifiers {
        Modifiers { shift: true, ..Modifiers::NONE }
    }

    fn alt() -> Modifiers {
        Modifiers { alt: true, ..Modifiers::NONE }
    }

    fn enc(key: Key, mods: Modifiers) -> Option<Vec<u8>> {
        encode_key(key, mods, KeyModes::default())
    }

    fn app() -> KeyModes {
        KeyModes { app_cursor: true, app_keypad: false }
    }

    #[test]
    fn control_keys() {
        assert_eq!(enc(Key::Enter, none()), Some(b"\r".to_vec()));
        assert_eq!(enc(Key::Tab, none()), Some(b"\t".to_vec()));
        assert_eq!(enc(Key::Backspace, none()), Some(vec![0x7f]));
        assert_eq!(enc(Key::Backspace, ctrl()), Some(vec![0x08]));
        assert_eq!(enc(Key::Escape, none()), Some(vec![0x1b]));
        assert_eq!(enc(Key::Space, ctrl()), Some(vec![0x00]));
        assert_eq!(enc(Key::Space, none()), None);
        assert_eq!(enc(Key::Tab, shift()), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn ctrl_letters_and_punctuation() {
        assert_eq!(enc(Key::C, ctrl()), Some(vec![3]));
        assert_eq!(enc(Key::A, ctrl()), Some(vec![1]));
        assert_eq!(enc(Key::Z, ctrl()), Some(vec![26]));
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::NONE };
        assert_eq!(enc(Key::C, ctrl_shift), Some(vec![3]));
        assert_eq!(enc(Key::OpenBracket, ctrl()), Some(vec![0x1b]));
        assert_eq!(enc(Key::Backslash, ctrl()), Some(vec![0x1c]));
        assert_eq!(enc(Key::CloseBracket, ctrl()), Some(vec![0x1d]));
        let command = Modifiers { command: true, ..Modifiers::NONE };
        assert_eq!(enc(Key::Backspace, command), Some(vec![0x08]));
        assert_eq!(enc(Key::C, command), Some(vec![3]));
    }

    #[test]
    fn plain_letters_arrive_as_text() {
        assert_eq!(enc(Key::A, none()), None);
        assert_eq!(enc(Key::A, shift()), None);
        assert_eq!(enc(Key::Num1, none()), None);
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(enc(Key::A, alt()), Some(b"\x1ba".to_vec()));
        let alt_shift = Modifiers { alt: true, shift: true, ..Modifiers::NONE };
        assert_eq!(enc(Key::A, alt_shift), Some(b"\x1bA".to_vec()));
        let alt_ctrl = Modifiers { alt: true, ctrl: true, ..Modifiers::NONE };
        assert_eq!(enc(Key::C, alt_ctrl), Some(b"\x1b\x03".to_vec()));
        assert_eq!(enc(Key::Enter, alt()), Some(b"\x1b\r".to_vec()));
    }

    #[test]
    fn arrows_normal_application_and_modified() {
        assert_eq!(enc(Key::ArrowUp, none()), Some(b"\x1b[A".to_vec()));
        assert_eq!(enc(Key::ArrowLeft, none()), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode_key(Key::ArrowUp, none(), app()), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key(Key::ArrowRight, none(), app()), Some(b"\x1bOC".to_vec()));
        assert_eq!(enc(Key::ArrowRight, shift()), Some(b"\x1b[1;2C".to_vec()));
        assert_eq!(enc(Key::ArrowDown, ctrl()), Some(b"\x1b[1;5B".to_vec()));
        assert_eq!(enc(Key::ArrowDown, alt()), Some(b"\x1b[1;3B".to_vec()));
        assert_eq!(encode_key(Key::ArrowDown, ctrl(), app()), Some(b"\x1b[1;5B".to_vec()));
    }

    #[test]
    fn home_end_and_editing_keys() {
        assert_eq!(enc(Key::Home, none()), Some(b"\x1b[H".to_vec()));
        assert_eq!(enc(Key::End, none()), Some(b"\x1b[F".to_vec()));
        assert_eq!(encode_key(Key::Home, none(), app()), Some(b"\x1bOH".to_vec()));
        assert_eq!(encode_key(Key::End, none(), app()), Some(b"\x1bOF".to_vec()));
        assert_eq!(enc(Key::End, ctrl()), Some(b"\x1b[1;5F".to_vec()));
        assert_eq!(enc(Key::Insert, none()), Some(b"\x1b[2~".to_vec()));
        assert_eq!(enc(Key::Delete, none()), Some(b"\x1b[3~".to_vec()));
        assert_eq!(enc(Key::PageUp, none()), Some(b"\x1b[5~".to_vec()));
        assert_eq!(enc(Key::PageDown, none()), Some(b"\x1b[6~".to_vec()));
        assert_eq!(enc(Key::Delete, ctrl()), Some(b"\x1b[3;5~".to_vec()));
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F1, none()), Some(b"\x1bOP".to_vec()));
        assert_eq!(enc(Key::F4, none()), Some(b"\x1bOS".to_vec()));
        assert_eq!(enc(Key::F1, shift()), Some(b"\x1b[1;2P".to_vec()));
        assert_eq!(enc(Key::F5, none()), Some(b"\x1b[15~".to_vec()));
        assert_eq!(enc(Key::F6, none()), Some(b"\x1b[17~".to_vec()));
        assert_eq!(enc(Key::F8, none()), Some(b"\x1b[19~".to_vec()));
        assert_eq!(enc(Key::F10, none()), Some(b"\x1b[21~".to_vec()));
        assert_eq!(enc(Key::F11, none()), Some(b"\x1b[23~".to_vec()));
        assert_eq!(enc(Key::F12, none()), Some(b"\x1b[24~".to_vec()));
        assert_eq!(enc(Key::F5, ctrl()), Some(b"\x1b[15;5~".to_vec()));
    }

    #[test]
    fn unhandled_keys_produce_nothing() {
        assert_eq!(enc(Key::F13, none()), None);
        assert_eq!(enc(Key::Num5, ctrl()), None);
    }

    #[test]
    fn text_and_paste() {
        assert_eq!(encode_text("a", alt()), b"\x1ba".to_vec());
        assert_eq!(encode_text("ü", none()), "ü".as_bytes().to_vec());
        assert_eq!(encode_paste("a\r\nb", true), b"\x1b[200~a\rb\x1b[201~".to_vec());
        assert_eq!(encode_paste("a\nb\r\n", false), b"a\rb\r".to_vec());
        assert_eq!(encode_paste("x\x1b[201~y", true), b"\x1b[200~x[201~y\x1b[201~".to_vec());
    }
}
