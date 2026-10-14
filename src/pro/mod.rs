//! Back ends for the advanced pages and the inventory pages.
//!
//! TMOP has no licensing: everything here is implemented from scratch on public
//! Windows APIs and is always available. Each module is independent and runs its
//! expensive work on a background thread (see [`Loader`]).
//!
//! * [`bench`]        CPU / memory / disk benchmarks (bounded, cancelable; temp file only)
//! * [`recorder`]     flight recorder ring buffer and CSV recording
//! * [`power`]        power scheme, processor policy, battery
//! * [`connections`]  TCP/UDP endpoints with owning process
//! * [`apps`]         installed applications (Uninstall registry keys)
//! * [`drivers`]      kernel driver services and loaded kernel modules
//! * [`diskspace`]    volume list and a cancelable folder-size scanner
//! * [`inventory`]    system info, services, startup apps

pub mod apps;
pub mod bench;
pub mod connections;
pub mod diskspace;
pub mod drivers;
pub mod icons;
pub mod inventory;
pub mod power;
pub mod recorder;
#[cfg(windows)]
pub(crate) mod winutil;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Result of a background load.
pub type LoadResult<T> = Result<T, String>;

/// Runs a closure on a worker thread and hands the result to the UI when it is ready.
pub struct Loader<T> {
    slot: Arc<Mutex<Option<LoadResult<T>>>>,
    value: Option<LoadResult<T>>,
    loading: bool,
    loaded_at: Option<Instant>,
}

impl<T> Default for Loader<T> {
    fn default() -> Self {
        Self { slot: Arc::new(Mutex::new(None)), value: None, loading: false, loaded_at: None }
    }
}

impl<T: Send + 'static> Loader<T> {
    /// Starts a load unless one is already running.
    pub fn start(&mut self, f: impl FnOnce() -> LoadResult<T> + Send + 'static) {
        if self.loading {
            return;
        }
        self.loading = true;
        let slot = self.slot.clone();
        let spawned = std::thread::Builder::new().name("tmop-loader".into()).spawn(move || {
            let r = f();
            if let Ok(mut g) = slot.lock() {
                *g = Some(r);
            }
        });
        if let Err(e) = spawned {
            self.loading = false;
            self.value = Some(Err(format!("could not start worker: {e}")));
        }
    }

    /// Moves a finished result into place. Returns true when a new result arrived.
    pub fn poll(&mut self) -> bool {
        if !self.loading {
            return false;
        }
        let done = self.slot.lock().ok().and_then(|mut g| g.take());
        match done {
            Some(r) => {
                self.value = Some(r);
                self.loading = false;
                self.loaded_at = Some(Instant::now());
                true
            }
            None => false,
        }
    }

    pub fn get(&self) -> Option<&LoadResult<T>> {
        self.value.as_ref()
    }

    pub fn ok(&self) -> Option<&T> {
        self.value.as_ref().and_then(|r| r.as_ref().ok())
    }

    pub fn error(&self) -> Option<&str> {
        self.value.as_ref().and_then(|r| r.as_ref().err()).map(|s| s.as_str())
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Time since the last completed load.
    pub fn age(&self) -> Option<Duration> {
        self.loaded_at.map(|t| t.elapsed())
    }

    /// True when nothing was loaded yet or the data is older than `max_age`.
    pub fn stale(&self, max_age: Duration) -> bool {
        !self.loading && self.age().map(|a| a > max_age).unwrap_or(true)
    }
}

/// `%LOCALAPPDATA%\TMOP`
pub fn data_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join(crate::APP_DIR)
}

/// Local wall-clock time as (year, month, day, hour, minute, second).
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    #[cfg(windows)]
    {
        // SAFETY: GetLocalTime only writes the returned struct.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
    }
    #[cfg(not(windows))]
    {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        (1970, 1, 1, ((s / 3600) % 24) as u16, ((s / 60) % 60) as u16, (s % 60) as u16)
    }
}

/// `YYYYMMDD-HHMMSS` in local time, for file names.
pub fn file_stamp() -> String {
    let (y, mo, d, h, mi, s) = local_time();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// `YYYY-MM-DD HH:MM:SS` in local time.
pub fn display_stamp() -> String {
    let (y, mo, d, h, mi, s) = local_time();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Expands `%VAR%` references using the process environment; unknown names are kept.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion() {
        std::env::set_var("TMOP_TEST_VAR", "x");
        assert_eq!(expand_env("a%TMOP_TEST_VAR%b"), "axb");
        assert_eq!(expand_env("100%"), "100%");
        assert_eq!(expand_env("%NOPE_NOT_SET_42%"), "%NOPE_NOT_SET_42%");
    }

    #[test]
    fn loader_delivers_result() {
        let mut l: Loader<u32> = Loader::default();
        assert!(l.stale(Duration::from_secs(1)));
        l.start(|| Ok(42));
        let t = Instant::now();
        while !l.poll() {
            assert!(t.elapsed() < Duration::from_secs(5), "loader never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(l.ok(), Some(&42));
        assert!(!l.is_loading());
        assert!(!l.stale(Duration::from_secs(60)));
    }

    #[test]
    fn stamps_have_expected_shape() {
        assert_eq!(file_stamp().len(), 15);
        assert_eq!(display_stamp().len(), 19);
    }
}
