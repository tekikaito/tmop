//! Keeping TMOP running in the notification area (opt-in, Windows only; see `Caps::tray`).
//!
//! * With `tray.keepRunning` on, closing or minimizing the main window hides it; the tray icon
//!   brings it back (left click shows or hides it, its menu has Open TMOP and Exit).
//! * `tray.shortcut` (with `tray.shortcutEnabled`) is registered with `RegisterHotKey`, through
//!   the `global-hotkey` crate. There is no keyboard hook: Windows tells TMOP about that one
//!   combination and nothing else. A combination another program holds is refused by Windows,
//!   and TMOP says so.
//! * [`autostart`] is the per-user Run entry that starts TMOP hidden in the tray (`--tray`).
//! * A second start while tray mode is on hands over to the running TMOP over the pipe of
//!   [`crate::taskmgr::hand_off`], which brings its window to the front. `TMOP --exit` asks it
//!   to exit over the same pipe (for scripts and installers, which otherwise could only kill
//!   it and leave a dead icon behind).
//!
//! The window side is [`Visibility`], a plain state machine; the icon and the shortcut are in
//! [`Tray`] (a stub off Windows).

pub mod autostart;
pub mod shortcut;

pub use shortcut::{Capture, Shortcut};

/// Command-line flag: start hidden in the notification area (the Run entry passes it).
pub const FLAG: &str = "--tray";
/// Also accepted for [`FLAG`].
pub const FLAG_ALIAS: &str = "--hidden";
/// Given by TMOP to a copy it starts in its own place (after an update, as administrator)
/// just before it exits: that copy does not hand over to the one that is exiting.
pub const REPLACING_FLAG: &str = "--replacing";

/// What happened to the window, the icon or the shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The window's close button, Alt+F4, or the system closing it.
    CloseRequested,
    /// The window was minimized.
    Minimized,
    /// Left click on the tray icon (the button coming up).
    IconClick,
    /// Left double click on the tray icon.
    IconDoubleClick,
    /// "Open TMOP" in the icon's menu.
    Open,
    /// "Exit" in the icon's menu.
    Exit,
    /// The global shortcut was pressed.
    Shortcut,
    /// TMOP was started again while running.
    SecondStart,
}

/// What the window has to do about an [`Event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Hide the window (it is in the tray from now on).
    Hide,
    /// Refuse the close and hide the window instead.
    HideInsteadOfClose,
    /// Show the window, restore it if it was minimized, and bring it to the front.
    Show,
    /// Let the close go ahead: TMOP exits.
    Close,
    /// Close the window, which ends TMOP (the next close request is let through).
    Exit,
}

/// Whether the main window is hidden in the tray, and what each event does to that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visibility {
    pub hidden: bool,
    /// Exit was chosen: the close that follows is not turned into a hide.
    exiting: bool,
    /// The last icon event was a double click, whose second button-up is not a click of its own.
    after_double: bool,
}

impl Visibility {
    /// Starts hidden (`--tray`) or shown.
    pub fn new(hidden: bool) -> Self {
        Self { hidden, ..Default::default() }
    }

    /// The window's answer to `event`. `keep_running`: tray mode is on and the icon is there
    /// (without an icon the window must never be hidden, or it could not be brought back).
    pub fn on(&mut self, event: Event, keep_running: bool) -> Action {
        let after_double = std::mem::replace(&mut self.after_double, event == Event::IconDoubleClick);
        match event {
            Event::CloseRequested if self.exiting || !keep_running => Action::Close,
            Event::CloseRequested => {
                self.hidden = true;
                Action::HideInsteadOfClose
            }
            Event::Minimized if keep_running && !self.hidden => {
                self.hidden = true;
                Action::Hide
            }
            Event::Minimized => Action::Nothing,
            Event::IconClick if after_double => Action::Nothing,
            Event::IconClick if self.hidden || !keep_running => self.show(),
            Event::IconClick => {
                self.hidden = true;
                Action::Hide
            }
            Event::IconDoubleClick | Event::Open | Event::Shortcut | Event::SecondStart => self.show(),
            Event::Exit => {
                self.exiting = true;
                self.hidden = false;
                Action::Exit
            }
        }
    }

    fn show(&mut self) -> Action {
        self.hidden = false;
        Action::Show
    }
}

/// What a `--tray` start sends to a TMOP that is already running: "I am here", which changes
/// nothing (any other message brings the window to the front).
pub const ALREADY_RUNNING: &str = "tray-start";
/// What `TMOP --exit` sends: the running TMOP exits, as with Exit in the icon's menu.
pub const EXIT_REQUEST: &str = "tray-exit";

/// The sampling interval while the window is hidden in the tray with no popout open:
/// `configured` while the flight recorder writes a recording (it needs every sample), else
/// none. Everything else the samples feed is on screen only.
pub fn hidden_interval(configured: Option<std::time::Duration>, recording: bool) -> Option<std::time::Duration> {
    configured.filter(|_| recording)
}

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{bring_to_front, cloak, win_key_down, Tray};

#[cfg(not(windows))]
mod stub {
    use super::{Event, Shortcut};

    /// No tray off Windows: the Settings section is hidden there.
    pub struct Tray;

    impl Tray {
        pub fn new(_wake: impl Fn() + Send + Sync + 'static) -> Self {
            Tray
        }
        pub fn set_icon(&mut self, _on: bool) -> Result<(), String> {
            Err("not available on this system".into())
        }
        pub fn has_icon(&self) -> bool {
            false
        }
        pub fn set_shortcut(&mut self, _shortcut: Option<&Shortcut>) -> Result<(), String> {
            Err("not available on this system".into())
        }
        pub fn events(&self) -> Vec<Event> {
            Vec::new()
        }
    }

    pub fn win_key_down() -> bool {
        false
    }

    pub fn cloak(_hwnd: isize, _on: bool) {}

    pub fn bring_to_front(_hwnd: isize) -> bool {
        false
    }
}
#[cfg(not(windows))]
pub use stub::{bring_to_front, cloak, win_key_down, Tray};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_and_minimize_hide_only_in_tray_mode() {
        let mut v = Visibility::new(false);
        assert_eq!(v.on(Event::CloseRequested, false), Action::Close);
        assert_eq!(v.on(Event::Minimized, false), Action::Nothing);
        assert!(!v.hidden);
        assert_eq!(v.on(Event::CloseRequested, true), Action::HideInsteadOfClose);
        assert!(v.hidden);
        assert_eq!(v.on(Event::Shortcut, true), Action::Show);
        assert_eq!(v.on(Event::Minimized, true), Action::Hide);
        assert!(v.hidden);
        // A hidden window that reports minimized again is left alone.
        assert_eq!(v.on(Event::Minimized, true), Action::Nothing);
    }

    #[test]
    fn icon_click_toggles_and_a_double_click_opens() {
        let mut v = Visibility::new(true);
        assert_eq!(v.on(Event::IconClick, true), Action::Show);
        assert_eq!(v.on(Event::IconClick, true), Action::Hide);
        assert_eq!(v.on(Event::IconClick, true), Action::Show);
        // Down, up (click), double click, up: the window ends up shown, whatever it was before.
        for start in [true, false] {
            let mut v = Visibility::new(start);
            v.on(Event::IconClick, true);
            assert_eq!(v.on(Event::IconDoubleClick, true), Action::Show);
            assert_eq!(v.on(Event::IconClick, true), Action::Nothing, "the second button-up of a double click");
            assert!(!v.hidden);
            assert_eq!(v.on(Event::IconClick, true), Action::Hide, "a later click counts again");
        }
    }

    #[test]
    fn open_shortcut_and_second_start_always_show() {
        for e in [Event::Open, Event::Shortcut, Event::SecondStart] {
            for hidden in [true, false] {
                let mut v = Visibility::new(hidden);
                assert_eq!(v.on(e, true), Action::Show, "{e:?} from hidden={hidden}");
                assert!(!v.hidden);
            }
        }
    }

    #[test]
    fn exit_lets_the_close_through() {
        let mut v = Visibility::new(true);
        assert_eq!(v.on(Event::Exit, true), Action::Exit);
        assert_eq!(v.on(Event::CloseRequested, true), Action::Close);
    }

    #[test]
    fn hidden_sampling_runs_only_for_a_recording() {
        let second = Some(std::time::Duration::from_secs(1));
        assert_eq!(hidden_interval(second, false), None);
        assert_eq!(hidden_interval(second, true), second);
        assert_eq!(hidden_interval(None, true), None, "paused stays paused");
    }

    #[test]
    fn without_an_icon_nothing_hides() {
        let mut v = Visibility::new(false);
        for e in [Event::CloseRequested, Event::Minimized, Event::IconClick] {
            assert_ne!(v.on(e, false), Action::Hide);
            assert!(!v.hidden, "{e:?}");
        }
    }
}
