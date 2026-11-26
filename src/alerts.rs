//! Alerts: a desktop notification when CPU usage, memory usage or the temperature stays above a
//! limit (Settings → Alerts). The temperature is the hottest reading TMOP has
//! ([`crate::metrics::ThermalSample::max_c`]): CPU, board sensors and each GPU's own temperature.
//! A GPU's hot spot and memory (junction) readings are not watched: they run well above the core
//! in normal use. The notice names the sensor that is hottest ("Hottest: GPU 92°C (WDDM)").
//!
//! The decision is a pure state machine, [`Tracker`]: samples and the time they arrived in,
//! [`Event`]s out. [`Monitor`] runs one tracker per [`Metric`] over a [`Snapshot`] and words the
//! notification. [`hook`] is the thin shell the sampler thread calls; [`send`] shows the
//! notification through [`crate::platform::notify`].

use crate::metrics::{Hottest, Snapshot};
use crate::process::{format_bytes, ProcessRow};
use crate::settings::Alerts;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How far below the limit a value must fall before the alert can fire again (percentage points).
pub const MARGIN_PCT: f32 = 5.0;
/// The same for temperatures (°C).
pub const MARGIN_C: f32 = 3.0;
/// One alert fires at most this often.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Samples further apart than this (updates were paused, the PC slept) do not count as one
/// unbroken stretch above the limit.
pub const MAX_GAP: Duration = Duration::from_secs(30);

/// What an alert watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Memory,
    Temperature,
}

impl Metric {
    pub const ALL: [Metric; 3] = [Metric::Cpu, Metric::Memory, Metric::Temperature];

    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU usage high",
            Metric::Memory => "Memory usage high",
            Metric::Temperature => "Temperature high",
        }
    }
}

/// One alert's settings, in the units of its value (percent, or °C).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub enabled: bool,
    /// The value must be strictly above this.
    pub limit: f32,
    /// ... for at least this long.
    pub hold: Duration,
    /// After firing, the value must drop strictly below `limit - margin` to re-arm.
    pub margin: f32,
}

impl Rule {
    pub const OFF: Rule = Rule { enabled: false, limit: 90.0, hold: Duration::from_secs(60), margin: MARGIN_PCT };
}

/// What a [`Tracker`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The value stayed above the limit long enough: notify.
    Fire,
    /// After a [`Event::Fire`], the value dropped far enough below the limit; the alert is armed again.
    Clear,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    /// At or below the limit (or armed again).
    #[default]
    Idle,
    /// Above the limit since this time, not long enough yet (or held back by [`MIN_INTERVAL`]).
    Above { since: Duration },
    /// Fired; silent until the value drops below `limit - margin`.
    Fired,
}

/// The state machine of one alert. Times are measured from any fixed starting point, and must
/// not go backwards.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracker {
    phase: Phase,
    last_fire: Option<Duration>,
    last_sample: Option<Duration>,
}

impl Tracker {
    /// Feeds one sample taken at `now`: the value, or `None` when there is none (no sensor, a
    /// failed read). Returns what, if anything, happened.
    ///
    /// * A disabled alert never fires and forgets any stretch above the limit.
    /// * A missing sample, or a gap of more than [`MAX_GAP`] before this one, breaks a stretch
    ///   above the limit; it cannot fire and does not re-arm a fired alert.
    /// * Above means strictly above `limit`; re-arming needs strictly below `limit - margin`.
    /// * Within [`MIN_INTERVAL`] of the last fire it stays quiet, and fires once that has passed
    ///   if the value is still above the limit.
    pub fn step(&mut self, rule: &Rule, now: Duration, value: Option<f32>) -> Option<Event> {
        if !rule.enabled {
            self.phase = Phase::Idle;
            self.last_sample = None;
            return None;
        }
        let Some(v) = value.filter(|v| v.is_finite()) else {
            if matches!(self.phase, Phase::Above { .. }) {
                self.phase = Phase::Idle;
            }
            return None;
        };
        let gap = self.last_sample.map(|t| now.saturating_sub(t));
        self.last_sample = Some(now);
        if gap.is_some_and(|g| g > MAX_GAP) && matches!(self.phase, Phase::Above { .. }) {
            self.phase = Phase::Idle;
        }
        match self.phase {
            Phase::Idle | Phase::Above { .. } => {
                if v <= rule.limit {
                    self.phase = Phase::Idle;
                    return None;
                }
                let since = match self.phase {
                    Phase::Above { since } => since,
                    _ => now,
                };
                self.phase = Phase::Above { since };
                let held = now.saturating_sub(since) >= rule.hold;
                let allowed = self.last_fire.is_none_or(|t| now.saturating_sub(t) >= MIN_INTERVAL);
                if held && allowed {
                    self.phase = Phase::Fired;
                    self.last_fire = Some(now);
                    return Some(Event::Fire);
                }
                None
            }
            Phase::Fired => {
                if v < rule.limit - rule.margin {
                    self.phase = Phase::Idle;
                    return Some(Event::Clear);
                }
                None
            }
        }
    }
}

/// The alert settings as the sampler thread reads them, in °C and whole seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub cpu: Rule,
    pub memory: Rule,
    pub temperature: Rule,
    /// The notification words the temperature in °F.
    pub fahrenheit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self::from_settings(&Alerts::default(), false)
    }
}

impl Config {
    /// Out-of-range values in a hand-edited file are clamped, not rejected.
    pub fn from_settings(a: &Alerts, fahrenheit: bool) -> Self {
        let rule = |enabled: bool, limit: f32, seconds: u32, margin: f32| Rule { enabled, limit, hold: Duration::from_secs(seconds.clamp(1, 3600) as u64), margin };
        Self {
            cpu: rule(a.cpu_enabled, a.cpu_above.clamp(1, 100) as f32, a.cpu_seconds, MARGIN_PCT),
            memory: rule(a.memory_enabled, a.memory_above.clamp(1, 100) as f32, a.memory_seconds, MARGIN_PCT),
            temperature: rule(a.temperature_enabled, a.temperature_above_c.clamp(30.0, 150.0) as f32, a.temperature_seconds, MARGIN_C),
            fahrenheit,
        }
    }

    fn rule(&self, m: Metric) -> &Rule {
        match m {
            Metric::Cpu => &self.cpu,
            Metric::Memory => &self.memory,
            Metric::Temperature => &self.temperature,
        }
    }

    /// Whether any alert is on.
    pub fn any(&self) -> bool {
        Metric::ALL.iter().any(|m| self.rule(*m).enabled)
    }
}

/// A notification to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub metric: Metric,
    pub title: String,
    pub body: String,
}

/// One [`Tracker`] per metric.
#[derive(Default)]
pub struct Monitor {
    trackers: [Tracker; 3],
}

impl Monitor {
    /// Feeds a sample taken at `now` and returns the notifications that became due.
    pub fn observe(&mut self, now: Duration, cfg: &Config, snap: &Snapshot, rows: &[ProcessRow]) -> Vec<Notice> {
        let mut out = Vec::new();
        for m in Metric::ALL {
            let value = match m {
                Metric::Cpu => Some(snap.cpu.total_pct),
                Metric::Memory => Some(snap.memory.used_pct),
                Metric::Temperature => snap.thermal.max_c(),
            };
            let rule = cfg.rule(m);
            if self.trackers[m.index()].step(rule, now, value) == Some(Event::Fire) {
                let hottest = if m == Metric::Temperature { snap.thermal.hottest() } else { None };
                out.push(Notice { metric: m, title: m.title().into(), body: body(m, rule, cfg.fahrenheit, rows, hottest.as_ref()) });
            }
        }
        out
    }
}

/// "Above 90% for 1 min. Busiest: chrome.exe (45%)", "Above 90°C for 30 s. Hottest: GPU 92°C
/// (WDDM)".
fn body(m: Metric, rule: &Rule, fahrenheit: bool, rows: &[ProcessRow], hottest: Option<&Hottest>) -> String {
    let degrees = |c: f32| if fahrenheit { format!("{:.0}\u{b0}F", c * 9.0 / 5.0 + 32.0) } else { format!("{c:.0}\u{b0}C") };
    let limit = match m {
        Metric::Temperature => degrees(rule.limit),
        _ => format!("{:.0}%", rule.limit),
    };
    let mut text = format!("Above {limit} for {}.", duration_text(rule.hold));
    // Which sensor it is: a GPU at its limit is not the processor.
    if let Some(h) = hottest.filter(|_| m == Metric::Temperature) {
        text.push_str(&format!(" Hottest: {} {} ({})", h.what, degrees(h.c), h.source));
    }
    let busiest = match m {
        Metric::Cpu => rows.iter().filter(|r| r.pid != 0).max_by(|a, b| a.cpu_pct.total_cmp(&b.cpu_pct)).map(|r| (r, format!("{:.0}%", r.cpu_pct))),
        Metric::Memory => rows.iter().filter(|r| r.pid != 0).max_by_key(|r| r.private_working_set).map(|r| (r, format_bytes(r.private_working_set as f64))),
        Metric::Temperature => None,
    };
    if let Some((r, what)) = busiest.filter(|(r, _)| !r.name.is_empty()) {
        text.push_str(&format!(" Busiest: {} ({what})", r.name));
    }
    text
}

/// "30 s", "1 min", "90 s".
fn duration_text(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// The first failure of [`send`], kept for the Settings card.
static FIRST_FAILURE: Mutex<Option<String>> = Mutex::new(None);

/// Shows a desktop notification (blocks for a moment; call it off the UI thread). The first
/// failure of the process is kept for [`failure`].
pub fn send(title: &str, body: &str) {
    if let Err(e) = crate::platform::notify(title, body) {
        let mut slot = FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(e);
    }
}

/// Why the first alert notification could not be shown, if it could not.
pub fn failure() -> Option<String> {
    FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The sampler's per-sample hook: evaluates the alerts on the sampler thread, so they work with
/// the window minimized, and sends each notification from a thread of its own.
pub fn hook(cfg: Arc<Mutex<Config>>) -> impl Fn(&Snapshot, &[ProcessRow]) + Send + 'static {
    let start = Instant::now();
    let monitor = Mutex::new(Monitor::default());
    move |snap, rows| {
        let cfg = *cfg.lock().unwrap_or_else(|e| e.into_inner());
        let notices = monitor.lock().unwrap_or_else(|e| e.into_inner()).observe(start.elapsed(), &cfg, snap, rows);
        for n in notices {
            std::thread::spawn(move || send(&n.title, &n.body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{CpuSample, MemorySample, ThermalSample};

    const fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn rule() -> Rule {
        Rule { enabled: true, limit: 90.0, hold: s(60), margin: 5.0 }
    }

    /// Feeds `(second, value)` pairs and returns the events with the second they came at.
    fn run(t: &mut Tracker, r: &Rule, samples: &[(u64, Option<f32>)]) -> Vec<(u64, Event)> {
        samples.iter().filter_map(|(at, v)| t.step(r, s(*at), *v).map(|e| (*at, e))).collect()
    }

    /// A value held above the limit once per second from `from` to `to` inclusive.
    fn hold(v: f32, from: u64, to: u64) -> Vec<(u64, Option<f32>)> {
        (from..=to).map(|t| (t, Some(v))).collect()
    }

    #[test]
    fn fires_after_the_hold_time_not_before() {
        let mut t = Tracker::default();
        let r = rule();
        assert!(run(&mut t, &r, &hold(95.0, 0, 59)).is_empty());
        assert_eq!(t.step(&r, s(60), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn one_long_spike_fires_once() {
        let mut t = Tracker::default();
        let events = run(&mut t, &rule(), &hold(99.0, 0, 3000));
        assert_eq!(events, vec![(60, Event::Fire)]);
    }

    #[test]
    fn exactly_at_the_limit_is_not_above() {
        let mut t = Tracker::default();
        assert!(run(&mut t, &rule(), &hold(90.0, 0, 600)).is_empty());
        // Just above is.
        let mut t = Tracker::default();
        assert_eq!(run(&mut t, &rule(), &hold(90.1, 0, 60)), vec![(60, Event::Fire)]);
    }

    #[test]
    fn a_dip_to_the_limit_restarts_the_timer() {
        let mut t = Tracker::default();
        let mut samples = hold(95.0, 0, 50);
        samples.push((51, Some(90.0)));
        samples.extend(hold(95.0, 52, 111));
        assert!(run(&mut t, &rule(), &samples).is_empty(), "only 59 s above since the dip");
        assert_eq!(t.step(&rule(), s(112), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn zero_hold_fires_on_the_first_sample_above() {
        let mut t = Tracker::default();
        let r = Rule { hold: Duration::ZERO, ..rule() };
        assert_eq!(t.step(&r, s(0), Some(91.0)), Some(Event::Fire));
    }

    #[test]
    fn hysteresis_rearm_needs_to_drop_below_limit_minus_margin() {
        let mut t = Tracker::default();
        let r = Rule { hold: s(2), ..rule() };
        assert_eq!(run(&mut t, &r, &hold(95.0, 0, 2)), vec![(2, Event::Fire)]);
        // Hovering between 85 and 90 (and exactly at 85) does not re-arm.
        assert!(run(&mut t, &r, &[(3, Some(89.0)), (4, Some(85.0)), (5, Some(86.0))]).is_empty());
        // Above again: still silent, even for long (and past the rate limit).
        assert!(run(&mut t, &r, &hold(95.0, 6, 1000)).is_empty());
    }

    #[test]
    fn dropping_below_the_margin_clears_and_rearms() {
        let mut t = Tracker::default();
        let r = Rule { hold: s(2), ..rule() };
        assert_eq!(run(&mut t, &r, &hold(95.0, 0, 2)), vec![(2, Event::Fire)]);
        assert_eq!(t.step(&r, s(3), Some(84.9)), Some(Event::Clear));
        // Armed, but the 10 minute limit holds the second notification back until 602.
        let events = run(&mut t, &r, &hold(95.0, 4, 700));
        assert_eq!(events, vec![(602, Event::Fire)]);
    }

    #[test]
    fn rearmed_after_the_interval_fires_at_the_hold_time() {
        let mut t = Tracker::default();
        let r = Rule { hold: s(2), ..rule() };
        assert_eq!(run(&mut t, &r, &hold(95.0, 0, 2)), vec![(2, Event::Fire)]);
        assert_eq!(t.step(&r, s(10), Some(50.0)), Some(Event::Clear));
        // Back above at 700: the hold time (2 s) applies, not the interval.
        assert_eq!(run(&mut t, &r, &hold(95.0, 700, 705)), vec![(702, Event::Fire)]);
    }

    #[test]
    fn rate_limit_boundary_is_exactly_ten_minutes() {
        let mut t = Tracker::default();
        let r = Rule { hold: Duration::ZERO, ..rule() };
        assert_eq!(t.step(&r, s(100), Some(95.0)), Some(Event::Fire));
        assert_eq!(t.step(&r, s(101), Some(10.0)), Some(Event::Clear));
        assert_eq!(t.step(&r, s(699), Some(95.0)), None, "599 s after the fire");
        assert_eq!(t.step(&r, s(700), Some(95.0)), Some(Event::Fire), "600 s after the fire");
    }

    #[test]
    fn disabled_never_fires() {
        let mut t = Tracker::default();
        let r = Rule { enabled: false, ..rule() };
        assert!(run(&mut t, &r, &hold(100.0, 0, 5000)).is_empty());
    }

    #[test]
    fn disabling_forgets_the_stretch_above_the_limit() {
        let mut t = Tracker::default();
        let on = rule();
        assert!(run(&mut t, &on, &hold(95.0, 0, 50)).is_empty());
        assert_eq!(t.step(&Rule { enabled: false, ..on }, s(51), Some(95.0)), None);
        // Switched on again: counts from here.
        assert_eq!(t.step(&on, s(52), Some(95.0)), None);
        assert!(run(&mut t, &on, &hold(95.0, 53, 111)).is_empty());
        assert_eq!(t.step(&on, s(112), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn switching_off_and_on_does_not_beat_the_rate_limit() {
        let mut t = Tracker::default();
        let on = Rule { hold: s(1), ..rule() };
        assert_eq!(run(&mut t, &on, &hold(95.0, 0, 1)), vec![(1, Event::Fire)]);
        t.step(&Rule { enabled: false, ..on }, s(2), Some(95.0));
        assert!(run(&mut t, &on, &hold(95.0, 3, 100)).is_empty());
    }

    #[test]
    fn missing_samples_never_fire() {
        let mut t = Tracker::default();
        let samples: Vec<_> = (0..=5000).map(|at| (at, None)).collect();
        assert!(run(&mut t, &rule(), &samples).is_empty());
        // NaN and infinity count as missing.
        assert!(run(&mut t, &rule(), &[(6000, Some(f32::NAN)), (6100, Some(f32::INFINITY))]).is_empty());
    }

    #[test]
    fn a_missing_sample_breaks_the_stretch() {
        let mut t = Tracker::default();
        let r = rule();
        assert!(run(&mut t, &r, &hold(95.0, 0, 50)).is_empty());
        assert_eq!(t.step(&r, s(51), None), None);
        assert!(run(&mut t, &r, &hold(95.0, 52, 111)).is_empty());
        assert_eq!(t.step(&r, s(112), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn a_missing_sample_does_not_rearm_or_refire() {
        let mut t = Tracker::default();
        let r = Rule { hold: s(1), ..rule() };
        assert_eq!(run(&mut t, &r, &hold(95.0, 0, 1)), vec![(1, Event::Fire)]);
        assert_eq!(t.step(&r, s(2), None), None);
        assert!(run(&mut t, &r, &hold(95.0, 3, 1500)).is_empty());
    }

    #[test]
    fn a_long_gap_breaks_the_stretch() {
        let mut t = Tracker::default();
        let r = rule();
        assert_eq!(t.step(&r, s(0), Some(95.0)), None);
        // Updates were paused for an hour: the first sample after is not 60 s above.
        assert_eq!(t.step(&r, s(3600), Some(95.0)), None);
        assert!(run(&mut t, &r, &hold(95.0, 3601, 3659)).is_empty());
        assert_eq!(t.step(&r, s(3660), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn a_gap_of_exactly_the_limit_does_not_break_it() {
        let mut t = Tracker::default();
        let r = rule();
        assert_eq!(t.step(&r, s(0), Some(95.0)), None);
        assert_eq!(t.step(&r, s(30), Some(95.0)), None);
        assert_eq!(t.step(&r, s(60), Some(95.0)), Some(Event::Fire));
    }

    /// Hidden in the notification area TMOP samples every [`crate::tray::HIDDEN_INTERVAL`] (5 s).
    /// The hold time is measured between the sample times, so an alert never fires early: it
    /// fires at the first sample at least `hold` after the first one seen above the limit, which
    /// is at most one interval late. The interval is below [`MAX_GAP`], so the slower samples
    /// still make one unbroken stretch, and the alert does fire.
    #[test]
    fn hidden_sampling_fires_on_time_or_one_interval_late_never_early() {
        const { assert!(crate::tray::HIDDEN_INTERVAL.as_secs() <= MAX_GAP.as_secs()) };
        let step = crate::tray::HIDDEN_INTERVAL.as_secs();
        for hold_s in [1, 2, 5, 7, 30, 60, 61, 3600] {
            let r = Rule { hold: s(hold_s), ..rule() };
            let mut t = Tracker::default();
            let samples: Vec<_> = (0..=(3600 + 2 * step) / step).map(|i| (i * step, Some(95.0))).collect();
            let fired = run(&mut t, &r, &samples);
            assert_eq!(fired.len(), 1, "hold {hold_s} s: {fired:?}");
            let at = fired[0].0;
            assert!(at >= hold_s && at < hold_s + step, "hold {hold_s} s fired at {at} s");
        }
    }

    /// Hiding the window in the middle of a stretch above the limit (1 s samples, then 5 s ones)
    /// keeps the time already counted; showing it again does the same the other way.
    #[test]
    fn hiding_mid_stretch_keeps_the_time_counted() {
        let step = crate::tray::HIDDEN_INTERVAL.as_secs();
        let r = rule();
        let mut t = Tracker::default();
        assert!(run(&mut t, &r, &hold(95.0, 0, 40)).is_empty(), "shown: 40 s above");
        // Hidden: 45, 50 and 55 s are not yet 60 s; 60 s is the first hidden sample past the hold.
        let hidden: Vec<_> = (1..=3).map(|i| (40 + i * step, Some(95.0))).collect();
        assert!(run(&mut t, &r, &hidden).is_empty());
        assert_eq!(t.step(&r, s(40 + 4 * step), Some(95.0)), Some(Event::Fire));
        // Hidden first, shown later: the same stretch.
        let mut t = Tracker::default();
        assert!(run(&mut t, &r, &[(0, Some(95.0)), (step, Some(95.0)), (2 * step, Some(95.0))]).is_empty());
        assert!(run(&mut t, &r, &hold(95.0, 11, 59)).is_empty());
        assert_eq!(t.step(&r, s(60), Some(95.0)), Some(Event::Fire));
    }

    #[test]
    fn a_long_gap_keeps_a_fired_alert_fired() {
        let mut t = Tracker::default();
        let r = Rule { hold: Duration::ZERO, ..rule() };
        assert_eq!(t.step(&r, s(0), Some(95.0)), Some(Event::Fire));
        assert_eq!(t.step(&r, s(5000), Some(95.0)), None);
    }

    #[test]
    fn a_changed_limit_applies_to_the_next_sample() {
        let mut t = Tracker::default();
        let r = rule();
        assert!(run(&mut t, &r, &hold(80.0, 0, 100)).is_empty());
        let lower = Rule { limit: 70.0, hold: s(5), ..r };
        assert!(run(&mut t, &lower, &hold(80.0, 101, 105)).is_empty());
        assert_eq!(t.step(&lower, s(106), Some(80.0)), Some(Event::Fire));
    }

    #[test]
    fn temperature_margin_is_three_degrees() {
        let mut t = Tracker::default();
        let r = Rule { enabled: true, limit: 90.0, hold: Duration::ZERO, margin: MARGIN_C };
        assert_eq!(t.step(&r, s(0), Some(91.0)), Some(Event::Fire));
        assert_eq!(t.step(&r, s(1), Some(87.0)), None);
        assert_eq!(t.step(&r, s(2), Some(86.9)), Some(Event::Clear));
    }

    fn snap(cpu: f32, mem: f32, temp: Option<f32>) -> Snapshot {
        Snapshot {
            cpu: CpuSample { total_pct: cpu, ..Default::default() },
            memory: MemorySample { used_pct: mem, ..Default::default() },
            thermal: ThermalSample { zones: temp.map(|t| vec![("TZ".into(), t)]).unwrap_or_default(), ..Default::default() },
            ..Default::default()
        }
    }

    fn row(pid: u32, name: &str, cpu: f32, mem: u64) -> ProcessRow {
        ProcessRow { pid, name: name.into(), cpu_pct: cpu, private_working_set: mem, ..Default::default() }
    }

    fn all_on() -> Config {
        let a = Alerts { cpu_enabled: true, memory_enabled: true, temperature_enabled: true, cpu_seconds: 2, memory_seconds: 2, temperature_seconds: 2, ..Default::default() };
        Config::from_settings(&a, false)
    }

    #[test]
    fn defaults_are_off_with_the_documented_limits() {
        let c = Config::default();
        assert!(!c.any());
        assert_eq!((c.cpu.limit, c.cpu.hold), (90.0, s(60)));
        assert_eq!((c.memory.limit, c.memory.hold), (90.0, s(30)));
        assert_eq!((c.temperature.limit, c.temperature.hold), (90.0, s(30)));
        let mut m = Monitor::default();
        let rows = [row(1, "a", 100.0, 1)];
        for at in 0..500 {
            assert!(m.observe(s(at), &c, &snap(100.0, 100.0, Some(120.0)), &rows).is_empty());
        }
    }

    #[test]
    fn config_clamps_hand_edited_values() {
        let a = Alerts { cpu_above: 0, cpu_seconds: 0, memory_above: 500, memory_seconds: 999_999, temperature_above_c: -5.0, ..Default::default() };
        let c = Config::from_settings(&a, false);
        assert_eq!((c.cpu.limit, c.cpu.hold), (1.0, s(1)));
        assert_eq!((c.memory.limit, c.memory.hold), (100.0, s(3600)));
        assert_eq!(c.temperature.limit, 30.0);
    }

    #[test]
    fn monitor_notifies_cpu_with_the_busiest_process() {
        let mut m = Monitor::default();
        let cfg = all_on();
        let rows = [row(0, "System Idle Process", 900.0, 0), row(4, "chrome.exe", 45.4, 5), row(5, "calc.exe", 3.0, 9)];
        let busy = snap(95.0, 10.0, None);
        assert!(m.observe(s(0), &cfg, &busy, &rows).is_empty());
        assert!(m.observe(s(1), &cfg, &busy, &rows).is_empty());
        let n = m.observe(s(2), &cfg, &busy, &rows);
        assert_eq!(n, vec![Notice { metric: Metric::Cpu, title: "CPU usage high".into(), body: "Above 90% for 2 s. Busiest: chrome.exe (45%)".into() }]);
    }

    #[test]
    fn monitor_notifies_memory_with_the_largest_process() {
        let mut m = Monitor::default();
        let a = Alerts { memory_enabled: true, memory_seconds: 60, ..Default::default() };
        let cfg = Config::from_settings(&a, false);
        let rows = [row(4, "chrome.exe", 45.0, 3 << 30), row(5, "calc.exe", 99.0, 1 << 20)];
        let hot = snap(10.0, 96.0, None);
        assert!(m.observe(s(0), &cfg, &hot, &rows).is_empty());
        assert!(m.observe(s(30), &cfg, &hot, &rows).is_empty());
        let n = m.observe(s(60), &cfg, &hot, &rows);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Memory usage high");
        assert_eq!(n[0].body, "Above 90% for 1 min. Busiest: chrome.exe (3.0 GB)");
    }

    #[test]
    fn temperature_notice_uses_the_users_unit() {
        let rule = Rule { enabled: true, limit: 90.0, hold: s(30), margin: MARGIN_C };
        assert_eq!(body(Metric::Temperature, &rule, false, &[], None), "Above 90\u{b0}C for 30 s.");
        assert_eq!(body(Metric::Temperature, &rule, true, &[], None), "Above 194\u{b0}F for 30 s.");
        let gpu = Hottest { what: "GPU".into(), c: 92.0, source: "WDDM".into() };
        assert_eq!(body(Metric::Temperature, &rule, false, &[], Some(&gpu)), "Above 90\u{b0}C for 30 s. Hottest: GPU 92\u{b0}C (WDDM)");
        assert_eq!(body(Metric::Temperature, &rule, true, &[], Some(&gpu)), "Above 194\u{b0}F for 30 s. Hottest: GPU 198\u{b0}F (WDDM)");
    }

    /// The alert watches the GPU's own temperature but not its hot spot or memory, which run hot
    /// in normal use; the notice names the sensor that is hot.
    #[test]
    fn the_temperature_alert_counts_the_gpu_core_and_names_it() {
        use crate::metrics::{GpuTempKind, GpuThermal};
        let a = Alerts { temperature_enabled: true, temperature_seconds: 1, ..Default::default() };
        let cfg = Config::from_settings(&a, false);
        let g = |kind, c| GpuThermal { label: String::new(), kind, c, source: "HWiNFO" };
        let mut warm = snap(0.0, 0.0, Some(50.0));
        warm.thermal.gpu = vec![g(GpuTempKind::Gpu, 70.0), g(GpuTempKind::MemoryJunction, 98.0), g(GpuTempKind::HotSpot, 95.0)];
        let mut m = Monitor::default();
        for at in 0..10 {
            assert!(m.observe(s(at), &cfg, &warm, &[]).is_empty(), "a healthy card's memory junction");
        }
        let mut hot = warm.clone();
        hot.thermal.gpu[0].c = 93.0;
        assert!(m.observe(s(10), &cfg, &hot, &[]).is_empty());
        let n = m.observe(s(11), &cfg, &hot, &[]);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].body, "Above 90\u{b0}C for 1 s. Hottest: GPU 93\u{b0}C (HWiNFO)");
    }

    #[test]
    fn missing_temperature_never_fires_through_the_monitor() {
        let mut m = Monitor::default();
        let a = Alerts { temperature_enabled: true, temperature_seconds: 1, ..Default::default() };
        let cfg = Config::from_settings(&a, false);
        for at in 0..100 {
            assert!(m.observe(s(at), &cfg, &snap(0.0, 0.0, None), &[]).is_empty());
        }
        // Once a sensor appears it works.
        assert!(m.observe(s(100), &cfg, &snap(0.0, 0.0, Some(95.0)), &[]).is_empty());
        assert_eq!(m.observe(s(101), &cfg, &snap(0.0, 0.0, Some(95.0)), &[]).len(), 1);
    }

    #[test]
    fn alerts_are_independent() {
        let mut m = Monitor::default();
        let cfg = all_on();
        let hot_cpu = snap(95.0, 10.0, Some(50.0));
        m.observe(s(0), &cfg, &hot_cpu, &[]);
        m.observe(s(1), &cfg, &hot_cpu, &[]);
        let n = m.observe(s(2), &cfg, &hot_cpu, &[]);
        assert_eq!(n.iter().map(|n| n.metric).collect::<Vec<_>>(), vec![Metric::Cpu]);
        // Memory now, CPU stays quiet.
        let hot_mem = snap(95.0, 95.0, Some(50.0));
        m.observe(s(3), &cfg, &hot_mem, &[]);
        let n = m.observe(s(5), &cfg, &hot_mem, &[]);
        assert_eq!(n.iter().map(|n| n.metric).collect::<Vec<_>>(), vec![Metric::Memory]);
    }

    #[test]
    fn duration_wording() {
        assert_eq!(duration_text(s(30)), "30 s");
        assert_eq!(duration_text(s(60)), "1 min");
        assert_eq!(duration_text(s(90)), "90 s");
        assert_eq!(duration_text(s(120)), "2 min");
    }
}
