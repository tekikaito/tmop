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
/// The same as [`REPLACING_FLAG`] for the copy an update starts: that is the new version, and
/// a version from before tray mode rejects an option it does not know (`cli::parse`, exit code
/// 2), so the update passes this environment variable instead, which such a version ignores.
pub const REPLACING_ENV: &str = "TMOP_REPLACING";

/// Whether this start takes the place of an exiting TMOP: [`REPLACING_FLAG`] in `args`, or
/// [`REPLACING_ENV`] set to `1`. `env` reads a variable (`std::env::var_os` in TMOP).
pub fn is_replacing(args: &[String], env: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    args.iter().any(|a| a == REPLACING_FLAG) || env(REPLACING_ENV).is_some_and(|v| v == "1")
}

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

impl Action {
    /// Whether TMOP ends: the close goes through, so eframe runs `App::on_exit`, which removes
    /// the icon and the shortcut, stops a recording, hangs up the Docker terminals (bounded to
    /// about a second) and saves the settings. A hide cancels the close (or was no close), so
    /// none of that runs and everything goes on while the window is hidden.
    pub fn ends_tmop(self) -> bool {
        matches!(self, Action::Close | Action::Exit)
    }
}

/// The event a message on the hand-off pipe stands for: [`EXIT_REQUEST`] (`TMOP --exit`) is
/// Exit, [`ALREADY_RUNNING`] (a `--tray` start at sign-in) is nothing, and anything else (empty,
/// or a page id) is a second start that brings the window to the front.
pub fn request_event(message: &str) -> Option<Event> {
    match message {
        ALREADY_RUNNING => None,
        EXIT_REQUEST => Some(Event::Exit),
        _ => Some(Event::SecondStart),
    }
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

/// What a second start in tray mode sends to the running TMOP, or `None` when it starts a
/// copy of its own. Only a start that asks for nothing but the window is handed over: no
/// arguments (an empty message: come to the front), `--page=<id>` (that page id, as a Task
/// Manager launch sends it), or [`FLAG`] ([`ALREADY_RUNNING`]). Any other argument
/// (`--record`, `--bench=`, `--disk-root=`, `--docker-terminal=`, `--window-size=`, ...) asks
/// for something the running TMOP would not do, so that start runs as it did before tray
/// mode; it is not the primary copy (see [`owns_tray`]).
pub fn hand_off_message(args: &[String]) -> Option<String> {
    let mut page = None;
    let mut tray = false;
    for a in args {
        if a == FLAG || a == FLAG_ALIAS {
            tray = true;
        } else {
            page = Some(a.strip_prefix("--page=").or_else(|| a.strip_prefix("--start-page="))?.trim());
        }
    }
    if tray {
        return Some(ALREADY_RUNNING.into());
    }
    // The pipe's own words are not page ids.
    let page = page.filter(|p| *p != ALREADY_RUNNING && *p != EXIT_REQUEST).unwrap_or("");
    Some(page.into())
}

/// Whether this TMOP has the tray icon and the shortcut (when tray mode is on). Only the
/// primary copy does: the one that has the hand-off pipe (`listening`), so later starts reach
/// it. Any other copy (one started with arguments of its own while a TMOP sits in the tray, or
/// a second window while tray mode is off) has no icon and no shortcut, and closing it ends it.
/// The popout-only window never has them.
pub fn owns_tray(caps_tray: bool, popout_only: bool, listening: bool) -> bool {
    caps_tray && !popout_only && listening
}

/// How long a start that takes the place of an exiting TMOP (`--replacing`) keeps trying to
/// get the pipe and the shortcut, which that TMOP holds until its process has ended.
pub const CLAIM_RETRY: std::time::Duration = std::time::Duration::from_secs(3);

/// What [`Retry::attempt`] says to do now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// Not yet.
    Wait,
    /// Try again; on failure keep quiet, there are more tries.
    Try,
    /// The last try; a failure is reported.
    Last,
}

/// Retries spaced [`Retry::STEP`] apart within a time limit (registering the shortcut in a
/// `--replacing` start).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    until: std::time::Instant,
    next: std::time::Instant,
}

impl Retry {
    pub const STEP: std::time::Duration = std::time::Duration::from_millis(250);

    /// Retries from `now` (after a first failed try) for `within`.
    pub fn new(now: std::time::Instant, within: std::time::Duration) -> Self {
        Retry { until: now + within, next: now + Self::STEP }
    }

    pub fn attempt(&mut self, now: std::time::Instant) -> Attempt {
        if now < self.next {
            return Attempt::Wait;
        }
        self.next = now + Self::STEP;
        if now >= self.until {
            Attempt::Last
        } else {
            Attempt::Try
        }
    }

    /// How long until the next try is due.
    pub fn wait(&self, now: std::time::Instant) -> std::time::Duration {
        self.next.saturating_duration_since(now)
    }
}

/// The slowest sampling while the window is hidden in the tray (see [`hidden_interval`]).
pub const HIDDEN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// The sampling interval while the window is hidden in the tray with no popout open:
/// `configured` while the flight recorder writes a recording (it needs every sample), else
/// `configured` slowed down to [`HIDDEN_INTERVAL`]. The flight recorder's ring (always on) and
/// its process timeline then go on, at a coarser grain, for exactly the time a tray user wants
/// to look back at. Paused (`None`) stays paused.
pub fn hidden_interval(configured: Option<std::time::Duration>, recording: bool) -> Option<std::time::Duration> {
    if recording {
        configured
    } else {
        configured.map(|c| c.max(HIDDEN_INTERVAL))
    }
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

    /// Hiding to the tray never ends TMOP (no `on_exit`: Docker terminals stay open, the
    /// recording goes on, alerts keep watching); each real exit does, from a shown or a hidden
    /// window: Exit in the icon's menu, `TMOP --exit` over the pipe, and `App::quit` (File →
    /// Exit, the update restart with `--replacing`, Restart as administrator), which all feed
    /// [`Event::Exit`] and let the close that follows through.
    #[test]
    fn hiding_never_ends_tmop_and_every_exit_does() {
        for hidden in [false, true] {
            let mut v = Visibility::new(hidden);
            let close = v.on(Event::CloseRequested, true);
            assert_eq!(close, Action::HideInsteadOfClose);
            assert!(!close.ends_tmop(), "the close button only hides");
            let mut v = Visibility::new(false);
            let min = v.on(Event::Minimized, true);
            assert!(min == Action::Hide && !min.ends_tmop());
            for e in [Event::IconClick, Event::IconDoubleClick, Event::Open, Event::Shortcut, Event::SecondStart] {
                let mut v = Visibility::new(hidden);
                assert!(!v.on(e, true).ends_tmop(), "{e:?}");
            }
        }
        // The three exit routes, each from a window that is hidden or shown.
        let routes = [Some(Event::Exit), request_event(EXIT_REQUEST)];
        for hidden in [false, true] {
            for route in routes {
                let mut v = Visibility::new(hidden);
                let first = v.on(route.expect("an exit event"), true);
                assert!(first == Action::Exit && first.ends_tmop());
                let then = v.on(Event::CloseRequested, true);
                assert!(then == Action::Close && then.ends_tmop(), "the close Exit sends goes through");
            }
        }
        // Without tray mode (or without an icon) a close ends TMOP as before.
        let mut v = Visibility::new(false);
        assert!(v.on(Event::CloseRequested, false).ends_tmop());
    }

    #[test]
    fn replacing_comes_from_the_flag_or_the_update_variable() {
        let none = |_: &str| None;
        let set = |v: &'static str| move |n: &str| (n == REPLACING_ENV).then(|| std::ffi::OsString::from(v));
        assert!(!is_replacing(&[], none));
        assert!(is_replacing(&args(&["--replacing", "--page=disk-space"]), none), "Restart as administrator");
        assert!(is_replacing(&[], set("1")), "the update restart");
        assert!(!is_replacing(&[], set("0")));
        assert!(!is_replacing(&[], set("")));
        assert!(!is_replacing(&args(&["--page=replacing"]), none));
    }

    #[test]
    fn pipe_messages_map_to_events() {
        assert_eq!(request_event(EXIT_REQUEST), Some(Event::Exit));
        assert_eq!(request_event(ALREADY_RUNNING), None, "a --tray start at sign-in changes nothing");
        assert_eq!(request_event(""), Some(Event::SecondStart));
        assert_eq!(request_event("processes"), Some(Event::SecondStart));
    }

    #[test]
    fn hidden_sampling_slows_down_unless_recording() {
        let secs = |s| Some(std::time::Duration::from_secs(s));
        assert_eq!(hidden_interval(secs(1), false), Some(HIDDEN_INTERVAL), "the flight ring goes on, slower");
        assert_eq!(hidden_interval(secs(1), true), secs(1), "a recording needs every sample");
        assert_eq!(hidden_interval(secs(10), false), secs(10), "never faster than configured");
        assert_eq!(hidden_interval(None, true), None, "paused stays paused");
        assert_eq!(hidden_interval(None, false), None);
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// Only a start that asks for the window alone is handed over; anything else starts a copy.
    #[test]
    fn only_window_starts_are_handed_over() {
        assert_eq!(hand_off_message(&[]).as_deref(), Some(""));
        assert_eq!(hand_off_message(&args(&["--page=processes"])).as_deref(), Some("processes"));
        assert_eq!(hand_off_message(&args(&["--start-page=docker"])).as_deref(), Some("docker"));
        assert_eq!(hand_off_message(&args(&["--page="])).as_deref(), Some(""));
        assert_eq!(hand_off_message(&args(&["--tray"])).as_deref(), Some(ALREADY_RUNNING));
        assert_eq!(hand_off_message(&args(&["--hidden", "--page=summary"])).as_deref(), Some(ALREADY_RUNNING));
        // The pipe's own messages are no page ids: `--page=tray-exit` does not end the running TMOP.
        assert_eq!(hand_off_message(&args(&["--page=tray-exit"])).as_deref(), Some(""));
        assert_eq!(hand_off_message(&args(&["--page=tray-start"])).as_deref(), Some(""));
        for other in [
            &["--record", "--record-seconds=60"][..],
            &["--record=C:\\rec"],
            &["--bench=all"],
            &["--disk-root=C:\\"],
            &["--disk-scan=D:\\"],
            &["--docker-terminal=web;sh"],
            &["--window-size=800x600"],
            &["--page=performance", "--device=gpu"],
            &["--maximized"],
            &["--popout"],
            &["--page=processes", "--record"],
            &["--tray", "--bench=cpu"],
            &["something-else"],
        ] {
            assert_eq!(hand_off_message(&args(other)), None, "{other:?}");
        }
    }

    /// The icon and the shortcut belong to the copy that has the pipe, and never to the popout.
    #[test]
    fn only_the_primary_copy_owns_the_tray() {
        assert!(owns_tray(true, false, true));
        assert!(!owns_tray(true, false, false), "a copy started with its own arguments");
        assert!(!owns_tray(true, true, true), "the popout-only window");
        assert!(!owns_tray(false, false, true), "no tray on this OS");
    }

    #[test]
    fn retries_are_spaced_and_end_with_a_last_try() {
        let t0 = std::time::Instant::now();
        let ms = |n| t0 + std::time::Duration::from_millis(n);
        let mut r = Retry::new(t0, std::time::Duration::from_secs(1));
        assert_eq!(r.attempt(ms(100)), Attempt::Wait);
        assert_eq!(r.wait(ms(100)), std::time::Duration::from_millis(150));
        assert_eq!(r.attempt(ms(250)), Attempt::Try);
        assert_eq!(r.attempt(ms(300)), Attempt::Wait, "spaced by a step");
        assert_eq!(r.attempt(ms(500)), Attempt::Try);
        assert_eq!(r.attempt(ms(750)), Attempt::Try);
        assert_eq!(r.attempt(ms(1000)), Attempt::Last);
        // A late wake-up (the window was busy) still ends with one last try, not several.
        let mut r = Retry::new(t0, std::time::Duration::from_secs(1));
        assert_eq!(r.attempt(ms(5000)), Attempt::Last);
        let tries = (1..=12).map(|i| r.attempt(ms(5000 + i * 250))).filter(|a| *a == Attempt::Try).count();
        assert_eq!(tries, 0);
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
