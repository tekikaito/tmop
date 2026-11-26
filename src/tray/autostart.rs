//! "Start with Windows": a per-user Run entry (`HKCU\...\CurrentVersion\Run`, value `TMOP`)
//! that starts TMOP hidden in the notification area.
//!
//! * The registry value is the only record of the setting, as with the Task Manager
//!   replacement: what Settings shows is read back from it.
//! * A portable TMOP.exe that was moved leaves a stale entry behind; [`classify`] tells it apart
//!   from this copy's entry.
//! * Explorer's `StartupApproved\Run` can switch the entry off (Task Manager's Startup apps,
//!   Settings → Apps → Startup) while it stays in place; [`State::On`] carries that.
//! * The registry is reached through [`RunStore`], so tests never write the real Run key.

use crate::taskmgr::{debugger_path, is_this_exe, plain};
use std::path::{Path, PathBuf};

/// The Run key under HKCU.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Explorer's on/off switches for the Run entries under HKCU.
pub const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// The value name of TMOP's entry.
pub const VALUE: &str = "TMOP";

/// Where the Run entry is kept.
pub trait RunStore {
    /// The entry's command line, if there is one.
    fn read(&self) -> Option<String>;
    fn write(&mut self, command: &str) -> Result<(), String>;
    /// Removes the entry; no entry is not an error.
    fn remove(&mut self) -> Result<(), String>;
    /// False when Explorer's Startup apps switch has the entry turned off.
    fn approved(&self) -> bool;
}

/// What the Run entry says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// No entry.
    Off,
    /// This copy starts with Windows. `approved`: false when the entry is switched off in
    /// Startup apps, so Windows skips it.
    On { approved: bool },
    /// The entry starts another TMOP.exe (a copy that was moved or deleted, or a second one).
    Stale { path: PathBuf, exists: bool },
}

impl State {
    /// Whether there is an entry at all (the switch shows on).
    pub fn has_entry(&self) -> bool {
        !matches!(self, State::Off)
    }
}

/// The command line for `exe`: quoted path, then [`super::FLAG`].
pub fn command(exe: &Path) -> String {
    format!("\"{}\" {}", plain(exe), super::FLAG)
}

/// Classifies the entry's command line against the running exe. `resolve` gives a path's full
/// long form when it exists (see [`crate::taskmgr::classify`]).
pub fn classify(value: Option<&str>, approved: bool, exe: &Path, resolve: impl Fn(&Path) -> Option<PathBuf>) -> State {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return State::Off;
    };
    let path = debugger_path(value);
    let ours = is_this_exe(&path, exe, &resolve);
    let flagged = value.split_whitespace().any(|a| a == super::FLAG || a == super::FLAG_ALIAS);
    if ours && flagged {
        State::On { approved }
    } else {
        let exists = resolve(&path).is_some();
        State::Stale { path, exists }
    }
}

/// The state `store` is in now.
pub fn state(store: &dyn RunStore, exe: &Path, resolve: impl Fn(&Path) -> Option<PathBuf>) -> State {
    classify(store.read().as_deref(), store.approved(), exe, resolve)
}

/// Turns the entry on (for `exe`) or off, then reads it back: some security software lets the
/// write succeed and undoes it, or refuses it. Returns the state read back.
pub fn set(store: &mut dyn RunStore, on: bool, exe: &Path, resolve: impl Fn(&Path) -> Option<PathBuf>) -> Result<State, String> {
    if on {
        store.write(&command(exe))?;
    } else {
        store.remove()?;
    }
    let after = state(store, exe, resolve);
    let ok = if on { matches!(after, State::On { .. }) } else { after == State::Off };
    if ok {
        Ok(after)
    } else if on {
        Err("Windows or your security software did not keep the start-up entry.".into())
    } else {
        Err("The start-up entry could not be removed.".into())
    }
}

/// The text Settings shows under the switch, if any.
pub fn describe(state: &State) -> Option<String> {
    match state {
        State::Off | State::On { approved: true } => None,
        State::On { approved: false } => Some("The entry is switched off in Startup apps (Windows Settings \u{2192} Apps \u{2192} Startup), so Windows does not start TMOP.".into()),
        State::Stale { path, exists: true } => Some(format!("Windows starts another copy of TMOP: {}", path.display())),
        State::Stale { path, exists: false } => Some(format!("Windows is set to start {}, which no longer exists. Was TMOP moved?", path.display())),
    }
}

#[cfg(windows)]
pub use win::Registry;

#[cfg(windows)]
mod win {
    use super::{RunStore, APPROVED_KEY, RUN_KEY, VALUE};
    use crate::pro::winutil::{wide, Key, Value, View};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ};

    /// The real Run key of the signed-in user.
    pub struct Registry;

    fn os_error(code: u32) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    impl RunStore for Registry {
        fn read(&self) -> Option<String> {
            Key::open(HKEY_CURRENT_USER, RUN_KEY, View::Default)?.string(VALUE)
        }

        fn write(&mut self, command: &str) -> Result<(), String> {
            let path = wide(RUN_KEY);
            let name = wide(VALUE);
            let mut h = HKEY::default();
            // SAFETY: valid NUL-terminated path; `h` receives the handle and is closed below.
            let st = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut h, None) };
            if st != ERROR_SUCCESS {
                return Err(os_error(st.0));
            }
            let data: Vec<u8> = wide(command).iter().flat_map(|c| c.to_le_bytes()).collect();
            // SAFETY: `h` is open; `data` is a NUL-terminated UTF-16 string.
            let st = unsafe { RegSetValueExW(h, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)) };
            // SAFETY: opened above.
            unsafe {
                let _ = RegCloseKey(h);
            }
            if st == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(os_error(st.0))
            }
        }

        fn remove(&mut self) -> Result<(), String> {
            let path = wide(RUN_KEY);
            let name = wide(VALUE);
            let mut h = HKEY::default();
            // SAFETY: as in `write`.
            let st = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, KEY_SET_VALUE, &mut h) };
            if st == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            if st != ERROR_SUCCESS {
                return Err(os_error(st.0));
            }
            // SAFETY: `h` is open.
            let st = unsafe { RegDeleteValueW(h, PCWSTR(name.as_ptr())) };
            // SAFETY: opened above.
            unsafe {
                let _ = RegCloseKey(h);
            }
            if st == ERROR_SUCCESS || st == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(os_error(st.0))
            }
        }

        fn approved(&self) -> bool {
            match Key::open(HKEY_CURRENT_USER, APPROVED_KEY, View::Default).and_then(|k| k.value(VALUE)) {
                Some(Value::Bytes(b)) => crate::pro::inventory::approved_enabled(&b),
                _ => true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taskmgr::same_path;

    /// A Run key in memory.
    #[derive(Default)]
    struct Fake {
        value: Option<String>,
        approved: bool,
        /// Security software that lets the write succeed and keeps nothing.
        drops_writes: bool,
        refuses: bool,
    }

    impl RunStore for Fake {
        fn read(&self) -> Option<String> {
            self.value.clone()
        }
        fn write(&mut self, command: &str) -> Result<(), String> {
            if self.refuses {
                return Err("Access is denied.".into());
            }
            if !self.drops_writes {
                self.value = Some(command.into());
            }
            Ok(())
        }
        fn remove(&mut self) -> Result<(), String> {
            self.value = None;
            Ok(())
        }
        fn approved(&self) -> bool {
            self.approved
        }
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\TMOP\TMOP.exe")
    }

    fn exists(p: &Path) -> Option<PathBuf> {
        same_path(p, &exe()).then(exe)
    }

    #[test]
    fn command_quotes_the_path_and_asks_for_the_tray() {
        assert_eq!(command(&exe()), r#""C:\Program Files\TMOP\TMOP.exe" --tray"#);
        assert_eq!(command(Path::new(r"\\?\D:\Tools\TMOP.exe")), r#""D:\Tools\TMOP.exe" --tray"#);
    }

    #[test]
    fn classifies_entries() {
        let approved = true;
        assert_eq!(classify(None, approved, &exe(), exists), State::Off);
        assert_eq!(classify(Some("  "), approved, &exe(), exists), State::Off);
        assert_eq!(classify(Some(&command(&exe())), approved, &exe(), exists), State::On { approved: true });
        assert_eq!(classify(Some(&command(&exe())), false, &exe(), exists), State::On { approved: false });
        // Case and slashes don't matter; --hidden is the same flag.
        assert_eq!(classify(Some(r#""c:/program files/tmop/tmop.exe" --hidden"#), approved, &exe(), exists), State::On { approved: true });
        // The exe was moved: the old path is gone.
        let old = r#""D:\Downloads\TMOP.exe" --tray"#;
        assert_eq!(classify(Some(old), approved, &exe(), exists), State::Stale { path: PathBuf::from(r"D:\Downloads\TMOP.exe"), exists: false });
        // Another copy that still exists.
        let other = |p: &Path| Some(p.to_path_buf());
        assert_eq!(classify(Some(old), approved, &exe(), other), State::Stale { path: PathBuf::from(r"D:\Downloads\TMOP.exe"), exists: true });
        // This exe without the flag would open a window at sign-in: not what TMOP writes.
        assert!(matches!(classify(Some(r#""C:\Program Files\TMOP\TMOP.exe""#), approved, &exe(), exists), State::Stale { exists: true, .. }));
        assert!(!State::Off.has_entry() && State::On { approved: false }.has_entry());
    }

    /// A Scoop install registers `...\current\TMOP.exe`; an entry with the version folder that
    /// `current` points at is this copy too, and so is the other way round.
    #[cfg(windows)]
    #[test]
    fn scoop_spellings_are_the_same_copy() {
        let current = PathBuf::from(r"C:\Users\me\scoop\apps\tmop\current\TMOP.exe");
        let version = PathBuf::from(r"C:\Users\me\scoop\apps\tmop\0.11.2\TMOP.exe");
        let junction = |p: &Path| {
            let s = plain(p).to_lowercase();
            Some(PathBuf::from(s.replace(r"\current\", r"\0.11.2\")))
        };
        assert_eq!(classify(Some(&command(&current)), true, &current, junction), State::On { approved: true });
        assert_eq!(classify(Some(&command(&version)), true, &current, junction), State::On { approved: true });
        assert_eq!(classify(Some(&command(&current)), true, &version, junction), State::On { approved: true });
        // An older version folder is another copy: after `scoop update` it is the old TMOP.
        let old = r#""C:\Users\me\scoop\apps\tmop\0.11.1\TMOP.exe" --tray"#;
        assert!(matches!(classify(Some(old), true, &current, junction), State::Stale { exists: true, .. }));
    }

    #[test]
    fn set_reads_the_entry_back() {
        let mut store = Fake { approved: true, ..Default::default() };
        assert_eq!(set(&mut store, true, &exe(), exists), Ok(State::On { approved: true }));
        assert_eq!(store.value.as_deref(), Some(command(&exe()).as_str()));
        assert_eq!(set(&mut store, false, &exe(), exists), Ok(State::Off));
        assert_eq!(store.value, None);
        // A stale entry is replaced by this copy's.
        store.value = Some(r#""D:\Old\TMOP.exe" --tray"#.into());
        assert_eq!(set(&mut store, true, &exe(), exists), Ok(State::On { approved: true }));
    }

    #[test]
    fn a_dropped_or_refused_write_is_reported() {
        let mut store = Fake { approved: true, drops_writes: true, ..Default::default() };
        assert!(set(&mut store, true, &exe(), exists).unwrap_err().contains("did not keep"));
        let mut store = Fake { approved: true, refuses: true, ..Default::default() };
        assert_eq!(set(&mut store, true, &exe(), exists), Err("Access is denied.".into()));
    }

    #[test]
    fn describes_what_needs_saying() {
        assert_eq!(describe(&State::Off), None);
        assert_eq!(describe(&State::On { approved: true }), None);
        assert!(describe(&State::On { approved: false }).unwrap().contains("Startup apps"));
        let gone = State::Stale { path: PathBuf::from(r"D:\Old\TMOP.exe"), exists: false };
        assert!(describe(&gone).unwrap().contains(r"D:\Old\TMOP.exe, which no longer exists"));
    }
}
