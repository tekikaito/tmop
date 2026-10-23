//! Disk space: volume list and a cancelable folder-size scanner (records files ≥ 1 MiB)
//! with a squarified treemap layout. The scanner only reads directory metadata (it never opens files),
//! does not follow junctions or symbolic links, and publishes partial results while it
//! runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    /// `C:\`
    pub root: String,
    pub label: String,
    pub fs: String,
    /// "Fixed", "Removable", "Network", ...
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f32 / self.total as f32
        }
    }
}

/// Free bytes available to the caller on the volume holding `path`.
#[cfg(windows)]
pub fn free_space(path: &Path) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w = super::winutil::wide(&path.to_string_lossy());
    let mut free = 0u64;
    // SAFETY: valid path; outputs are plain integers.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(not(windows))]
pub fn free_space(_path: &Path) -> Option<u64> {
    None
}

#[cfg(windows)]
pub fn volumes() -> Vec<Volume> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let w = super::winutil::wide(&root);
        // SAFETY: valid root path strings; output buffers sized as passed.
        let ty = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
        let kind = match ty {
            2 => "Removable",
            3 => "Fixed",
            4 => "Network",
            5 => "Optical",
            6 => "RAM disk",
            _ => continue,
        };
        let (mut free, mut total) = (0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), Some(&mut total), None) }.is_err() {
            continue;
        }
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let _ = unsafe { GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut label), None, None, None, Some(&mut fs)) };
        out.push(Volume { root, label: super::winutil::from_wide(&label), fs: super::winutil::from_wide(&fs), kind, total, free });
    }
    out
}

#[cfg(not(windows))]
pub fn volumes() -> Vec<Volume> {
    Vec::new()
}

/// A file directly in one folder (see [`list_files`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DirFile {
    pub name: String,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    /// Hidden or system attribute set.
    pub hidden: bool,
}

/// Files directly in `dir` (no sub-folders, no symlinks), unsorted. The error is a short
/// user-facing sentence.
pub fn list_files(dir: &Path) -> Result<Vec<DirFile>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Can't read this folder (access denied)".to_string(),
        std::io::ErrorKind::NotFound => "This folder no longer exists".to_string(),
        _ => format!("Can't read this folder ({e})"),
    })?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let hidden = is_hidden(&md, &name);
        out.push(DirFile { name, size: md.len(), modified: mtime(&md), hidden });
    }
    Ok(out)
}

/// Modified time, `None` when missing or before 1970 (bogus stamps such as FILETIME 0).
fn mtime(md: &std::fs::Metadata) -> Option<std::time::SystemTime> {
    md.modified().ok().filter(|t| *t > std::time::UNIX_EPOCH)
}

#[cfg(windows)]
fn is_hidden(md: &std::fs::Metadata, _name: &str) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};
    md.file_attributes() & (FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0) != 0
}

#[cfg(not(windows))]
fn is_hidden(_md: &std::fs::Metadata, name: &str) -> bool {
    name.starts_with('.')
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// "just now", "5 min ago", "3 hrs ago", "3 days ago", "2 mo ago", "2 yrs ago".
pub fn relative_time(now: std::time::SystemTime, then: std::time::SystemTime) -> String {
    let s = now.duration_since(then).map(|d| d.as_secs()).unwrap_or(0); // future -> just now
    let unit = |n: u64, one: &str, many: &str| if n == 1 { format!("1 {one} ago") } else { format!("{n} {many} ago") };
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => unit(s / 3600, "hr", "hrs"),
        86_400..=2_591_999 => unit(s / 86_400, "day", "days"), // < 30 days
        2_592_000..=31_103_999 => format!("{} mo ago", s / 2_592_000), // < 12 months
        _ => unit(s / 31_104_000, "yr", "yrs"), // 12 x 30 days
    }
}

/// `YYYY-MM-DD HH:MM` in local time (empty if the conversion fails).
#[cfg(windows)]
pub fn local_stamp(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let Ok(d) = t.duration_since(UNIX_EPOCH) else { return String::new() }; // pre-1970
    let ticks = (d.as_nanos() / 100) as u64 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut loc = SYSTEMTIME::default();
    // SAFETY: plain out-structs.
    let r = unsafe { FileTimeToSystemTime(&ft, &mut utc).and_then(|_| SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc)) };
    if r.is_err() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", loc.wYear, loc.wMonth, loc.wDay, loc.wHour, loc.wMinute)
}

/// `YYYY-MM-DD HH:MM` (UTC off Windows).
#[cfg(not(windows))]
pub fn local_stamp(t: std::time::SystemTime) -> String {
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() }; // pre-1970
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

pub const NO_PARENT: u32 = u32::MAX;

/// Files at or above this size get a record in `Tree::files`.
pub const MIN_RECORD: u64 = 1 << 20;

/// A file >= `MIN_RECORD` bytes found by the scanner.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRec {
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    /// Index of the containing folder in `Tree::nodes`.
    pub dir: u32,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub parent: u32,
    /// Total bytes below this folder (after roll-up).
    pub size: u64,
    /// Bytes of the files directly in this folder.
    pub own: u64,
    /// Files below this folder (after roll-up).
    pub files: u64,
    pub children: Vec<u32>,
    pub denied: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    /// Every file >= MIN_RECORD, scan order.
    pub files: Vec<FileRec>,
    /// (extension, bytes, files), biggest first.
    pub extensions: Vec<(String, u64, u64)>,
}

impl Tree {
    pub fn path_of(&self, idx: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut i = idx;
        while i != NO_PARENT && (i as usize) < self.nodes.len() {
            let n = &self.nodes[i as usize];
            if n.parent == NO_PARENT {
                break;
            }
            parts.push(n.name.as_str());
            i = n.parent;
        }
        let mut p = self.root.clone();
        for part in parts.iter().rev() {
            p.push(part);
        }
        p
    }

    /// Chain of node indices from the root to `idx`.
    pub fn ancestry(&self, idx: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut i = idx;
        while i != NO_PARENT && (i as usize) < self.nodes.len() {
            v.push(i);
            i = self.nodes[i as usize].parent;
        }
        v.reverse();
        v
    }

    /// Full path of a file record.
    pub fn file_path(&self, f: &FileRec) -> PathBuf {
        self.path_of(f.dir).join(&f.name)
    }

    /// `mask[i]` is true for `root` and every folder below it. O(nodes): children are always
    /// created after their parent, so one forward pass over `root..` suffices.
    pub fn subtree_mask(&self, root: u32) -> Vec<bool> {
        let mut m = vec![false; self.nodes.len()];
        if (root as usize) < m.len() {
            m[root as usize] = true;
        }
        for i in (root as usize + 1)..self.nodes.len() {
            let p = self.nodes[i].parent;
            m[i] = p != NO_PARENT && m[p as usize];
        }
        m
    }

    /// Indices into `files` of the `limit` biggest files in `root`'s subtree, biggest first.
    pub fn largest_under(&self, root: u32, limit: usize) -> Vec<usize> {
        let m = self.subtree_mask(root);
        let mut v: Vec<usize> = (0..self.files.len()).filter(|i| m[self.files[*i].dir as usize]).collect();
        if v.len() > limit {
            v.select_nth_unstable_by_key(limit, |i| std::cmp::Reverse(self.files[*i].size));
            v.truncate(limit);
        }
        v.sort_by_key(|i| std::cmp::Reverse(self.files[*i].size));
        v
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanProgress {
    pub running: bool,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub denied: u64,
    pub current: String,
    pub elapsed: Duration,
    pub cancelled: bool,
    pub finished: bool,
}

#[derive(Default)]
struct ScanShared {
    progress: ScanProgress,
    tree: Option<Arc<Tree>>,
    generation: u64,
}

#[derive(Default)]
pub struct Scanner {
    shared: Arc<Mutex<ScanShared>>,
    cancel: Arc<AtomicBool>,
}

impl Scanner {
    pub fn start(&mut self, root: PathBuf) {
        if self.progress().running {
            return;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        let shared = self.shared.clone();
        if let Ok(mut g) = shared.lock() {
            g.progress = ScanProgress { running: true, current: root.display().to_string(), ..Default::default() };
            g.tree = None;
            g.generation += 1;
        }
        let cancel = self.cancel.clone();
        let _ = std::thread::Builder::new().name("tmop-diskspace".into()).spawn(move || scan(root, &shared, &cancel));
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> ScanProgress {
        self.shared.lock().map(|g| g.progress.clone()).unwrap_or_default()
    }

    /// Latest published tree and its generation (changes on every publish).
    pub fn tree(&self) -> (Option<Arc<Tree>>, u64) {
        self.shared.lock().map(|g| (g.tree.clone(), g.generation)).unwrap_or((None, 0))
    }
}

fn finish(nodes: &[Node], root: &Path, files: &[FileRec], exts: &HashMap<String, (u64, u64)>) -> Tree {
    let mut nodes = nodes.to_vec();
    for n in nodes.iter_mut() {
        n.size = n.own;
    }
    // Children are always created after their parent, so a reverse pass rolls sizes up.
    for i in (1..nodes.len()).rev() {
        let (size, files, parent) = (nodes[i].size, nodes[i].files, nodes[i].parent);
        if parent != NO_PARENT {
            nodes[parent as usize].size += size;
            nodes[parent as usize].files += files;
        }
    }
    let sizes: Vec<u64> = nodes.iter().map(|n| n.size).collect();
    for n in nodes.iter_mut() {
        n.children.sort_by(|a, b| sizes[*b as usize].cmp(&sizes[*a as usize]));
    }
    let mut ext: Vec<(String, u64, u64)> = exts.iter().map(|(k, v)| (k.clone(), v.0, v.1)).collect();
    ext.sort_by_key(|e| std::cmp::Reverse(e.1));
    ext.truncate(40);
    Tree { root: root.to_path_buf(), nodes, files: files.to_vec(), extensions: ext }
}

fn scan(root: PathBuf, shared: &Mutex<ScanShared>, cancel: &AtomicBool) {
    let start = Instant::now();
    let mut nodes: Vec<Node> = vec![Node { name: root.display().to_string(), parent: NO_PARENT, ..Default::default() }];
    let mut stack: Vec<(u32, PathBuf)> = vec![(0, root.clone())];
    let mut files: Vec<FileRec> = Vec::new();
    let mut exts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut p = ScanProgress { running: true, ..Default::default() };
    let mut last_publish = Instant::now();
    let mut last_progress = Instant::now();
    while let Some((idx, dir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            p.cancelled = true;
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                nodes[idx as usize].denied = true;
                p.denied += 1;
                continue;
            }
        };
        p.dirs += 1;
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                let child = nodes.len() as u32;
                nodes.push(Node { name: entry.file_name().to_string_lossy().to_string(), parent: idx, ..Default::default() });
                nodes[idx as usize].children.push(child);
                stack.push((child, entry.path()));
            } else if let Ok(md) = entry.metadata() {
                let len = md.len();
                let n = &mut nodes[idx as usize];
                n.own += len;
                n.files += 1;
                p.files += 1;
                p.bytes += len;
                let name = entry.file_name().to_string_lossy().to_string();
                let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| e.len() <= 8).unwrap_or_else(|| "(none)".into());
                let e = exts.entry(ext).or_default();
                e.0 += len;
                e.1 += 1;
                if len >= MIN_RECORD {
                    files.push(FileRec { size: len, modified: mtime(&md), dir: idx, name: name.clone() });
                }
            }
        }
        if last_progress.elapsed() > Duration::from_millis(100) {
            last_progress = Instant::now();
            p.current = dir.display().to_string();
            p.elapsed = start.elapsed();
            if let Ok(mut g) = shared.lock() {
                g.progress = p.clone();
            }
        }
        if last_publish.elapsed() > Duration::from_millis(1500) {
            let t = Arc::new(finish(&nodes, &root, &files, &exts));
            last_publish = Instant::now();
            if let Ok(mut g) = shared.lock() {
                g.tree = Some(t);
                g.generation += 1;
            }
        }
    }
    let t = Arc::new(finish(&nodes, &root, &files, &exts));
    p.running = false;
    p.finished = !p.cancelled;
    p.elapsed = start.elapsed();
    p.current = root.display().to_string();
    if let Ok(mut g) = shared.lock() {
        g.progress = p;
        g.tree = Some(t);
        g.generation += 1;
    }
}

// ---------------------------------------------------------------------------
// Treemap
// ---------------------------------------------------------------------------

/// Squarified treemap: lays out `sizes` (sorted biggest first) in the rectangle
/// (x, y, w, h). Returns one rectangle per size, in input order.
pub fn squarify(sizes: &[f64], x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32, f32, f32)> {
    let total: f64 = sizes.iter().sum();
    let mut out = vec![(x, y, 0.0, 0.0); sizes.len()];
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return out;
    }
    let scale = (w as f64 * h as f64) / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    let (mut rx, mut ry, mut rw, mut rh) = (x as f64, y as f64, w as f64, h as f64);
    let mut i = 0;
    while i < areas.len() {
        let short = rw.min(rh);
        if short <= 0.0 {
            break;
        }
        // Grow the row while the worst aspect ratio improves.
        let mut j = i + 1;
        let worst = |row: &[f64]| -> f64 {
            let s: f64 = row.iter().sum();
            let (mx, mn) = row.iter().fold((0.0f64, f64::MAX), |(a, b), v| (a.max(*v), b.min(*v)));
            if s <= 0.0 || mn <= 0.0 {
                return f64::MAX;
            }
            ((short * short * mx) / (s * s)).max((s * s) / (short * short * mn))
        };
        while j < areas.len() && worst(&areas[i..j + 1]) <= worst(&areas[i..j]) {
            j += 1;
        }
        let row = &areas[i..j];
        let sum: f64 = row.iter().sum();
        if rw >= rh {
            // Column on the left.
            let cw = if rh > 0.0 { sum / rh } else { 0.0 };
            let mut yy = ry;
            for (k, a) in row.iter().enumerate() {
                let hh = if cw > 0.0 { a / cw } else { 0.0 };
                out[i + k] = (rx as f32, yy as f32, cw as f32, hh as f32);
                yy += hh;
            }
            rx += cw;
            rw -= cw;
        } else {
            // Row on top.
            let rh2 = if rw > 0.0 { sum / rw } else { 0.0 };
            let mut xx = rx;
            for (k, a) in row.iter().enumerate() {
                let ww = if rh2 > 0.0 { a / rh2 } else { 0.0 };
                out[i + k] = (xx as f32, ry as f32, ww as f32, rh2 as f32);
                xx += ww;
            }
            ry += rh2;
            rh -= rh2;
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treemap_covers_area() {
        let sizes = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rects = squarify(&sizes, 0.0, 0.0, 600.0, 400.0);
        let area: f32 = rects.iter().map(|r| r.2 * r.3).sum();
        assert!((area - 240_000.0).abs() < 1.0, "area {area}");
        for (r, s) in rects.iter().zip(sizes) {
            assert!((r.2 * r.3 - (s as f32 / 24.0) * 240_000.0).abs() < 1.0);
            assert!(r.0 >= -0.01 && r.1 >= -0.01 && r.0 + r.2 <= 600.01 && r.1 + r.3 <= 400.01);
        }
    }

    #[test]
    fn scans_a_folder_tree() {
        let dir = std::env::temp_dir().join(format!("tmop-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/big.bin"), vec![0u8; 1 << 20]).unwrap();
        std::fs::write(dir.join("a/small.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("top.txt"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("top.bin"), vec![0u8; (1 << 20) + 512]).unwrap();
        let mut s = Scanner::default();
        s.start(dir.clone());
        let t0 = Instant::now();
        loop {
            let p = s.progress();
            if !p.running && p.finished {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let (tree, _) = s.tree();
        let tree = tree.unwrap();
        assert_eq!(tree.nodes[0].size, 2 * (1 << 20) + 622);
        assert_eq!(tree.nodes[0].files, 4);
        let a = tree.nodes[0].children[0];
        assert_eq!(tree.nodes[a as usize].name, "a");
        assert_eq!(tree.nodes[a as usize].size, (1 << 20) + 100);
        assert_eq!(tree.path_of(a), dir.join("a"));
        assert_eq!(tree.files.len(), 2);
        let now = std::time::SystemTime::now();
        for f in &tree.files {
            let m = f.modified.expect("modified time");
            assert!(now.duration_since(m).unwrap_or_default() < Duration::from_secs(3600));
        }
        let all = tree.largest_under(0, 100);
        assert_eq!(all.len(), 2);
        assert_eq!(tree.files[all[0]].name, "top.bin");
        let under_a = tree.largest_under(a, 100);
        assert_eq!(under_a.len(), 1);
        assert_eq!(tree.files[under_a[0]].name, "big.bin");
        assert_eq!(tree.file_path(&tree.files[under_a[0]]), dir.join("a/b/big.bin"));
        assert_eq!(tree.largest_under(0, 1).len(), 1);
        assert!(tree.subtree_mask(a)[a as usize] && !tree.subtree_mask(a)[0]);
        assert_eq!(tree.extensions[0].0, "bin");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_one_folder() {
        let dir = std::env::temp_dir().join(format!("tmop-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("x.txt"), b"abc").unwrap();
        std::fs::write(dir.join("y.bin"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("sub/z.txt"), b"z").unwrap();
        let mut v = list_files(&dir).unwrap();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].name.as_str(), v[0].size), ("x.txt", 3));
        assert_eq!((v[1].name.as_str(), v[1].size), ("y.bin", 10));
        assert!(v.iter().all(|f| f.modified.is_some() && !f.hidden));
        let e = list_files(&dir.join("missing")).unwrap_err();
        assert!(e.contains("no longer exists"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_dates() {
        use std::time::UNIX_EPOCH;
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let at = |s: u64| t0 - Duration::from_secs(s);
        assert_eq!(relative_time(t0, t0), "just now");
        assert_eq!(relative_time(t0, at(59)), "just now");
        assert_eq!(relative_time(t0, at(60)), "1 min ago");
        assert_eq!(relative_time(t0, at(5 * 60)), "5 min ago");
        assert_eq!(relative_time(t0, at(3600)), "1 hr ago");
        assert_eq!(relative_time(t0, at(3 * 3600)), "3 hrs ago");
        assert_eq!(relative_time(t0, at(86_400)), "1 day ago");
        assert_eq!(relative_time(t0, at(3 * 86_400)), "3 days ago");
        assert_eq!(relative_time(t0, at(45 * 86_400)), "1 mo ago");
        assert_eq!(relative_time(t0, at(359 * 86_400)), "11 mo ago");
        assert_eq!(relative_time(t0, at(360 * 86_400)), "1 yr ago");
        assert_eq!(relative_time(t0, at(2 * 365 * 86_400)), "2 yrs ago");
        assert_eq!(relative_time(at(60), t0), "just now");
    }

    #[test]
    fn local_stamp_shape() {
        let s = local_stamp(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(s.len(), 16);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[13..14], ":");
        assert!(s.starts_with("2023"));
    }

    #[cfg(windows)]
    #[test]
    fn lists_system_volume() {
        let v = volumes();
        assert!(v.iter().any(|v| v.total > 0 && v.kind == "Fixed"));
        assert!(free_space(Path::new("C:\\")).is_some());
    }
}
