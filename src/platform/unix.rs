//! What Linux and macOS share: libc local time, host name and effective uid, and spawning
//! helper programs without leaving zombies.

use std::process::{Command, Stdio};

/// Local wall-clock time as (year, month, day, hour, minute, second), via `localtime_r`.
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let now = secs as libc::time_t;
    // SAFETY: an all-zero `tm` is valid; localtime_r only reads `now` and writes `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return (1970, 1, 1, ((secs / 3600) % 24) as u16, ((secs / 60) % 60) as u16, (secs % 60) as u16);
    }
    ((tm.tm_year + 1900) as u16, (tm.tm_mon + 1) as u16, tm.tm_mday as u16, tm.tm_hour as u16, tm.tm_min as u16, tm.tm_sec.min(59) as u16)
}

/// The host name from `gethostname`, else `$HOSTNAME`.
pub fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; the name is NUL-terminated when it fits.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    let name = (rc == 0).then(|| {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).trim().to_string()
    });
    name.filter(|n| !n.is_empty()).or_else(|| std::env::var("HOSTNAME").ok().filter(|v| !v.trim().is_empty()))
}

/// True when running as root (effective uid 0).
pub fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// No console windows to hide off Windows.
pub fn hide_console(_cmd: &mut Command) {}

/// Starts `cmd` with null stdio and reaps it on a background thread.
pub(super) fn spawn_reaped(cmd: &mut Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| format!("could not start {program}: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
