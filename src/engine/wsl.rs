//! wsl.exe wrappers for the engine distro: version check, distro listing, import and removal,
//! command execution with binary-safe stdio, and the elevated WSL install.
//!
//! wsl.exe's own output is UTF-16LE when its stdout is redirected, and [`decode_output`] handles
//! it. Output of `--exec` programs is UTF-8 text and is only checked for UTF-8, never rewritten.
//! Output is never parsed from help text.

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::platform::CREATE_NO_WINDOW;
/// Upper bound for `--import` and `--unregister`, which copy or delete a whole VHD.
const LONG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Upper bound for quick status queries and `--terminate`.
const SHORT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long output readers may still run after the child exits. A reader that is still blocked
/// then (a grandchild holds the pipe open) is abandoned, and its output is lost.
const READER_GRACE: Duration = Duration::from_secs(2);

/// Captured result of a wsl.exe run, with both streams decoded by [`decode_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Why [`check`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WslProblem {
    /// wsl.exe does not exist.
    NotInstalled,
    /// wsl.exe exists but rejects `--version` (inbox WSL, too old for the engine).
    NoVersionCommand,
    /// Anything else, with a short description.
    Failed(String),
}

/// One row of `wsl -l -v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistroEntry {
    pub name: String,
    pub running: bool,
    pub version: u8,
    pub default: bool,
}

/// Decodes wsl.exe's own output to a `String`.
///
/// NUL bytes at odd positions (or more than a quarter NULs) mean UTF-16LE; otherwise UTF-8.
/// A leading BOM is removed and CRLF becomes LF. Do not use this for `--exec` output.
pub fn decode_output(bytes: &[u8]) -> String {
    let text = if looks_utf16le(bytes) {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.strip_prefix('\u{feff}').unwrap_or(&text).replace("\r\n", "\n")
}

fn looks_utf16le(bytes: &[u8]) -> bool {
    let pairs = bytes.len() / 2;
    let zeros = bytes.iter().filter(|b| **b == 0).count();
    let odd_zeros = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    pairs > 0 && (zeros * 4 > bytes.len() || odd_zeros * 2 >= pairs)
}

/// Absolute path of the 64-bit wsl.exe, falling back to a PATH lookup.
fn wsl_path() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("wsl.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("wsl.exe"))
}

/// Bytes captured from a run, before decoding.
struct RawOutput {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl RawOutput {
    /// For wsl.exe's own output: UTF-16LE or UTF-8, see [`decode_output`].
    fn into_wsl(self) -> WslOutput {
        WslOutput { status: self.status, stdout: decode_output(&self.stdout), stderr: decode_output(&self.stderr) }
    }

    /// For `--exec` output: UTF-8 with invalid bytes replaced. Nothing else is changed.
    fn into_exec(self) -> WslOutput {
        WslOutput {
            status: self.status,
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
        }
    }
}

/// Runs wsl.exe with `args` and captures its output.
pub fn run(args: &[&str], timeout: Duration) -> Result<WslOutput, String> {
    run_cmd(args, None, timeout).map(RawOutput::into_wsl).map_err(|e| e.to_string())
}

/// Failure to run wsl.exe at all, as opposed to a non-zero exit code.
#[derive(Debug)]
enum RunError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    TimedOut(Duration),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "could not start wsl.exe: {e}"),
            RunError::Wait(e) => write!(f, "waiting for wsl.exe failed: {e}"),
            RunError::TimedOut(t) => write!(f, "wsl.exe timed out after {} s", t.as_secs()),
        }
    }
}

fn run_cmd(args: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> Result<RawOutput, RunError> {
    let mut cmd = Command::new(wsl_path());
    cmd.args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;

    // The feeder is not joined: once the child has exited its write fails, and a grandchild that
    // holds stdin open must not block this call.
    let _feeder = match (stdin, child.stdin.take()) {
        (Some(data), Some(mut pipe)) => {
            let data = data.to_vec();
            Some(thread::spawn(move || {
                // A child that exits early breaks the pipe; its exit status reports the cause.
                let _ = pipe.write_all(&data);
            }))
        }
        _ => None,
    };
    let out_reader = pipe_reader(child.stdout.take());
    let err_reader = pipe_reader(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(RunError::Wait(e)),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::TimedOut(timeout));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let grace = Instant::now() + READER_GRACE;
    Ok(RawOutput {
        status: status.code().unwrap_or(-1),
        stdout: collect(out_reader, grace),
        stderr: collect(err_reader, grace),
    })
}

/// Drains a child pipe on its own thread so a full pipe buffer cannot stall the child. The bytes
/// are sent on the returned channel once the pipe closes.
fn pipe_reader(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// The bytes from a reader, or empty when it has not finished by `deadline` (it is abandoned).
fn collect(rx: mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap_or_default()
}

/// Like [`run`], but a non-zero exit code is an error that carries the output text.
fn run_checked(args: &[&str], timeout: Duration) -> Result<WslOutput, String> {
    let out = run(args, timeout)?;
    if out.status == 0 {
        return Ok(out);
    }
    let detail = [&out.stderr, &out.stdout].into_iter().map(|s| s.trim()).find(|s| !s.is_empty()).unwrap_or("no output");
    Err(format!("wsl {} failed (exit code {}): {detail}", args.join(" "), out.status))
}

/// Pulls the version out of `wsl --version` output: the first line containing "WSL" and then a
/// dotted number such as `2.4.13.0`. Other lines (kernel version) are ignored.
pub fn extract_version(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = &line[line.find("WSL")? + 3..];
        rest.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map(|t| t.trim_matches('.'))
            .find(|t| is_version(t))
            .map(str::to_string)
    })
}

fn is_version(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    (3..=4).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Checks that WSL is installed and new enough. Returns the version, such as `2.4.13.0`.
pub fn check() -> Result<String, WslProblem> {
    let out = match run_cmd(&["--version"], None, SHORT_TIMEOUT).map(RawOutput::into_wsl) {
        Ok(out) => out,
        Err(RunError::Spawn(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err(WslProblem::NotInstalled),
        Err(e) => return Err(WslProblem::Failed(e.to_string())),
    };
    if let Some(version) = extract_version(&out.stdout) {
        return Ok(version);
    }
    if out.status != 0 {
        return Err(WslProblem::NoVersionCommand);
    }
    Err(WslProblem::Failed(format!("unexpected output from wsl --version: {}", out.stdout.trim())))
}

/// Parses one `wsl -l -v` row: optional `*` default marker, name, state, version.
fn parse_row(line: &str) -> Option<(bool, &str, &str, u8)> {
    let trimmed = line.trim_start();
    let default = trimmed.starts_with('*');
    let mut cols = trimmed.trim_start_matches('*').split_whitespace();
    let name = cols.next()?;
    let state = cols.next()?;
    let version = cols.next()?.parse().ok()?;
    Some((default, name, state, version))
}

/// Parses `wsl -l -v` text. Header and message lines have no numeric version and are skipped.
///
/// `running` lists the names from `wsl -l --running`; it is only consulted when the state column
/// is neither `Running` nor `Stopped`.
pub fn parse_list(text: &str, running: &[String]) -> Vec<DistroEntry> {
    text.lines()
        .filter_map(parse_row)
        .map(|(default, name, state, version)| {
            let is_running = match state {
                "Running" => true,
                "Stopped" => false,
                _ => running.iter().any(|r| r.eq_ignore_ascii_case(name)),
            };
            DistroEntry { name: name.to_string(), running: is_running, version, default }
        })
        .collect()
}

/// Names from `wsl -l --running`: the first token of each line after the header.
fn running_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Lists the registered distros.
pub fn list() -> Result<Vec<DistroEntry>, String> {
    let out = run_checked(&["-l", "-v"], SHORT_TIMEOUT)?;
    let unknown_state = out.stdout.lines().filter_map(parse_row).any(|(_, _, s, _)| s != "Running" && s != "Stopped");
    let running = if unknown_state { running_names(&run_checked(&["-l", "--running"], SHORT_TIMEOUT)?.stdout) } else { Vec::new() };
    Ok(parse_list(&out.stdout, &running))
}

/// True when a distro with this name is registered (names are case-insensitive).
pub fn is_registered(distro: &str) -> bool {
    list().map(|v| v.iter().any(|e| e.name.eq_ignore_ascii_case(distro))).unwrap_or(false)
}

/// True when the distro is registered and running.
pub fn is_running(distro: &str) -> bool {
    list().map(|v| v.iter().any(|e| e.name.eq_ignore_ascii_case(distro) && e.running)).unwrap_or(false)
}

fn path_arg(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("path is not valid Unicode: {}", path.display()))
}

/// Imports a rootfs tarball as a WSL 2 distro stored in `install_dir`.
pub fn import(distro: &str, install_dir: &Path, tar: &Path) -> Result<(), String> {
    let dir = path_arg(install_dir)?;
    let tar = path_arg(tar)?;
    run_checked(&["--import", distro, dir, tar, "--version", "2"], LONG_TIMEOUT).map(|_| ())
}

/// Stops a running distro.
pub fn terminate(distro: &str) -> Result<(), String> {
    run_checked(&["--terminate", distro], SHORT_TIMEOUT).map(|_| ())
}

/// Unregisters a distro and deletes its virtual disk.
pub fn unregister(distro: &str) -> Result<(), String> {
    run_checked(&["--unregister", distro], LONG_TIMEOUT).map(|_| ())
}

fn exec_args<'a>(distro: &'a str, argv: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["-d", distro, "--exec"];
    args.extend_from_slice(argv);
    args
}

/// Runs `argv` inside the distro without a shell. Stdout and stderr are UTF-8 text (invalid bytes
/// replaced) with no line-ending changes.
pub fn exec(distro: &str, argv: &[&str], timeout: Duration) -> Result<WslOutput, String> {
    run_cmd(&exec_args(distro, argv), None, timeout).map(RawOutput::into_exec).map_err(|e| e.to_string())
}

/// Like [`exec`], writing `stdin_bytes` to the program's stdin and then closing it.
pub fn exec_input(distro: &str, argv: &[&str], stdin_bytes: &[u8], timeout: Duration) -> Result<WslOutput, String> {
    run_cmd(&exec_args(distro, argv), Some(stdin_bytes), timeout).map(RawOutput::into_exec).map_err(|e| e.to_string())
}

/// Starts `argv` in the distro with piped stdin and stdout (stderr discarded), killed when dropped.
pub fn spawn_exec(distro: &str, argv: &[&str]) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(wsl_path())
        .args(exec_args(distro, argv))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()
}

/// Installs WSL with a UAC prompt (`wsl --install --no-distribution`).
pub fn install_wsl_elevated() -> Result<(), String> {
    crate::engine::winreg::run_elevated(&wsl_path().to_string_lossy(), "--install --no-distribution")
}

/// `\\wsl$\<distro>\<linux_path>` for a Linux path inside the distro.
pub fn unc_path(distro: &str, linux_path: &str) -> PathBuf {
    let rel = linux_path.trim_start_matches('/').replace('/', "\\");
    PathBuf::from(format!(r"\\wsl$\{distro}\{rel}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "  NAME              STATE           VERSION\n\
* Ubuntu            Running         2\n\
  docker-desktop    Running         2\n\
  tmop-engine       Stopped         2\n";

    #[test]
    fn decode_utf16le_from_wsl_list() {
        // "  N\r\n" as wsl.exe writes it when redirected.
        let bytes = [0x20, 0x00, 0x20, 0x00, 0x4e, 0x00, 0x0d, 0x00, 0x0a, 0x00];
        assert_eq!(decode_output(&bytes), "  N\n");
    }

    #[test]
    fn decode_utf16le_strips_bom() {
        let bytes = [0xff, 0xfe, 0x55, 0x00, 0x62, 0x00];
        assert_eq!(decode_output(&bytes), "Ub");
    }

    #[test]
    fn decode_utf16le_non_ascii() {
        // "Kä" in UTF-16LE: the 0xE4 byte is at an even position, the NULs at odd ones.
        let bytes = [0x4b, 0x00, 0xe4, 0x00];
        assert_eq!(decode_output(&bytes), "Kä");
    }

    #[test]
    fn decode_utf8_passes_through() {
        assert_eq!(decode_output(b"Ubuntu\r\nx"), "Ubuntu\nx");
        assert_eq!(decode_output(b""), "");
    }

    #[test]
    fn decode_utf8_multibyte_is_not_utf16() {
        // "Käse" in UTF-8: no NUL bytes, so it must not be read as UTF-16.
        assert_eq!(decode_output(b"K\xc3\xa4se"), "Käse");
    }

    #[test]
    fn parse_list_reads_sample() {
        let rows = parse_list(SAMPLE, &[]);
        assert_eq!(
            rows,
            vec![
                DistroEntry { name: "Ubuntu".into(), running: true, version: 2, default: true },
                DistroEntry { name: "docker-desktop".into(), running: true, version: 2, default: false },
                DistroEntry { name: "tmop-engine".into(), running: false, version: 2, default: false },
            ]
        );
    }

    #[test]
    fn parse_list_uses_running_names_for_unknown_state() {
        let text = "  NAME STATE VERSION\n  Debian Ausgeführt 2\n  Alpine Gestoppt 2\n";
        let rows = parse_list(text, &["debian".to_string()]);
        assert!(rows[0].running);
        assert!(!rows[1].running);
    }

    #[test]
    fn parse_list_skips_message_lines() {
        assert!(parse_list("Windows-Subsystem für Linux hat keine Distributionen.\n", &[]).is_empty());
    }

    #[test]
    fn running_names_skip_header() {
        let text = "NAME\r\nUbuntu (Default)\r\ndocker-desktop\r\n";
        assert_eq!(running_names(&decode_output(text.as_bytes())), vec!["Ubuntu", "docker-desktop"]);
    }

    #[test]
    fn extract_version_from_wsl_version_output() {
        let text = "WSL-Version: 2.4.13.0\nKernelversion: 5.15.167.4-1\nWSLg-Version: 1.0.65\n";
        assert_eq!(extract_version(text).as_deref(), Some("2.4.13.0"));
        assert_eq!(extract_version("Kernelversion: 5.15.167.4-1"), None);
        assert_eq!(extract_version("WSL-Version: 2.4.13"), Some("2.4.13".into()));
    }

    #[test]
    fn exec_output_is_utf8_without_crlf_rewrite() {
        let raw = RawOutput { status: 0, stdout: b"a\r\nb\xff".to_vec(), stderr: b"\x00x".to_vec() };
        let out = raw.into_exec();
        assert_eq!(out.stdout, "a\r\nb\u{fffd}");
        assert_eq!(out.stderr, "\u{0}x");
    }

    #[test]
    fn wsl_output_is_decoded_from_utf16() {
        let raw = RawOutput { status: 1, stdout: vec![0x4b, 0x00, 0x0d, 0x00, 0x0a, 0x00], stderr: Vec::new() };
        let out = raw.into_wsl();
        assert_eq!((out.status, out.stdout.as_str()), (1, "K\n"));
    }

    #[test]
    fn reader_output_is_abandoned_after_the_grace_deadline() {
        // Sender kept alive but never sending: the reader never finishes.
        let (_tx, rx) = mpsc::channel::<Vec<u8>>();
        let start = Instant::now();
        assert!(collect(rx, Instant::now() + Duration::from_millis(50)).is_empty());
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(collect(pipe_reader(Some(&b"xyz"[..])), Instant::now() + Duration::from_secs(5)), b"xyz");
    }

    #[test]
    fn unc_path_maps_linux_path() {
        assert_eq!(
            unc_path("tmop-engine", "/usr/share/tmop/windows"),
            PathBuf::from(r"\\wsl$\tmop-engine\usr\share\tmop\windows")
        );
        assert_eq!(unc_path("tmop-engine", "/etc/docker"), PathBuf::from(r"\\wsl$\tmop-engine\etc\docker"));
    }

    #[test]
    #[ignore = "runs wsl.exe (read-only)"]
    fn check_finds_wsl_on_this_machine() {
        let version = check().expect("wsl --version");
        println!("WSL version: {version}");
        assert!(is_version(&version));
    }

    #[test]
    #[ignore = "runs wsl.exe (read-only)"]
    fn list_contains_ubuntu() {
        let rows = list().expect("wsl -l -v");
        println!("{rows:?}");
        assert!(rows.iter().any(|e| e.name == "Ubuntu"));
    }
}
