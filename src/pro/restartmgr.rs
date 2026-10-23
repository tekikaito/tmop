//! Restart Manager: which processes hold a file open. Read-only; used by the delete dialog to
//! warn about files in use.

use super::winutil::{from_wide, wide};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_MORE_DATA;
use windows::Win32::System::RestartManager::{RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO};

/// Most files checked per call (each one costs a Restart Manager session).
pub const MAX_FILES: usize = 64;

/// "name (PID n)" of every process using `path`; empty when none or the check failed.
pub fn holders(path: &Path) -> Vec<String> {
    let mut session = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: `session` and `key` are valid for writes; the key buffer has the documented size.
    if unsafe { RmStartSession(&mut session, 0, PWSTR(key.as_mut_ptr())) }.0 != 0 {
        return Vec::new();
    }
    let w = wide(&path.to_string_lossy());
    let mut out = Vec::new();
    // SAFETY: `w` is NUL-terminated and outlives the calls; buffers are sized as passed.
    unsafe {
        if RmRegisterResources(session, Some(&[PCWSTR(w.as_ptr())]), None, None).0 == 0 {
            let (mut needed, mut count, mut reasons) = (0u32, 0u32, 0u32);
            let mut st = RmGetList(session, &mut needed, &mut count, None, &mut reasons);
            let mut tries = 0;
            while st == ERROR_MORE_DATA && needed > 0 && tries < 3 {
                let mut buf = vec![RM_PROCESS_INFO::default(); needed as usize];
                count = needed;
                st = RmGetList(session, &mut needed, &mut count, Some(buf.as_mut_ptr()), &mut reasons);
                if st.0 == 0 {
                    for p in buf.iter().take(count as usize) {
                        out.push(format!("{} (PID {})", from_wide(&p.strAppName), p.Process.dwProcessId));
                    }
                }
                tries += 1;
            }
        }
        let _ = RmEndSession(session);
    }
    out
}

/// Holders per file (only files that are in use appear). At most [`MAX_FILES`] files are checked.
pub fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    let mut m = HashMap::new();
    for f in files.iter().take(MAX_FILES) {
        let h = holders(f);
        if !h.is_empty() {
            m.insert(f.clone(), h);
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
        let got = holders(&p);
        drop(f);
        let _ = std::fs::remove_file(&p);
        // Restart Manager can be unavailable (service disabled, session 0): then nothing is reported.
        if got.is_empty() {
            return;
        }
        let exe = std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| s.to_string_lossy().to_lowercase())).unwrap_or_default();
        assert!(got.iter().any(|g| g.to_lowercase().contains(&exe[..exe.len().min(8)])), "{got:?}");
    }
}
