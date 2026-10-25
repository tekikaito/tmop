//! Deleting from Disk Space: the safety classifier (one pure function used by the manual delete
//! and by Cleanup), the inventory it consults, and the delete worker (Recycle Bin / Trash through
//! [`super::trash`], or permanent through `std::fs`). Nothing here touches the disk unless `run`
//! is called.
//!
//! Rules are deterministic: [`classify`] returns `Blocked` (never deletable), `Warn` (deletable
//! after an explicit acknowledgement) or `Allowed`. System locations are resolved at runtime
//! (Windows may live on `D:`), never hard-coded. Off Windows the rules are those of [`unix`]
//! (protected-paths table, mounts, home folder); the Windows rules stay pure and judge WSL's
//! Windows drives there too.

use super::diskspace::Tree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod unix;

pub use unix::UnixPaths;

// ---------------------------------------------------------------- path helpers (pure)

/// The comparison form of `p` for this OS: [`norm_path_win`] on Windows, [`unix::norm_unix`]
/// elsewhere (`/`-separated, case kept on Linux).
pub fn norm_path(p: &Path) -> String {
    #[cfg(windows)]
    {
        norm_path_win(&p.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        unix::norm_unix(unix::FLAVOR, &p.to_string_lossy())
    }
}

/// Lower-case, `\`-separated, no `\\?\` / `\\?\UNC\` prefix, no trailing `\` except on a drive root.
pub fn norm_path_win(p: &str) -> String {
    let mut s = p.replace('/', "\\");
    let low = s.to_ascii_lowercase();
    if low.starts_with(r"\\?\unc\") {
        s = format!(r"\\{}", &s[8..]);
    } else if low.starts_with(r"\\?\") || low.starts_with(r"\??\") || low.starts_with(r"\\.\") {
        s = s[4..].to_string();
    }
    let s = admin_share_to_drive(s.to_lowercase());
    let unc = s.starts_with(r"\\");
    let rooted = s.starts_with('\\');
    let floor = if unc {
        2
    } else if s.as_bytes().get(1) == Some(&b':') {
        1
    } else {
        0
    };
    // What Win32 resolves lexically before touching the disk: `.` / `..`, doubled separators and
    // trailing dots / spaces on a component (`Windows ` is `Windows`).
    let mut parts: Vec<&str> = Vec::new();
    for c in s.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.len() > floor {
                    parts.pop();
                }
            }
            _ => {
                let t = if c.len() == 2 && c.ends_with(':') { c } else { c.trim_end_matches(['.', ' ']) };
                if !t.is_empty() {
                    parts.push(t);
                }
            }
        }
    }
    let mut out = String::new();
    if unc {
        out.push_str(r"\\");
    } else if rooted {
        out.push('\\');
    }
    out.push_str(&parts.join("\\"));
    if parts.len() == 1 && parts[0].len() == 2 && parts[0].ends_with(':') {
        out.push('\\');
    }
    out
}

/// `\\localhost\c$\x`, `\\127.0.0.1\c$\x` and `\\<this computer>\c$\x` name the local drive.
fn admin_share_to_drive(s: String) -> String {
    let Some(rest) = s.strip_prefix(r"\\") else { return s };
    let Some((host, tail)) = rest.split_once('\\') else { return s };
    let this = std::env::var("COMPUTERNAME").unwrap_or_default().to_lowercase();
    if !(host == "localhost" || host == "127.0.0.1" || (!this.is_empty() && host == this)) {
        return s;
    }
    let (share, more) = tail.split_once('\\').unwrap_or((tail, ""));
    let b = share.as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b'$' {
        format!("{}:\\{}", b[0] as char, more)
    } else {
        s
    }
}

/// [`norm_path`] after expanding 8.3 short names (Windows, only when the path contains `~` and exists).
pub fn norm_path_long(p: &Path) -> String {
    #[cfg(windows)]
    if p.to_string_lossy().contains('~') {
        if let Some(l) = super::winutil::long_path(p) {
            return norm_path(&l);
        }
    }
    norm_path(p)
}

/// `c:\` or a bare UNC share (`\\server\share`).
pub fn is_drive_root(n: &str) -> bool {
    let b = n.as_bytes();
    if b.len() == 3 && b[1] == b':' && b[2] == b'\\' {
        return true;
    }
    n.strip_prefix(r"\\").is_some_and(|r| r.split('\\').filter(|c| !c.is_empty()).count() == 2)
}

/// `n == dir` or `n` is below `dir` (both normalised).
pub fn under(n: &str, dir: &str) -> bool {
    if dir.is_empty() {
        return false;
    }
    n == dir || (n.len() > dir.len() && n.starts_with(dir) && (dir.ends_with('\\') || n.as_bytes()[dir.len()] == b'\\'))
}

pub fn components(n: &str) -> Vec<&str> {
    n.split('\\').filter(|c| !c.is_empty()).collect()
}

fn join(a: &str, b: &str) -> String {
    format!("{}\\{}", a.trim_end_matches('\\'), b)
}

// ---------------------------------------------------------------- system locations and inventory

/// System locations, resolved once per [`Rules`] build (all normalised).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemPaths {
    pub system_root: String,
    pub program_files: Vec<String>,
    pub program_data: String,
    pub users_root: String,
    pub profiles: Vec<String>,
    /// `<profile>\appdata` of every profile.
    pub appdata: Vec<String>,
    /// Known folders of the current user: (path, "Documents" | "Pictures" | ...).
    pub known: Vec<(String, &'static str)>,
    pub onedrive: Vec<String>,
    pub temp: String,
    /// `%LOCALAPPDATA%` of the current user (Cleanup).
    pub local_appdata: String,
    /// What the Linux and macOS rules need (empty on Windows).
    pub unix: UnixPaths,
}

#[cfg(windows)]
pub fn system_paths() -> SystemPaths {
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86,
        FOLDERID_RoamingAppData, FOLDERID_SavedGames, FOLDERID_UserProfiles, FOLDERID_Videos,
    };
    let kf = |id| super::winutil::known_folder(id).map(|p| norm_path_long(&p));
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(|v| norm_path_long(Path::new(&v)));
    let mut sp = SystemPaths { system_root: env("SystemRoot").or_else(|| env("windir")).unwrap_or_default(), ..Default::default() };
    for p in [kf(&FOLDERID_ProgramFiles), kf(&FOLDERID_ProgramFilesX86), env("ProgramW6432")].into_iter().flatten() {
        if !sp.program_files.contains(&p) {
            sp.program_files.push(p);
        }
    }
    sp.program_data = kf(&FOLDERID_ProgramData).or_else(|| env("ProgramData")).unwrap_or_default();
    sp.users_root = kf(&FOLDERID_UserProfiles).unwrap_or_default();
    // Profiles: the ProfileList registry values plus the folders directly under the users root.
    {
        use super::winutil::{Key, View};
        use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        if let Some(k) = Key::open(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList", View::Default) {
            for sub in k.subkeys() {
                if let Some(p) = k.subkey(&sub).and_then(|s| s.string("ProfileImagePath")).filter(|p| !p.is_empty()) {
                    sp.profiles.push(norm_path_long(Path::new(&p)));
                }
            }
        }
        if !sp.users_root.is_empty() {
            if let Ok(rd) = std::fs::read_dir(&sp.users_root) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        sp.profiles.push(norm_path(&e.path()));
                    }
                }
            }
        }
        if let Some(u) = env("USERPROFILE") {
            sp.profiles.push(u);
        }
        sp.profiles.sort();
        sp.profiles.dedup();
    }
    sp.appdata = sp.profiles.iter().map(|p| join(p, "appdata")).collect();
    if let Some(r) = kf(&FOLDERID_RoamingAppData) {
        if let Some((parent, _)) = r.rsplit_once('\\') {
            if !sp.appdata.iter().any(|a| a == parent) {
                sp.appdata.push(parent.to_string());
            }
        }
    }
    for (id, name) in [
        (&FOLDERID_Documents, "Documents"),
        (&FOLDERID_Pictures, "Pictures"),
        (&FOLDERID_Desktop, "Desktop"),
        (&FOLDERID_Downloads, "Downloads"),
        (&FOLDERID_Music, "Music"),
        (&FOLDERID_Videos, "Videos"),
        (&FOLDERID_SavedGames, "Saved Games"),
    ] {
        if let Some(p) = kf(id) {
            sp.known.push((p, name));
        }
    }
    for k in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(p) = env(k) {
            if !sp.onedrive.contains(&p) {
                sp.onedrive.push(p);
            }
        }
    }
    sp.temp = env("TEMP").unwrap_or_default();
    sp.local_appdata = env("LOCALAPPDATA").unwrap_or_default();
    sp
}

/// Linux and macOS: the user folders (`known`) and [`UnixPaths`]; the Windows fields stay empty.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn system_paths() -> SystemPaths {
    let (known, unix) = unix::build();
    SystemPaths { known, unix, ..Default::default() }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn system_paths() -> SystemPaths {
    SystemPaths::default()
}

/// What is installed or running, from the existing TMOP inventory modules.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    /// (normalised install dir, app name)
    pub app_dirs: Vec<(String, String)>,
    /// (normalised image path, service or driver display name)
    pub service_images: Vec<(String, String)>,
    /// (normalised target, startup entry name)
    pub startup_targets: Vec<(String, String)>,
    /// (normalised exe path, pid, process name)
    pub process_exes: Vec<(String, u32, String)>,
}

impl Inventory {
    /// Runs the readers; slow, call from a worker thread. `procs` are (pid, name, exe path) rows.
    pub fn build(procs: Vec<(u32, String, Option<String>)>) -> Inventory {
        let mut inv = Inventory::default();
        if let Ok(apps) = super::apps::read() {
            for a in apps {
                let loc = a.location.trim().trim_matches('"');
                if !loc.is_empty() && Path::new(loc).is_absolute() {
                    inv.app_dirs.push((norm_path_long(Path::new(loc)), a.name));
                }
            }
        }
        let mut svcs = super::drivers::read(super::drivers::Kind::Drivers).unwrap_or_default();
        svcs.extend(super::inventory::services().unwrap_or_default());
        for s in svcs {
            if !s.path.is_empty() {
                let p = super::drivers::normalize_image_path(&s.path);
                if Path::new(&p).is_absolute() {
                    inv.service_images.push((norm_path_long(Path::new(&p)), if s.display.is_empty() { s.name } else { s.display }));
                }
            }
        }
        for s in super::inventory::startup_apps().unwrap_or_default() {
            let p = super::inventory::command_path(&s.command);
            if Path::new(&p).is_absolute() {
                inv.startup_targets.push((norm_path_long(Path::new(&p)), s.name));
            }
        }
        for (pid, name, path) in procs {
            if let Some(p) = path.filter(|p| Path::new(p).is_absolute()) {
                inv.process_exes.push((norm_path_long(Path::new(&p)), pid, name));
            }
        }
        inv
    }
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub sys: SystemPaths,
    pub inv: Inventory,
}

/// What the UI loader runs when the delete menu or dialog first needs the rules.
pub fn build_rules(procs: Vec<(u32, String, Option<String>)>) -> Rules {
    Rules { sys: system_paths(), inv: Inventory::build(procs) }
}

// ---------------------------------------------------------------- classify (pure)

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Blocked(String),
    Warn(Vec<String>),
    Allowed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemMeta {
    pub is_dir: bool,
    pub size: u64,
    pub is_reparse: bool,
}

const WU_REASON: &str = "Windows Update download cache; Windows re-downloads what it needs";

fn blocked(r: &str) -> Verdict {
    Verdict::Blocked(r.to_string())
}

/// Safety verdict for deleting `path`. Pure apart from 8.3 expansion of paths containing `~`
/// (Windows); off Windows the lexical [`unix`] rules (owner and links unknown).
pub fn classify(path: &Path, meta: ItemMeta, rules: &Rules) -> Verdict {
    #[cfg(windows)]
    {
        classify_norm(&norm_path_long(path), meta, rules)
    }
    #[cfg(not(windows))]
    {
        unix::classify_lexical(unix::FLAVOR, &path.to_string_lossy(), meta, rules)
    }
}

/// The Windows rules for a Windows path, on any host (tests; WSL's drives).
pub fn classify_win(path: &str, meta: ItemMeta, rules: &Rules) -> Verdict {
    classify_norm(&norm_path_win(path), meta, rules)
}

/// Strictest of the lexical verdict and the verdict for the path as the file system resolves it:
/// junctions and symlinks in the parent chain are followed, so `C:\Documents and Settings\x`
/// cannot hide `C:\Users\x`. The item itself is not followed (a link is deleted, not its target).
/// Off Windows see [`unix::classify_fs`] (also owner, a link item's target and bind mounts).
pub fn classify_resolved(path: &Path, meta: ItemMeta, rules: &Rules) -> Verdict {
    #[cfg(unix)]
    {
        unix::classify_fs(unix::FLAVOR, &path.to_string_lossy(), meta, rules, &unix::RealFs)
    }
    #[cfg(not(unix))]
    {
        classify_resolved_win(path, meta, rules)
    }
}

#[cfg(not(unix))]
fn classify_resolved_win(path: &Path, meta: ItemMeta, rules: &Rules) -> Verdict {
    let lexical = classify(path, meta, rules);
    if matches!(lexical, Verdict::Blocked(_)) {
        return lexical;
    }
    let Some(real) = resolved_path(path) else { return lexical };
    if norm_path_long(&real) == norm_path_long(path) {
        return lexical;
    }
    match (lexical, classify(&real, meta, rules)) {
        (_, Verdict::Blocked(r)) => Verdict::Blocked(format!("{r} (resolves to {})", real.display())),
        (Verdict::Warn(mut a), Verdict::Warn(b)) => {
            a.extend(b);
            a.dedup();
            Verdict::Warn(a)
        }
        (Verdict::Allowed, Verdict::Warn(mut b)) => {
            b.push(format!("resolves to {}", real.display()));
            Verdict::Warn(b)
        }
        (l, _) => l,
    }
}

/// `path` with its parent chain resolved (links followed), verbatim prefix removed.
#[cfg(not(unix))]
fn resolved_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    let real = std::fs::canonicalize(parent).ok()?;
    Some(strip_verbatim(real).join(name))
}

/// The scan root as the file system resolves it (`..`, junctions, `\\?\Volume{..}\`, `\\localhost\C$`),
/// so every path in the scan tree is the real one and the classifier sees real locations. Falls back to
/// `root` when it cannot be resolved.
pub fn canonical_root(root: PathBuf) -> PathBuf {
    match std::fs::canonicalize(&root) {
        Ok(c) => strip_verbatim(c),
        Err(_) => root,
    }
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\s` -> `\\srv\s`.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{r}"))
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        PathBuf::from(r.to_string())
    } else {
        p
    }
}

fn classify_norm(n: &str, meta: ItemMeta, rules: &Rules) -> Verdict {
    let sys = &rules.sys;
    let comps = components(n);
    let name = comps.last().copied().unwrap_or("");
    let on_drive = comps.first().is_some_and(|c| c.len() == 2 && c.ends_with(':'));
    let mut warn: Vec<String> = Vec::new();

    // --- system root with its exceptions (only the CONTENTS of these folders are deletable)
    let sr = sys.system_root.as_str();
    if !sr.is_empty() && under(n, sr) {
        let flat = ["temp", "logs", "prefetch", "minidump"].map(|d| join(sr, d));
        let wu_dir = join(sr, r"softwaredistribution\download");
        let dopt_dir = join(sr, r"serviceprofiles\networkservice\appdata\local\microsoft\windows\deliveryoptimization\cache");
        if flat.iter().any(|d| n == d) || n == wu_dir || n == dopt_dir {
            return blocked("system temp/cache folder itself; delete its contents instead");
        }
        let allowed_dirs = flat.iter().any(|d| under(n, d));
        let wu = under(n, &wu_dir);
        // Delivery Optimization's cache lives under the NetworkService profile; it is a pure cache.
        let dopt = under(n, &dopt_dir);
        if allowed_dirs || n == join(sr, "memory.dmp") || dopt {
            // cleanup locations: not blocked
        } else if wu {
            warn.push(WU_REASON.to_string());
        } else {
            return blocked("Windows system folder");
        }
    }

    // --- blocked
    if is_drive_root(n) {
        return blocked("drive root");
    }
    if sys.program_files.iter().any(|p| n == p) {
        return blocked("Program Files root");
    }
    if !sys.program_data.is_empty() && n == sys.program_data {
        return blocked("ProgramData root");
    }
    if sys.program_files.iter().any(|p| under(n, &join(p, "windowsapps"))) {
        return blocked("protected Windows Store apps folder");
    }
    if !sys.users_root.is_empty() && n == sys.users_root {
        return blocked("Users folder");
    }
    if sys.profiles.iter().any(|p| n == p) {
        return blocked("user profile root");
    }
    if sys.appdata.iter().any(|a| n == a || ["roaming", "local", "locallow"].iter().any(|s| *n == join(a, s))) {
        return blocked("user AppData folder");
    }
    if name.starts_with("ntuser.dat") || name.starts_with("usrclass.dat") {
        return blocked("registry hive of a user profile");
    }
    for a in &sys.appdata {
        for base in ["roaming", "local"] {
            let b = join(&join(a, base), "microsoft");
            let (cred, prot) = (join(&b, "credentials"), join(&b, "protect"));
            if under(n, &cred) || under(n, &prot) {
                return blocked("Windows credential / DPAPI store");
            }
            if meta.is_dir && (under(&cred, n) || under(&prot, n)) {
                return blocked("contains the Windows credential / DPAPI store");
            }
        }
    }
    // The user's Temp folder itself (its contents are fair game).
    if (!sys.temp.is_empty() && n == sys.temp) || sys.appdata.iter().any(|a| *n == join(&join(a, "local"), "temp")) {
        return blocked("Temp folder itself; delete its contents instead");
    }
    // A folder that contains a protected location (relocated profiles, Users or Program Files under
    // a parent folder) is as untouchable as the location itself.
    if meta.is_dir {
        let protected = std::iter::once((&sys.system_root, "the Windows folder"))
            .chain(std::iter::once((&sys.program_data, "ProgramData")))
            .chain(std::iter::once((&sys.users_root, "the Users folder")))
            .chain(sys.program_files.iter().map(|p| (p, "Program Files")))
            .chain(sys.profiles.iter().map(|p| (p, "a user profile")))
            .chain(sys.appdata.iter().map(|p| (p, "a user AppData folder")));
        for (p, what) in protected {
            if !p.is_empty() && p != n && under(p, n) {
                return blocked(&format!("contains {what}"));
            }
        }
        for (k, label) in &sys.known {
            if k != n && under(k, n) {
                warn.push(format!("contains your {label} folder"));
            }
        }
        for o in &sys.onedrive {
            if o != n && under(o, n) {
                warn.push("contains your OneDrive folder".into());
            }
        }
    }
    if on_drive && comps.len() >= 2 {
        match comps[1] {
            "system volume information" => return blocked("System Volume Information"),
            "$recycle.bin" => return blocked("Recycle Bin storage"),
            "recovery" => return blocked("Windows recovery data"),
            "pagefile.sys" | "hiberfil.sys" | "swapfile.sys" if comps.len() == 2 => return blocked("Windows system file"),
            _ => {}
        }
    }

    // --- warn: inventory
    let inv = &rules.inv;
    let skip_broad = |d: &str| is_drive_root(d) || d.is_empty() || sys.program_files.iter().any(|p| p == d) || d == sys.program_data || d == sys.users_root;
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
            warn.push(format!("image of service/driver {svc}"));
        }
    }
    for (t, s) in &inv.startup_targets {
        if n == t || (meta.is_dir && under(t, n)) {
            warn.push(format!("startup entry {s}"));
        }
    }
    let running: Vec<String> = inv
        .process_exes
        .iter()
        .filter(|(p, _, _)| if meta.is_dir { under(p, n) } else { p == n })
        .map(|(_, pid, name)| format!("{name} (PID {pid})"))
        .collect();
    match running.len() {
        0 => {}
        1..=3 if meta.is_dir => warn.push(format!("a running process lives here: {}", running.join(", "))),
        1..=3 => warn.push(format!("running program {}", running.join(", "))),
        k => warn.push(format!("{k} running processes live here, e.g. {}", running[0])),
    }

    // --- warn: inventory-free
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if !meta.is_dir {
        match ext {
            "vhdx" | "vhd" | "vmdk" | "vdi" | "qcow2" => warn.push("virtual machine / WSL disk".into()),
            "kdbx" => warn.push("password database".into()),
            "pst" | "ost" => warn.push("mail store".into()),
            _ => {}
        }
    }
    if comps.contains(&".git") {
        warn.push("Git repository data".into());
    }
    if comps.iter().any(|c| matches!(*c, ".ssh" | ".gnupg" | ".aws" | ".kube")) {
        warn.push("credentials / keys".into());
    }
    for (k, label) in &sys.known {
        if *label == "Saved Games" {
            if under(n, k) {
                warn.push("game saves".into());
            }
        } else if n == k {
            warn.push(format!("your {label} folder"));
        }
        if *label == "Documents" && under(n, &join(k, "my games")) {
            warn.push("game saves".into());
        }
    }
    if sys.onedrive.iter().any(|o| n == o) {
        warn.push("OneDrive root".into());
    }
    if on_drive && comps.len() >= 2 && comps[1] == "windows.old" {
        warn.push("previous Windows installation (use Disk Cleanup to remove it safely)".into());
    }
    let pkg_cache = (!sys.program_data.is_empty() && under(n, &join(&sys.program_data, "package cache"))) || sys.appdata.iter().any(|a| under(n, &join(&join(a, "local"), "package cache")));
    if pkg_cache {
        warn.push("installer Package Cache (uninstall or repair may need it)".into());
    }
    // Browser and mail profiles: the profile folders themselves (not the caches inside).
    let seq_rest = |seq: &[&str]| -> Option<usize> { comps.windows(seq.len()).position(|w| w == seq).map(|i| comps.len() - i - seq.len()) };
    let chromium = |seq: &[&str]| {
        seq_rest(seq).is_some_and(|rest| {
            rest == 0 || (rest == 1 && {
                let p = comps[comps.len() - 1];
                p == "default" || p == "guest profile" || p.starts_with("profile ")
            })
        })
    };
    if chromium(&["google", "chrome", "user data"])
        || chromium(&["microsoft", "edge", "user data"])
        || seq_rest(&["mozilla", "firefox", "profiles"]).is_some_and(|r| r <= 1)
        || seq_rest(&["thunderbird", "profiles"]).is_some_and(|r| r <= 1)
    {
        warn.push("browser / mail profile (passwords, history, mail)".into());
    }

    let mut seen = std::collections::HashSet::new();
    warn.retain(|w| seen.insert(w.clone()));
    if warn.is_empty() {
        Verdict::Allowed
    } else {
        Verdict::Warn(warn)
    }
}

// ---------------------------------------------------------------- assessment

#[derive(Clone, Debug)]
pub struct Target {
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    /// Node index in the scan tree (folders).
    pub node: Option<u32>,
    /// Index into `Tree::files` (files).
    pub file: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Assessed {
    pub target: Target,
    pub verdict: Verdict,
}

/// More files than this are not checked against the Restart Manager; [`in_use_skipped`] tells
/// the confirm dialogs how many went unchecked.
pub const IN_USE_CAP: usize = 64;

/// How many files [`assess`] did not check for being in use (0 when the check ran for all).
pub fn in_use_skipped<'a>(items: impl IntoIterator<Item = &'a Assessed>) -> usize {
    let n = items.into_iter().filter(|a| !a.target.is_dir && !matches!(a.verdict, Verdict::Blocked(_))).count();
    if n > IN_USE_CAP {
        n
    } else {
        0
    }
}

/// [`classify`] plus a Restart Manager check for files. Worker thread.
pub fn assess(targets: Vec<Target>, rules: &Rules) -> Vec<Assessed> {
    let mut out: Vec<Assessed> = targets
        .into_iter()
        .map(|t| {
            let verdict = classify_resolved(&t.path, ItemMeta { is_dir: t.is_dir, size: t.size, is_reparse: false }, rules);
            Assessed { target: t, verdict }
        })
        .collect();
    let idx: Vec<usize> = (0..out.len()).filter(|&i| !out[i].target.is_dir && !matches!(out[i].verdict, Verdict::Blocked(_))).collect();
    if idx.len() <= IN_USE_CAP {
        let files: Vec<PathBuf> = idx.iter().map(|&i| out[i].target.path.clone()).collect();
        for (p, holders) in in_use(&files) {
            if let Some(a) = out.iter_mut().find(|a| a.target.path == p) {
                // Unix deletes an open file at once, but its space comes back only when it is closed.
                let reason = if cfg!(windows) { format!("in use by {}", holders.join(", ")) } else { format!("in use by {}; the space is freed once it closes the file", holders.join(", ")) };
                a.verdict = match std::mem::replace(&mut a.verdict, Verdict::Allowed) {
                    Verdict::Warn(mut r) => {
                        r.push(reason);
                        Verdict::Warn(r)
                    }
                    _ => Verdict::Warn(vec![reason]),
                };
            }
        }
    }
    out
}

/// Processes holding each file open (see [`super::inuse`]).
pub(crate) fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    super::inuse::in_use(files)
}

/// Targets for the deleted record in a scan tree: folders by node, files by index.
pub fn tree_updates(done: &[Target]) -> (Vec<u32>, Vec<usize>) {
    (done.iter().filter_map(|t| t.node).collect(), done.iter().filter_map(|t| t.file).collect())
}

/// Convenience for callers that need a folder's target from the tree.
pub fn folder_target(t: &Tree, node: u32) -> Target {
    Target { path: t.path_of(node), size: t.nodes[node as usize].size, is_dir: true, node: Some(node), file: None }
}

// ---------------------------------------------------------------- delete worker

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Recycle,
    Permanent,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Recycle => "recycle",
            Mode::Permanent => "permanent",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub deleted: Vec<Target>,
    pub skipped: Vec<(Target, String)>,
    pub freed: u64,
    pub mode: Mode,
}

/// One line of `deletions.log`: `time<TAB>mode<TAB>size<TAB>result<TAB>path`.
pub fn log_line(stamp: &str, mode: Mode, size: u64, result: &Result<(), String>, path: &Path) -> String {
    let res = match result {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("failed: {e}"),
    };
    format!("{stamp}\t{}\t{size}\t{res}\t{}", mode.word(), path.display())
}

pub fn default_log() -> PathBuf {
    crate::platform::data_dir().join("deletions.log")
}

/// Runs on a worker thread. Callers should have removed Blocked targets; with `rules` every target
/// is classified once more right before it is deleted and Blocked ones are skipped. Where
/// [`Caps::delete_safe`](crate::platform::Caps) is off nothing is touched and every target is skipped.
pub fn run(targets: Vec<Target>, mode: Mode, rules: Option<&Rules>) -> Report {
    if !crate::platform::Caps::get().delete_safe {
        return refuse_all(targets, mode);
    }
    // Off Windows no caller can skip the classifier, and the mounts are read again: something
    // may have been mounted since the rules were built.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let fresh = {
        let mut r = rules.cloned().unwrap_or_else(|| Rules { sys: system_paths(), inv: Inventory::default() });
        unix::refresh_mounts(&mut r.sys.unix);
        r
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let rules = Some(&fresh);
    run_with_log(targets, mode, Some(&default_log()), rules)
}

/// Every target skipped, nothing deleted or logged: deleting is not offered on this OS yet.
fn refuse_all(targets: Vec<Target>, mode: Mode) -> Report {
    let why = format!("deleting is not available on {} yet", crate::platform::os_name());
    Report { skipped: targets.into_iter().map(|t| (t, why.clone())).collect(), mode, ..Default::default() }
}

/// Why `path` must never be deleted whatever the rules say (pure, no I/O).
pub(crate) fn refuse_reason(path: &Path) -> Option<&'static str> {
    if cfg!(windows) {
        (is_drive_root(&norm_path(path)) || path.file_name().is_none()).then_some("refusing to delete a drive root")
    } else {
        (!path.has_root() || path.file_name().is_none() || norm_path(path) == "/" || path.components().any(|c| matches!(c, std::path::Component::ParentDir)))
            .then_some("refusing to delete the root, a relative path or a path with '..'")
    }
}

fn run_with_log(targets: Vec<Target>, mode: Mode, log: Option<&Path>, rules: Option<&Rules>) -> Report {
    let mut rep = Report { mode, ..Default::default() };
    let mut log_file = log.and_then(|p| {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let stamp = super::display_stamp();
    let trasher = (mode == Mode::Recycle).then(super::trash::Trasher::new);
    for t in targets {
        let blocked_now = rules.and_then(|r| match classify_resolved(&t.path, ItemMeta { is_dir: t.is_dir, size: t.size, is_reparse: false }, r) {
            Verdict::Blocked(why) => Some(why),
            _ => None,
        });
        let result = if let Some(why) = refuse_reason(&t.path) {
            Err(why.to_string())
        } else if let Some(why) = blocked_now {
            Err(format!("blocked ({why})"))
        } else {
            // A Recycle never falls back to a permanent delete: what cannot go to the bin stays.
            match (&trasher, mode) {
                (Some(r), Mode::Recycle) => r.trash(&t.path, t.size),
                (None, Mode::Recycle) => Err(format!("could not reach the {}", crate::platform::trash_word())),
                (_, Mode::Permanent) => delete_permanent(&t.path),
            }
        };
        if let Some(f) = log_file.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{}", log_line(&stamp, mode, t.size, &result, &t.path));
        }
        match result {
            Ok(()) => {
                rep.freed += t.size;
                rep.deleted.push(t);
            }
            Err(e) => rep.skipped.push((t, e)),
        }
    }
    rep
}

pub(crate) fn short_error(e: &std::io::Error) -> String {
    match (e.kind(), e.raw_os_error()) {
        (std::io::ErrorKind::NotFound, _) => "no longer exists".into(),
        (_, Some(32)) | (_, Some(33)) => "in use by another process".into(),
        (std::io::ErrorKind::PermissionDenied, _) => "access denied".into(),
        _ => e.to_string(),
    }
}

/// `path` in `\\?\` verbatim form, which Win32 does not normalise: a name with a trailing dot or
/// space is looked up as written and can never resolve to a sibling (`foo.` is not `foo`). Drive
/// and UNC paths only; anything else (relative, already verbatim, device) is returned unchanged.
pub fn verbatim(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().replace('/', "\\");
    if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") || s.starts_with(r"\??\") {
        return PathBuf::from(s);
    }
    let b = s.as_bytes();
    if let Some(unc) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{unc}"))
    } else if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        PathBuf::from(format!(r"\\?\{s}"))
    } else {
        path.to_path_buf()
    }
}

/// A component ends with a dot or space: Win32 (and the shell) fold it away, so the name could resolve
/// to a sibling (`foo.` -> `foo`). Pure.
pub fn folds_to_sibling(path: &Path) -> bool {
    path.components().any(|c| matches!(c, std::path::Component::Normal(n) if n.to_string_lossy().ends_with(['.', ' '])))
}

/// Permanent delete. A link (symlink or junction) is removed itself, never its target. Works on the
/// verbatim form of `path` (see [`verbatim`]) on Windows.
pub fn delete_permanent(path: &Path) -> Result<(), String> {
    // Win32 does not normalise verbatim paths (`..` would not resolve) and cannot make a relative path
    // verbatim (a trailing dot or space would still fold to a sibling).
    #[cfg(windows)]
    {
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("the path contains '..'".into());
        }
    }
    #[cfg(windows)]
    let path = verbatim(path);
    #[cfg(windows)]
    let path = path.as_path();
    #[cfg(windows)]
    if !path.to_string_lossy().starts_with(r"\\") && folds_to_sibling(path) {
        return Err("the name ends with a dot or space".into());
    }
    let md = std::fs::symlink_metadata(path).map_err(|e| short_error(&e))?;
    #[cfg(windows)]
    let (link, dir) = {
        use std::os::windows::fs::MetadataExt;
        let a = md.file_attributes();
        (md.file_type().is_symlink() || a & 0x400 != 0, a & 0x10 != 0)
    };
    #[cfg(not(windows))]
    let (link, dir) = (md.file_type().is_symlink(), md.is_dir());
    // `remove_dir_all` does not follow links but does descend into mounted file systems.
    #[cfg(unix)]
    if dir && !link {
        if let Some(m) = unix::other_device_below(path) {
            return Err(format!("contains a mount point ({}); nothing was deleted", m.display()));
        }
    }
    let r = if dir && link {
        std::fs::remove_dir(path)
    } else if dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    r.map_err(|e| short_error(&e))
}

/// The Windows Recycle Bin check, now in [`super::trash`].
#[cfg(windows)]
pub use super::trash::has_recycle_bin;

/// Blocks the items of a [`Mode::Recycle`] delete whose volume has no Recycle Bin / Trash, with
/// the reason: they are left where they are, never deleted permanently instead. Returns how
/// many it blocked and whether every bin involved keeps everything ([`super::trash::limit_known`]).
pub fn block_untrashable(items: &mut [Assessed], vols: &mut super::trash::Volumes) -> (usize, bool) {
    let (mut blocked, mut capped) = (0, true);
    for a in items.iter_mut().filter(|a| !matches!(a.verdict, Verdict::Blocked(_))) {
        let (why, keeps) = vols.check(&a.target.path);
        match why {
            Some(why) => {
                a.verdict = Verdict::Blocked(format!("can't go to the {}: {why}", crate::platform::trash_word()));
                blocked += 1;
            }
            None => capped &= keeps,
        }
    }
    (blocked, capped)
}

// ---------------------------------------------------------------- elevation

/// Command line for the elevated copy. Windows argv parsing reads `\"` as a literal quote, so a
/// backslash run at the end of the quoted root (`C:\`) is doubled.
pub fn relaunch_args(root: &Path) -> String {
    let s = root.display().to_string();
    let trailing = s.len() - s.trim_end_matches('\\').len();
    format!("--page=disk-space --disk-root=\"{s}{}\"", "\\".repeat(trailing))
}

/// Starts this exe through the UAC prompt, on Disk Space with `root`. Ok once the new process
/// started; Err("cancelled by the user") on a declined prompt. Blocks for the prompt: use a Loader.
pub fn relaunch_elevated(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::engine::winreg::run_elevated(&crate::taskmgr::this_exe().display().to_string(), &relaunch_args(root))
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err("only available on Windows".into())
    }
}

#[cfg(test)]
mod tests;
