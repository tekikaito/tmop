//! Characters produced by AltGr on the active keyboard layout.
//!
//! Windows reports AltGr as Ctrl+Alt, and egui-winit sends no `Event::Text` while Ctrl is
//! held. The character therefore has to be looked up from the layout by the physical key.

use eframe::egui::Key;

/// Keyboard state for ToUnicodeEx with the flag below: the layout is not modified.
#[cfg(windows)]
const DO_NOT_CHANGE_KEYBOARD_STATE: u32 = 0x4;

/// US scancode (set 1) of the physical key position, for keys that can carry an AltGr character.
#[cfg(windows)]
fn scancode(physical: Key) -> Option<u32> {
    Some(match physical {
        Key::Num1 => 0x02,
        Key::Num2 => 0x03,
        Key::Num3 => 0x04,
        Key::Num4 => 0x05,
        Key::Num5 => 0x06,
        Key::Num6 => 0x07,
        Key::Num7 => 0x08,
        Key::Num8 => 0x09,
        Key::Num9 => 0x0A,
        Key::Num0 => 0x0B,
        Key::Minus => 0x0C,
        Key::Equals => 0x0D,
        Key::Q => 0x10,
        Key::W => 0x11,
        Key::E => 0x12,
        Key::R => 0x13,
        Key::T => 0x14,
        Key::Y => 0x15,
        Key::U => 0x16,
        Key::I => 0x17,
        Key::O => 0x18,
        Key::P => 0x19,
        Key::OpenBracket => 0x1A,
        Key::CloseBracket => 0x1B,
        Key::A => 0x1E,
        Key::S => 0x1F,
        Key::D => 0x20,
        Key::F => 0x21,
        Key::G => 0x22,
        Key::H => 0x23,
        Key::J => 0x24,
        Key::K => 0x25,
        Key::L => 0x26,
        Key::Semicolon => 0x27,
        Key::Quote => 0x28,
        Key::Backtick => 0x29,
        Key::Backslash => 0x2B,
        Key::Z => 0x2C,
        Key::X => 0x2D,
        Key::C => 0x2E,
        Key::V => 0x2F,
        Key::B => 0x30,
        Key::N => 0x31,
        Key::M => 0x32,
        Key::Comma => 0x33,
        Key::Period => 0x34,
        Key::Slash => 0x35,
        // ISO key left of Z (`<` on German). egui-winit maps it to no physical key, so it
        // arrives as the logical `|` (AltGr+`<` on German) and is looked up here.
        Key::Pipe => 0x56,
        _ => return None,
    })
}

/// Character produced by AltGr (Ctrl+Alt) plus the given physical key under the
/// active keyboard layout, if the layout defines one.
#[cfg(windows)]
pub fn altgr_char(physical: Key) -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardLayout, MapVirtualKeyExW, MAPVK_VSC_TO_VK_EX, VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU,
    };

    let scan = scancode(physical)?;
    // SAFETY: plain user32 queries with no side effects; no input is injected.
    let layout = unsafe { GetKeyboardLayout(0) };
    let vk = unsafe { MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, layout) };
    if vk == 0 {
        return None;
    }

    let mut altgr_state = [0u8; 256];
    for key in [VK_CONTROL, VK_MENU, VK_LCONTROL, VK_RMENU] {
        altgr_state[key.0 as usize] = 0x80;
    }
    let altgr = to_unicode(vk, scan, &altgr_state, layout)?;
    let plain = to_unicode(vk, scan, &[0u8; 256], layout);

    let mut chars = altgr.chars();
    let c = chars.next()?;
    // Only a single printable character counts; control characters and dead keys do not.
    if chars.next().is_some() || c < ' ' || plain.as_deref() == Some(altgr.as_str()) {
        return None;
    }
    Some(c)
}

#[cfg(windows)]
fn to_unicode(vk: u32, scan: u32, state: &[u8; 256], layout: windows::Win32::UI::Input::KeyboardAndMouse::HKL) -> Option<String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::ToUnicodeEx;

    let mut buf = [0u16; 8];
    // SAFETY: the buffer and state array are valid for the lengths passed.
    let n = unsafe { ToUnicodeEx(vk, scan, state, &mut buf, DO_NOT_CHANGE_KEYBOARD_STATE, layout) };
    // Negative: a dead key, which produces no character by itself.
    if n <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

#[cfg(not(windows))]
pub fn altgr_char(_physical: Key) -> Option<char> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;

    #[test]
    fn altgr_follows_the_active_layout() {
        // Low word of the HKL is the language id: 0x0407 German, 0x0409 US English.
        let layout = unsafe { GetKeyboardLayout(0) }.0 as usize & 0xffff;
        eprintln!("keyboard layout id: {layout:#06x}");
        eprintln!("altgr_char: Q={:?} Num7={:?} Minus={:?}", altgr_char(Key::Q), altgr_char(Key::Num7), altgr_char(Key::Minus));
        match layout {
            0x0407 => {
                assert_eq!(altgr_char(Key::Q), Some('@'));
                assert_eq!(altgr_char(Key::Num7), Some('{'));
            }
            0x0409 => assert_eq!(altgr_char(Key::Q), None),
            _ => {
                let _ = altgr_char(Key::Q);
            }
        }
    }
}
