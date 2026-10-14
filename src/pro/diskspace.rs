//! Disk space: volume list and a cancelable folder-size scanner with a squarified
//! treemap layout. The scanner only reads directory metadata (it never opens files),
//! does not follow junctions or symbolic links, and publishes partial results while it
//! runs.

use std::collections::{BinaryHeap, HashMap};
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

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

pub const NO_PARENT: u32 = u32::MAX;

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
    /// Largest files, biggest first.
    pub largest: Vec<(u64, String)>,
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

fn finish(nodes: &[Node], root: &Path, largest: &BinaryHeap<std::cmp::Reverse<(u64, String)>>, exts: &HashMap<String, (u64, u64)>) -> Tree {
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
    let mut big: Vec<(u64, String)> = largest.iter().map(|r| r.0.clone()).collect();
    big.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut ext: Vec<(String, u64, u64)> = exts.iter().map(|(k, v)| (k.clone(), v.0, v.1)).collect();
    ext.sort_by_key(|e| std::cmp::Reverse(e.1));
    ext.truncate(40);
    Tree { root: root.to_path_buf(), nodes, largest: big, extensions: ext }
}

fn scan(root: PathBuf, shared: &Mutex<ScanShared>, cancel: &AtomicBool) {
    let start = Instant::now();
    let mut nodes: Vec<Node> = vec![Node { name: root.display().to_string(), parent: NO_PARENT, ..Default::default() }];
    let mut stack: Vec<(u32, PathBuf)> = vec![(0, root.clone())];
    let mut largest: BinaryHeap<std::cmp::Reverse<(u64, String)>> = BinaryHeap::new();
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
                if largest.len() < 100 || largest.peek().map(|m| len > m.0 .0).unwrap_or(true) {
                    largest.push(std::cmp::Reverse((len, entry.path().display().to_string())));
                    if largest.len() > 100 {
                        largest.pop();
                    }
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
            let t = Arc::new(finish(&nodes, &root, &largest, &exts));
            last_publish = Instant::now();
            if let Ok(mut g) = shared.lock() {
                g.tree = Some(t);
                g.generation += 1;
            }
        }
    }
    let t = Arc::new(finish(&nodes, &root, &largest, &exts));
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
        std::fs::write(dir.join("a/b/big.bin"), vec![0u8; 5000]).unwrap();
        std::fs::write(dir.join("a/small.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("top.txt"), vec![0u8; 10]).unwrap();
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
        assert_eq!(tree.nodes[0].size, 5110);
        assert_eq!(tree.nodes[0].files, 3);
        let a = tree.nodes[0].children[0];
        assert_eq!(tree.nodes[a as usize].name, "a");
        assert_eq!(tree.nodes[a as usize].size, 5100);
        assert_eq!(tree.path_of(a), dir.join("a"));
        assert_eq!(tree.largest[0].0, 5000);
        assert_eq!(tree.extensions[0].0, "bin");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn lists_system_volume() {
        let v = volumes();
        assert!(v.iter().any(|v| v.total > 0 && v.kind == "Fixed"));
        assert!(free_space(Path::new("C:\\")).is_some());
    }
}
