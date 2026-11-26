//! Low-power mode (#194): one switch, plus "turn on automatically on battery".
//!
//! While the mode is active TMOP draws graphs without glide, glow or gradient fills, samples at
//! most once a second, and waits longer between frames while its window is not focused. It never
//! rewrites the user's own settings for these: the effective values are worked out here at use,
//! so switching the mode off gives back exactly what was set. A running recording keeps the
//! configured sample rate; alerts work from timestamps and stay correct at the slower rate.
//!
//! The decisions are pure functions; [`BatteryWatch`] reads the power source in the background.

use crate::pro::power::Source;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The slowest the mode samples on its own: once a second. A slower setting of the user stays.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The longest wait for a frame while nothing moves (as in `App::update_inner`) ...
pub const HEARTBEAT: Duration = Duration::from_millis(500);
/// ... and, in the mode, while the window is not focused. Samples still bring a frame each.
pub const UNFOCUSED_HEARTBEAT: Duration = Duration::from_secs(2);

/// How often [`BatteryWatch`] looks at the power source.
pub const BATTERY_EVERY: Duration = Duration::from_secs(30);

/// Why the mode is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The "Low-power mode" switch.
    Setting,
    /// "Turn on automatically on battery", and the machine runs on its battery.
    Battery,
}

/// Whether the mode is on, and why. The switch wins over the battery.
pub fn active(setting: bool, auto_on_battery: bool, on_battery: bool) -> Option<Why> {
    if setting {
        Some(Why::Setting)
    } else if auto_on_battery && on_battery {
        Some(Why::Battery)
    } else {
        None
    }
}

/// The sample interval to use: the user's `configured` one, at least [`MIN_INTERVAL`] while the
/// mode is on. A recording keeps the configured rate; paused (`None`) stays paused.
pub fn interval(configured: Option<Duration>, on: bool, recording: bool) -> Option<Duration> {
    if on && !recording {
        configured.map(|c| c.max(MIN_INTERVAL))
    } else {
        configured
    }
}

/// Whether graphs glide between samples (high-frequency visuals): the user's choice, off in the mode.
pub fn glide(configured: bool, on: bool) -> bool {
    configured && !on
}

/// The glow strength to draw with: the user's, none in the mode.
pub fn bloom(configured: f32, on: bool) -> f32 {
    if on {
        0.0
    } else {
        configured
    }
}

/// Whether graphs get their gradient fill under the trace: always, except in the mode.
pub fn fills(on: bool) -> bool {
    !on
}

/// The longest wait for the next frame while nothing moves.
pub fn heartbeat(on: bool, focused: bool) -> Duration {
    if on && !focused {
        UNFOCUSED_HEARTBEAT
    } else {
        HEARTBEAT
    }
}

/// The status-bar marker's tooltip.
pub fn tooltip(why: Why, configured: Option<Duration>, recording: bool) -> String {
    let rate = match interval(configured, true, recording) {
        _ if recording => "sampling at the set rate while recording".to_string(),
        None => "sampling paused".to_string(),
        Some(d) if Some(d) == configured => format!("sampling every {}", secs(d)),
        Some(d) => format!("sampling every {} instead of {}", secs(d), configured.map(secs).unwrap_or_default()),
    };
    let reason = match why {
        Why::Setting => "Low-power mode is on (Settings > General).",
        Why::Battery => "Low-power mode is on because this computer runs on its battery (Settings > General).",
    };
    format!("{reason}\nGraphs: no glide, glow or fill; {rate}; fewer redraws while the window is in the background. Readings stay exact.")
}

fn secs(d: Duration) -> String {
    let text = format!("{:.2}", d.as_secs_f32());
    format!("{} s", text.trim_end_matches('0').trim_end_matches('.'))
}

/// Reads the power source on a thread of its own: once at the start (to know whether there is a
/// battery at all), then every [`BATTERY_EVERY`] while [`BatteryWatch::set_wanted`] asks for it.
/// A machine without a battery ends the thread after the first look.
pub struct BatteryWatch {
    state: Arc<AtomicU8>,
    wanted: Arc<AtomicBool>,
}

const UNREAD: u8 = 0;

fn code(s: Source) -> u8 {
    match s {
        Source::NoBattery => 1,
        Source::Mains => 2,
        Source::Battery => 3,
    }
}

impl BatteryWatch {
    /// Starts the watch; `changed` runs on its thread when the source changes (a repaint).
    pub fn start(read: fn() -> Source, changed: impl Fn() + Send + 'static) -> Self {
        let state = Arc::new(AtomicU8::new(UNREAD));
        let wanted = Arc::new(AtomicBool::new(false));
        let (st, w) = (state.clone(), wanted.clone());
        let spawned = std::thread::Builder::new().name("tmop-battery".into()).spawn(move || loop {
            let now = code(read());
            if st.swap(now, Ordering::Relaxed) != now {
                changed();
            }
            if now == code(Source::NoBattery) {
                return;
            }
            std::thread::sleep(BATTERY_EVERY);
            while !w.load(Ordering::Relaxed) {
                std::thread::sleep(BATTERY_EVERY);
            }
        });
        if spawned.is_err() {
            state.store(code(Source::NoBattery), Ordering::Relaxed);
        }
        Self { state, wanted }
    }

    /// Whether the power source is looked at again (the battery option is on).
    pub fn set_wanted(&self, on: bool) {
        self.wanted.store(on, Ordering::Relaxed);
    }

    /// Whether a battery was found (the option is offered then); false until the first look.
    pub fn has_battery(&self) -> bool {
        matches!(self.state.load(Ordering::Relaxed), 2 | 3)
    }

    /// Whether the machine runs on its battery now.
    pub fn on_battery(&self) -> bool {
        self.state.load(Ordering::Relaxed) == code(Source::Battery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: Option<Duration> = Some(Duration::from_millis(500));

    #[test]
    fn the_switch_or_the_battery_turns_it_on() {
        assert_eq!(active(false, false, false), None);
        assert_eq!(active(false, false, true), None, "on battery without the option");
        assert_eq!(active(false, true, false), None, "the option on mains");
        assert_eq!(active(false, true, true), Some(Why::Battery));
        assert_eq!(active(true, false, false), Some(Why::Setting));
        assert_eq!(active(true, true, true), Some(Why::Setting), "the switch wins");
    }

    #[test]
    fn sampling_slows_to_once_a_second_but_keeps_a_slower_choice() {
        assert_eq!(interval(HALF, true, false), Some(MIN_INTERVAL));
        assert_eq!(interval(Some(Duration::from_millis(250)), true, false), Some(MIN_INTERVAL));
        assert_eq!(interval(Some(Duration::from_secs(2)), true, false), Some(Duration::from_secs(2)), "slower stays");
        assert_eq!(interval(None, true, false), None, "paused stays paused");
    }

    #[test]
    fn a_recording_keeps_its_rate() {
        assert_eq!(interval(HALF, true, true), HALF);
        assert_eq!(interval(Some(Duration::from_millis(250)), true, true), Some(Duration::from_millis(250)));
    }

    /// Off, every effective value is the user's own, whatever it is.
    #[test]
    fn off_gives_back_exactly_what_was_set() {
        for configured in [None, Some(Duration::from_millis(250)), HALF, Some(Duration::from_secs(2))] {
            for recording in [false, true] {
                assert_eq!(interval(configured, false, recording), configured);
            }
        }
        for hf in [false, true] {
            assert_eq!(glide(hf, false), hf);
        }
        for b in [0.0, 0.1, 0.7, 1.0] {
            assert_eq!(bloom(b, false), b);
        }
        assert!(fills(false));
        assert_eq!(heartbeat(false, false), HEARTBEAT);
        assert_eq!(heartbeat(false, true), HEARTBEAT);
    }

    /// The mode works out values at use: the user's settings are inputs, never changed.
    #[test]
    fn the_users_settings_are_left_alone() {
        let mut s = crate::settings::Settings::default();
        s.appearance.high_frequency_visuals = true;
        s.appearance.bloom = true;
        s.general.update_speed = "fast".into();
        let before = s.clone();
        let on = active(true, false, false).is_some();
        let _ = (glide(s.appearance.high_frequency_visuals, on), bloom(0.7, on), interval(s.update_speed().interval(), on, false), fills(on));
        assert_eq!(s, before);
        assert!(!s.general.low_power && !s.general.low_power_on_battery, "both are off by default");
    }

    #[test]
    fn on_the_graphs_and_frames_are_plain_and_slow_in_the_background() {
        assert!(!glide(true, true));
        assert_eq!(bloom(0.7, true), 0.0);
        assert!(!fills(true));
        assert_eq!(heartbeat(true, false), UNFOCUSED_HEARTBEAT);
        assert_eq!(heartbeat(true, true), HEARTBEAT, "a focused window keeps the normal rate");
    }

    #[test]
    fn the_tooltip_says_why_and_what() {
        let t = tooltip(Why::Battery, HALF, false);
        assert!(t.contains("battery") && t.contains("every 1 s instead of 0.5 s") && t.contains("no glide"), "{t}");
        let t = tooltip(Why::Setting, Some(Duration::from_secs(2)), false);
        assert!(!t.contains("battery") && t.contains("every 2 s") && !t.contains("instead"), "{t}");
        assert!(tooltip(Why::Setting, HALF, true).contains("set rate while recording"));
        assert!(tooltip(Why::Setting, None, false).contains("paused"));
    }

    #[test]
    fn the_watch_reports_what_it_read() {
        fn wait(w: &BatteryWatch, f: impl Fn(&BatteryWatch) -> bool) -> bool {
            (0..200).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                f(w)
            })
        }
        let w = BatteryWatch::start(|| Source::Battery, || {});
        assert!(wait(&w, |w| w.on_battery() && w.has_battery()));
        let w = BatteryWatch::start(|| Source::Mains, || {});
        assert!(wait(&w, |w| w.has_battery()) && !w.on_battery());
        let w = BatteryWatch::start(|| Source::NoBattery, || {});
        std::thread::sleep(Duration::from_millis(50));
        assert!(!w.has_battery() && !w.on_battery());
    }

    #[test]
    fn the_power_source_never_reads_battery_when_unsure() {
        use crate::pro::power::source_of;
        assert_eq!(source_of(Some(false), true), Source::Battery);
        assert_eq!(source_of(Some(true), true), Source::Mains);
        assert_eq!(source_of(None, true), Source::Mains, "unknown mains: not on battery");
        assert_eq!(source_of(Some(false), false), Source::NoBattery);
    }
}
