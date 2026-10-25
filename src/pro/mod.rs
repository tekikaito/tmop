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
//! * [`apps`]         installed applications (Uninstall keys, package databases, .app bundles)
//! * [`drivers`]      drivers and services (SCM; systemd and kernel modules; launchd and kexts)
//! * [`plist`]        property-list reader (XML and binary) for the macOS backends
//! * [`cleanup`]      Disk Space cleanup categories and their measuring
//! * [`volcache`]     Windows Disk Cleanup handlers (helper process, `--volume-caches=`)
//! * [`mft`]          NTFS Master File Table reader (fast scan method when elevated)
//! * [`walker`]       parallel directory walker (scan method otherwise)
//! * [`mounts`]       mounted file systems off Windows (mountinfo, getfsstat)
//! * [`diskdelete`]    delete safety classifier and the delete worker (recycle bin or permanent)
//! * [`inuse`]        which processes hold a file open (Restart Manager, /proc, libproc)
//! * [`inventory`]    system info, services, startup apps
//! * [`sysfmt`]       pure parsers for os-release, `.desktop` files and XML property lists

pub mod apps;
pub mod bench;
pub mod cleanup;
pub mod connections;
pub mod diskdelete;
pub mod diskspace;
pub mod drivers;
pub mod icons;
pub mod inuse;
pub mod inventory;
pub mod mft;
pub mod mounts;
pub mod plist;
pub mod power;
pub mod recorder;
pub mod sysfmt;
pub mod trash;
#[cfg(windows)]
pub(crate) mod restartmgr;
pub mod volcache;
pub mod walker;
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

    pub fn ok_mut(&mut self) -> Option<&mut T> {
        self.value.as_mut().and_then(|r| r.as_mut().ok())
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

/// `YYYYMMDD-HHMMSS` in local time, for file names.
pub fn file_stamp() -> String {
    let (y, mo, d, h, mi, s) = crate::platform::local_time();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// `YYYY-MM-DD HH:MM:SS` in local time.
pub fn display_stamp() -> String {
    let (y, mo, d, h, mi, s) = crate::platform::local_time();
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
