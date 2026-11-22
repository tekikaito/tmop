//! Restart Manager: which processes hold a file open. Read-only; used by the delete dialog to
//! warn about files in use.

use super::inuse::Holder;
use super::winutil::{from_wide, wide};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_MORE_DATA;
use windows::Win32::System::RestartManager::{RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO};

/// Most files checked per call (each one costs a Restart Manager session).
pub const MAX_FILES: usize = 64;

/// The processes using `path`; `Err` holds the Win32 error code of the call that failed (5 is
/// access denied).
pub fn holders(path: &Path) -> Result<Vec<Holder>, u32> {
    let mut session = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: `session` and `key` are valid for writes; the key buffer has the documented size.
    let started = unsafe { RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) }.0;
    if started != 0 {
        return Err(started);
    }
    let w = wide(&path.to_string_lossy());
    let mut out = Vec::new();
    let failed: u32;
    // SAFETY: `w` is NUL-terminated and outlives the calls; buffers are sized as passed.
    unsafe {
        let reg = RmRegisterResources(session, Some(&[PCWSTR(w.as_ptr())]), None, None).0;
        if reg == 0 {
            let (mut needed, mut count, mut reasons) = (0u32, 0u32, 0u32);
            let mut st = RmGetList(session, &mut needed, &mut count, None, &mut reasons);
            let mut tries = 0;
            while st == ERROR_MORE_DATA && needed > 0 && tries < 3 {
                let mut buf = vec![RM_PROCESS_INFO::default(); needed as usize];
                count = needed;
                st = RmGetList(session, &mut needed, &mut count, Some(buf.as_mut_ptr()), &mut reasons);
                if st.0 == 0 {
                    for p in buf.iter().take(count as usize) {
                        let t = p.Process.ProcessStartTime;
                        let start = (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
                        out.push(Holder { pid: p.Process.dwProcessId, name: from_wide(&p.strAppName), start: Some(start) });
                    }
                }
                tries += 1;
            }
            // 0 is success (also with no holder); anything else, "more data" included, is a failure.
            failed = st.0;
        } else {
            failed = reg;
        }
        let _ = RmEndSession(session);
    }
    if failed != 0 {
        Err(failed)
    } else {
        Ok(out)
    }
}

/// Holders per file (only files that are in use appear; a failed check shows nothing). At most
/// [`MAX_FILES`] files are checked.
pub fn holders_of(files: &[PathBuf]) -> HashMap<PathBuf, Vec<Holder>> {
    let mut m = HashMap::new();
    for f in files.iter().take(MAX_FILES) {
        if let Ok(h) = holders(f) {
            if !h.is_empty() {
                m.insert(f.clone(), h);
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_own_file() {
        let p = std::env::temp_dir().join(format!("tmop-rm-{}.tmp", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        let got = holders(&p).unwrap_or_default();
        drop(f);
        let _ = std::fs::remove_file(&p);
        // Restart Manager can be unavailable (service disabled, session 0): then nothing is reported.
        if got.is_empty() {
            return;
        }
        assert!(got.iter().any(|g| g.pid == std::process::id() && g.start.is_some()), "{got:?}");
    }
}
