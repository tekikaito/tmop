//! Which processes hold a file open, for the delete dialogs' "in use by" warning. Read-only.
//!
//! Windows asks the Restart Manager ([`super::restartmgr`]). Linux reads `/proc/<pid>/fd` (open
//! descriptors) and `/proc/<pid>/maps` (mapped files such as libraries and databases); macOS asks
//! libproc for every process's vnode descriptors. Without root only the user's own processes are
//! visible, which is enough for a warning. Holders read "name (PID n)".

use std::collections::HashMap;
use std::path::PathBuf;

/// Most files checked per call.
pub const MAX_FILES: usize = 64;

/// Holders per file (only files that are in use appear). At most [`MAX_FILES`] files are checked.
#[cfg(windows)]
pub fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    super::restartmgr::in_use(files)
}

/// One file-backed line of `/proc/<pid>/maps`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    /// (major, minor) of the file's device.
    pub dev: (u32, u32),
    pub ino: u64,
    pub path: String,
}

/// The file-backed mappings in `/proc/<pid>/maps` text (`addr perms offset maj:min inode path`).
/// Anonymous, pseudo (`[heap]`) and deleted mappings are left out; the path may hold spaces.
pub fn parse_maps(text: &str) -> Vec<MapEntry> {
    let mut out: Vec<MapEntry> = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        let mut f = [""; 5];
        for slot in f.iter_mut() {
            let t = rest.trim_start();
            let end = t.find(char::is_whitespace).unwrap_or(t.len());
            *slot = &t[..end];
            rest = &t[end..];
        }
        let path = rest.trim_start();
        if !path.starts_with('/') || path.ends_with(" (deleted)") {
            continue;
        }
        let Some((maj, min)) = f[3].split_once(':') else { continue };
        let (Ok(maj), Ok(min), Ok(ino)) = (u32::from_str_radix(maj, 16), u32::from_str_radix(min, 16), f[4].parse::<u64>()) else { continue };
        let e = MapEntry { dev: (maj, min), ino, path: path.to_string() };
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

/// (major, minor) of a Linux `st_dev` (glibc's encoding: 12-bit major and 20-bit minor split).
pub fn dev_numbers(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

/// A file to look for: as given, as resolved, and its identity.
#[cfg(unix)]
struct Wanted {
    path: PathBuf,
    real: String,
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn wanted(files: &[PathBuf]) -> Vec<Wanted> {
    use std::os::unix::fs::MetadataExt;
    files
        .iter()
        .take(MAX_FILES)
        .filter_map(|f| {
            let real = std::fs::canonicalize(f).ok()?;
            let md = std::fs::metadata(&real).ok().filter(|m| m.is_file())?;
            Some(Wanted { path: f.clone(), real: real.to_string_lossy().into_owned(), dev: md.dev(), ino: md.ino() })
        })
        .collect()
}

#[cfg(target_os = "linux")]
pub fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    let want = wanted(files);
    let mut out: HashMap<PathBuf, Vec<String>> = HashMap::new();
    if want.is_empty() {
        return out;
    }
    let Ok(procs) = std::fs::read_dir("/proc") else { return out };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let dir = p.path();
        let mut hit = vec![false; want.len()];
        // Only the link text: following a descriptor could hang on a dead network mount.
        if let Ok(fds) = std::fs::read_dir(dir.join("fd")) {
            for fd in fds.flatten() {
                if let Ok(l) = std::fs::read_link(fd.path()) {
                    let l = l.to_string_lossy();
                    for (i, w) in want.iter().enumerate() {
                        hit[i] |= l == w.real;
                    }
                }
            }
        }
        if let Ok(maps) = std::fs::read_to_string(dir.join("maps")) {
            for m in parse_maps(&maps) {
                for (i, w) in want.iter().enumerate() {
                    hit[i] |= m.path == w.real || (m.ino == w.ino && m.dev == dev_numbers(w.dev));
                }
            }
        }
        if hit.iter().any(|h| *h) {
            let name = std::fs::read_to_string(dir.join("comm")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "?".into());
            for (_, w) in want.iter().enumerate().filter(|(i, _)| hit[*i]) {
                out.entry(w.path.clone()).or_default().push(format!("{name} (PID {pid})"));
            }
        }
    }
    out
}

/// `struct vnode_fdinfowithpath` (`sys/proc_info.h`), which the libc crate does not declare.
#[cfg(target_os = "macos")]
#[repr(C)]
struct VnodeFdInfoWithPath {
    /// `struct proc_fileinfo`: open flags, status, offset, type, guard flags.
    pfi: [u64; 3],
    pvip: libc::vnode_info_path,
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<VnodeFdInfoWithPath>() == 1200);

/// `PROC_PIDFDVNODEPATHINFO`
#[cfg(target_os = "macos")]
const PIDFD_VNODE_PATH: libc::c_int = 2;

#[cfg(target_os = "macos")]
pub fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    use libc::c_int;
    use std::mem::size_of;
    let want = wanted(files);
    let mut out: HashMap<PathBuf, Vec<String>> = HashMap::new();
    if want.is_empty() {
        return out;
    }
    // SAFETY: a null buffer asks for the number of pids.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return out;
    }
    let mut pids = vec![0i32; n as usize + 64];
    // SAFETY: `pids` is valid for the byte count passed.
    let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as c_int) };
    pids.truncate(usize::try_from(got).unwrap_or(0));
    let mut fds: Vec<libc::proc_fdinfo> = Vec::new();
    for pid in pids.into_iter().filter(|p| *p > 0) {
        // SAFETY: a null buffer asks for the size of the descriptor list.
        let bytes = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            continue;
        }
        let slots = bytes as usize / size_of::<libc::proc_fdinfo>() + 16;
        fds.clear();
        fds.resize(slots, libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 });
        // SAFETY: `fds` holds `slots` proc_fdinfo records; the kernel writes at most the bytes passed.
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), (slots * size_of::<libc::proc_fdinfo>()) as c_int) };
        let count = usize::try_from(got).unwrap_or(0) / size_of::<libc::proc_fdinfo>();
        let mut hit = vec![false; want.len()];
        for fd in fds.iter().take(count).filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32) {
            // SAFETY: all-zero bytes are a valid VnodeFdInfoWithPath; the kernel writes at most its size.
            let mut vi: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
            let r = unsafe { libc::proc_pidfdinfo(pid, fd.proc_fd, PIDFD_VNODE_PATH, (&mut vi as *mut VnodeFdInfoWithPath).cast(), size_of::<VnodeFdInfoWithPath>() as c_int) };
            if r as usize != size_of::<VnodeFdInfoWithPath>() {
                continue;
            }
            let st = &vi.pvip.vip_vi.vi_stat;
            let path: Vec<u8> = vi.pvip.vip_path.iter().flatten().take_while(|c| **c != 0).map(|c| *c as u8).collect();
            for (i, w) in want.iter().enumerate() {
                hit[i] |= (st.vst_ino == w.ino && st.vst_dev == w.dev as u32) || path == w.real.as_bytes();
            }
        }
        if hit.iter().any(|h| *h) {
            let mut name = [0u8; 256];
            // SAFETY: the buffer is valid for its length; proc_name writes a NUL-terminated name.
            let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
            let name = if len > 0 { String::from_utf8_lossy(&name[..len as usize]).into_owned() } else { "?".into() };
            for (_, w) in want.iter().enumerate().filter(|(i, _)| hit[*i]) {
                out.entry(w.path.clone()).or_default().push(format!("{name} (PID {pid})"));
            }
        }
    }
    out
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn in_use(_: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lines() {
        let text = "\
55d0c0a00000-55d0c0a02000 r--p 00000000 103:02 1311000                   /usr/bin/cat
7f1e2c000000-7f1e2c021000 rw-p 00000000 00:00 0
7f1e2d000000-7f1e2d1c8000 r-xp 00028000 fd:01 2752532                    /usr/lib/x86_64-linux-gnu/libc.so.6
7f1e2e000000-7f1e2e001000 r--p 00000000 08:11 99                         /home/me/My Data/db.sqlite
7f1e2f000000-7f1e2f001000 r--p 00000000 08:11 98                         /home/me/old.db (deleted)
7ffd5a1f0000-7ffd5a211000 rw-p 00000000 00:00 0                          [stack]
7f1e2d000000-7f1e2d1c8000 r-xp 00028000 fd:01 2752532                    /usr/lib/x86_64-linux-gnu/libc.so.6
garbage
";
        let m = parse_maps(text);
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0], MapEntry { dev: (0x103, 2), ino: 1311000, path: "/usr/bin/cat".into() });
        assert_eq!(m[1].dev, (0xfd, 1));
        assert_eq!(m[2].path, "/home/me/My Data/db.sqlite");
    }

    #[test]
    fn device_numbers() {
        // makedev(259, 2) and makedev(8, 300) in glibc's encoding.
        assert_eq!(dev_numbers(0x10302), (259, 2));
        let dev = (300u64 & 0xff) | ((8u64 & 0xfff) << 8) | ((300u64 & !0xff) << 12);
        assert_eq!(dev_numbers(dev), (8, 300));
        let big = ((0x1234u64 & !0xfff) << 32) | ((0x1234u64 & 0xfff) << 8);
        assert_eq!(dev_numbers(big), (0x1234, 0));
    }

    /// This test holds a temp file open and must find itself among its holders.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sees_own_open_file() {
        let p = std::env::temp_dir().join(format!("tmop-inuse-{}.tmp", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        let got = in_use(std::slice::from_ref(&p));
        drop(f);
        let free = in_use(std::slice::from_ref(&p));
        let _ = std::fs::remove_file(&p);
        let me = format!("(PID {})", std::process::id());
        assert!(got.get(&p).is_some_and(|h| h.iter().any(|x| x.ends_with(&me))), "{got:?}");
        assert!(free.get(&p).is_none_or(|h| !h.iter().any(|x| x.ends_with(&me))), "{free:?}");
    }
}
