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
use std::sync::{Arc, Condvar, Mutex};
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
/// battery at all), then every [`BATTERY_EVERY`] while [`BatteryWatch::set_wanted`] asks for it,
/// and at once when it starts asking. Only a first look that finds no battery ends the thread; a
/// later "no battery" (an unreadable status, a battery missing for a moment after resume, a
/// `pmset` that timed out) keeps the last state and the watch goes on.
pub struct BatteryWatch {
    shared: Arc<Watch>,
}

#[derive(Default)]
struct Watch {
    state: AtomicU8,
    wanted: AtomicBool,
    /// Read now: the battery option was just switched on.
    poke: AtomicBool,
    stop: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Watch {
    /// Call after setting a flag the thread checks; the lock makes sure it is not lost.
    fn wake(&self) {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }
}

const UNREAD: u8 = 0;

fn code(s: Source) -> u8 {
    match s {
        Source::NoBattery => 1,
        Source::Mains => 2,
        Source::Battery => 3,
    }
}

/// The state after a reading `now` when the last one was `last`: a later "no battery" keeps it.
fn next_state(last: u8, now: Source) -> u8 {
    if now == Source::NoBattery && last != UNREAD {
        last
    } else {
        code(now)
    }
}

impl BatteryWatch {
    /// Starts the watch; `changed` runs on its thread when the source changes (a repaint).
    pub fn start(read: impl Fn() -> Source + Send + 'static, changed: impl Fn() + Send + 'static) -> Self {
        Self::start_every(read, BATTERY_EVERY, changed)
    }

    /// [`BatteryWatch::start`] with its own period (tests).
    fn start_every(read: impl Fn() -> Source + Send + 'static, every: Duration, changed: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Watch::default());
        let w = shared.clone();
        let spawned = std::thread::Builder::new().name("tmop-battery".into()).spawn(move || {
            let first = read();
            w.state.store(code(first), Ordering::Relaxed);
            changed();
            if first == Source::NoBattery {
                return;
            }
            loop {
                {
                    let held = w.lock.lock().unwrap_or_else(|e| e.into_inner());
                    if !w.stop.load(Ordering::Relaxed) && !w.poke.load(Ordering::Relaxed) {
                        // Not wanted: sleep until asked (the hour is only a safety net).
                        let wait = if w.wanted.load(Ordering::Relaxed) { every } else { Duration::from_secs(3600) };
                        let _ = w.cv.wait_timeout(held, wait);
                    }
                }
                if w.stop.load(Ordering::Relaxed) {
                    return;
                }
                let poked = w.poke.swap(false, Ordering::Relaxed);
                if !poked && !w.wanted.load(Ordering::Relaxed) {
                    continue;
                }
                let last = w.state.load(Ordering::Relaxed);
                let now = next_state(last, read());
                if now != last {
                    w.state.store(now, Ordering::Relaxed);
                    changed();
                }
            }
        });
        if spawned.is_err() {
            shared.state.store(code(Source::NoBattery), Ordering::Relaxed);
        }
        Self { shared }
    }

    /// Whether the power source is looked at again (the battery option is on). Switching it on
    /// reads the source at once.
    pub fn set_wanted(&self, on: bool) {
        if self.shared.wanted.swap(on, Ordering::Relaxed) != on && on {
            self.shared.poke.store(true, Ordering::Relaxed);
            self.shared.wake();
        }
    }

    /// Whether a battery was found (the option is offered then); false until the first look.
    pub fn has_battery(&self) -> bool {
        matches!(self.shared.state.load(Ordering::Relaxed), 2 | 3)
    }

    /// Whether the machine runs on its battery now.
    pub fn on_battery(&self) -> bool {
        self.shared.state.load(Ordering::Relaxed) == code(Source::Battery)
    }
}

impl Drop for BatteryWatch {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake();
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

    /// A source the test sets, and how often it was read.
    fn scripted(first: Source) -> (Arc<AtomicU8>, Arc<std::sync::atomic::AtomicUsize>, impl Fn() -> Source + Send + 'static) {
        let now = Arc::new(AtomicU8::new(code(first)));
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (n, r) = (now.clone(), reads.clone());
        let read = move || {
            r.fetch_add(1, Ordering::Relaxed);
            match n.load(Ordering::Relaxed) {
                1 => Source::NoBattery,
                2 => Source::Mains,
                _ => Source::Battery,
            }
        };
        (now, reads, read)
    }

    fn until(f: impl Fn() -> bool) -> bool {
        (0..300).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            f()
        })
    }

    /// The source changes over time: mains, battery, a moment of "no battery" (kept as it was),
    /// mains again. Only the first look ends the watch when it finds no battery.
    #[test]
    fn the_watch_follows_the_source_and_rides_out_a_missing_battery() {
        let (now, reads, read) = scripted(Source::Mains);
        let w = BatteryWatch::start_every(read, Duration::from_millis(20), || {});
        w.set_wanted(true);
        assert!(until(|| w.has_battery() && !w.on_battery()), "mains");
        now.store(code(Source::Battery), Ordering::Relaxed);
        assert!(until(|| w.on_battery()), "unplugged");
        now.store(code(Source::NoBattery), Ordering::Relaxed);
        let seen = reads.load(Ordering::Relaxed);
        assert!(until(|| reads.load(Ordering::Relaxed) >= seen + 3), "still reading");
        assert!(w.on_battery() && w.has_battery(), "a later 'no battery' keeps the last state");
        now.store(code(Source::Mains), Ordering::Relaxed);
        assert!(until(|| !w.on_battery()), "plugged in again after the gap");
        assert!(w.has_battery());

        let (_, reads, read) = scripted(Source::NoBattery);
        let w = BatteryWatch::start_every(read, Duration::from_millis(20), || {});
        w.set_wanted(true);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(reads.load(Ordering::Relaxed), 1, "no battery at the first look: the watch ends");
        assert!(!w.has_battery());
    }

    /// Switching the battery option on reads the source at once, not a period later; off, the
    /// watch does not read at all.
    #[test]
    fn switching_the_option_on_reads_at_once() {
        let (now, reads, read) = scripted(Source::Mains);
        let w = BatteryWatch::start_every(read, Duration::from_secs(3600), || {});
        assert!(until(|| reads.load(Ordering::Relaxed) == 1), "the first look");
        now.store(code(Source::Battery), Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(reads.load(Ordering::Relaxed), 1, "not wanted: no reads");
        assert!(!w.on_battery());
        w.set_wanted(true);
        assert!(until(|| w.on_battery()), "read at once when switched on");
        assert_eq!(reads.load(Ordering::Relaxed), 2);
        w.set_wanted(true);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(reads.load(Ordering::Relaxed), 2, "only a switch from off to on reads at once");
    }

    /// A settings save while the mode is on writes what the user set, not the effective values:
    /// fast sampling, glide and bloom on, as chosen.
    #[test]
    fn a_save_while_on_writes_the_configured_values() {
        let mut s = crate::settings::Settings::default();
        s.general.low_power = true;
        s.general.update_speed = "fast".into();
        s.appearance.high_frequency_visuals = true;
        s.appearance.bloom = true;
        let on = active(s.general.low_power, s.general.low_power_on_battery, false).is_some();
        assert!(on);
        // What the app works out from the settings while the mode is on.
        assert_eq!(interval(s.update_speed().interval(), on, false), Some(MIN_INTERVAL));
        assert!(!glide(s.appearance.high_frequency_visuals, on) && bloom(0.7, on) == 0.0);
        let dir = std::env::temp_dir().join(format!("tmop-lowpower-save-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.json");
        s.save(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read back");
        let _ = std::fs::remove_dir_all(&dir);
        let back = crate::settings::Settings::from_json(&text).expect("parse").settings;
        assert_eq!(back, s, "the file holds exactly the configured settings");
        assert_eq!(back.update_speed(), crate::settings::UpdateSpeed::Fast);
        assert!(back.appearance.high_frequency_visuals && back.appearance.bloom && back.general.low_power);
    }

    #[test]
    fn a_later_no_battery_keeps_the_last_state() {
        assert_eq!(next_state(UNREAD, Source::NoBattery), code(Source::NoBattery));
        assert_eq!(next_state(code(Source::Battery), Source::NoBattery), code(Source::Battery));
        assert_eq!(next_state(code(Source::Mains), Source::NoBattery), code(Source::Mains));
        assert_eq!(next_state(code(Source::Mains), Source::Battery), code(Source::Battery));
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
