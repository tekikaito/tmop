//! Pure helpers behind the Linux (and stub) backends: XDG base directories resolved from an
//! environment lookup, `file://` URIs and the FileManager1 call. No I/O, so the tests run on
//! every host.

use std::ffi::OsString;
use std::path::PathBuf;

/// TMOP's folder name under the XDG base directories (lower case, like the app id).
pub const DIR: &str = "tmop";
/// Skins folder under [`data_dir`] and under each `$XDG_DATA_DIRS` entry's [`DIR`].
pub const SKINS: &str = "skins";
/// `$XDG_DATA_DIRS` when unset or empty.
const DEFAULT_DATA_DIRS: &str = "/usr/local/share/:/usr/share/";

/// An environment lookup (`std::env::var_os` or a test map).
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// `var` when it holds an absolute path; the spec says relative values are invalid and ignored.
fn absolute(env: Env, var: &str) -> Option<PathBuf> {
    // `has_root`, not `is_absolute`: the same Unix paths then also count on a Windows test host.
    env(var).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.has_root())
}

/// `$var`, else `$HOME/<fallback>`; `None` with neither.
fn base(env: Env, var: &str, fallback: &str) -> Option<PathBuf> {
    absolute(env, var).or_else(|| absolute(env, "HOME").map(|h| h.join(fallback)))
}

/// `$XDG_DATA_HOME/tmop`, else `~/.local/share/tmop`.
pub fn data_dir(env: Env) -> Option<PathBuf> {
    base(env, "XDG_DATA_HOME", ".local/share").map(|d| d.join(DIR))
}

/// `$XDG_CONFIG_HOME/tmop`, else `~/.config/tmop`.
pub fn config_dir(env: Env) -> Option<PathBuf> {
    base(env, "XDG_CONFIG_HOME", ".config").map(|d| d.join(DIR))
}

/// The user's skins (`<data>/skins`), then `<dir>/tmop/skins` for every `$XDG_DATA_DIRS` entry
/// (default `/usr/local/share:/usr/share`), highest priority first and without duplicates.
pub fn skin_dirs(env: Env) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = data_dir(env).map(|d| d.join(SKINS)).into_iter().collect();
    let system = env("XDG_DATA_DIRS").filter(|v| !v.is_empty()).map(|v| v.to_string_lossy().into_owned()).unwrap_or_else(|| DEFAULT_DATA_DIRS.into());
    for entry in system.split(':').map(PathBuf::from).filter(|p| p.has_root()) {
        let d = entry.join(DIR).join(SKINS);
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs
}

/// True when winit will pick Wayland: `WAYLAND_DISPLAY` or `WAYLAND_SOCKET` is set.
pub fn wayland_session(env: Env) -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|k| env(k).is_some_and(|v| !v.is_empty()))
}

/// `file://` URI of an absolute path given as raw bytes; everything but unreserved characters
/// and `/` is percent-encoded, so the result is also safe inside a quoted GVariant string.
pub fn file_uri(path: &[u8]) -> String {
    let mut uri = String::from("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// `gdbus` arguments that ask the desktop's file manager to show (select) `uri`.
pub fn show_items_args(uri: &str) -> Vec<String> {
    [
        "call",
        "--session",
        "--timeout",
        "5",
        "--dest",
        "org.freedesktop.FileManager1",
        "--object-path",
        "/org/freedesktop/FileManager1",
        "--method",
        "org.freedesktop.FileManager1.ShowItems",
    ]
    .into_iter()
    .map(String::from)
    .chain([format!("['{uri}']"), "''".into()])
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn with(vars: &[(&str, &str)], f: impl FnOnce(Env)) {
        let map: HashMap<String, OsString> = vars.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        f(&move |k| map.get(k).cloned())
    }

    #[test]
    fn home_defaults() {
        with(&[("HOME", "/home/alex")], |env| {
            assert_eq!(data_dir(env), Some(Path::new("/home/alex").join(".local/share").join("tmop")));
            assert_eq!(config_dir(env), Some(Path::new("/home/alex").join(".config").join("tmop")));
        });
    }

    #[test]
    fn xdg_overrides_win_over_home() {
        with(&[("HOME", "/home/alex"), ("XDG_DATA_HOME", "/data"), ("XDG_CONFIG_HOME", "/cfg")], |env| {
            assert_eq!(data_dir(env), Some(Path::new("/data").join("tmop")));
            assert_eq!(config_dir(env), Some(Path::new("/cfg").join("tmop")));
        });
    }

    #[test]
    fn relative_or_empty_overrides_are_ignored() {
        with(&[("HOME", "/home/alex"), ("XDG_DATA_HOME", "rel/data"), ("XDG_CONFIG_HOME", "")], |env| {
            assert_eq!(data_dir(env), Some(Path::new("/home/alex").join(".local/share").join("tmop")));
            assert_eq!(config_dir(env), Some(Path::new("/home/alex").join(".config").join("tmop")));
        });
    }

    #[test]
    fn nothing_without_home() {
        with(&[], |env| {
            assert_eq!((data_dir(env), config_dir(env)), (None, None));
        });
        with(&[("HOME", "relative")], |env| assert_eq!(data_dir(env), None));
        with(&[("XDG_CONFIG_HOME", "/cfg")], |env| assert_eq!(config_dir(env), Some(Path::new("/cfg").join("tmop"))));
    }

    #[test]
    fn skin_dirs_user_first_then_system() {
        with(&[("HOME", "/home/alex")], |env| {
            assert_eq!(
                skin_dirs(env),
                [Path::new("/home/alex").join(".local/share").join("tmop").join("skins"), Path::new("/usr/local/share/").join("tmop").join("skins"), Path::new("/usr/share/").join("tmop").join("skins")]
            );
        });
        with(&[("XDG_DATA_HOME", "/d"), ("XDG_DATA_DIRS", "/opt/share:rel::/d")], |env| {
            // The relative and empty entries are skipped; `/d` is the user dir already.
            assert_eq!(skin_dirs(env), [Path::new("/d").join("tmop").join("skins"), Path::new("/opt/share").join("tmop").join("skins")]);
        });
    }

    #[test]
    fn wayland_detection() {
        with(&[("WAYLAND_DISPLAY", "wayland-0")], |env| assert!(wayland_session(env)));
        with(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0")], |env| assert!(!wayland_session(env)));
    }

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(file_uri(b"/home/alex/a b/x'y.txt"), "file:///home/alex/a%20b/x%27y.txt");
        assert_eq!(file_uri("/tmp/ä,\\".as_bytes()), "file:///tmp/%C3%A4%2C%5C");
    }

    #[test]
    fn show_items_call() {
        let args = show_items_args("file:///tmp/x");
        assert_eq!(args[0], "call");
        assert_eq!(&args[args.len() - 2..], ["['file:///tmp/x']", "''"]);
    }
}
