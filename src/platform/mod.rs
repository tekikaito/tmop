//! The operating-system seam: folders, local time, host name, file-manager hand-off, console
//! hiding, elevation, subprocess capture and the words the UI uses for OS concepts.
//!
//! One backend per target, chosen at compile time and re-exported here with identical free
//! functions: `windows.rs`, `linux.rs`, `macos.rs`, and `stub.rs` for anything else. Pure helpers
//! (XDG resolution, file URIs) live in [`xdg`] and are tested on every host.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | [`data_dir`] | `%LOCALAPPDATA%\TMOP` | `$XDG_DATA_HOME/tmop` (`~/.local/share/tmop`) | `~/Library/Application Support/TMOP` |
//! | [`config_dir`] | `%LOCALAPPDATA%\TMOP` | `$XDG_CONFIG_HOME/tmop` (`~/.config/tmop`) | `~/Library/Application Support/TMOP` |
//! | [`skin_dirs`] | `TMOP\Skins`, `TMOG\Skins`, `Program Files\Task Manager TMOG\Skins` | `<data>/skins`, `$XDG_DATA_DIRS/tmop/skins` | `<data>/Skins` |
//! | [`reveal`] | `explorer /select,` | FileManager1 `ShowItems` over D-Bus, else `xdg-open` of the parent | `open -R` |
//! | [`is_elevated`] | administrator token | `geteuid() == 0` | `geteuid() == 0` |
//! | [`notify`] | WinRT toast | `notify-send`, else `gdbus` | `osascript` |

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub mod caps;
pub mod desktop;
pub mod xdg;

pub use caps::Caps;

#[cfg(unix)]
mod unix;

#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), path = "stub.rs")]
mod imp;

pub use imp::*;

/// The operating system this build targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Linux,
    MacOs,
    Other,
}

/// This build's [`Os`].
pub const OS: Os = if cfg!(windows) {
    Os::Windows
} else if cfg!(target_os = "linux") {
    Os::Linux
} else if cfg!(target_os = "macos") {
    Os::MacOs
} else {
    Os::Other
};

/// "Windows", "Linux", "macOS" (else the target's name).
pub const fn os_name() -> &'static str {
    match OS {
        Os::Windows => "Windows",
        Os::Linux => "Linux",
        Os::MacOs => "macOS",
        Os::Other => std::env::consts::OS,
    }
}

/// The file manager, as in "Open in Explorer".
pub const fn file_manager() -> &'static str {
    match OS {
        Os::Windows => "Explorer",
        Os::MacOs => "Finder",
        _ => "file manager",
    }
}

/// Where deleted files go: "Recycle Bin" or "Trash".
pub const fn trash_word() -> &'static str {
    match OS {
        Os::Windows => "Recycle Bin",
        _ => "Trash",
    }
}

/// Disk-backed virtual memory: "Page file" or "Swap".
pub const fn swap_word() -> &'static str {
    match OS {
        Os::Windows => "Page file",
        _ => "Swap",
    }
}

/// Who may do everything: "administrator" or "root".
pub const fn elevation_word() -> &'static str {
    match OS {
        Os::Windows => "administrator",
        _ => "root",
    }
}

/// The OS as the subject of a sentence, as in "{} moves data to disk": "Windows" or "the system".
pub const fn os_subject() -> &'static str {
    match OS {
        Os::Windows => "Windows",
        _ => "the system",
    }
}

/// What the OS does when memory runs out: "paging" or "swapping".
pub const fn paging_word() -> &'static str {
    match OS {
        Os::Windows => "paging",
        _ => "swapping",
    }
}

/// True when the session is a Wayland one (winit then picks its Wayland backend): no window
/// levels, no taskbar hiding and possibly no server-side decorations.
pub fn is_wayland() -> bool {
    OS == Os::Linux && xdg::wayland_session(&|k| std::env::var_os(k))
}

/// How long [`run_capture`] still waits for the output pipes after the child exits. A reader
/// still blocked then (a grandchild holds the pipe open) is abandoned with what it has not read.
const READER_GRACE: Duration = Duration::from_secs(2);

/// Runs `cmd args` with `LC_ALL=C`, no stdin and no console window and returns its stdout.
/// Fails with a short text when it cannot start, exits non-zero (the first stderr line) or
/// runs longer than `timeout` (it is killed). Blocks; call it off the UI thread.
pub fn run_capture(cmd: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let (status, stdout, stderr) = run_output(cmd, args, timeout)?;
    if status.success() {
        return Ok(stdout);
    }
    let why = stderr.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| format!(": {l}")).unwrap_or_default();
    Err(match status.code() {
        Some(code) => format!("{cmd} failed with exit code {code}{why}"),
        None => format!("{cmd} was terminated{why}"),
    })
}

/// [`run_capture`] for tools whose exit code is not a verdict (`lsof` exits 1 when it could not
/// see some files): (exit code, stdout, stderr) of a run that finished, else why it did not.
pub fn run_capture_any(cmd: &str, args: &[&str], timeout: Duration) -> Result<(Option<i32>, String, String), String> {
    run_output(cmd, args, timeout).map(|(status, out, err)| (status.code(), out, err))
}

fn run_output(cmd: &str, args: &[&str], timeout: Duration) -> Result<(std::process::ExitStatus, String, String), String> {
    let mut command = Command::new(cmd);
    command.args(args).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut command);
    let mut child = command.spawn().map_err(|e| format!("could not start {cmd}: {e}"))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
        }
        rx
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{cmd} timed out after {:.1} s", timeout.as_secs_f32()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for {cmd} failed: {e}"));
            }
        }
    };
    let grace = Instant::now() + READER_GRACE;
    let collect = |rx: mpsc::Receiver<Vec<u8>>| String::from_utf8_lossy(&rx.recv_timeout(grace.saturating_duration_since(Instant::now())).unwrap_or_default()).into_owned();
    Ok((status, collect(out), collect(err)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_target() {
        if cfg!(windows) {
            assert_eq!((os_name(), file_manager(), trash_word(), swap_word(), elevation_word()), ("Windows", "Explorer", "Recycle Bin", "Page file", "administrator"));
        } else {
            assert_eq!((trash_word(), swap_word(), elevation_word()), ("Trash", "Swap", "root"));
        }
        assert!(!os_name().is_empty());
    }

    #[test]
    fn dirs_are_absolute_and_named_for_tmop() {
        for dir in [data_dir(), config_dir()] {
            assert!(dir.is_absolute(), "{}", dir.display());
            assert!(dir.file_name().unwrap().eq_ignore_ascii_case(crate::APP_DIR), "{}", dir.display());
        }
        assert!(!skin_dirs().is_empty());
    }

    #[test]
    fn local_time_is_plausible() {
        let (y, mo, d, h, mi, s) = local_time();
        assert!(y >= 2024 && (1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60 && s < 61, "{y}-{mo}-{d} {h}:{mi}:{s}");
    }

    #[test]
    fn hostname_is_known() {
        assert!(hostname().is_some_and(|h| !h.trim().is_empty()));
    }

    #[cfg(windows)]
    fn shell(script: &str) -> (&'static str, Vec<String>) {
        ("cmd", vec!["/C".into(), script.into()])
    }

    #[cfg(not(windows))]
    fn shell(script: &str) -> (&'static str, Vec<String>) {
        ("sh", vec!["-c".into(), script.into()])
    }

    fn run(script: &str, timeout: Duration) -> Result<String, String> {
        let (cmd, args) = shell(script);
        run_capture(cmd, &args.iter().map(String::as_str).collect::<Vec<_>>(), timeout)
    }

    #[test]
    fn run_capture_returns_stdout_with_c_locale() {
        let script = if cfg!(windows) { "echo %LC_ALL%" } else { "echo $LC_ALL" };
        assert_eq!(run(script, Duration::from_secs(20)).unwrap().trim(), "C");
    }

    #[test]
    fn run_capture_reports_failures() {
        let err = run("echo nope 1>&2 && exit 3", Duration::from_secs(20)).unwrap_err();
        assert!(err.contains("exit code 3") && err.contains("nope"), "{err}");
        let (cmd, args) = shell("echo out && echo nope 1>&2 && exit 3");
        let (code, out, err) = run_capture_any(cmd, &args.iter().map(String::as_str).collect::<Vec<_>>(), Duration::from_secs(20)).unwrap();
        assert_eq!((code, out.trim(), err.trim()), (Some(3), "out", "nope"));
        let err = run_capture("tmop-no-such-program", &[], Duration::from_secs(5)).unwrap_err();
        assert!(err.starts_with("could not start tmop-no-such-program"), "{err}");
    }

    #[test]
    fn run_capture_kills_on_timeout() {
        let script = if cfg!(windows) { "ping -n 6 127.0.0.1 >NUL" } else { "sleep 5" };
        let start = Instant::now();
        let err = run(script, Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
