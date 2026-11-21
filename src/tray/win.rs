//! The tray icon and the global shortcut on Windows.
//!
//! Both crates create a hidden message window on the calling thread, so [`Tray`] lives on the
//! UI thread, whose winit loop pumps their messages. Their events arrive through process-wide
//! handlers (set once, in [`Tray::new`]) that queue them and wake the UI, which reads them
//! with [`Tray::events`] in `App::logic`, also while the window is hidden.

use super::{Event, Shortcut};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const OPEN_ID: &str = "tmop-tray-open";
const EXIT_ID: &str = "tmop-tray-exit";

pub struct Tray {
    icon: Option<TrayIcon>,
    /// Created on first use: until then TMOP has no hidden hotkey window at all.
    hotkeys: Option<GlobalHotKeyManager>,
    registered: Option<HotKey>,
    events: Receiver<Event>,
}

impl Tray {
    /// Installs the event handlers. `wake` is called after each event (the UI's repaint).
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, events) = channel();
        let wake = Arc::new(wake);
        {
            let (tx, wake) = (tx.clone(), wake.clone());
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                let event = match e {
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => Event::IconClick,
                    TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => Event::IconDoubleClick,
                    _ => return,
                };
                if tx.send(event).is_ok() {
                    wake();
                }
            }));
        }
        {
            let (tx, wake) = (tx.clone(), wake.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let event = match e.id.as_ref() {
                    OPEN_ID => Event::Open,
                    EXIT_ID => Event::Exit,
                    _ => return,
                };
                if tx.send(event).is_ok() {
                    wake();
                }
            }));
        }
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state == HotKeyState::Pressed && tx.send(Event::Shortcut).is_ok() {
                wake();
            }
        }));
        Tray { icon: None, hotkeys: None, registered: None, events }
    }

    /// Adds or removes the notification area icon.
    pub fn set_icon(&mut self, on: bool) -> Result<(), String> {
        if !on {
            self.icon = None;
            return Ok(());
        }
        if self.icon.is_some() {
            return Ok(());
        }
        let menu = Menu::new();
        let open = MenuItem::with_id(OPEN_ID, "Open TMOP", true, None);
        let exit = MenuItem::with_id(EXIT_ID, "Exit", true, None);
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &exit]).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_id("tmop")
            .with_tooltip(format!("{} \u{2013} {}", crate::APP_NAME, crate::APP_LONG_NAME))
            .with_icon(icon()?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        self.icon = Some(icon);
        Ok(())
    }

    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    /// Registers `shortcut` in place of the current one (`None`: none). On failure no
    /// shortcut is registered.
    pub fn set_shortcut(&mut self, shortcut: Option<&Shortcut>) -> Result<(), String> {
        if let (Some(old), Some(m)) = (self.registered.take(), &self.hotkeys) {
            let _ = m.unregister(old);
        }
        let Some(s) = shortcut else { return Ok(()) };
        let hotkey: HotKey = s.hotkey_text().parse().map_err(|_| format!("{} cannot be used as a shortcut.", s.label()))?;
        if self.hotkeys.is_none() {
            self.hotkeys = Some(GlobalHotKeyManager::new().map_err(|e| e.to_string())?);
        }
        let m = self.hotkeys.as_ref().expect("created above");
        match m.register(hotkey) {
            Ok(()) => {
                self.registered = Some(hotkey);
                Ok(())
            }
            // ERROR_HOTKEY_ALREADY_REGISTERED, and Windows' own reserved combinations.
            Err(global_hotkey::Error::AlreadyRegistered(_)) | Err(global_hotkey::Error::OsError(_)) => Err(format!("{} is taken: another program or Windows itself uses it. Choose another combination.", s.label())),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Events since the last call, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

/// The exe's own icon at the notification area's size, or the 64 px window icon.
fn icon() -> Result<Icon, String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON, SM_CYSMICON};
    // SAFETY: plain metric queries.
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    // build.rs puts the icon group in as resource 1.
    if let Ok(i) = Icon::from_resource(1, (w > 0 && h > 0).then_some((w as u32, h as u32))) {
        return Ok(i);
    }
    let data = crate::icon::window_icon().ok_or("no icon")?;
    Icon::from_rgba(data.rgba.clone(), data.width, data.height).map_err(|e| e.to_string())
}

/// Cloaks or uncloaks a window (DWM keeps a cloaked window off the screen). eframe shows the
/// window after its first frame whatever the app asks; a `--tray` start cloaks it so that
/// frame never shows before the window is hidden.
pub fn cloak(hwnd: isize, on: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    if hwnd == 0 {
        return;
    }
    let value = i32::from(on);
    // SAFETY: `hwnd` is the main window; DWMWA_CLOAK takes a BOOL.
    unsafe {
        let _ = DwmSetWindowAttribute(HWND(hwnd as *mut _), DWMWA_CLOAK, (&raw const value).cast(), std::mem::size_of::<i32>() as u32);
    }
}

/// Brings the shown window to the front with `SetForegroundWindow`. Windows allows that right
/// after the shortcut, a click on the icon, or a second start (which calls
/// `AllowSetForegroundWindow`); otherwise the taskbar button flashes. Used instead of
/// `ViewportCommand::Focus`, whose winit implementation injects an Alt key press to force it;
/// TMOP sends that command nowhere on Windows. Returns whether Windows agreed.
pub fn bring_to_front(hwnd: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    // SAFETY: `hwnd` is the main window (or 0, which the call rejects).
    hwnd != 0 && unsafe { SetForegroundWindow(HWND(hwnd as *mut _)).as_bool() }
}

/// Whether a Windows key is held down (egui reports no Windows key). Reads the key state, as
/// every program may; no hook.
pub fn win_key_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_RWIN};
    // SAFETY: plain key state queries.
    unsafe { (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0 || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows gives a combination to one window only: the second registration is refused with
    /// the "taken" message, and works once the first lets go. An odd combination nobody uses,
    /// held for a moment.
    #[test]
    fn a_taken_combination_is_reported() {
        let s = Shortcut::parse("Ctrl+Alt+Shift+F24").unwrap();
        let (mut first, mut second) = (Tray::new(|| {}), Tray::new(|| {}));
        if first.set_shortcut(Some(&s)).is_err() {
            return; // something on this machine holds it already
        }
        let e = second.set_shortcut(Some(&s)).unwrap_err();
        assert_eq!(e, "Ctrl + Alt + Shift + F24 is taken: another program or Windows itself uses it. Choose another combination.");
        first.set_shortcut(None).unwrap();
        second.set_shortcut(Some(&s)).unwrap();
        second.set_shortcut(None).unwrap();
    }
}
