//! The delete classifier off Windows (Linux and macOS share one protected-paths table).
//!
//! Paths are `/`-separated and compared exactly on Linux; macOS folds case (APFS default) and
//! reads `/System/Volumes/Data/x` as `/x` (the Data volume's firmlinks). `..` is never resolved
//! lexically (a symlink in front of it changes what it means): such paths are refused.
//!
//! [`classify_unix`] is the pure core: everything it knows comes from [`UnixPaths`] (home, uid,
//! mounts, user folders...) and the item's owner, so the tests run on every host. [`classify_fs`]
//! adds what the file system says: the parent chain and a symlink item resolved, bind mounts,
//! another name for a protected folder. [`build`] fills [`UnixPaths`] from this machine.
//!
//! Never deletable: `/`, every top-level folder itself and the system trees below most of them
//! (`/etc`, `/usr`, `/var`, `/System`, `/Library`...), every mount point and any folder holding
//! one, the home folder and its essential folders (`~/.ssh` entirely, `~/.config`, `~/Library`
//! and the user folders such as `~/Documents` themselves), other users' homes, Trash and volume
//! metadata stores, and anything owned by root unless TMOP runs as root. The contents of the
//! temporary folders stay deletable.

use super::{classify_norm, norm_path_win, Inventory, ItemMeta, Rules, SystemPaths, Verdict};

/// Which Unix the rules are for: Linux compares names exactly, macOS folds case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Linux,
    MacOs,
}

/// This build's [`Flavor`] (other Unix targets follow Linux).
pub const FLAVOR: Flavor = if cfg!(target_os = "macos") { Flavor::MacOs } else { Flavor::Linux };

/// What the Unix rules know about this machine (all paths normalised with [`norm_unix`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnixPaths {
    /// The user's home folder, as given and as resolved.
    pub homes: Vec<String>,
    /// Effective uid; `None` (unknown) counts as not root.
    pub uid: Option<u32>,
    /// Every mount point.
    pub mounts: Vec<String>,
    /// (bind mount point, the folder it shows).
    pub binds: Vec<(String, String)>,
    /// WSL's Windows drives, judged by the Windows rules.
    pub wsl: Vec<WslDrive>,
    /// Home folders of other accounts (from the passwd database).
    pub other_homes: Vec<String>,
    /// Temporary folders: their contents are deletable, they themselves are not.
    pub temp: Vec<String>,
    /// (st_dev, st_ino) of folders protected themselves, to recognise them under another name.
    pub protected_ids: Vec<(u64, u64)>,
}

/// A Windows drive mounted by WSL (`/mnt/c`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WslDrive {
    pub point: String,
    pub letter: char,
    /// Windows locations on that drive (`c:\windows`, `c:\users\<profile>`...).
    pub sys: SystemPaths,
}

/// `/`-separated, no empty or `.` components, no trailing `/`; `..` is kept (see the module
/// docs). macOS: lower-case, and `/System/Volumes/Data/x` is `/x`.
pub fn norm_unix(flavor: Flavor, s: &str) -> String {
    let parts: Vec<&str> = s.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let mut out = String::with_capacity(s.len());
    if s.starts_with('/') {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if flavor == Flavor::MacOs {
        out = out.to_lowercase();
        if let Some(rest) = out.strip_prefix("/system/volumes/data") {
            if rest.is_empty() {
                return "/".into();
            }
            if rest.starts_with('/') {
                return rest.to_string();
            }
        }
    }
    out
}

/// `n == dir` or `n` is below `dir` (whole components).
pub fn under(n: &str, dir: &str) -> bool {
    !dir.is_empty() && (n == dir || (n.starts_with(dir) && (dir == "/" || n.as_bytes().get(dir.len()) == Some(&b'/'))))
}

fn strictly_under(n: &str, dir: &str) -> bool {
    n != dir && under(n, dir)
}

fn comps(n: &str) -> Vec<&str> {
    n.split('/').filter(|c| !c.is_empty()).collect()
}

/// `n` relative to `dir` (`n` strictly below it).
fn rel<'a>(n: &'a str, dir: &str) -> &'a str {
    n[dir.len()..].trim_start_matches('/')
}

fn join(a: &str, b: &str) -> String {
    format!("{}/{}", a.trim_end_matches('/'), b)
}

/// A table entry in the flavor's case.
fn k(flavor: Flavor, s: &str) -> String {
    if flavor == Flavor::MacOs {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

/// Top-level folders whose whole tree belongs to the system (Linux and macOS together: a name
/// from the other OS protects nothing real but costs nothing). Names starting with `lib` too.
const SYSTEM_TREES: &[&str] = &[
    "bin", "boot", "dev", "etc", "opt", "proc", "root", "run", "sbin", "srv", "sys", "usr", "var", "snap", "nix", "gnu", "lost+found", "efi", "selinux", "sysroot", "System", "Library", "private", "cores",
    "Network", ".vol", ".nofollow", ".resolve",
];
/// Folders whose children are home folders.
const USERS_ROOTS: &[&str] = &["/home", "/Users", "/var/home"];
/// Temporary folders that may sit inside a system tree.
const FIXED_TEMP: &[&str] = &["/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp"];
/// Home folders that live inside a system tree.
const ROOT_HOMES: &[&str] = &["/root", "/var/root", "/private/var/root"];
/// Trash and file system metadata stores, wherever they are (any component).
const STORES: &[&str] = &[".Trash", ".Trashes", ".Spotlight-V100", ".fseventsd", ".DocumentRevisions-V100", ".TemporaryItems", ".MobileBackups"];
/// Inside the home folder: blocked with everything below.
const HOME_TREES: &[(&str, &str)] = &[
    (".ssh", "SSH keys"),
    (".gnupg", "GnuPG keys"),
    (".pki", "certificate store"),
    (".password-store", "password store"),
    (".local/share/keyrings", "keyring"),
    (".local/share/Trash", "Trash storage; empty the Trash instead"),
    ("Library/Keychains", "keychain"),
    ("Library/Accounts", "account store"),
];
/// Inside the home folder: blocked themselves; their contents follow the other rules.
const HOME_ESSENTIAL: &[&str] = &[
    ".config",
    ".local",
    ".local/share",
    ".local/state",
    ".local/bin",
    ".local/lib",
    ".cache",
    ".var",
    ".var/app",
    "snap",
    "Applications",
    "Library",
    "Library/Application Support",
    "Library/Preferences",
    "Library/Caches",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mail",
    "Library/Messages",
    "Library/Mobile Documents",
    "Library/CloudStorage",
    "Library/Cookies",
    "Library/LaunchAgents",
    "Library/Logs",
    "Library/Fonts",
    "Library/Calendars",
    "Library/Safari",
    "Library/Saved Application State",
    "Library/Developer",
    "Library/Photos",
];
/// Inside the home folder: a warning for everything below.
const HOME_WARN: &[(&str, &str)] = &[
    (".config", "application settings"),
    (".local/share", "application data"),
    (".local/state", "application state"),
    (".var/app", "Flatpak application data"),
    ("snap", "Snap application data"),
    ("Library/Application Support", "application data"),
    ("Library/Preferences", "application settings"),
    ("Library/Containers", "application data"),
    ("Library/Group Containers", "application data"),
    ("Library/Mail", "mail store"),
    ("Library/Messages", "Messages history"),
    ("Library/Mobile Documents", "iCloud Drive: deleting removes it from all your devices"),
    ("Library/CloudStorage", "cloud-synced files: deleting removes them everywhere"),
    (".mozilla", "browser profile (passwords, history)"),
    (".thunderbird", "mail profile"),
    (".config/google-chrome", "browser profile (passwords, history)"),
    (".config/chromium", "browser profile (passwords, history)"),
    (".config/BraveSoftware", "browser profile (passwords, history)"),
    (".config/microsoft-edge", "browser profile (passwords, history)"),
    ("Library/Application Support/Google/Chrome", "browser profile (passwords, history)"),
    ("Library/Application Support/Firefox", "browser profile (passwords, history)"),
];

/// The system tree `n` is in (`etc` for `/etc/x`), if any.
fn system_tree(flavor: Flavor, n: &str) -> Option<String> {
    let first = *comps(n).first()?;
    (first.starts_with("lib") || SYSTEM_TREES.iter().any(|t| k(flavor, t) == first)).then(|| first.to_string())
}

/// A home folder TMOP may treat as the user's: absolute, not `/`, no `..`, and not inside a
/// system tree (unless it is root's own). A `$HOME` of `/etc` protects `/etc`, it does not open it.
fn home_ok(flavor: Flavor, h: &str) -> bool {
    h.starts_with('/') && h != "/" && !comps(h).contains(&"..") && (system_tree(flavor, h).is_none() || ROOT_HOMES.iter().any(|r| k(flavor, r) == h) || strictly_under(h, &k(flavor, "/var/home")))
}

/// A temporary folder whose contents are deletable: one of the usual ones, a per-user macOS
/// `$TMPDIR` (`/var/folders/xx/yyy/T`), or any folder outside the system trees.
fn temp_ok(flavor: Flavor, t: &str) -> bool {
    let folders = |base: &str| strictly_under(t, &k(flavor, base)) && comps(t).len() >= comps(base).len() + 3;
    t.starts_with('/') && t != "/" && !comps(t).contains(&"..") && (FIXED_TEMP.iter().any(|f| k(flavor, f) == t) || folders("/var/folders") || folders("/private/var/folders") || system_tree(flavor, t).is_none())
}

fn blocked(r: impl Into<String>) -> Verdict {
    Verdict::Blocked(r.into())
}

/// Lexical verdict for a path as given (no file system access).
pub fn classify_lexical(flavor: Flavor, path: &str, meta: ItemMeta, rules: &Rules) -> Verdict {
    classify_unix(flavor, &norm_unix(flavor, path), meta, None, rules)
}

/// The pure Unix rules for the normalised path `n` whose owner is `owner` (when known).
pub fn classify_unix(flavor: Flavor, n: &str, meta: ItemMeta, owner: Option<u32>, rules: &Rules) -> Verdict {
    let u = &rules.sys.unix;
    let c = comps(n);
    if !n.starts_with('/') {
        return blocked("not an absolute path");
    }
    if c.contains(&"..") {
        return blocked("the path contains '..'");
    }
    if n == "/" {
        return blocked("file system root");
    }
    if c.iter().any(|x| STORES.iter().any(|s| k(flavor, s) == *x) || x.starts_with(&k(flavor, ".Trash-"))) {
        return blocked("Trash or file system metadata store");
    }
    let mut warn: Vec<String> = Vec::new();

    // --- mounts: the point itself, and anything holding one (a recursive delete would cross into it)
    if u.mounts.iter().any(|m| m == n) {
        return blocked("mount point");
    }
    if meta.is_dir {
        if let Some(m) = u.mounts.iter().find(|m| strictly_under(m, n)) {
            return blocked(format!("contains a mount point ({m})"));
        }
    }
    // --- WSL: a Windows drive is judged by the Windows rules
    if let Some(d) = u.wsl.iter().find(|d| strictly_under(n, &d.point)) {
        let win = format!("{}:\\{}", d.letter, rel(n, &d.point).replace('/', "\\"));
        match classify_norm(&norm_path_win(&win), meta, &Rules { sys: d.sys.clone(), inv: Inventory::default() }) {
            Verdict::Blocked(r) => return blocked(format!("{r} (Windows drive {}:)", d.letter.to_ascii_uppercase())),
            Verdict::Warn(r) => warn.extend(r),
            Verdict::Allowed => {}
        }
    }

    // --- homes
    let homes: Vec<&String> = u.homes.iter().filter(|h| home_ok(flavor, h)).collect();
    if homes.iter().any(|h| *h == n) {
        return blocked("your home folder");
    }
    if meta.is_dir && homes.iter().any(|h| strictly_under(h, n)) {
        return blocked("contains your home folder");
    }
    let others = u.other_homes.iter().filter(|o| o.starts_with('/') && *o != "/" && !homes.iter().any(|h| under(h, o)));
    for o in others {
        if under(n, o) {
            return blocked("another user's home folder");
        }
        if meta.is_dir && strictly_under(o, n) {
            return blocked("contains another user's home folder");
        }
    }
    for (kd, label) in &rules.sys.known {
        if kd == n {
            return blocked(format!("your {label} folder; delete its contents instead"));
        }
        if meta.is_dir && strictly_under(kd, n) {
            return blocked(format!("contains your {label} folder"));
        }
    }
    let temps: Vec<&String> = u.temp.iter().filter(|t| temp_ok(flavor, t)).collect();
    if temps.iter().any(|t| *t == n) {
        return blocked("temporary folder itself; delete its contents instead");
    }
    if meta.is_dir {
        if let Some(t) = temps.iter().find(|t| strictly_under(t, n)) {
            return blocked(format!("contains the temporary folder {t}"));
        }
    }
    if owner == Some(0) && u.uid != Some(0) {
        return blocked("owned by root");
    }
    if let (Some(o), Some(me)) = (owner, u.uid) {
        if o != me && o != 0 && me != 0 {
            warn.push("owned by another user".into());
        }
    }

    if let Some(h) = homes.iter().find(|h| strictly_under(n, h)) {
        // --- inside the home folder
        let r = rel(n, h);
        for (t, why) in HOME_TREES {
            let t = k(flavor, t);
            if under(r, &t) {
                return blocked(*why);
            }
            if meta.is_dir && strictly_under(&t, r) {
                return blocked(format!("contains your {why}"));
            }
        }
        if HOME_ESSENTIAL.iter().any(|e| k(flavor, e) == r) {
            return blocked("essential folder of your home folder");
        }
        for (w, why) in HOME_WARN {
            if strictly_under(r, &k(flavor, w)) {
                warn.push((*why).into());
            }
        }
        let rc = comps(r);
        if rc.len() == 1 && rc[0].starts_with('.') {
            warn.push("hidden settings or data in your home folder".into());
        }
    } else {
        // --- everywhere else
        if let Some(root) = USERS_ROOTS.iter().map(|r| k(flavor, r)).find(|r| strictly_under(n, r)) {
            let rest = comps(rel(n, &root));
            if rest[0] != k(flavor, "Shared") {
                return blocked("another user's home folder");
            }
            if rest.len() == 1 {
                return blocked("folder shared by all users; delete its contents instead");
            }
        }
        if c.len() == 1 {
            return blocked("top-level system folder");
        }
        // Removable media under /run/media (udisks) hold user files, not system ones.
        let media = u.mounts.iter().any(|m| strictly_under(m, &k(flavor, "/run/media")) && strictly_under(n, m));
        if !media && !temps.iter().any(|t| strictly_under(n, t)) {
            if let Some(t) = system_tree(flavor, n) {
                return blocked(format!("system folder (/{t})"));
            }
        }
        if c[0] == k(flavor, "Applications") && c.len() == 2 && c[1].ends_with(".app") {
            warn.push("installed application".into());
        }
    }
    // Inside `<...>/Applications/<...>/X.app/` (not a cache folder named like a bundle id).
    let apps = k(flavor, "Applications");
    if let Some(i) = c.iter().position(|x| x.ends_with(".app")) {
        if i + 1 < c.len() && c[..i].contains(&apps.as_str()) {
            warn.push("inside an application bundle".into());
        }
    }

    // --- warn: inventory
    let inv = &rules.inv;
    let skip_broad = |d: &str| comps(d).len() < 2 || homes.iter().any(|h| under(h, d)) || ["/usr/local", "/usr/bin", "/usr/lib", "/usr/share", "/opt/homebrew", "/usr/local/bin", "/snap/bin"].iter().any(|b| k(flavor, b) == d);
    for (d, app) in &inv.app_dirs {
        if skip_broad(d) {
            continue;
        }
        if under(n, d) {
            warn.push(format!("install folder of {app}"));
        } else if meta.is_dir && under(d, n) {
            warn.push(format!("contains the install folder of {app}"));
        }
    }
    for (img, svc) in &inv.service_images {
        if n == img || (meta.is_dir && under(img, n)) {
            warn.push(format!("program of service {svc}"));
        }
    }
    for (t, s) in &inv.startup_targets {
        if n == t || (meta.is_dir && under(t, n)) {
            warn.push(format!("startup entry {s}"));
        }
    }
    let running: Vec<String> = inv.process_exes.iter().filter(|(p, _, _)| if meta.is_dir { under(p, n) } else { p == n }).map(|(_, pid, name)| format!("{name} (PID {pid})")).collect();
    match running.len() {
        0 => {}
        1..=3 if meta.is_dir => warn.push(format!("a running process lives here: {}", running.join(", "))),
        1..=3 => warn.push(format!("running program {}", running.join(", "))),
        x => warn.push(format!("{x} running processes live here, e.g. {}", running[0])),
    }

    // --- warn: inventory-free
    let name = c.last().copied().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if !meta.is_dir {
        match ext.as_str() {
            "vhdx" | "vhd" | "vmdk" | "vdi" | "qcow2" => warn.push("virtual machine disk".into()),
            "kdbx" => warn.push("password database".into()),
            "pst" | "ost" => warn.push("mail store".into()),
            _ => {}
        }
    } else if matches!(ext.as_str(), "photoslibrary" | "musiclibrary" | "tvlibrary") {
        warn.push("media library".into());
    }
    if c.contains(&".git") {
        warn.push("Git repository data".into());
    }
    if c.iter().any(|x| [".aws", ".kube", ".docker", ".azure"].iter().any(|s| k(flavor, s) == *x)) {
        warn.push("credentials / keys".into());
    }

    let mut seen = std::collections::HashSet::new();
    warn.retain(|w| seen.insert(w.clone()));
    if warn.is_empty() {
        Verdict::Allowed
    } else {
        Verdict::Warn(warn)
    }
}

/// What [`classify_fs`] asks the file system (a test fake or [`RealFs`]).
pub trait UnixFs {
    /// The item itself, links not followed.
    fn lstat(&self, path: &str) -> Option<FsInfo>;
    /// The path with every link resolved (`realpath`).
    fn canonical(&self, path: &str) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FsInfo {
    pub owner: u32,
    /// (st_dev, st_ino)
    pub id: (u64, u64),
    pub is_link: bool,
    pub is_dir: bool,
}

/// `path` with its parent resolved and its own name kept (the item is not followed).
fn parent_resolved(path: &str, fs: &impl UnixFs) -> Option<String> {
    let t = path.trim_end_matches('/');
    let (parent, name) = t.rsplit_once('/')?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    let real = fs.canonical(if parent.is_empty() { "/" } else { parent })?;
    Some(join(&real, name))
}

/// The folder a bind mount shows at `n`, when `n` is inside one.
fn through_bind(u: &UnixPaths, n: &str) -> Option<String> {
    let (point, shown) = u.binds.iter().filter(|(p, _)| under(n, p)).max_by_key(|(p, _)| p.len())?;
    Some(if n == point { shown.clone() } else { join(shown, rel(n, point)) })
}

/// Strictest verdict for `path` as given and as the file system resolves it: the parent chain
/// (a symlinked folder cannot hide `/etc`), the item itself when it is a symlink (only the link
/// would be deleted, but a link to a protected place is refused all the same), bind mounts, and
/// the identity of protected folders.
pub fn classify_fs(flavor: Flavor, path: &str, meta: ItemMeta, rules: &Rules, fs: &impl UnixFs) -> Verdict {
    let u = &rules.sys.unix;
    let n = norm_unix(flavor, path);
    let info = fs.lstat(path);
    let owner = info.map(|i| i.owner);
    let mut v = classify_unix(flavor, &n, meta, owner, rules);
    if matches!(v, Verdict::Blocked(_)) {
        return v;
    }
    if info.is_some_and(|i| u.protected_ids.contains(&i.id)) {
        return blocked("a protected folder under another name");
    }
    // (path, meta, owner, how it was reached, whether its warnings count)
    let mut views: Vec<(String, ItemMeta, Option<u32>, &str, bool)> = Vec::new();
    if let Some(real) = parent_resolved(path, fs).filter(|r| norm_unix(flavor, r) != n) {
        views.push((real, meta, owner, "resolves to", true));
    }
    if info.is_some_and(|i| i.is_link) {
        if let Some(t) = fs.canonical(path) {
            let ti = fs.lstat(&t);
            views.push((t, ItemMeta { is_dir: ti.is_some_and(|i| i.is_dir), ..meta }, ti.map(|i| i.owner), "link to", false));
        }
    }
    for i in 0..views.len() + 1 {
        let p = if i == 0 { n.clone() } else { norm_unix(flavor, &views[i - 1].0) };
        if let Some(b) = through_bind(u, &p) {
            let (m, o) = if i == 0 { (meta, owner) } else { (views[i - 1].1, views[i - 1].2) };
            views.push((b, m, o, "bind mount of", true));
        }
    }
    for (p, m, o, how, warns) in views {
        match classify_unix(flavor, &norm_unix(flavor, &p), m, o, rules) {
            Verdict::Blocked(r) => return blocked(format!("{r} ({how} {p})")),
            Verdict::Warn(b) if warns => {
                v = match v {
                    Verdict::Warn(mut a) => {
                        a.extend(b);
                        let mut seen = std::collections::HashSet::new();
                        a.retain(|w| seen.insert(w.clone()));
                        Verdict::Warn(a)
                    }
                    _ => Verdict::Warn(b.into_iter().chain([format!("{how} {p}")]).collect()),
                }
            }
            _ => {}
        }
    }
    v
}

/// The file system as it is.
#[cfg(unix)]
pub struct RealFs;

#[cfg(unix)]
impl UnixFs for RealFs {
    fn lstat(&self, path: &str) -> Option<FsInfo> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(FsInfo { owner: m.uid(), id: (m.dev(), m.ino()), is_link: m.file_type().is_symlink(), is_dir: m.is_dir() })
    }
    fn canonical(&self, path: &str) -> Option<String> {
        std::fs::canonicalize(path).ok().map(|p| p.to_string_lossy().into_owned())
    }
}

// ---------------------------------------------------------------- reading this machine (pure parsers)

/// Home folders from `/etc/passwd` text: (uid, home) per valid line.
pub fn parse_passwd(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 7).then_some(())?;
            Some((f[2].parse().ok()?, f[5].to_string()))
        })
        .collect()
}

/// The user folders (label, path) from `user-dirs.dirs` text (`XDG_DOCUMENTS_DIR="$HOME/Docs"`).
/// A folder set to the home folder itself means "disabled" and is skipped.
pub fn parse_user_dirs(text: &str, home: &str) -> Vec<(&'static str, String)> {
    const NAMES: &[(&str, &str)] = &[
        ("DESKTOP", "Desktop"),
        ("DOCUMENTS", "Documents"),
        ("DOWNLOAD", "Downloads"),
        ("MUSIC", "Music"),
        ("PICTURES", "Pictures"),
        ("PUBLICSHARE", "Public"),
        ("TEMPLATES", "Templates"),
        ("VIDEOS", "Videos"),
    ];
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        let Some((key, val)) = line.split_once('=') else { continue };
        let Some(label) = key.strip_prefix("XDG_").and_then(|k| k.strip_suffix("_DIR")).and_then(|k| NAMES.iter().find(|(n, _)| *n == k)).map(|(_, l)| *l) else { continue };
        let val = val.trim().trim_matches('"');
        let path = if let Some(rest) = val.strip_prefix("$HOME") {
            format!("{}{}", home.trim_end_matches('/'), rest)
        } else if val.starts_with('/') {
            val.to_string()
        } else {
            continue;
        };
        if path.trim_end_matches('/') != home.trim_end_matches('/') {
            out.push((label, path));
        }
    }
    out
}

/// The default user folders of both systems (`~/Movies` on macOS, `~/Videos` and
/// `~/Templates` on Linux; the others are shared).
pub const DEFAULT_USER_DIRS: &[(&str, &str)] =
    &[("Desktop", "Desktop"), ("Documents", "Documents"), ("Downloads", "Downloads"), ("Music", "Music"), ("Pictures", "Pictures"), ("Public", "Public"), ("Templates", "Templates"), ("Videos", "Videos"), ("Movies", "Movies")];

/// [`SystemPaths::known`] and [`UnixPaths`] of this machine. Reads mountinfo / getfsstat,
/// passwd, `user-dirs.dirs` and stats a few dozen folders; no network mount is touched.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn build() -> (Vec<(String, &'static str)>, UnixPaths) {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    let fl = FLAVOR;
    let n = |p: &str| norm_unix(fl, p);
    let canon = |p: &str| std::fs::canonicalize(p).ok().map(|c| c.to_string_lossy().into_owned());
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let passwd = std::fs::read_to_string("/etc/passwd").map(|t| parse_passwd(&t)).unwrap_or_default();

    // Home: $HOME (or the passwd entry), trusted only when it is a folder this user owns.
    let mut raw_homes: Vec<String> = Vec::new();
    for h in crate::platform::home().map(|h| h.to_string_lossy().into_owned()).into_iter().chain(passwd.iter().filter(|(u, _)| *u == uid).map(|(_, h)| h.clone())) {
        if std::fs::metadata(&h).is_ok_and(|m| m.is_dir() && m.uid() == uid) && !raw_homes.contains(&h) {
            raw_homes.push(h);
        }
    }
    let mut homes: Vec<String> = Vec::new();
    for h in &raw_homes {
        for v in [Some(n(h)), canon(h).map(|c| n(&c))].into_iter().flatten() {
            if !homes.contains(&v) {
                homes.push(v);
            }
        }
    }

    // User folders.
    let mut known: Vec<(String, &'static str)> = Vec::new();
    let mut add_known = |p: String, label: &'static str| {
        let p = n(&p);
        if !homes.contains(&p) && !known.iter().any(|(k, _)| *k == p) {
            known.push((p, label));
        }
    };
    if let Some(h) = raw_homes.first() {
        let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|c| c.starts_with('/')).unwrap_or_else(|| join(h, ".config"));
        for (label, p) in std::fs::read_to_string(join(&config, "user-dirs.dirs")).map(|t| parse_user_dirs(&t, h)).unwrap_or_default() {
            add_known(p, label);
        }
        for (label, name) in DEFAULT_USER_DIRS {
            add_known(join(h, name), label);
        }
    }
    let resolved: Vec<(String, &'static str)> = known.iter().filter_map(|(p, l)| canon(p).map(|c| (n(&c), *l))).collect();
    for (p, l) in resolved {
        if !known.iter().any(|(k, _)| *k == p) {
            known.push((p, l));
        }
    }

    let table = super::super::mounts::table();
    let mounts: Vec<String> = table.points.iter().map(|p| n(p)).collect();
    let binds = table.binds.iter().map(|(p, s)| (n(p), n(s))).collect();
    let wsl = table
        .drives
        .iter()
        .map(|(point, letter)| {
            let d = |s: &str| format!("{letter}:\\{s}");
            let mut sys = SystemPaths {
                system_root: d("windows"),
                program_files: vec![d("program files"), d("program files (x86)")],
                program_data: d("programdata"),
                users_root: d("users"),
                ..Default::default()
            };
            if let Ok(rd) = std::fs::read_dir(Path::new(point).join("Users")) {
                for e in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
                    sys.profiles.push(d(&format!("users\\{}", e.file_name().to_string_lossy().to_lowercase())));
                }
            }
            sys.appdata = sys.profiles.iter().map(|p| format!("{p}\\appdata")).collect();
            WslDrive { point: n(point), letter: *letter, sys }
        })
        .collect();

    // Other accounts: passwd entries, and every folder in a users root that is not ours.
    let mut other_homes: Vec<String> = Vec::new();
    let mut add_other = |p: &str| {
        for v in [Some(n(p)), canon(p).map(|c| n(&c))].into_iter().flatten() {
            if v != "/" && !homes.iter().any(|h| under(h, &v)) && !other_homes.contains(&v) {
                other_homes.push(v);
            }
        }
    };
    for (u, h) in &passwd {
        if *u != uid && h.starts_with('/') {
            add_other(h);
        }
    }

    // Temporary folders, as given and resolved (`/tmp` is `/private/tmp` on macOS).
    let mut temp: Vec<String> = Vec::new();
    let env_tmp = std::env::var("TMPDIR").ok().filter(|t| t.starts_with('/'));
    for t in FIXED_TEMP.iter().map(|s| s.to_string()).chain(env_tmp).chain(Some(std::env::temp_dir().to_string_lossy().into_owned())) {
        for v in [Some(n(&t)), canon(&t).map(|c| n(&c))].into_iter().flatten() {
            if v != "/" && !temp.contains(&v) {
                temp.push(v);
            }
        }
    }

    // Identities of folders that are protected themselves (never a mount point: a dead network
    // share would hang the stat).
    let mut ids: Vec<(u64, u64)> = Vec::new();
    let mut exact: Vec<String> = vec!["/".into()];
    exact.extend(raw_homes.iter().cloned());
    exact.extend(known.iter().map(|(p, _)| p.clone()));
    for h in &raw_homes {
        exact.extend(HOME_TREES.iter().map(|(t, _)| join(h, t)));
        exact.extend(HOME_ESSENTIAL.iter().map(|e| join(h, e)));
    }
    if let Ok(rd) = std::fs::read_dir("/") {
        exact.extend(rd.flatten().map(|e| e.path().to_string_lossy().into_owned()));
    }
    for p in exact {
        if p != "/" && mounts.contains(&n(&p)) {
            continue;
        }
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            if m.is_dir() && !ids.contains(&(m.dev(), m.ino())) {
                ids.push((m.dev(), m.ino()));
            }
        }
    }
    let u = UnixPaths { homes, uid: Some(uid), mounts, binds, wsl, other_homes, temp, protected_ids: ids };
    (known, u)
}

/// Mounts again (right before deleting: something may have been mounted since the rules were built).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn refresh_mounts(u: &mut UnixPaths) {
    let t = super::super::mounts::table();
    u.mounts = t.points.iter().map(|p| norm_unix(FLAVOR, p)).collect();
    u.binds = t.binds.iter().map(|(p, s)| (norm_unix(FLAVOR, p), norm_unix(FLAVOR, s))).collect();
}

/// The first folder below `dir` (links not followed) on another file system than `dir`: a
/// recursive delete would empty that mount too.
#[cfg(unix)]
pub fn other_device_below(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::symlink_metadata(dir).ok()?.dev();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
            if m.is_dir() {
                if m.dev() != dev {
                    return Some(e.path());
                }
                stack.push(e.path());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
