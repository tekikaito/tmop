//! Deleting from Disk Space: the safety classifier (one pure function used by the manual delete
//! and by Cleanup), the inventory it consults, and the delete worker (Recycle Bin through the
//! shell, or permanent through `std::fs`). Nothing here touches the disk unless `run` is called.
//!
//! Rules are deterministic: [`classify`] returns `Blocked` (never deletable), `Warn` (deletable
//! after an explicit acknowledgement) or `Allowed`. System locations are resolved at runtime
//! (Windows may live on `D:`), never hard-coded.

use super::diskspace::Tree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- path helpers (pure)

/// Lower-case, `\`-separated, no `\\?\` / `\\?\UNC\` prefix, no trailing `\` except on a drive root.
pub fn norm_path(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('/', "\\");
    let low = s.to_ascii_lowercase();
    if low.starts_with(r"\\?\unc\") {
        s = format!(r"\\{}", &s[8..]);
    } else if low.starts_with(r"\\?\") || low.starts_with(r"\??\") || low.starts_with(r"\\.\") {
        s = s[4..].to_string();
    }
    let mut s = s.to_lowercase();
    while s.len() > 1 && s.ends_with('\\') && !(s.len() == 3 && s.as_bytes()[1] == b':') {
        s.pop();
    }
    if s.len() == 2 && s.as_bytes()[1] == b':' {
        s.push('\\');
    }
    s
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

#[cfg(not(windows))]
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

/// Safety verdict for deleting `path`. Pure apart from 8.3 expansion of paths containing `~`.
pub fn classify(path: &Path, meta: ItemMeta, rules: &Rules) -> Verdict {
    let n = norm_path_long(path);
    classify_norm(&n, meta, rules)
}

fn classify_norm(n: &str, meta: ItemMeta, rules: &Rules) -> Verdict {
    let sys = &rules.sys;
    let comps = components(n);
    let name = comps.last().copied().unwrap_or("");
    let on_drive = comps.first().is_some_and(|c| c.len() == 2 && c.ends_with(':'));
    let mut warn: Vec<String> = Vec::new();

    // --- system root with its exceptions
    let sr = sys.system_root.as_str();
    if !sr.is_empty() && under(n, sr) {
        let allowed_dirs = ["temp", "logs", "prefetch", "minidump"].iter().any(|d| under(n, &join(sr, d)));
        let wu = under(n, &join(sr, r"softwaredistribution\download"));
        // Delivery Optimization's cache lives under the NetworkService profile; it is a pure cache.
        let dopt = under(n, &join(sr, r"serviceprofiles\networkservice\appdata\local\microsoft\windows\deliveryoptimization\cache"));
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
            if under(n, &join(&b, "credentials")) || under(n, &join(&b, "protect")) {
                return blocked("Windows credential / DPAPI store");
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

/// More files than this are not checked against the Restart Manager (the dialog says so).
pub const IN_USE_CAP: usize = 64;

/// [`classify`] plus a Restart Manager check for files. Worker thread.
pub fn assess(targets: Vec<Target>, rules: &Rules) -> Vec<Assessed> {
    let mut out: Vec<Assessed> = targets
        .into_iter()
        .map(|t| {
            let verdict = classify(&t.path, ItemMeta { is_dir: t.is_dir, size: t.size, is_reparse: false }, rules);
            Assessed { target: t, verdict }
        })
        .collect();
    let idx: Vec<usize> = (0..out.len()).filter(|&i| !out[i].target.is_dir && !matches!(out[i].verdict, Verdict::Blocked(_))).collect();
    if idx.len() <= IN_USE_CAP {
        let files: Vec<PathBuf> = idx.iter().map(|&i| out[i].target.path.clone()).collect();
        for (p, holders) in in_use(&files) {
            if let Some(a) = out.iter_mut().find(|a| a.target.path == p) {
                let reason = format!("in use by {}", holders.join(", "));
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

#[cfg(windows)]
pub(crate) fn in_use(files: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    super::restartmgr::in_use(files)
}

#[cfg(not(windows))]
pub(crate) fn in_use(_: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
    HashMap::new()
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
    super::data_dir().join("deletions.log")
}

/// Runs on a worker thread. Callers must have removed Blocked targets.
pub fn run(targets: Vec<Target>, mode: Mode) -> Report {
    run_with_log(targets, mode, Some(&default_log()))
}

pub fn run_with_log(targets: Vec<Target>, mode: Mode, log: Option<&Path>) -> Report {
    let mut rep = Report { mode, ..Default::default() };
    let mut log_file = log.and_then(|p| {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let stamp = super::display_stamp();
    let recycler = (mode == Mode::Recycle).then(Recycler::new);
    for t in targets {
        let result = if is_drive_root(&norm_path(&t.path)) || t.path.file_name().is_none() {
            Err("refusing to delete a drive root".to_string())
        } else {
            match (&recycler, mode) {
                (Some(r), Mode::Recycle) => r.delete(&t.path),
                _ => delete_permanent(&t.path),
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

fn short_error(e: &std::io::Error) -> String {
    match (e.kind(), e.raw_os_error()) {
        (std::io::ErrorKind::NotFound, _) => "no longer exists".into(),
        (_, Some(32)) | (_, Some(33)) => "in use by another process".into(),
        (std::io::ErrorKind::PermissionDenied, _) => "access denied".into(),
        _ => e.to_string(),
    }
}

/// Permanent delete. A link (symlink or junction) is removed itself, never its target.
pub fn delete_permanent(path: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(path).map_err(|e| short_error(&e))?;
    #[cfg(windows)]
    let (link, dir) = {
        use std::os::windows::fs::MetadataExt;
        let a = md.file_attributes();
        (md.file_type().is_symlink() || a & 0x400 != 0, a & 0x10 != 0)
    };
    #[cfg(not(windows))]
    let (link, dir) = (md.file_type().is_symlink(), md.is_dir());
    let r = if dir && link {
        std::fs::remove_dir(path)
    } else if dir {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    r.map_err(|e| short_error(&e))
}

/// Moves items to the Recycle Bin through `IFileOperation`, one operation per item so a failure
/// is attributed to its item. COM lives on the worker thread that created this.
struct Recycler {
    #[cfg(windows)]
    com: bool,
}

#[cfg(windows)]
impl Recycler {
    fn new() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: balanced with CoUninitialize in Drop when this succeeded.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Recycler { com }
    }

    fn delete(&self, path: &Path) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
        use windows::Win32::UI::Shell::{
            FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        };
        if std::fs::symlink_metadata(path).is_err() {
            return Err("no longer exists".into());
        }
        let w = super::winutil::wide(&path.to_string_lossy());
        // SAFETY: COM calls on the thread that initialised COM; `w` is NUL-terminated and outlives them.
        let r: windows::core::Result<bool> = unsafe {
            (|| {
                let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
                op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0 | FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0))?;
                let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None)?;
                op.DeleteItem(&item, None)?;
                op.PerformOperations()?;
                Ok(op.GetAnyOperationsAborted()?.as_bool())
            })()
        };
        match r {
            Ok(false) if std::fs::symlink_metadata(path).is_err() => Ok(()),
            Ok(_) => Err("could not be moved to the Recycle Bin".into()),
            Err(e) if e.code().0 as u32 == 0x8007_0005 => Err("access denied".into()),
            Err(e) => Err(e.message()),
        }
    }
}

#[cfg(windows)]
impl Drop for Recycler {
    fn drop(&mut self) {
        if self.com {
            // SAFETY: matches the successful CoInitializeEx in `new`.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(not(windows))]
impl Recycler {
    fn new() -> Self {
        Recycler {}
    }
    fn delete(&self, _: &Path) -> Result<(), String> {
        Err("the Recycle Bin is only available on Windows".into())
    }
}

/// False for network, optical and RAM volumes and when the volume has no Recycle Bin (removable
/// FAT sticks): deletion there is permanent and the dialog says so.
#[cfg(windows)]
pub fn has_recycle_bin(path: &Path) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let n = norm_path(path);
    let b = n.as_bytes();
    if b.len() < 3 || b[1] != b':' || b[2] != b'\\' {
        return false;
    }
    let root = super::winutil::wide(&format!("{}:\\", b[0] as char));
    // SAFETY: NUL-terminated root; the info struct has cbSize set.
    unsafe {
        if !matches!(GetDriveTypeW(PCWSTR(root.as_ptr())), 2 | 3) {
            return false;
        }
        let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
        SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info).is_ok()
    }
}

#[cfg(not(windows))]
pub fn has_recycle_bin(_: &Path) -> bool {
    false
}

// ---------------------------------------------------------------- elevation

pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        super::winutil::is_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Command line for the elevated copy.
pub fn relaunch_args(root: &Path) -> String {
    format!("--page=disk-space --disk-root=\"{}\"", root.display())
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
