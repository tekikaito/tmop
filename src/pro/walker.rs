//! Parallel directory walker: the scan method when the MFT reader is not available (not elevated,
//! not NTFS, network drive, or the MFT read failed).
//!
//! A pool of worker threads (one per CPU) shares a LIFO stack of pending folders behind one mutex.
//! A worker takes a few folders, enumerates them without the lock (64 KiB buffers via
//! `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` on Windows, `read_dir` elsewhere) and
//! then appends the results under the lock, so a child's node index is always above its
//! parent's. Reparse points (symlinks, junctions, mount points) are never followed.

use super::diskspace::{ext_of, FileRec, Names, Node, Publisher, ScanMethod, F_HIDDEN, NO_PARENT};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// A directory entry as seen by one enumeration.
struct Entry {
    name: String,
    dir: bool,
    size: u64,
    mtime: u64,
    flags: u8,
}

#[derive(Default)]
struct Shared {
    nodes: Vec<Node>,
    files: Vec<FileRec>,
    names: Names,
    exts: HashMap<String, (u64, u64)>,
    /// Pending folders (node index, path).
    stack: Vec<(u32, PathBuf)>,
    /// Workers currently holding a batch.
    active: usize,
    files_n: u64,
    dirs: u64,
    bytes: u64,
    denied: u64,
    current: String,
}

impl Shared {
    fn finished(&self) -> bool {
        self.stack.is_empty() && self.active == 0
    }
}

/// What one worker found in a batch of folders, appended to [`Shared`] in one go.
#[derive(Default)]
struct Found {
    dirs: Vec<(u32, String, PathBuf)>,
    files: Vec<(u32, Entry)>,
    denied: Vec<u32>,
    read: u64,
    exts: HashMap<String, (u64, u64)>,
    last: String,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn scan(root: PathBuf, cancel: &AtomicBool, publish: &Publisher) {
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 32);
    let sh = Mutex::new(Shared {
        nodes: vec![Node { name: root.display().to_string(), parent: NO_PARENT, ..Default::default() }],
        stack: vec![(0, root.clone())],
        ..Default::default()
    });
    let (work, idle) = (Condvar::new(), Condvar::new());
    publish.progress(|p| p.method = ScanMethod::Walker);
    std::thread::scope(|s| {
        let mut spawned = 0;
        for n in 0..workers {
            if std::thread::Builder::new().name(format!("tmop-walk-{n}")).spawn_scoped(s, || worker(&sh, &work, &idle, cancel, workers)).is_ok() {
                spawned += 1;
            }
        }
        if spawned == 0 {
            worker(&sh, &work, &idle, cancel, 1);
        }
        // This thread publishes progress and partial trees until the workers run dry.
        let mut g = lock(&sh);
        loop {
            let cancelled = cancel.load(Ordering::Relaxed);
            publish.progress(|p| {
                p.files = g.files_n;
                p.dirs = g.dirs;
                p.bytes = g.bytes;
                p.denied = g.denied;
                p.current = g.current.clone();
            });
            if g.finished() || cancelled {
                work.notify_all();
                break;
            }
            publish.maybe_publish(|| publish.build(&g.nodes, &g.files, &g.names, &g.exts, ScanMethod::Walker));
            g = idle.wait_timeout(g, Duration::from_millis(100)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
        }
    });
    let cancelled = cancel.load(Ordering::Relaxed);
    let g = sh.into_inner().unwrap_or_else(|e| e.into_inner());
    let (files_n, dirs, bytes, denied) = (g.files_n, g.dirs, g.bytes, g.denied);
    publish.progress(|p| {
        p.files = files_n;
        p.dirs = dirs;
        p.bytes = bytes;
        p.denied = denied;
    });
    let tree = publish.build_owned(g.nodes, g.files, g.names, &g.exts, ScanMethod::Walker);
    publish.finish(Some(tree), cancelled, ScanMethod::Walker);
}

fn worker(sh: &Mutex<Shared>, work: &Condvar, idle: &Condvar, cancel: &AtomicBool, workers: usize) {
    let mut buf = vec![0u64; 8192]; // 64 KiB, 8-byte aligned
    loop {
        let batch: Vec<(u32, PathBuf)> = {
            let mut g = lock(sh);
            loop {
                if cancel.load(Ordering::Relaxed) || g.finished() {
                    return;
                }
                if !g.stack.is_empty() {
                    let n = (g.stack.len() / workers).clamp(1, 8);
                    let at = g.stack.len() - n;
                    g.active += 1;
                    break g.stack.split_off(at);
                }
                g = work.wait_timeout(g, Duration::from_millis(100)).map(|r| r.0).unwrap_or_else(|e| e.into_inner().0);
            }
        };
        let mut found = Found::default();
        let mut entries = Vec::new();
        for (idx, dir) in &batch {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            entries.clear();
            if !enumerate(dir, &mut buf, &mut entries) {
                found.denied.push(*idx);
                continue;
            }
            found.read += 1;
            for e in entries.drain(..) {
                if e.dir {
                    let path = dir.join(&e.name);
                    found.dirs.push((*idx, e.name, path));
                } else {
                    let x = found.exts.entry(ext_of(&e.name)).or_default();
                    x.0 += e.size;
                    x.1 += 1;
                    found.files.push((*idx, e));
                }
            }
            found.last = dir.display().to_string();
        }
        let mut g = lock(sh);
        let pushed = !found.dirs.is_empty();
        append(&mut g, found);
        g.active -= 1;
        if g.finished() {
            idle.notify_all();
            work.notify_all();
        } else if pushed {
            work.notify_all();
        }
    }
}

/// Appends a batch under the lock. Child nodes are created after their parent exists.
fn append(g: &mut Shared, f: Found) {
    for i in f.denied {
        g.nodes[i as usize].denied = true;
        g.denied += 1;
    }
    g.dirs += f.read;
    for (parent, name, path) in f.dirs {
        let child = g.nodes.len() as u32;
        g.nodes.push(Node { name, parent, ..Default::default() });
        g.nodes[parent as usize].children.push(child);
        g.stack.push((child, path));
    }
    for (dir, e) in f.files {
        let (name_off, name_len) = g.names.push(&e.name);
        g.files.push(FileRec { size: e.size, mtime: e.mtime, dir, name_off, name_len, flags: e.flags });
        let n = &mut g.nodes[dir as usize];
        n.own += e.size;
        n.files += 1;
        g.files_n += 1;
        g.bytes += e.size;
    }
    for (k, v) in f.exts {
        let x = g.exts.entry(k).or_default();
        x.0 += v.0;
        x.1 += v.1;
    }
    if !f.last.is_empty() {
        g.current = f.last;
    }
}

/// Lists `dir` into `out`. False when the folder cannot be read.
fn enumerate(dir: &Path, buf: &mut [u64], out: &mut Vec<Entry>) -> bool {
    #[cfg(windows)]
    if enumerate_win(dir, buf, out) {
        return true;
    }
    let _ = buf;
    out.clear();
    enumerate_std(dir, out)
}

#[cfg(windows)]
fn reparse_flag(md: &std::fs::Metadata) -> u8 {
    use std::os::windows::fs::MetadataExt;
    if md.file_attributes() & 0x400 != 0 { super::diskspace::F_REPARSE } else { 0 }
}

#[cfg(not(windows))]
fn reparse_flag(_: &std::fs::Metadata) -> u8 {
    0
}

fn enumerate_std(dir: &Path, out: &mut Vec<Entry>) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            out.push(Entry { name, dir: true, size: 0, mtime: 0, flags: 0 });
        } else if let Ok(md) = entry.metadata() {
            let flags = if super::diskspace::is_hidden(&md, &name) { F_HIDDEN } else { 0 } | reparse_flag(&md);
            let mtime = md.modified().map(super::diskspace::system_to_filetime).unwrap_or(0);
            out.push(Entry { name, dir: false, size: md.len(), mtime, flags });
        }
    }
    true
}

#[cfg(windows)]
fn enumerate_win(dir: &Path, buf: &mut [u64], out: &mut Vec<Entry>) -> bool {
    use std::ffi::c_void;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandleEx, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO,
        FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    const DIRECTORY: u32 = 0x10;
    const REPARSE: u32 = 0x400;
    const SYMLINK_TAG: u32 = 0xA000_000C;
    const MOUNT_TAG: u32 = 0xA000_0003;
    let w = super::winutil::wide(&long_form(dir));
    // SAFETY: valid NUL-terminated path; the handle is closed below.
    let h = unsafe {
        CreateFileW(PCWSTR(w.as_ptr()), FILE_LIST_DIRECTORY.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, None)
    };
    let Ok(h) = h else { return false };
    let mut class = FileIdExtdDirectoryRestartInfo;
    let mut first = true;
    let ok = loop {
        // SAFETY: `buf` is writable memory of the stated size.
        let r = unsafe { GetFileInformationByHandleEx(h, class, buf.as_mut_ptr() as *mut c_void, (buf.len() * 8) as u32) };
        class = FileIdExtdDirectoryInfo;
        if r.is_err() {
            break !first; // ERROR_NO_MORE_FILES ends the listing; an error on the first call means unsupported
        }
        first = false;
        let base = buf.as_ptr() as *const u8;
        let name_at = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
        let mut off = 0usize;
        loop {
            // SAFETY: the kernel filled the buffer with a chain of 8-byte-aligned records.
            let e = unsafe { &*(base.add(off) as *const FILE_ID_EXTD_DIR_INFO) };
            let units = e.FileNameLength as usize / 2;
            // SAFETY: the name follows the fixed part inside the same record.
            let name16 = unsafe { std::slice::from_raw_parts(base.add(off + name_at) as *const u16, units) };
            let dot = name16 == [b'.' as u16] || name16 == [b'.' as u16, b'.' as u16];
            if !dot {
                let attrs = e.FileAttributes;
                let is_dir = attrs & DIRECTORY != 0;
                let reparse = attrs & REPARSE != 0;
                let skip = reparse && (is_dir || matches!(e.ReparsePointTag, SYMLINK_TAG | MOUNT_TAG));
                if !skip {
                    let mut flags = if attrs & 0x6 != 0 { F_HIDDEN } else { 0 };
                    if reparse {
                        flags |= super::diskspace::F_REPARSE;
                    }
                    out.push(Entry { name: String::from_utf16_lossy(name16), dir: is_dir, size: if is_dir { 0 } else { e.EndOfFile.max(0) as u64 }, mtime: e.LastWriteTime.max(0) as u64, flags });
                }
            }
            if e.NextEntryOffset == 0 {
                break;
            }
            off += e.NextEntryOffset as usize;
        }
    };
    // SAFETY: handle from CreateFileW.
    unsafe {
        let _ = CloseHandle(h);
    }
    ok
}

/// `\\?\`-prefixed form of long paths so the 260-character limit does not apply.
#[cfg(windows)]
fn long_form(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") || s.len() < 240 {
        s.to_string()
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{unc}")
    } else {
        format!(r"\\?\{s}")
    }
}

/// Runs the walker to completion on this thread (tests: deterministic method, no MFT).
#[cfg(test)]
pub(crate) fn scan_blocking(root: &Path) -> std::sync::Arc<super::diskspace::Tree> {
    let shared = Mutex::new(super::diskspace::ScanShared::default());
    let publish = Publisher::new(&shared, root);
    scan(root.to_path_buf(), &AtomicBool::new(false), &publish);
    let t = lock(&shared).tree.clone();
    t.expect("walker publishes a tree")
}

#[cfg(test)]
mod tests {
    use super::super::diskspace::{ScanShared, Scanner};
    use super::*;
    use std::time::Instant;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tmop-walk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(dir: &Path) -> std::sync::Arc<super::super::diskspace::Tree> {
        scan_blocking(dir)
    }

    #[test]
    fn child_after_parent_invariant() {
        let dir = temp("inv");
        fn make(d: &Path, depth: u32) {
            std::fs::write(d.join("f.txt"), b"x").unwrap();
            if depth > 0 {
                for i in 0..3 {
                    let c = d.join(format!("d{i}"));
                    std::fs::create_dir_all(&c).unwrap();
                    make(&c, depth - 1);
                }
            }
        }
        make(&dir, 4);
        let t = run(&dir);
        assert_eq!(t.nodes.len(), 1 + 3 + 9 + 27 + 81);
        assert_eq!(t.files.len(), t.nodes.len());
        for i in 1..t.nodes.len() {
            assert!((t.nodes[i].parent as usize) < i, "node {i}");
        }
        assert_eq!(t.nodes[0].files, t.nodes.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_stops_walker() {
        let dir = temp("cancel");
        for i in 0..2000 {
            std::fs::create_dir_all(dir.join(format!("d{i}"))).unwrap();
        }
        let mut s = Scanner::default();
        s.start(dir.clone());
        std::thread::sleep(Duration::from_millis(5));
        s.cancel();
        let t0 = Instant::now();
        while s.progress().running {
            assert!(t0.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(10));
        }
        let p = s.progress();
        // The scan may have finished before the cancel landed; both ends are valid, but never hang.
        assert!(p.cancelled || p.finished);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn denied_node_rolls_up() {
        // Publisher::build over hand-made state: a denied folder stays empty and flagged.
        let shared = Mutex::new(ScanShared::default());
        let p = Publisher::new(&shared, Path::new("r"));
        let mut nodes = vec![Node { name: "r".into(), parent: NO_PARENT, own: 5, files: 1, ..Default::default() }, Node { name: "locked".into(), parent: 0, denied: true, ..Default::default() }];
        nodes[0].children.push(1);
        let t = p.build(&nodes, &[], &Names::default(), &HashMap::new(), ScanMethod::Walker);
        assert_eq!(t.nodes[0].size, 5);
        assert!(t.nodes[1].denied && t.nodes[1].size == 0);
    }

    #[cfg(windows)]
    #[test]
    fn win_enumeration_matches_std() {
        let dir = temp("enum");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), vec![0u8; 123]).unwrap();
        std::fs::write(dir.join("{e4}{65e5}.bin"), vec![0u8; 5]).unwrap();
        let mut buf = vec![0u64; 8192];
        let (mut w, mut s) = (Vec::new(), Vec::new());
        assert!(enumerate_win(&dir, &mut buf, &mut w), "FileIdExtdDirectoryInfo must work on the temp volume");
        assert!(enumerate_std(&dir, &mut s));
        let key = |e: &Entry| (e.name.clone(), e.dir, e.size);
        w.sort_by_key(key);
        s.sort_by_key(key);
        assert_eq!(w.iter().map(key).collect::<Vec<_>>(), s.iter().map(key).collect::<Vec<_>>());
        assert_eq!(w.len(), 3);
        assert!(w.iter().all(|e| e.dir || e.mtime > 0));
        assert!(!enumerate_win(&dir.join("missing"), &mut buf, &mut Vec::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn walker_skips_symlinks_and_junctions() {
        let dir = temp("links");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/a.bin"), vec![0u8; 100]).unwrap();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::write(dir.join("outside/b.bin"), vec![0u8; 100]).unwrap();
        if std::os::windows::fs::symlink_dir(dir.join("outside"), dir.join("real/link")).is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            return; // no symlink privilege
        }
        let t = run(&dir.join("real"));
        assert_eq!(t.files.len(), 1, "link target must not be descended");
        assert_eq!(t.nodes.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
