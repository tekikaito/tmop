//! The command line: what `TMOP --version`, `--help` and an unknown option do (#106), and the
//! list of every argument TMOP understands.
//!
//! TMOP is also started by others: Windows passes Task Manager's own arguments when TMOP stands
//! in for it, the Run key, the updater and the elevation prompt start it with internal switches,
//! and macOS may add `-psn_...`. So only an argument that starts with `--` and is on no list
//! here is rejected, and never when Windows started TMOP as Task Manager.

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// Start as usual (windows, `--dump`, helper switches, ...).
    Run,
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h`.
    Help,
    /// An option starting with `--` that TMOP does not have.
    Unknown(String),
}

/// Options that carry a value: `--name=value`.
const VALUE_OPTIONS: &[&str] = &[
    // Meant for users.
    "--page=",
    "--device=",
    "--disk-root=",
    "--screenshot=",
    "--window-size=",
    "--theme=",
    "--settings-section=",
    "--wait=",
    "--frames=",
    "--every=",
    "--shot-home=",
    "--popout-skin=",
    "--record=",
    "--record-seconds=",
    "--bench=",
    // Aliases and test hooks.
    "--start-page=",
    "--start-device=",
    "--disk-scan=",
    "--docker-terminal=",
    "--docker-tab=",
    "--engine-test=",
    // TMOP's own helper processes.
    "--set-taskmgr-replacement=",
    "--volume-caches=",
];

/// Options without a value.
const FLAG_OPTIONS: &[&str] = &[
    "--dump",
    "--popout",
    "--maximized",
    "--record",
    "--restore-taskmgr",
    "--manual",
    // Internal: the Docker engine relay (Run key) and the elevated helpers.
    "--engine-relay",
    "--engine-relay-stop",
    "--open-taskmgr",
    "--clear-runasadmin",
];

fn known(arg: &str) -> bool {
    FLAG_OPTIONS.contains(&arg) || VALUE_OPTIONS.iter().any(|p| arg.starts_with(p))
}

/// Classifies the arguments after the program name.
pub fn parse(args: &[String]) -> Cli {
    if crate::taskmgr::taskmgr_launch(args).is_some() {
        return Cli::Run;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Cli::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Cli::Help;
    }
    match args.iter().find(|a| a.starts_with("--") && !known(a)) {
        Some(a) => Cli::Unknown(a.clone()),
        None => Cli::Run,
    }
}

/// `tmop <version>`.
pub fn version_text() -> String {
    format!("tmop {}\n", env!("CARGO_PKG_VERSION"))
}

/// The usage text: only the options meant for users.
pub fn help_text() -> String {
    format!(
        "tmop {} - Task Manager OverPowered

Usage: tmop [options]

Options:
  --page=<id>              start on summary, performance, processes, docker or settings
  --device=<id>            start on this device of the Performance page
  --disk-root=<path>       start on Disk Space and scan <path>
  --maximized              start with a maximized window
  --window-size=<W>x<H>    start with this window size
  --popout                 open only the skinned pop-out window
  --popout-skin=<id>       use this pop-out skin
  --dump                   print two samples of CPU, memory, disk, network, GPU and the
                           biggest processes to stdout and exit (no window)
  --screenshot=<file.png>  render the window's own frame to a PNG and exit; with
                           --theme=dark|light, --settings-section=<name>, --wait=<s>,
                           --frames=<n> --every=<ms> and --shot-home=<dir>
  --restore-taskmgr        stop opening TMOP in place of Windows Task Manager (Windows)
  -V, --version            print the version and exit
  -h, --help               print this text and exit
",
        env!("CARGO_PKG_VERSION")
    )
}

/// The message for an unknown option, on stderr.
pub fn unknown_text(arg: &str) -> String {
    format!("tmop: unknown option '{arg}'\nTry 'tmop --help' for the options.\n")
}

/// On Linux a window needs an X11 or Wayland session. `has(name)` tells whether an environment
/// variable is set. winit takes Wayland from `WAYLAND_DISPLAY` or `WAYLAND_SOCKET`, X11 from
/// `DISPLAY`; with none of them it cannot connect.
pub fn display_missing(has: impl Fn(&str) -> bool) -> bool {
    !(has("DISPLAY") || has("WAYLAND_DISPLAY") || has("WAYLAND_SOCKET"))
}

/// The one line printed for [`display_missing`].
pub const NO_DISPLAY_TEXT: &str = "tmop: no display found (neither DISPLAY nor WAYLAND_DISPLAY is set). Start TMOP in a desktop session, or use --dump for a text report.\n";

/// A GUI-subsystem TMOP on Windows has no console of its own. When it was started from one
/// without redirected output, attaches to the parent's console and points stdout and stderr
/// there, so `--version`, `--help` and the error reach the terminal. No effect elsewhere, or
/// for a stream that is already redirected (`> file`, `| more`).
pub fn attach_console() {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE};
        fn missing(which: STD_HANDLE) -> bool {
            // SAFETY: reads the process's standard handle.
            unsafe { GetStdHandle(which) }.map(|h| h.is_invalid()).unwrap_or(true)
        }
        if !missing(STD_OUTPUT_HANDLE) && !missing(STD_ERROR_HANDLE) {
            return;
        }
        // SAFETY: plain console call.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if !missing(which) {
                continue;
            }
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                let handle = HANDLE(f.as_raw_handle());
                // The handle stays the standard handle until the process exits.
                std::mem::forget(f);
                // SAFETY: `handle` is a valid, never-closed console handle.
                let _ = unsafe { SetStdHandle(which, handle) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Cli {
        parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn version_and_help() {
        assert_eq!(p(&["--version"]), Cli::Version);
        assert_eq!(p(&["-V"]), Cli::Version);
        assert_eq!(p(&["--help"]), Cli::Help);
        assert_eq!(p(&["-h"]), Cli::Help);
        assert_eq!(p(&["--page=docker", "--help"]), Cli::Help);
        assert_eq!(p(&["--help", "--version"]), Cli::Version);
    }

    #[test]
    fn user_options_run() {
        for a in [
            &[][..],
            &["--dump"],
            &["--screenshot=a.png", "--page=performance", "--device=disk", "--window-size=1584x877", "--theme=light"],
            &["--screenshot=a.png", "--frames=3", "--every=500", "--wait=2", "--shot-home=x", "--settings-section=graphs"],
            &["--popout", "--popout-skin=x", "--maximized"],
            &["--disk-root=C:\\", "--disk-scan=/", "--start-page=summary", "--start-device=cpu"],
            &["--record", "--record=x.csv", "--record-seconds=5", "--bench=all"],
            &["--docker-terminal=web;sh", "--docker-tab=images", "--engine-test=setup"],
        ] {
            assert_eq!(p(a), Cli::Run, "{a:?}");
        }
    }

    #[test]
    fn internal_options_are_still_known() {
        for a in [
            "--restore-taskmgr",
            "--set-taskmgr-replacement=on",
            "--open-taskmgr",
            "--clear-runasadmin",
            "--engine-relay",
            "--engine-relay-stop",
            "--volume-caches=query:C",
            "--manual",
        ] {
            assert_eq!(p(&[a]), Cli::Run, "{a}");
        }
        assert_eq!(p(&["--page=disk-space", "--disk-root=\"C:\\Program Files\""]), Cli::Run);
    }

    #[test]
    fn unknown_double_dash_is_rejected() {
        assert_eq!(p(&["--frobnicate"]), Cli::Unknown("--frobnicate".into()));
        assert_eq!(p(&["--page=docker", "--versoin"]), Cli::Unknown("--versoin".into()));
        assert_eq!(p(&["--dump=1"]), Cli::Unknown("--dump=1".into()));
    }

    #[test]
    fn other_launchers_arguments_are_not_rejected() {
        // Screensaver conventions, macOS' process serial number, single-dash and slash switches.
        for a in [&["/s"][..], &["/p", "1234"], &["/c:99"], &["-s"], &["-psn_0_12345"], &["-x"], &["somefile"]] {
            assert_eq!(p(a), Cli::Run, "{a:?}");
        }
    }

    #[test]
    fn task_manager_launches_are_never_rejected() {
        assert_eq!(p(&[r"C:\WINDOWS\system32\taskmgr.exe", "/7"]), Cli::Run);
        assert_eq!(p(&[r"C:\Windows\System32\Taskmgr.exe", "/0", "/startup"]), Cli::Run);
        assert_eq!(p(&[r"C:\Windows\System32\Taskmgr.exe", "--whatever"]), Cli::Run);
        assert_eq!(p(&["\"C:\\Windows\\System32\\taskmgr.exe\"", "/0"]), Cli::Run);
    }

    #[test]
    fn texts() {
        assert_eq!(version_text(), format!("tmop {}\n", env!("CARGO_PKG_VERSION")));
        let help = help_text();
        for o in ["--dump", "--screenshot=<file.png>", "--version", "--help", "--page=<id>"] {
            assert!(help.contains(o), "{o}");
        }
        assert!(unknown_text("--x").contains("--help"));
    }

    #[test]
    fn display_check() {
        assert!(display_missing(|_| false));
        assert!(!display_missing(|n| n == "DISPLAY"));
        assert!(!display_missing(|n| n == "WAYLAND_DISPLAY"));
        assert!(!display_missing(|n| n == "WAYLAND_SOCKET"));
    }
}
