//! Starting a program from the "Run new task" dialog.
//!
//! Windows hands the line to `ShellExecuteExW` (verb `open`, or `runas`), so documents, URLs, PATH
//! names and `%VARS%` behave as in the Run dialog. Linux and macOS split the line themselves
//! (quote-aware, no shell) and start the executable detached, so it outlives TMOP. Something that
//! is not an executable is refused there rather than guessed at with `xdg-open` or `open`.
//! The working directory is the user's home either way.
//!
//! The pieces that decide something (splitting a line, telling the file from the parameters,
//! turning an error code into words) are pure and tested on every host.

use std::path::Path;

/// What happened to a request that did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The program was started (its process id, when the OS gave one).
    Started(Option<u32>),
    /// The user declined the administrator prompt. Not an error: nothing is shown.
    Cancelled,
}

/// Starts `line`; `admin` asks for administrator rights (Windows only, ignored elsewhere). Blocks
/// while a permission prompt is open, so call it off the UI thread.
pub fn run(line: &str, admin: bool) -> Result<Outcome, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("Type the program, file or address to open.".into());
    }
    imp::run(line, admin)
}

/// A line as the Windows shell wants it: the file or address, and the parameters after it.
/// `exists` says whether a path is on disk, so an unquoted `C:\Program Files\x\y.exe /s` finds
/// where the file name ends. `None` for an empty line.
#[cfg(any(windows, test))]
fn windows_target(line: &str, exists: impl Fn(&str) -> bool) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix('"') {
        return Some(match rest.split_once('"') {
            Some((file, params)) => (file.to_string(), params.trim().to_string()),
            None => (rest.to_string(), String::new()),
        });
    }
    if exists(line) {
        return Some((line.to_string(), String::new()));
    }
    // Unquoted: the shortest start of the line that is a file on disk is the file (a start like "C: Program" is none).
    for (i, c) in line.char_indices() {
        if c.is_whitespace() && i > 0 && exists(&line[..i]) {
            return Some((line[..i].to_string(), line[i..].trim().to_string()));
        }
    }
    Some(match line.split_once(char::is_whitespace) {
        Some((file, params)) => (file.to_string(), params.trim().to_string()),
        None => (line.to_string(), String::new()),
    })
}

/// The Win32 error behind an HRESULT from the shell (`0x8007xxxx` carries a Win32 code).
#[cfg(any(windows, test))]
fn win32_code(hresult: i32) -> u32 {
    let h = hresult as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        h & 0xFFFF
    } else {
        h
    }
}

/// Words for a failed `ShellExecuteExW`; `None` when the user cancelled the permission prompt.
/// `inst` is `hInstApp`, which holds an `SE_ERR_*` code when the call fails.
#[cfg(any(windows, test))]
fn shell_error(code: u32, inst: usize, name: &str) -> Option<String> {
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    const ERROR_PATH_NOT_FOUND: u32 = 3;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_BAD_EXE_FORMAT: u32 = 193;
    const ERROR_CANCELLED: u32 = 1223;
    const ERROR_NO_ASSOCIATION: u32 = 1155;
    const ERROR_ELEVATION_REQUIRED: u32 = 740;
    const SE_ERR_NOASSOC: usize = 31;
    const SE_ERR_ASSOCINCOMPLETE: usize = 27;
    Some(match code {
        ERROR_CANCELLED => return None,
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => format!("\"{name}\" was not found. Check the spelling or the path."),
        ERROR_ACCESS_DENIED => format!("Access to \"{name}\" was denied."),
        ERROR_BAD_EXE_FORMAT => format!("\"{name}\" is not a program this version of Windows can run."),
        ERROR_NO_ASSOCIATION => format!("No program is set to open \"{name}\"."),
        ERROR_ELEVATION_REQUIRED => format!("\"{name}\" needs administrator rights. Tick Run as administrator."),
        _ if inst == SE_ERR_NOASSOC || inst == SE_ERR_ASSOCINCOMPLETE => format!("No program is set to open \"{name}\"."),
        _ => format!("Could not start \"{name}\" (error {code})."),
    })
}

/// Splits a command line into words like a POSIX shell would, without running one: whitespace
/// separates words; single quotes keep everything literally; inside double quotes a backslash
/// escapes only `"` and `\`; outside quotes a backslash escapes the next character. Adjacent
/// quoted and bare text join into one word (`a"b c"d` is `ab cd`), and `""` is an empty word.
/// No expansion of any kind is done. An open quote or a trailing backslash is an error.
#[cfg(any(unix, test))]
fn split_command_line(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("A single quote is not closed.".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => return Err("A double quote is not closed.".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("A double quote is not closed.".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(e) => cur.push(e),
                    None => return Err("The line ends with a backslash.".into()),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// Words for a failed spawn of `program` off Windows.
#[cfg(any(unix, test))]
fn spawn_error(kind: std::io::ErrorKind, raw: Option<i32>, program: &str) -> String {
    // ENOEXEC (8): a file that is no executable and has no interpreter line.
    const ENOEXEC: i32 = 8;
    match kind {
        std::io::ErrorKind::NotFound => format!("\"{program}\" was not found. Check the spelling or the path."),
        std::io::ErrorKind::PermissionDenied => format!("\"{program}\" is not an executable file, or you may not run it."),
        _ if raw == Some(ENOEXEC) => format!("\"{program}\" is not a program the system can run."),
        _ => format!("Could not start \"{program}\"."),
    }
}

/// `~` and `~/x` to the home folder; anything else as it is.
#[cfg(any(unix, test))]
fn expand_tilde(word: &str, home: Option<&Path>) -> String {
    match (word, home) {
        ("~", Some(h)) => h.display().to_string(),
        (w, Some(h)) if w.starts_with("~/") => h.join(&w[2..]).display().to_string(),
        _ => word.to_string(),
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::GetProcessId;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_DOENVSUBST, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::{SHOW_WINDOW_CMD, SW_SHOWNORMAL};

    pub fn run(line: &str, admin: bool) -> Result<Outcome, String> {
        shell_run(line, admin, SW_SHOWNORMAL)
    }

    /// [`run`] with the window state of the new program; the tests hide theirs.
    pub fn shell_run(line: &str, admin: bool, show: SHOW_WINDOW_CMD) -> Result<Outcome, String> {
        let (file, params) = windows_target(line, |p| Path::new(p).exists()).ok_or("Type the program, file or address to open.")?;
        let dir = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).filter(|d| d.is_dir());
        let (wfile, wparams, wdir) = (HSTRING::from(file.as_str()), HSTRING::from(params.as_str()), dir.map(|d| HSTRING::from(d.as_os_str())));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            // NO_UI: the shell would pop up its own error box; this app words the failure itself.
            fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_DOENVSUBST | SEE_MASK_FLAG_NO_UI,
            lpVerb: if admin { windows::core::w!("runas") } else { windows::core::w!("open") },
            lpFile: PCWSTR(wfile.as_ptr()),
            lpParameters: if params.is_empty() { PCWSTR::null() } else { PCWSTR(wparams.as_ptr()) },
            lpDirectory: wdir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            nShow: show.0,
            ..Default::default()
        };
        // SAFETY: COM is initialised for this thread for the call and released after; `info` and
        // the strings it points to outlive the call; the process handle it returns is closed here.
        unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = ShellExecuteExW(&mut info);
            let out = match result {
                Ok(()) => {
                    let pid = (!info.hProcess.is_invalid()).then(|| GetProcessId(info.hProcess)).filter(|p| *p != 0);
                    if !info.hProcess.is_invalid() {
                        let _ = CloseHandle(info.hProcess);
                    }
                    Ok(Outcome::Started(pid))
                }
                Err(e) => match shell_error(win32_code(e.code().0), info.hInstApp.0 as usize, &file) {
                    Some(words) => Err(words),
                    None => Ok(Outcome::Cancelled),
                },
            };
            if com.is_ok() {
                CoUninitialize();
            }
            out
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    pub fn run(line: &str, _admin: bool) -> Result<Outcome, String> {
        let words = split_command_line(line)?;
        let home = crate::platform::home();
        let Some((program, args)) = words.split_first() else {
            return Err("Type the program to start.".into());
        };
        let program = expand_tilde(program, home.as_deref());
        let mut cmd = Command::new(&program);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // Its own process group: a Ctrl+C or a closed terminal aimed at TMOP does not reach it.
        cmd.process_group(0);
        if let Some(h) = home.as_ref().filter(|h| h.is_dir()) {
            cmd.current_dir(h);
        }
        let mut child = cmd.spawn().map_err(|e| spawn_error(e.kind(), e.raw_os_error(), &program))?;
        let pid = child.id();
        // Reaped when it ends, so no zombie stays behind while TMOP runs; if TMOP ends first the
        // program carries on under init.
        std::thread::spawn(move || child.wait());
        Ok(Outcome::Started(Some(pid)))
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use super::*;

    pub fn run(_line: &str, _admin: bool) -> Result<Outcome, String> {
        Err("Not available on this system.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        split_command_line(s).unwrap()
    }

    #[test]
    fn splits_plain_and_quoted_words() {
        assert_eq!(words("  ls   -la  /tmp "), ["ls", "-la", "/tmp"]);
        assert_eq!(words(r#"echo "hello world" 'a  b'"#), ["echo", "hello world", "a  b"]);
        assert_eq!(words(r#"a"b c"d"#), ["ab cd"]);
        assert_eq!(words(r#"x "" y"#), ["x", "", "y"]);
        assert_eq!(words(""), Vec::<String>::new());
    }

    #[test]
    fn backslashes_follow_shell_rules() {
        assert_eq!(words(r"a\ b c"), ["a b", "c"]);
        assert_eq!(words(r#""a \"q\" \\ \n""#), [r#"a "q" \ \n"#]);
        assert_eq!(words(r"'a\b'"), [r"a\b"]);
        assert_eq!(words(r#"echo $HOME `x` *"#), ["echo", "$HOME", "`x`", "*"]);
    }

    #[test]
    fn open_quotes_and_trailing_backslash_are_errors() {
        assert!(split_command_line("echo 'abc").is_err());
        assert!(split_command_line("echo \"abc").is_err());
        assert!(split_command_line("echo \"abc\\").is_err());
        assert!(split_command_line("echo abc\\").is_err());
    }

    #[test]
    fn windows_target_finds_the_file_part() {
        let exists = |p: &str| p == r"C:\Program Files\App\a.exe" || p == r"C:\Docs";
        let t = |l: &str| windows_target(l, exists).unwrap();
        assert_eq!(t("notepad"), ("notepad".into(), "".into()));
        assert_eq!(t("notepad  C:\\x y.txt "), ("notepad".into(), "C:\\x y.txt".into()));
        assert_eq!(t(r#""C:\Program Files\App\a.exe" /s"#), (r"C:\Program Files\App\a.exe".into(), "/s".into()));
        assert_eq!(t(r"C:\Program Files\App\a.exe /s /q"), (r"C:\Program Files\App\a.exe".into(), "/s /q".into()));
        assert_eq!(t(r"C:\Docs"), (r"C:\Docs".into(), "".into()));
        assert_eq!(t("https://example.com/a b"), ("https://example.com/a".into(), "b".into()));
        assert_eq!(t(r#""unclosed arg"#), ("unclosed arg".into(), "".into()));
        assert_eq!(windows_target("   ", exists), None);
    }

    #[test]
    fn win32_error_codes() {
        assert_eq!(win32_code(0x8007_04C7_u32 as i32), 1223);
        assert_eq!(win32_code(0x8000_4005_u32 as i32), 0x8000_4005);
    }

    #[test]
    fn shell_errors_are_plain_and_a_declined_prompt_is_silent() {
        assert_eq!(shell_error(1223, 0, "x"), None);
        assert_eq!(shell_error(2, 2, "x").unwrap(), "\"x\" was not found. Check the spelling or the path.");
        assert_eq!(shell_error(3, 3, "x").unwrap(), "\"x\" was not found. Check the spelling or the path.");
        assert_eq!(shell_error(5, 5, "x").unwrap(), "Access to \"x\" was denied.");
        assert!(shell_error(1155, 31, "x.zzz").unwrap().starts_with("No program is set"));
        assert!(shell_error(0, 31, "x.zzz").unwrap().starts_with("No program is set"));
        assert!(shell_error(740, 0, "x").unwrap().contains("Run as administrator"));
        assert_eq!(shell_error(1234, 0, "x").unwrap(), "Could not start \"x\" (error 1234).");
    }

    #[test]
    fn spawn_errors_are_plain() {
        use std::io::ErrorKind::*;
        assert!(spawn_error(NotFound, Some(2), "x").contains("was not found"));
        assert!(spawn_error(PermissionDenied, Some(13), "x").contains("not an executable"));
        assert!(spawn_error(Other, Some(8), "x").contains("not a program"));
        assert_eq!(spawn_error(Other, None, "x"), "Could not start \"x\".");
    }

    #[test]
    fn tilde_is_the_home_folder() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde("~", Some(home)), "/home/u");
        assert_eq!(expand_tilde("~/bin/x", Some(home)), Path::new("/home/u").join("bin/x").display().to_string());
        assert_eq!(expand_tilde("~x", Some(home)), "~x");
        assert_eq!(expand_tilde("~/a", None), "~/a");
    }

    #[test]
    fn an_empty_line_is_refused_before_anything_runs() {
        assert!(run("   ", false).is_err());
    }

    /// Starts a child of its own (a hidden `ping`), checks the process id the shell reports is
    /// really that process, and ends it. Touches nothing else.
    #[cfg(windows)]
    #[test]
    fn shell_starts_a_program_and_reports_its_pid() {
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        let Outcome::Started(Some(pid)) = imp::shell_run("ping -n 30 127.0.0.1", false, SW_HIDE).unwrap() else { panic!("no pid") };
        let alive = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]).output().unwrap();
        let text = String::from_utf8_lossy(&alive.stdout).to_lowercase();
        let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output();
        assert!(text.contains("ping.exe"), "{text}");
    }

    #[cfg(windows)]
    #[test]
    fn shell_reports_a_missing_program_in_words() {
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
        let e = imp::shell_run("tmop-no-such-program-xyz", false, SW_HIDE).unwrap_err();
        assert!(e.contains("was not found"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_starts_a_detached_program_and_refuses_a_missing_one() {
        assert!(matches!(run("true", false), Ok(Outcome::Started(Some(_)))));
        assert!(run("tmop-no-such-program-xyz", false).unwrap_err().contains("was not found"));
        assert!(run("echo 'open", false).is_err());
    }
}
