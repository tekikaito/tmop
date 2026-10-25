//! Disk Space "Clean up": the categories, where their files live and how they are measured.
//! Nothing is deleted here; the items go through `diskdelete::assess` / `diskdelete::run`.
//!
//! Fixed locations (temp, caches, dumps, ...) are measured straight from disk, folders with a
//! bounded walk. Old Downloads and developer artifacts come from the last scan tree.

use super::diskdelete::{Mode, SystemPaths};
use super::diskspace::{filetime_to_system, Tree, NO_PARENT};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    UserTemp,
    WindowsTemp,
    BrowserCaches,
    CrashDumps,
    WerReports,
    ThumbnailCache,
    UpdateDownloads,
    DeliveryOptimization,
    RecycleBin,
    OldDownloads,
    DevArtifacts,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::UserTemp,
        Category::WindowsTemp,
        Category::BrowserCaches,
        Category::CrashDumps,
        Category::WerReports,
        Category::ThumbnailCache,
        Category::UpdateDownloads,
        Category::DeliveryOptimization,
        Category::RecycleBin,
        Category::OldDownloads,
        Category::DevArtifacts,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::UserTemp => "User temp files older than 24 h",
            Category::WindowsTemp => "Windows temp",
            Category::BrowserCaches => "Browser caches",
            Category::CrashDumps => "Crash dumps",
            Category::WerReports => "Windows Error Reporting",
            Category::ThumbnailCache => "Thumbnail cache",
            Category::UpdateDownloads => "Windows Update downloads",
            Category::DeliveryOptimization => "Delivery Optimization cache",
            Category::RecycleBin if cfg!(windows) => "Empty Recycle Bin",
            Category::RecycleBin => "Empty Trash",
            Category::OldDownloads => "Downloads older than 90 days",
            Category::DevArtifacts => "Developer artifacts",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Category::UserTemp => "Files and folders in your temp folder that were not touched for a day",
            Category::WindowsTemp => "The system temp folder",
            Category::BrowserCaches => "Edge, Chrome and Firefox cache folders only; never profiles, passwords or history",
            Category::CrashDumps => "Crash dumps, minidumps and MEMORY.DMP",
            Category::WerReports => "Archived and queued error reports",
            Category::ThumbnailCache => "Explorer thumbnail and icon caches; files Explorer is using are skipped",
            Category::UpdateDownloads => "Downloaded update files Windows can fetch again; locked files are skipped",
            Category::DeliveryOptimization => "Update files cached for sharing with other PCs; Windows fetches them again",
            Category::RecycleBin if cfg!(windows) => "Permanently removes everything in the Recycle Bin (handled by Windows)",
            Category::RecycleBin => "Permanently removes everything in your Trash; Trash folders on other drives are left alone",
            Category::OldDownloads if cfg!(windows) => "Goes to the Recycle Bin; needs a scan that covers your Downloads folder",
            Category::OldDownloads => "Goes to the Trash; needs a scan that covers your Downloads folder",
            Category::DevArtifacts if cfg!(windows) => "node_modules, Rust target, Gradle/npm/pip/Yarn caches; goes to the Recycle Bin; needs a scan",
            Category::DevArtifacts => "node_modules, Rust target, Gradle/npm/pip/Yarn caches; goes to the Trash; needs a scan",
        }
    }

    /// Offered on this OS. Emptying the bin is not on macOS (see [`super::trash::can_empty`]).
    pub fn supported(self) -> bool {
        self != Category::RecycleBin || super::trash::can_empty()
    }

    pub fn preselected(self) -> bool {
        !matches!(self, Category::RecycleBin | Category::OldDownloads | Category::DevArtifacts)
    }

    /// Whole category needs administrator rights (CrashDumps and WerReports have admin-only items, see `CleanItem`).
    pub fn needs_admin(self) -> bool {
        matches!(self, Category::WindowsTemp | Category::UpdateDownloads | Category::DeliveryOptimization)
    }

    /// Measured from the last scan instead of from disk.
    pub fn needs_scan(self) -> bool {
        matches!(self, Category::OldDownloads | Category::DevArtifacts)
    }

    pub fn mode(self) -> Mode {
        if self.needs_scan() {
            Mode::Recycle
        } else {
            Mode::Permanent
        }
    }
}

#[derive(Clone, Debug)]
pub struct CleanItem {
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub needs_admin: bool,
    /// Scan tree node (folders) / index into `Tree::files` (files), for the in-place update.
    pub node: Option<u32>,
    pub file: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Measured {
    pub cat: Category,
    /// Biggest first.
    pub items: Vec<CleanItem>,
    pub bytes: u64,
    pub count: u64,
    pub error: Option<String>,
}

impl Measured {
    fn new(cat: Category) -> Self {
        Measured { cat, items: Vec::new(), bytes: 0, count: 0, error: None }
    }

    fn with_items(cat: Category, mut items: Vec<CleanItem>) -> Self {
        items.sort_by_key(|i| std::cmp::Reverse(i.size));
        Measured { cat, bytes: items.iter().map(|i| i.size).sum(), count: items.len() as u64, items, error: None }
    }

    /// Drops the items at the paths in `gone` (deleted meanwhile).
    pub fn forget(&mut self, gone: &std::collections::HashSet<&Path>) {
        self.items.retain(|i| !gone.contains(i.path.as_path()));
        self.bytes = self.items.iter().map(|i| i.size).sum();
        self.count = self.items.len() as u64;
    }

    fn failed(cat: Category, why: &str) -> Self {
        Measured { error: Some(why.to_string()), ..Measured::new(cat) }
    }
}

// ---------------------------------------------------------------- pure rules

/// Modified at least `days` days before `now`. Unknown times are never old.
pub fn is_old(modified: Option<SystemTime>, now: SystemTime, days: u64) -> bool {
    older_than(modified, now, Duration::from_secs(days * 86_400))
}

fn older_than(modified: Option<SystemTime>, now: SystemTime, age: Duration) -> bool {
    modified.is_some_and(|m| now.duration_since(m).is_ok_and(|d| d >= age))
}

/// Existing cache folders of Edge, Chrome (`Cache`, `Code Cache`, `GPUCache` of the Default,
/// `Profile N` and Guest profiles) and Firefox (`cache2` of every profile).
pub fn browser_cache_dirs(local_appdata: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for ud in [local_appdata.join("Microsoft").join("Edge").join("User Data"), local_appdata.join("Google").join("Chrome").join("User Data")] {
        let Ok(rd) = std::fs::read_dir(&ud) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !(name == "default" || name == "guest profile" || name.starts_with("profile ")) || !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            for c in ["Cache", "Code Cache", "GPUCache"] {
                let p = e.path().join(c);
                if p.is_dir() {
                    out.push(p);
                }
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(local_appdata.join("Mozilla").join("Firefox").join("Profiles")) {
        for e in rd.flatten() {
            let p = e.path().join("cache2");
            if p.is_dir() {
                out.push(p);
            }
        }
    }
    out
}

/// Kind of developer artifact the folder `node` is, if any. `has_file(dir, cachedir_tag)` tells
/// whether `dir` holds a CACHEDIR.TAG (true) or a Cargo.toml (false).
fn dev_kind(tree: &Tree, node: u32, has_file: &dyn Fn(u32, bool) -> bool) -> Option<&'static str> {
    let n = tree.nodes.get(node as usize)?;
    if node == 0 || n.removed || n.parent == NO_PARENT {
        return None;
    }
    let name = n.name.to_lowercase();
    let pname = tree.nodes[n.parent as usize].name.to_lowercase();
    match name.as_str() {
        "node_modules" => {
            let nested = tree.ancestry(node).iter().rev().skip(1).any(|&a| tree.nodes[a as usize].name.eq_ignore_ascii_case("node_modules"));
            (!nested).then_some("node_modules")
        }
        "target" if has_file(node, true) || has_file(n.parent, false) => Some("Rust target folder"),
        "caches" if pname == ".gradle" => Some("Gradle caches"),
        "npm-cache" => Some("npm cache"),
        "cache" if pname == "pip" => Some("pip cache"),
        "cache" if pname == "yarn" => Some("Yarn cache"),
        _ => None,
    }
}

/// Why folder `node` is a removable developer artifact, if it is.
pub fn is_dev_artifact(tree: &Tree, node: u32) -> Option<&'static str> {
    let has = |dir: u32, tag: bool| {
        let want = if tag { "cachedir.tag" } else { "cargo.toml" };
        tree.files_in(dir).into_iter().any(|i| tree.file_name(&tree.files[i]).eq_ignore_ascii_case(want))
    };
    dev_kind(tree, node, &has)
}

// ---------------------------------------------------------------- measuring fixed locations

/// A path under `base`, or None when `base` is empty (never fall back to a relative path).
fn under_base(base: &str, rel: &str) -> Option<PathBuf> {
    (!base.is_empty()).then(|| rel.split('\\').filter(|c| !c.is_empty()).fold(real(base), |p, c| p.join(c)))
}

/// `s` with its real letter case and no `\\?\` prefix (the rules keep paths lower-case).
fn real(s: &str) -> PathBuf {
    let p = PathBuf::from(s);
    match std::fs::canonicalize(&p) {
        Ok(c) => {
            let t = c.to_string_lossy();
            match t.strip_prefix(r"\\?\") {
                Some(r) if !r.starts_with("UNC\\") => PathBuf::from(r),
                _ => c,
            }
        }
        Err(_) => p,
    }
}

/// Bytes below `path` and the newest file time in it, not following links, giving up after
/// `max_entries` entries.
pub fn dir_stats(path: &Path, max_entries: usize) -> (u64, Option<SystemTime>) {
    let (mut total, mut seen) = (0u64, 0usize);
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > max_entries {
                return (total, newest);
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total += m.len();
                newest = newest.max(m.modified().ok());
            }
        }
    }
    (total, newest)
}

const DIR_SIZE_CAP: usize = 200_000;

/// Top-level entries of `dir` for which `keep(name, modified)` holds.
fn entries(dir: &Path, needs_admin: bool, keep: &dyn Fn(&str, Option<SystemTime>) -> bool) -> Vec<CleanItem> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(md) = std::fs::symlink_metadata(e.path()) else { continue };
        let link = md.file_type().is_symlink();
        let is_dir = md.is_dir() && !link;
        // A folder is as old as the newest file inside it: its own time only changes when entries
        // are added or removed directly in it.
        let (dir_bytes, newest) = if is_dir { dir_stats(&e.path(), DIR_SIZE_CAP) } else { (0, None) };
        let modified = md.modified().ok().max(newest);
        if !keep(&e.file_name().to_string_lossy(), modified) {
            continue;
        }
        let size = if is_dir { dir_bytes } else if link { 0 } else { md.len() };
        out.push(CleanItem { path: e.path(), size, is_dir, needs_admin, node: None, file: None });
    }
    out
}

/// Everything except the Recycle Bin and the scan-based categories. Background thread.
pub fn measure_fixed(sys: &SystemPaths, now: SystemTime) -> Vec<Measured> {
    let all = |_: &str, _: Option<SystemTime>| true;
    let mut out = Vec::new();

    let items = under_base(&sys.temp, "").map(|d| entries(&d, false, &|_, m| older_than(m, now, Duration::from_secs(24 * 3600)))).unwrap_or_default();
    out.push(Measured::with_items(Category::UserTemp, items));

    let items = under_base(&sys.system_root, "Temp").map(|d| entries(&d, true, &all)).unwrap_or_default();
    out.push(Measured::with_items(Category::WindowsTemp, items));

    let mut items = Vec::new();
    for d in under_base(&sys.local_appdata, "").map(|l| browser_cache_dirs(&l)).unwrap_or_default() {
        items.extend(entries(&d, false, &all));
    }
    out.push(Measured::with_items(Category::BrowserCaches, items));

    // Crash dumps Windows Error Reporting does not own; its archive and queue are their own category.
    let mut items = Vec::new();
    for (base, rel, admin) in [(&sys.local_appdata, "CrashDumps", false), (&sys.system_root, "Minidump", true)] {
        if let Some(d) = under_base(base, rel) {
            items.extend(entries(&d, admin, &all));
        }
    }
    if let Some(f) = under_base(&sys.system_root, "MEMORY.DMP") {
        if let Ok(md) = std::fs::symlink_metadata(&f) {
            items.push(CleanItem { path: f, size: md.len(), is_dir: false, needs_admin: true, node: None, file: None });
        }
    }
    out.push(Measured::with_items(Category::CrashDumps, items));

    let mut items = Vec::new();
    for (base, rel, admin) in [
        (&sys.program_data, r"Microsoft\Windows\WER\ReportArchive", true),
        (&sys.program_data, r"Microsoft\Windows\WER\ReportQueue", true),
        (&sys.local_appdata, r"Microsoft\Windows\WER\ReportArchive", false),
        (&sys.local_appdata, r"Microsoft\Windows\WER\ReportQueue", false),
    ] {
        if let Some(d) = under_base(base, rel) {
            items.extend(entries(&d, admin, &all));
        }
    }
    out.push(Measured::with_items(Category::WerReports, items));

    let mut items = under_base(&sys.local_appdata, r"Microsoft\Windows\Explorer")
        .map(|d| {
            entries(&d, false, &|n, _| {
                let n = n.to_lowercase();
                (n.starts_with("thumbcache_") || n.starts_with("iconcache_")) && n.ends_with(".db")
            })
        })
        .unwrap_or_default();
    // Skip files Explorer holds open.
    let busy = super::diskdelete::in_use(&items.iter().map(|i| i.path.clone()).collect::<Vec<_>>());
    items.retain(|i| !busy.contains_key(&i.path));
    out.push(Measured::with_items(Category::ThumbnailCache, items));

    let items = under_base(&sys.system_root, r"SoftwareDistribution\Download").map(|d| entries(&d, true, &all)).unwrap_or_default();
    out.push(Measured::with_items(Category::UpdateDownloads, items));

    let items = under_base(&sys.system_root, r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache").map(|d| entries(&d, true, &all)).unwrap_or_default();
    out.push(Measured::with_items(Category::DeliveryOptimization, items));
    out
}

/// Recycle Bin / Trash size and item count (read-only, see [`super::trash::size`]).
pub fn measure_recycle_bin() -> Measured {
    match super::trash::size() {
        Some((bytes, count)) => Measured { bytes, count, ..Measured::new(Category::RecycleBin) },
        None => Measured::failed(Category::RecycleBin, &format!("could not read the {}", crate::platform::trash_word())),
    }
}

// ---------------------------------------------------------------- measuring from the scan tree

/// Newest file time (FILETIME ticks) at or below every node; 0 when a folder holds no files.
fn newest_mtime(t: &Tree) -> Vec<u64> {
    let mut v = vec![0u64; t.nodes.len()];
    for f in &t.files {
        let e = &mut v[f.dir as usize];
        *e = (*e).max(f.mtime);
    }
    for i in (1..t.nodes.len()).rev() {
        let p = t.nodes[i].parent;
        if p != NO_PARENT {
            v[p as usize] = v[p as usize].max(v[i]);
        }
    }
    v
}

/// Old Downloads and developer artifacts, from the last scan.
pub fn measure_from_tree(tree: &Tree, sys: &SystemPaths, now: SystemTime) -> Vec<Measured> {
    vec![old_downloads(tree, sys, now), dev_artifacts(tree)]
}

fn old_downloads(t: &Tree, sys: &SystemPaths, now: SystemTime) -> Measured {
    let cat = Category::OldDownloads;
    let Some((dl, _)) = sys.known.iter().find(|(_, n)| *n == "Downloads") else { return Measured::failed(cat, "Downloads folder not found") };
    let Some(dir) = t.find_node(Path::new(dl)) else { return Measured::failed(cat, "Downloads is not inside the scanned folder") };
    let mut items = Vec::new();
    for i in t.files_in(dir) {
        let f = &t.files[i];
        if is_old(f.modified(), now, 90) {
            items.push(CleanItem { path: t.file_path(f), size: f.size, is_dir: false, needs_admin: false, node: None, file: Some(i) });
        }
    }
    let newest = newest_mtime(t);
    for &k in &t.nodes[dir as usize].children {
        let n = &t.nodes[k as usize];
        if n.removed {
            continue;
        }
        let path = t.path_of(k);
        let modified = match newest[k as usize] {
            0 => std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            ticks => filetime_to_system(ticks),
        };
        if is_old(modified, now, 90) {
            items.push(CleanItem { path, size: n.size, is_dir: true, needs_admin: false, node: Some(k), file: None });
        }
    }
    Measured::with_items(cat, items)
}

fn dev_artifacts(t: &Tree) -> Measured {
    let mut marks: HashSet<(u32, bool)> = HashSet::new();
    for f in &t.files {
        let n = t.file_name(f);
        if n.eq_ignore_ascii_case("cachedir.tag") {
            marks.insert((f.dir, true));
        } else if n.eq_ignore_ascii_case("cargo.toml") {
            marks.insert((f.dir, false));
        }
    }
    let has = |dir: u32, tag: bool| marks.contains(&(dir, tag));
    // `covered[i]`: i is an artifact or inside one; parents come first, so nested ones are not listed twice.
    let mut covered = vec![false; t.nodes.len()];
    let mut items = Vec::new();
    for i in 1..t.nodes.len() {
        let n = &t.nodes[i];
        if n.removed || n.parent == NO_PARENT {
            continue;
        }
        if covered[n.parent as usize] {
            covered[i] = true;
        } else if dev_kind(t, i as u32, &has).is_some() {
            covered[i] = true;
            items.push(CleanItem { path: t.path_of(i as u32), size: n.size, is_dir: true, needs_admin: false, node: Some(i as u32), file: None });
        }
    }
    Measured::with_items(Category::DevArtifacts, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::diskspace::{FileRec, Names, Node, ScanMethod};

    const DAY: u64 = 86_400;

    /// Tree from (name, parent) folders in order and (dir, name, size, mtime ticks) files.
    fn tree(dirs: &[(&str, u32)], files: &[(u32, &str, u64, u64)]) -> Tree {
        let mut nodes: Vec<Node> = dirs.iter().map(|(n, p)| Node { name: n.to_string(), parent: *p, ..Default::default() }).collect();
        for i in 1..nodes.len() {
            let p = nodes[i].parent as usize;
            nodes[p].children.push(i as u32);
        }
        let mut names = Names::default();
        let mut recs = Vec::new();
        for (dir, name, size, mtime) in files {
            let (off, len) = names.push(name);
            recs.push(FileRec { size: *size, mtime: *mtime, dir: *dir, name_off: off, name_len: len, flags: 0 });
            let mut i = *dir;
            while i != NO_PARENT {
                let n = &mut nodes[i as usize];
                n.size += size;
                n.files += 1;
                if i == *dir {
                    n.own += size;
                }
                i = n.parent;
            }
        }
        Tree { root: PathBuf::from(r"c:\scan"), nodes, files: recs, names, extensions: vec![], method: ScanMethod::Walker }
    }

    #[cfg(windows)]
    fn ticks(t: SystemTime) -> u64 {
        crate::pro::diskspace::system_to_filetime(t)
    }

    #[test]
    fn is_old_boundaries() {
        let now = SystemTime::now();
        assert!(!is_old(Some(now - Duration::from_secs(89 * DAY)), now, 90));
        assert!(is_old(Some(now - Duration::from_secs(91 * DAY)), now, 90));
        assert!(!is_old(None, now, 90));
        assert!(!is_old(Some(now + Duration::from_secs(DAY)), now, 90));
    }

    #[test]
    fn dev_artifacts_in_synthetic_tree() {
        // 0 root, 1 proj, 2 proj\target, 3 other, 4 other\target, 5 x, 6 x\target, 7 a, 8 a\node_modules,
        // 9 a\node_modules\b, 10 a\node_modules\b\node_modules, 11 .gradle, 12 .gradle\caches
        let dirs = [
            ("scan", NO_PARENT),
            ("proj", 0),
            ("target", 1),
            ("other", 0),
            ("target", 3),
            ("x", 0),
            ("target", 5),
            ("a", 0),
            ("node_modules", 7),
            ("b", 8),
            ("node_modules", 9),
            (".gradle", 0),
            ("caches", 11),
        ];
        let files = [(1, "Cargo.toml", 10, 0), (2, "out.bin", 500, 0), (4, "out.bin", 7, 0), (6, "CACHEDIR.TAG", 1, 0), (10, "deep.js", 3, 0), (12, "c.bin", 9, 0)];
        let t = tree(&dirs, &files);
        let got: Vec<u32> = (0..13).filter(|&i| is_dev_artifact(&t, i).is_some()).collect();
        assert_eq!(got, vec![2, 6, 8, 12], "{got:?}");
        assert_eq!(is_dev_artifact(&t, 4), None);
        // The nested node_modules is covered by the outer one: listed once.
        let m = dev_artifacts(&t);
        let nodes: Vec<u32> = m.items.iter().filter_map(|i| i.node).collect();
        assert_eq!(m.items.len(), 4);
        assert!(!nodes.contains(&10) && nodes.contains(&8));
        assert_eq!(m.bytes, 500 + 1 + 3 + 9);
    }

    #[cfg(windows)]
    #[test]
    fn browser_cache_dirs_only_caches() {
        let root = sandbox("browsers");
        let mk = |p: &str| std::fs::create_dir_all(root.join(p)).unwrap();
        mk(r"Google\Chrome\User Data\Default\Cache");
        mk(r"Google\Chrome\User Data\Profile 1\Code Cache");
        mk(r"Google\Chrome\User Data\System Profile\Cache");
        mk(r"Mozilla\Firefox\Profiles\x.default\cache2");
        mk(r"Mozilla\Firefox\Profiles\x.default\storage");
        std::fs::write(root.join(r"Google\Chrome\User Data\Default\Login Data"), b"x").unwrap();
        let mut got: Vec<String> = browser_cache_dirs(&root).iter().map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('/', "\\")).collect();
        got.sort();
        assert_eq!(got, vec![r"Google\Chrome\User Data\Default\Cache", r"Google\Chrome\User Data\Profile 1\Code Cache", r"Mozilla\Firefox\Profiles\x.default\cache2"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    fn sandbox(tag: &str) -> PathBuf {
        let n = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("tmop-clean-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[cfg(windows)]
    fn write(p: &Path, len: usize, age_days: u64) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let f = std::fs::File::create(p).unwrap();
        f.set_len(len as u64).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_days * DAY)).unwrap();
    }

    #[cfg(windows)]
    fn names_of(m: &Measured) -> Vec<String> {
        let mut v: Vec<String> = m.items.iter().map(|i| i.path.file_name().unwrap().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    #[cfg(windows)]
    #[test]
    fn measure_fixed_in_temp_sandbox() {
        let root = sandbox("fixed");
        let s = |p: &str| root.join(p).to_string_lossy().to_lowercase();
        let sys = SystemPaths { temp: s("temp"), system_root: s("windows"), program_data: s("pd"), local_appdata: s("local"), ..Default::default() };
        write(&root.join(r"temp\old.tmp"), 100, 3);
        write(&root.join(r"temp\new.tmp"), 100, 0);
        // A folder whose own time is old but which holds a fresh file is not "old"; a wholly old one is.
        write(&root.join(r"temp\busy\fresh.tmp"), 10, 0);
        write(&root.join(r"temp\stale\inner.tmp"), 10, 3);
        for d in ["busy", "stale"] {
            use std::os::windows::fs::OpenOptionsExt;
            let h = std::fs::OpenOptions::new().write(true).custom_flags(0x0200_0000).open(root.join("temp").join(d)).unwrap();
            h.set_modified(SystemTime::now() - Duration::from_secs(3 * DAY)).unwrap();
        }
        write(&root.join(r"windows\Temp\a.tmp"), 50, 0);
        write(&root.join(r"windows\Minidump\a.dmp"), 10, 0);
        write(&root.join(r"windows\MEMORY.DMP"), 20, 0);
        write(&root.join(r"local\CrashDumps\x.dmp"), 5, 0);
        write(&root.join(r"local\Microsoft\Windows\Explorer\thumbcache_32.db"), 8, 0);
        write(&root.join(r"local\Microsoft\Windows\Explorer\other.db"), 8, 0);
        write(&root.join(r"local\Google\Chrome\User Data\Default\Cache\f_000001"), 30, 0);
        write(&root.join(r"windows\SoftwareDistribution\Download\u.cab"), 40, 0);
        write(&root.join(r"local\Microsoft\Windows\WER\ReportArchive\r1\Report.wer"), 4, 0);
        write(&root.join(r"windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache\c1"), 6, 0);
        let got = measure_fixed(&sys, SystemTime::now());
        let get = |c: Category| got.iter().find(|m| m.cat == c).unwrap();
        assert_eq!(names_of(get(Category::UserTemp)), vec!["old.tmp", "stale"]);
        assert!(!get(Category::UserTemp).items[0].needs_admin);
        let wt = get(Category::WindowsTemp);
        assert_eq!(names_of(wt), vec!["a.tmp"]);
        assert!(wt.items.iter().all(|i| i.needs_admin));
        assert_eq!(names_of(get(Category::CrashDumps)), vec!["MEMORY.DMP", "a.dmp", "x.dmp"]);
        let admin: Vec<bool> = get(Category::CrashDumps).items.iter().map(|i| i.needs_admin).collect();
        assert_eq!(admin.iter().filter(|a| **a).count(), 2);
        assert_eq!(names_of(get(Category::ThumbnailCache)), vec!["thumbcache_32.db"]);
        assert_eq!(names_of(get(Category::BrowserCaches)), vec!["f_000001"]);
        assert_eq!(names_of(get(Category::UpdateDownloads)), vec!["u.cab"]);
        assert_eq!(names_of(get(Category::WerReports)), vec!["r1"]);
        assert_eq!(names_of(get(Category::DeliveryOptimization)), vec!["c1"]);
        assert_eq!(get(Category::UserTemp).bytes, 110);
        // Empty settings measure nothing and never fall back to relative paths.
        assert!(measure_fixed(&SystemPaths::default(), SystemTime::now()).iter().all(|m| m.items.is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn old_downloads_from_tree() {
        let now = SystemTime::now();
        let old = ticks(now - Duration::from_secs(120 * DAY));
        let new = ticks(now - Duration::from_secs(2 * DAY));
        // 0 scan, 1 Downloads, 2 Downloads\oldsub, 3 Downloads\newsub
        let dirs = [("scan", NO_PARENT), ("Downloads", 0), ("oldsub", 1), ("newsub", 1)];
        let files = [(1, "old.zip", 100, old), (1, "new.zip", 200, new), (2, "a.bin", 7, old), (3, "b.bin", 9, new), (3, "c.bin", 9, old)];
        let t = tree(&dirs, &files);
        let sys = SystemPaths { known: vec![(r"c:\scan\downloads".into(), "Downloads")], ..Default::default() };
        let m = old_downloads(&t, &sys, now);
        assert!(m.error.is_none());
        let mut got = names_of(&m);
        got.sort();
        assert_eq!(got, vec!["old.zip", "oldsub"]);
        assert_eq!(m.bytes, 107);
        assert!(m.items.iter().any(|i| i.is_dir && i.node == Some(2)) && m.items.iter().any(|i| i.file.is_some()));
        // Downloads outside the scan.
        let other = SystemPaths { known: vec![(r"d:\users\me\downloads".into(), "Downloads")], ..Default::default() };
        assert!(old_downloads(&t, &other, now).error.is_some());
    }

    #[test]
    fn category_modes_and_preselection() {
        let pre: Vec<bool> = Category::ALL.iter().map(|c| c.preselected()).collect();
        assert_eq!(pre, vec![true, true, true, true, true, true, true, true, false, false, false]);
        for c in Category::ALL {
            assert_eq!(c.mode() == Mode::Recycle, matches!(c, Category::OldDownloads | Category::DevArtifacts), "{c:?}");
            assert_eq!(c.needs_scan(), c.mode() == Mode::Recycle);
        }
        assert!(Category::WindowsTemp.needs_admin() && Category::UpdateDownloads.needs_admin() && Category::DeliveryOptimization.needs_admin());
        assert!(!Category::UserTemp.needs_admin() && !Category::CrashDumps.needs_admin());
    }

    #[cfg(windows)]
    #[test]
    fn recycle_bin_size_reads() {
        assert!(crate::pro::trash::size().is_some());
    }

    #[test]
    fn trash_category_per_os() {
        assert_eq!(Category::RecycleBin.supported(), cfg!(any(windows, target_os = "linux")));
        assert!(Category::ALL.iter().filter(|c| **c != Category::RecycleBin).all(|c| c.supported()));
        let word = crate::platform::trash_word();
        assert!(Category::RecycleBin.title().ends_with(word) && Category::DevArtifacts.detail().contains(word));
    }
}
