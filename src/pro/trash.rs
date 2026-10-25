//! The Recycle Bin / Trash: moving an item there, whether its volume has one, and the size and
//! emptying of the bin for Clean up.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | [`Trasher::trash`] | `IFileOperation` with `FOFX_RECYCLEONDELETE` | FreeDesktop Trash spec 1.0: the home Trash, else the volume's `.Trash/$uid` or `.Trash-$uid` | `NSFileManager trashItemAtURL` (Put Back works) |
//! | [`unavailable`] | no bin on the drive, or it is turned off | no usable Trash on the item's file system | network or read-only volume |
//! | [`size`] / [`empty`] | `SHQueryRecycleBin` / `SHEmptyRecycleBin` | `files/` and `info/` of the home Trash | not offered, see [`can_empty`] |
//!
//! Rule: an item that cannot go to the bin is reported and left where it is. Nothing here ever
//! deletes permanently instead, and Linux never copies an item across file systems.
//!
//! The `.trashinfo` format, its percent-encoding and the collision names are pure and tested on
//! every host.

use crate::platform::trash_word;
use std::path::Path;

// ---------------------------------------------------------------- .trashinfo (pure)

/// Bytes a path keeps in a `.trashinfo` `Path=`: RFC 2396 unreserved characters and `/`.
fn keep(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b)
}

/// A path as the Trash spec stores it: every byte but the unreserved ones and `/` as `%XX`.
pub fn encode_path(path: &[u8]) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path {
        if keep(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The bytes of an [`encode_path`] value; `None` for a broken `%` escape.
pub fn decode_path(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The contents of a `.trashinfo` file: `path` (absolute for the home Trash, relative to the
/// volume's top folder otherwise) and the local deletion time (year, month, day, h, min, s).
pub fn trashinfo(path: &[u8], t: (u16, u16, u16, u16, u16, u16)) -> String {
    let (y, mo, d, h, mi, s) = t;
    format!("[Trash Info]\nPath={}\nDeletionDate={y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}\n", encode_path(path))
}

/// (path bytes, deletion date) of a `.trashinfo` file. Only keys of the `[Trash Info]` group
/// count, which must come first; `None` without a valid `Path`.
pub fn parse_trashinfo(text: &str) -> Option<(Vec<u8>, String)> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    if lines.next()? != "[Trash Info]" {
        return None;
    }
    let (mut path, mut date) = (None, String::new());
    for l in lines {
        if l.starts_with('[') {
            break;
        }
        match l.split_once('=') {
            Some(("Path", v)) if path.is_none() => path = Some(decode_path(v.trim())?),
            Some(("DeletionDate", v)) => date = v.trim().to_string(),
            _ => {}
        }
    }
    path.filter(|p| !p.is_empty()).map(|p| (p, date))
}

/// Longest file name most file systems take (`NAME_MAX`).
pub const NAME_MAX: usize = 255;
const INFO_EXT: &str = ".trashinfo";

/// The `n`-th candidate name for `name` in the Trash: the name itself, then `stem.2.ext`,
/// `stem.3.ext` ... Shortened (never inside a UTF-8 character) so that `<name>.trashinfo` still
/// fits in [`NAME_MAX`] bytes.
pub fn unique_name(name: &[u8], n: u32) -> Vec<u8> {
    let max = NAME_MAX - INFO_EXT.len();
    // A leading dot starts a hidden name, not an extension.
    let (stem, ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, &b""[..]),
    };
    let tag = if n == 0 { String::new() } else { format!(".{n}") };
    let ext = if ext.len() + tag.len() >= max { &b""[..] } else { ext };
    let room = max.saturating_sub(tag.len() + ext.len());
    let mut cut = stem.len().min(room);
    if let Ok(s) = std::str::from_utf8(stem) {
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
    }
    [&stem[..cut], tag.as_bytes(), ext].concat()
}

// ---------------------------------------------------------------- the per-volume check

/// Asks [`unavailable`] and [`limit_known`] once per volume of a batch of items: per drive on
/// Windows, per mount (mount point and file system) on Linux and macOS.
#[derive(Default)]
pub struct Volumes {
    seen: Vec<(String, Option<String>, bool)>,
    /// Mount points, read on the first check.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    points: Option<Vec<String>>,
}

impl Volumes {
    /// (why `path` cannot go to the bin, the bin keeps everything) for the volume of `path`. An
    /// item that no longer exists is no question of the volume (its siblings share the key): it
    /// passes uncached, and moving it reports that it is gone.
    pub fn check(&mut self, path: &Path) -> (Option<String>, bool) {
        if std::fs::symlink_metadata(path).is_err() {
            return (None, true);
        }
        let key = self.key(path);
        if let Some((_, why, capped)) = self.seen.iter().find(|(k, ..)| *k == key) {
            return (why.clone(), *capped);
        }
        let (why, capped) = (unavailable(path), limit_known(path));
        self.seen.push((key, why.clone(), capped));
        (why, capped)
    }

    /// The mount `path` is on: the mount point holding its (resolved) parent and the file system
    /// of the nearest existing item. A bind mount of a folder of the same file system is a
    /// separate key: its Trash is decided from a different top folder.
    fn key(&mut self, path: &Path) -> String {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::unix::fs::MetadataExt;
            let points = self.points.get_or_insert_with(|| super::mounts::table().points);
            if let Some((a, m)) = path.ancestors().find_map(|a| std::fs::symlink_metadata(a).ok().map(|m| (a, m))) {
                // The parent, resolved: the item itself may be a link to another mount.
                let real = a.parent().filter(|p| !p.as_os_str().is_empty()).and_then(|p| std::fs::canonicalize(p).ok()).unwrap_or_else(|| a.to_path_buf());
                let point = super::mounts::mount_of(&real.to_string_lossy(), points).unwrap_or("");
                // The folder's device: on an overlay a file's st_dev names the layer it comes from.
                let dev = std::fs::metadata(&real).map(|r| r.dev()).unwrap_or(m.dev());
                return format!("{point}\n{dev}");
            }
        }
        volume_key(path)
    }
}

/// The drive (Windows) or the file system (`st_dev`, other Unix targets) of `path`; its first
/// component when nothing of it exists.
fn volume_key(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(m) = path.ancestors().find_map(|a| std::fs::symlink_metadata(a).ok()) {
            return format!("dev:{}", m.dev());
        }
    }
    path.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Deleting through the bin exists on this OS at all.
pub const fn supported() -> bool {
    cfg!(any(windows, target_os = "linux", target_os = "macos"))
}

/// Clean up can empty the bin. Not on macOS: listing `~/.Trash` needs Full Disk Access and
/// emptying it through Finder needs the Automation permission, so the Finder does it.
pub const fn can_empty() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

/// Why `path` cannot go to the bin (`None` when it can). Checks without creating anything.
pub fn unavailable(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        (!has_recycle_bin(path)).then(|| "this drive has no Recycle Bin (network or removable drive, or the bin is turned off)".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        fdo::locate(&absolute(path).ok()?, crate::platform::data_home().map(|d| d.join("Trash")).as_deref(), false).err()
    }
    #[cfg(target_os = "macos")]
    {
        mac::unavailable(path)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        Some(format!("there is no {} on this system", trash_word()))
    }
}

/// The bin keeps everything moved to it, so a restore can be promised. Windows only when the
/// drive's bin has a size limit on record (else the shell may delete large items for good);
/// the Linux and macOS Trash has no size limit.
pub fn limit_known(path: &Path) -> bool {
    #[cfg(windows)]
    {
        recycle_limit_known(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        true
    }
}

/// Bytes and items in the bin (Windows: every drive; Linux: the home Trash). `None` when it
/// cannot be read.
pub fn size() -> Option<(u64, u64)> {
    #[cfg(windows)]
    {
        recycle_bin_size()
    }
    #[cfg(target_os = "linux")]
    {
        crate::platform::data_home().map(|d| fdo::size_in(&d.join("Trash")))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// Empties the bin for good (Windows: every drive, no UI; Linux: the home Trash, Trash folders
/// on other drives are left alone). Not where [`can_empty`] is false, nor while deleting is off.
pub fn empty() -> Result<(), String> {
    if !crate::platform::Caps::get().delete_safe {
        return Err(format!("emptying the {} is not available on {} yet", trash_word(), crate::platform::os_name()));
    }
    #[cfg(windows)]
    {
        empty_recycle_bin()
    }
    #[cfg(target_os = "linux")]
    {
        match crate::platform::data_home() {
            Some(d) => fdo::empty_in(&d.join("Trash")),
            None => Err(format!("no home folder, so no {}", trash_word())),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(format!("TMOP does not empty the {}; use the Finder", trash_word()))
    }
}

// ---------------------------------------------------------------- Trasher

/// Moves items to the bin, one at a time so a failure is attributed to its item. On Windows it
/// holds COM for the worker thread that created it.
pub struct Trasher {
    #[cfg(windows)]
    com: bool,
}

#[cfg(not(windows))]
impl Trasher {
    pub fn new() -> Self {
        Trasher {}
    }

    /// Moves `path` to the bin, or says why not; never deletes it otherwise.
    pub fn trash(&self, path: &Path, _size: u64) -> Result<(), String> {
        self.trash_anchored(&super::diskdelete::anchored::Anchor::new(path)?)
    }

    /// [`Self::trash`] for an item whose parent folder is already open (see
    /// [`super::diskdelete::anchored`]). Linux moves it relative to that folder; macOS hands the
    /// resolved path to the system after checking it still names the same item.
    pub fn trash_anchored(&self, a: &super::diskdelete::anchored::Anchor) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            fdo::trash_anchored(a, crate::platform::data_home().map(|d| d.join("Trash")).as_deref())
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt;
            let path = a.path();
            let now = std::fs::symlink_metadata(&path).ok().map(|m| (m.dev(), m.ino()));
            if now.is_none() || now != a.id() {
                return Err("the path changed while it was checked; nothing was moved".into());
            }
            mac::trash(&path)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = a;
            Err(format!("there is no {} on this system", trash_word()))
        }
    }
}

#[cfg(windows)]
impl Trasher {
    pub fn new() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: balanced with CoUninitialize in Drop when this succeeded.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Trasher { com }
    }

    pub fn trash(&self, path: &Path, size: u64) -> Result<(), String> {
        use super::diskdelete::{folds_to_sibling, verbatim};
        use windows::core::PCWSTR;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
        use windows::Win32::UI::Shell::{
            FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName, FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        };
        // The exact name is looked up in verbatim form (no trailing dot / space folding to a sibling). The shell
        // rejects verbatim (backslash-backslash-question) paths with E_INVALIDARG, so it gets the plain path, and only for names Win32 cannot fold.
        let vpath = verbatim(path);
        if folds_to_sibling(path) {
            return Err("the name ends with a dot or space; use permanent delete".into());
        }
        if std::fs::symlink_metadata(&vpath).is_err() {
            return Err("no longer exists".into());
        }
        // The shell destroys instead of recycling when the bin is switched off or too small for the
        // item, without saying so: refuse rather than delete permanently behind the dialog's back.
        if let Some(limits) = bin_limits(path) {
            if limits.nuke {
                return Err("the Recycle Bin is turned off for this drive; use permanent delete".into());
            }
            if limits.max_bytes.is_some_and(|m| size > m) {
                return Err("too large for the Recycle Bin; use permanent delete".into());
            }
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
            Ok(false) if std::fs::symlink_metadata(&vpath).is_err() => Ok(()),
            Ok(_) => Err("could not be moved to the Recycle Bin".into()),
            Err(e) if e.code().0 as u32 == 0x8007_0005 => Err("access denied".into()),
            Err(e) => Err(e.message()),
        }
    }
}

#[cfg(windows)]
impl Drop for Trasher {
    fn drop(&mut self) {
        if self.com {
            // SAFETY: matches the successful CoInitializeEx in `new`.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

impl Default for Trasher {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- Windows: the Recycle Bin

/// How the Recycle Bin of a volume is configured.
#[cfg(windows)]
struct BinLimits {
    /// "Don't move files to the Recycle Bin" (volume setting or policy): the shell deletes for good.
    nuke: bool,
    /// MaxCapacity in bytes; bigger items are deleted for good by the shell.
    max_bytes: Option<u64>,
}

#[cfg(windows)]
fn bin_limits(path: &Path) -> Option<BinLimits> {
    use super::diskdelete::norm_path;
    use super::winutil::{Key, View};
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let n = norm_path(path);
    let b = n.as_bytes();
    if b.len() < 3 || b[1] != b':' {
        return None;
    }
    let mut limits = BinLimits { nuke: false, max_bytes: None };
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Some(k) = Key::open(root, r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer", View::Default) {
            limits.nuke |= k.u32("NoRecycleFiles").unwrap_or(0) != 0;
        }
    }
    let mount = super::winutil::wide(&format!("{}:\\", b[0] as char));
    let mut vol = [0u16; 64];
    // SAFETY: NUL-terminated mount point, output buffer sized as passed.
    if unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount.as_ptr()), &mut vol) }.is_ok() {
        // `\?\Volume{guid}\` -> `{guid}`
        let name = super::winutil::from_wide(&vol);
        if let (Some(a), Some(z)) = (name.find('{'), name.find('}')) {
            let sub = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{}", &name[a..=z]);
            if let Some(k) = Key::open(HKEY_CURRENT_USER, &sub, View::Default) {
                limits.nuke |= k.u32("NukeOnDelete").unwrap_or(0) != 0;
                limits.max_bytes = k.u32("MaxCapacity").filter(|&m| m > 0).map(|m| m as u64 * 1024 * 1024);
            }
        }
    }
    Some(limits)
}

/// False for network, optical and RAM volumes, when the volume has no Recycle Bin (removable FAT
/// sticks) or the bin is switched off.
#[cfg(windows)]
pub fn has_recycle_bin(path: &Path) -> bool {
    use super::diskdelete::norm_path;
    if bin_limits(path).is_some_and(|l| l.nuke) {
        return false;
    }
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

/// The volume of `path` has a Recycle Bin size limit on record (`BitBucket\Volume\{guid}\MaxCapacity`).
/// Without one the shell may delete large items for good, so the dialogs must not promise a restore.
#[cfg(windows)]
fn recycle_limit_known(path: &Path) -> bool {
    bin_limits(path).is_some_and(|l| l.max_bytes.is_some())
}

#[cfg(windows)]
fn recycle_bin_size() -> Option<(u64, u64)> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: a null root queries all drives; the info struct has cbSize set.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.ok()?;
    Some((info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64))
}

/// Empties the Recycle Bin of every drive without any Windows UI.
#[cfg(windows)]
fn empty_recycle_bin() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
    // SAFETY: no window, no root path (all drives); flags are plain constants.
    unsafe { SHEmptyRecycleBinW(HWND::default(), PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) }.map_err(|e| e.message())
}

// ---------------------------------------------------------------- Linux: FreeDesktop Trash

/// `path` with its parent chain resolved (links followed) and the item itself not followed.
#[cfg(target_os = "linux")]
fn absolute(path: &Path) -> Result<std::path::PathBuf, String> {
    let name = path.file_name().ok_or("the path has no file name")?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).ok_or("the path is relative")?;
    let parent = std::fs::canonicalize(parent).map_err(|e| super::diskdelete::short_error(&e))?;
    Ok(parent.join(name))
}

/// The highest folder above `item` on the same file system and the same mount (the volume's
/// "top directory"; a bind mount's own root, where the kernel tells mounts apart).
#[cfg(target_os = "linux")]
fn topdir(item: &Path, dev: u64) -> std::path::PathBuf {
    use super::diskdelete::anchored::mount_id_of;
    use std::os::unix::fs::MetadataExt;
    let mut top = item.parent().unwrap_or(item).to_path_buf();
    let mnt = mount_id_of(&top);
    while let Some(up) = top.parent() {
        if std::fs::symlink_metadata(up).map(|m| m.dev()).ok() != Some(dev) || (mnt.is_some() && mount_id_of(up) != mnt) {
            break;
        }
        top = up.to_path_buf();
    }
    top
}

/// `access(path, W_OK | X_OK)`: we may create entries in `path` (false on a read-only mount).
#[cfg(target_os = "linux")]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a NUL-terminated path; access only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

#[cfg(target_os = "linux")]
mod fdo {
    use super::{topdir, trash_word, trashinfo, unique_name, INFO_EXT};
    use crate::pro::diskdelete::anchored::{mount_id_of, Anchor};
    use crate::pro::diskdelete::short_error;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};

    /// A Trash folder (holding `files/` and `info/`) and the top folder its `Path=` values are
    /// relative to (`None` for the home Trash, which stores absolute paths).
    #[derive(Debug, PartialEq, Eq)]
    pub struct Loc {
        pub dir: PathBuf,
        pub topdir: Option<PathBuf>,
    }

    fn uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn mkdir(p: &Path) -> std::io::Result<()> {
        std::fs::DirBuilder::new().mode(0o700).create(p)
    }

    /// Some(true): a real folder (not a link) owned by `uid`; Some(false): something else is
    /// there; None: nothing is.
    fn own_dir(p: &Path, uid: u32) -> Option<bool> {
        let m = std::fs::symlink_metadata(p).ok()?;
        Some(m.is_dir() && !m.file_type().is_symlink() && m.uid() == uid)
    }

    /// The administrator-made `$topdir/.Trash`: a real folder with the sticky bit (spec 1.0).
    fn admin_ok(p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && m.mode() & libc::S_ISVTX != 0)
    }

    /// `files/` and `info/` in `dir`, created when missing; neither may be a link.
    fn make_parts(dir: &Path) -> Result<(), String> {
        for part in ["files", "info"] {
            let p = dir.join(part);
            match mkdir(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("could not create {}: {}", p.display(), short_error(&e))),
            }
            if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
                return Err(format!("{} is not a folder", p.display()));
            }
        }
        Ok(())
    }

    /// The Trash `item` (absolute, parents resolved) goes to: the home Trash `home` when it is
    /// on the same file system, else the volume's `.Trash/$uid` (only when `.Trash` has the
    /// sticky bit and is no link) or `.Trash-$uid`. With `create` the folders are made; without,
    /// only whether they could be. Never another file system: a rename cannot cross one, and
    /// copying is not done.
    pub fn locate(item: &Path, home: Option<&Path>, create: bool) -> Result<Loc, String> {
        let item_dev = std::fs::symlink_metadata(item).map_err(|e| short_error(&e))?.dev();
        let parent = item.parent().ok_or("the path has no parent")?;
        // The folder holding the item says which file system it is on (on an overlay a file's
        // st_dev names the layer it comes from).
        let dev = std::fs::symlink_metadata(parent).map_err(|e| short_error(&e))?.dev();
        // A mount point cannot be renamed: told by the mount id where the kernel says (bind mounts
        // of the same file system included), else by the device.
        let mnt = mount_id_of(parent);
        let is_mount = match (mount_id_of(item), mnt) {
            (Some(a), Some(b)) => a != b,
            _ => item_dev != dev,
        };
        if is_mount {
            return Err(format!("it is a mount point, which cannot go to the {}", trash_word()));
        }
        if let Some(h) = home {
            // The home Trash may not exist yet: its nearest existing folder decides. Same file
            // system and same mount, or the rename would fail.
            let near = h.ancestors().find(|a| std::fs::metadata(a).is_ok()).and_then(|a| std::fs::canonicalize(a).ok());
            let same_mount = mnt.is_none() || near.as_deref().and_then(mount_id_of) == mnt;
            if near.as_deref().and_then(|a| std::fs::metadata(a).ok()).map(|m| m.dev()) == Some(dev) && same_mount {
                if create {
                    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(h).map_err(|e| format!("could not create {}: {}", h.display(), short_error(&e)))?;
                    make_parts(h)?;
                }
                return Ok(Loc { dir: h.to_path_buf(), topdir: None });
            }
        }
        let top = topdir(item, dev);
        let uid = uid();
        let found = |dir: PathBuf| -> Result<Loc, String> {
            if create {
                make_parts(&dir)?;
            }
            Ok(Loc { dir, topdir: Some(top.clone()) })
        };
        let admin = top.join(".Trash");
        if admin_ok(&admin) {
            let mine = admin.join(uid.to_string());
            match own_dir(&mine, uid) {
                Some(true) => return found(mine),
                Some(false) => {}
                None if create => {
                    if mkdir(&mine).is_ok() {
                        return found(mine);
                    }
                }
                None if super::writable(&admin) => return found(mine),
                None => {}
            }
        }
        let alt = top.join(format!(".Trash-{uid}"));
        match own_dir(&alt, uid) {
            Some(true) => return found(alt),
            Some(false) => return Err(format!("{} is not a folder of yours, so this volume has no {} you can use", alt.display(), trash_word())),
            None if create => {
                if mkdir(&alt).is_ok() {
                    return found(alt);
                }
            }
            None if super::writable(&top) => return found(alt),
            None => {}
        }
        Err(format!("{} is read-only or not writable, so this volume has no {}", top.display(), trash_word()))
    }

    fn cvt(rc: libc::c_long) -> std::io::Result<()> {
        if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Moves the anchored item to `to` without ever replacing an existing `to` (`renameat2` with
    /// `RENAME_NOREPLACE`). Where the file system does not support the flag, a file or link is
    /// hard-linked (which fails on an existing name) and then unlinked; a folder, or a file on a
    /// file system without hard links, is renamed after checking the name is free (its record in
    /// `info/` was created exclusively first, which reserves the name among Trash users).
    fn rename_noreplace(a: &Anchor, to: &Path) -> std::io::Result<()> {
        let b = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let (dir, name) = (a.dir_fd(), a.cname());
        // SAFETY: a NUL-terminated name in an open folder and a NUL-terminated absolute path.
        let rc = unsafe { libc::syscall(libc::SYS_renameat2, dir, name.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) {
            return Err(e);
        }
        if a.kind().ok() != Some(Some(true)) {
            // SAFETY: as above; flags 0: a link is linked itself, never followed.
            match cvt(unsafe { libc::linkat(dir, name.as_ptr(), libc::AT_FDCWD, b.as_ptr(), 0) } as libc::c_long) {
                Ok(()) => {
                    // SAFETY: as above.
                    return match cvt(unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } as libc::c_long) {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            let _ = std::fs::remove_file(to);
                            Err(e)
                        }
                    };
                }
                Err(e) if !matches!(e.raw_os_error(), Some(libc::EPERM) | Some(libc::EOPNOTSUPP) | Some(libc::EMLINK)) => return Err(e),
                Err(_) => {}
            }
        }
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        // SAFETY: as above.
        cvt(unsafe { libc::renameat(dir, name.as_ptr(), libc::AT_FDCWD, b.as_ptr()) } as libc::c_long)
    }

    /// Moves `path` into its Trash (see [`trash_anchored`]).
    #[cfg(test)]
    pub fn trash_in(path: &Path, home: Option<&Path>) -> Result<(), String> {
        trash_anchored(&Anchor::new(path)?, home)
    }

    /// Moves the anchored item into its Trash (see [`locate`]): writes `info/<name>.trashinfo`
    /// first (created exclusively, so two moves never share a name), then renames the item,
    /// relative to its open parent folder, to `files/<name>`. When the rename fails the record is
    /// removed again and the item stays where it was.
    pub fn trash_anchored(a: &Anchor, home: Option<&Path>) -> Result<(), String> {
        let item = a.path();
        let loc = locate(&item, home, true)?;
        let real = std::fs::canonicalize(&loc.dir).unwrap_or_else(|_| loc.dir.clone());
        if real.starts_with(&item) || item.starts_with(&real) {
            return Err(format!("that is the {} itself or holds it", trash_word()));
        }
        let shown = match &loc.topdir {
            Some(t) => item.strip_prefix(t).unwrap_or(&item).as_os_str().as_bytes().to_vec(),
            None => item.as_os_str().as_bytes().to_vec(),
        };
        let text = trashinfo(&shown, crate::platform::local_time());
        let name = item.file_name().ok_or("the path has no file name")?.as_bytes().to_vec();
        for n in 0..10_000 {
            let cand = OsString::from_vec(unique_name(&name, if n == 0 { 0 } else { n + 1 }));
            let mut info_name = cand.clone();
            info_name.push(INFO_EXT);
            let info = loc.dir.join("info").join(&info_name);
            let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info);
            let mut file = match file {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("could not write to the {}: {}", trash_word(), short_error(&e))),
            };
            let dest = loc.dir.join("files").join(&cand);
            let undo = |why: String| {
                let _ = std::fs::remove_file(&info);
                Err(why)
            };
            if std::fs::symlink_metadata(&dest).is_ok() {
                drop(file);
                let _ = std::fs::remove_file(&info);
                continue;
            }
            use std::io::Write;
            if let Err(e) = file.write_all(text.as_bytes()).and_then(|_| file.sync_all()) {
                return undo(format!("could not write to the {}: {}", trash_word(), short_error(&e)));
            }
            drop(file);
            return match rename_noreplace(a, &dest) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = std::fs::remove_file(&info);
                    continue;
                }
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) => undo(format!("it is on another file system than its {}; nothing was moved", trash_word())),
                Err(e) => undo(short_error(&e)),
            };
        }
        Err(format!("no free name in the {}", trash_word()))
    }

    /// (bytes, items) of the Trash folder `dir`: everything below `files/` and the
    /// `.trashinfo` records. An absent Trash is empty.
    pub fn size_in(dir: &Path) -> (u64, u64) {
        let count = std::fs::read_dir(dir.join("info")).map(|rd| rd.flatten().filter(|e| e.file_name().as_bytes().ends_with(INFO_EXT.as_bytes())).count()).unwrap_or(0);
        let (bytes, _) = crate::pro::cleanup::dir_stats(&dir.join("files"), usize::MAX);
        (bytes, count as u64)
    }

    /// Deletes everything in the Trash folder `dir` for good: each entry of `files/` (never
    /// following links or entering other file systems), then its record, then records without
    /// an entry and the `directorysizes` cache. An entry that cannot be removed keeps its record.
    pub fn empty_in(dir: &Path) -> Result<(), String> {
        let (files, info) = (dir.join("files"), dir.join("info"));
        for d in [&files, &info] {
            if let Ok(m) = std::fs::symlink_metadata(d) {
                if !m.is_dir() || m.file_type().is_symlink() {
                    return Err(format!("{} is not a folder; nothing was removed", d.display()));
                }
            }
        }
        let record = |name: &OsStr| {
            let mut n = name.to_os_string();
            n.push(INFO_EXT);
            info.join(n)
        };
        let (mut failed, mut first) = (0usize, None);
        if let Ok(rd) = std::fs::read_dir(&files) {
            for e in rd.flatten() {
                match crate::pro::diskdelete::delete_permanent(&e.path()) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(record(&e.file_name()));
                    }
                    Err(why) => {
                        failed += 1;
                        first.get_or_insert(format!("{}: {why}", e.path().display()));
                    }
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(&info) {
            for e in rd.flatten() {
                let n = e.file_name();
                let Some(entry) = n.as_bytes().strip_suffix(INFO_EXT.as_bytes()) else { continue };
                if std::fs::symlink_metadata(files.join(OsStr::from_bytes(entry))).is_err() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let _ = std::fs::remove_file(dir.join("directorysizes"));
        match (failed, first) {
            (0, _) | (_, None) => Ok(()),
            (1, Some(f)) => Err(format!("1 item could not be removed: {f}")),
            (n, Some(f)) => Err(format!("{n} items could not be removed, e.g. {f}")),
        }
    }
}

// ---------------------------------------------------------------- macOS: NSFileManager

#[cfg(target_os = "macos")]
mod mac {
    use super::trash_word;
    use std::path::Path;

    /// Network volumes have no Trash (the Finder deletes there at once), read-only ones cannot
    /// take one. Everything else gets `.Trashes/$uid` from the system when needed.
    pub fn unavailable(path: &Path) -> Option<String> {
        use std::os::unix::ffi::OsStrExt;
        // The folder holding the item decides (statfs follows links: a link on this volume to a
        // network share is trashed here).
        let near = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(path).ancestors().find(|a| std::fs::symlink_metadata(a).is_ok())?;
        let c = std::ffi::CString::new(near.as_os_str().as_bytes()).ok()?;
        // SAFETY: an all-zero statfs is valid; statfs reads the NUL-terminated path and fills `s`.
        let mut s: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        let point: Vec<u8> = s.f_mntonname.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        let point = String::from_utf8_lossy(&point).into_owned();
        let flags = s.f_flags as u64;
        if flags & libc::MNT_LOCAL as u64 == 0 {
            Some(format!("{point} is a network volume, which has no {}", trash_word()))
        } else if flags & libc::MNT_RDONLY as u64 != 0 {
            Some(format!("{point} is read-only, so it has no {}", trash_word()))
        } else {
            None
        }
    }

    /// `NSFileManager trashItemAtURL:resultingItemURL:error:`, which records where the item came
    /// from, so the Finder's Put Back works.
    pub fn trash(path: &Path) -> Result<(), String> {
        trash_to(path).map(|_| ())
    }

    /// [`trash`], also giving where the item went (when the system says).
    pub fn trash_to(path: &Path) -> Result<Option<std::path::PathBuf>, String> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};
        let md = std::fs::symlink_metadata(path).map_err(|e| crate::pro::diskdelete::short_error(&e))?;
        let s = path.to_str().ok_or("the name is not valid UTF-8")?;
        objc2::rc::autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(s), md.is_dir());
            let mut went = None;
            NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, Some(&mut went)).map_err(|e| e.localizedDescription().to_string())?;
            Ok(went.and_then(|u| u.to_file_path()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_round_trips() {
        assert_eq!(encode_path(b"/home/alex/a b/x'y(1).txt"), "/home/alex/a%20b/x'y(1).txt");
        assert_eq!(encode_path("/tmp/ä%,\\#?".as_bytes()), "/tmp/%C3%A4%25%2C%5C%23%3F");
        assert_eq!(encode_path(b"rel/\n\x7f"), "rel/%0A%7F");
        for raw in [&b"/x/a b/\xff\xfe"[..], "/naïve/日本".as_bytes(), b"%41", b""] {
            assert_eq!(decode_path(&encode_path(raw)).as_deref(), Some(raw));
        }
        assert_eq!(decode_path("a%2"), None);
        assert_eq!(decode_path("a%zz"), None);
        assert_eq!(decode_path("%41%62c").unwrap(), b"Abc");
    }

    #[test]
    fn trashinfo_round_trips() {
        let text = trashinfo(b"/home/alex/My File #1.txt", (2026, 3, 7, 9, 5, 2));
        assert_eq!(text, "[Trash Info]\nPath=/home/alex/My%20File%20%231.txt\nDeletionDate=2026-03-07T09:05:02\n");
        assert_eq!(parse_trashinfo(&text), Some((b"/home/alex/My File #1.txt".to_vec(), "2026-03-07T09:05:02".into())));
        // Relative to the volume's top folder; other groups and unknown keys are ignored.
        let other = "# c\n[Trash Info]\nX=1\nDeletionDate=2020-01-01T00:00:00\nPath=dir/a%20b\n[Other]\nPath=/nope\n";
        assert_eq!(parse_trashinfo(other), Some((b"dir/a b".to_vec(), "2020-01-01T00:00:00".into())));
        for bad in ["", "Path=/x\n", "[Other]\nPath=/x\n", "[Trash Info]\nDeletionDate=2020-01-01T00:00:00\n", "[Trash Info]\nPath=%zz\n"] {
            assert_eq!(parse_trashinfo(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn collision_names() {
        let s = |n: &[u8], k| String::from_utf8(unique_name(n, k)).unwrap();
        assert_eq!(s(b"a.txt", 0), "a.txt");
        assert_eq!(s(b"a.txt", 2), "a.2.txt");
        assert_eq!(s(b"archive.tar.gz", 3), "archive.tar.3.gz");
        assert_eq!(s(b".bashrc", 2), ".bashrc.2");
        assert_eq!(s(b"Makefile", 10), "Makefile.10");
        // Long names are cut so that `<name>.trashinfo` fits, never inside a character.
        let long = "é".repeat(200);
        for k in [0, 2, 999] {
            let n = unique_name(long.as_bytes(), k);
            assert!(n.len() + INFO_EXT.len() <= NAME_MAX, "{k}: {}", n.len());
            assert!(std::str::from_utf8(&n).is_ok());
        }
        assert!(s(format!("{}.txt", "x".repeat(300)).as_bytes(), 2).ends_with(".2.txt"));
    }

    #[test]
    fn volumes_are_asked_once() {
        let mut v = Volumes::default();
        let here = std::env::temp_dir().join(format!("tmop-trash-vols-{}", std::process::id()));
        std::fs::create_dir_all(&here).unwrap();
        let (a, b) = (here.join("a"), here.join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        // A sibling that is gone first: it is not asked about, and nothing is remembered for it.
        assert_eq!(v.check(&here.join("gone")), (None, true));
        assert!(v.seen.is_empty());
        assert_eq!(v.check(&a), v.check(&b));
        assert_eq!(v.seen.len(), 1);
        let _ = std::fs::remove_dir_all(&here);
    }

    #[test]
    fn availability_per_os() {
        assert_eq!(supported(), cfg!(any(windows, target_os = "linux", target_os = "macos")));
        assert_eq!(can_empty(), cfg!(any(windows, target_os = "linux")));
        if cfg!(not(windows)) {
            assert!(limit_known(Path::new("/x")), "the Trash has no size limit");
        }
    }

    /// NSFileManager moves a temp file into the user's Trash (Put Back recorded by the system):
    /// it leaves its folder and turns up in a `.Trash` / `.Trashes` folder, then is removed there.
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_trashes_a_temp_file() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("tmop-mac-trash-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(mac::unavailable(&dir), None, "the temp folder is on a local, writable volume");
        assert_eq!(unavailable(&dir.join("not-yet")), None, "decided by the volume, not the item");
        let f = dir.join("tmop trash me.txt");
        std::fs::write(&f, b"x").unwrap();
        let went = mac::trash_to(&f).unwrap();
        assert!(std::fs::symlink_metadata(&f).is_err(), "still in its folder");
        let went = went.expect("the system says where the item went");
        assert!(std::fs::symlink_metadata(&went).is_ok(), "{}", went.display());
        assert!(went.ancestors().any(|a| a.file_name().is_some_and(|n| n == ".Trash" || n == ".Trashes")), "{}", went.display());
        let _ = std::fs::remove_file(&went);
        assert!(mac::trash(&f).is_err(), "a missing item is an error, nothing else is moved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    fn sandbox(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("tmop-trash-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The home Trash with `XDG_DATA_HOME` pointed at a temp folder: the item lands in `files/`,
    /// its record in `info/` with the absolute, percent-encoded path; a second item of the same
    /// name gets another one; emptying removes both.
    #[cfg(target_os = "linux")]
    #[test]
    fn trashes_into_the_home_trash() {
        use std::ffi::OsString;
        let root = sandbox("home");
        let data = root.join("data");
        let env = |k: &str| (k == "XDG_DATA_HOME").then(|| OsString::from(data.as_os_str()));
        let home = crate::platform::xdg::data_home(&env).unwrap().join("Trash");
        assert_eq!(home, data.join("Trash"));
        use fdo::{empty_in, size_in, trash_in};
        let victim = root.join("my file #1.txt");
        std::fs::write(&victim, b"hello").unwrap();
        let abs = absolute(&victim).unwrap();
        assert_eq!(fdo::locate(&abs, Some(&home), false).map(|l| l.dir), Ok(home.clone()));
        trash_in(&victim, Some(&home)).unwrap();
        assert!(!victim.exists());
        assert_eq!(std::fs::read(home.join("files").join("my file #1.txt")).unwrap(), b"hello");
        let info = std::fs::read_to_string(home.join("info").join("my file #1.txt.trashinfo")).unwrap();
        let (path, date) = parse_trashinfo(&info).unwrap();
        assert_eq!(path, abs.as_os_str().as_encoded_bytes());
        assert!(info.contains("%20") && info.contains("%23") && date.len() == 19 && date.as_bytes()[10] == b'T', "{info}");
        // Same name again, and a folder.
        std::fs::write(&victim, b"again").unwrap();
        trash_in(&victim, Some(&home)).unwrap();
        assert_eq!(std::fs::read(home.join("files").join("my file #1.2.txt")).unwrap(), b"again");
        assert!(home.join("info").join("my file #1.2.txt.trashinfo").exists());
        let dir = root.join("folder");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("x"), [0u8; 10]).unwrap();
        trash_in(&dir, Some(&home)).unwrap();
        assert!(!dir.exists() && home.join("files").join("folder").join("sub").join("x").exists());
        assert_eq!(size_in(&home), (5 + 5 + 10, 3));
        // The Trash itself and what holds it never go into it.
        assert!(trash_in(&data, Some(&home)).is_err() && data.exists());
        assert!(trash_in(&root.join("missing"), Some(&home)).is_err());
        empty_in(&home).unwrap();
        assert_eq!(size_in(&home), (0, 0));
        assert_eq!(std::fs::read_dir(home.join("files")).unwrap().count() + std::fs::read_dir(home.join("info")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Without a home Trash on its file system an item uses the volume's `.Trash-$uid`, and its
    /// record holds the path relative to the volume's top folder.
    #[cfg(target_os = "linux")]
    #[test]
    fn volume_trash_without_a_home_trash() {
        use std::os::unix::fs::MetadataExt;
        let root = sandbox("vol");
        let victim = root.join("v.bin");
        std::fs::write(&victim, b"v").unwrap();
        let item = absolute(&victim).unwrap();
        let top = topdir(&item, std::fs::metadata(&item).unwrap().dev());
        // The top folder of the temp file system may not be writable (`/` without root): then
        // there is no Trash there unless one was made before.
        match fdo::locate(&item, None, false) {
            Ok(loc) => {
                assert_eq!(loc.topdir.as_deref(), Some(top.as_path()));
                // SAFETY: geteuid has no preconditions and cannot fail.
                let uid = unsafe { libc::geteuid() };
                assert!(loc.dir == top.join(".Trash").join(uid.to_string()) || loc.dir == top.join(format!(".Trash-{uid}")), "{loc:?}");
            }
            Err(why) => assert!(!writable(&top) && why.contains("no Trash"), "{why}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An item on another file system than the home Trash (`/dev/shm`, a tmpfs) goes to that
    /// volume's `.Trash-$uid`, with its path relative to the volume's top folder; nothing is copied.
    #[cfg(target_os = "linux")]
    #[test]
    fn other_file_system_uses_the_volume_trash() {
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        let home_root = sandbox("other");
        let home = home_root.join("Trash");
        let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        if dev(shm).is_none() || dev(shm) == dev(&home_root) || !writable(shm) {
            let _ = std::fs::remove_dir_all(&home_root);
            return;
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let vol = shm.join(format!(".Trash-{uid}"));
        let made = !vol.exists() && !shm.join(".Trash").exists();
        let name = format!("tmop-vol-{}.txt", std::process::id());
        let victim = shm.join(&name);
        std::fs::write(&victim, b"v").unwrap();
        let loc = fdo::locate(&victim, Some(&home), false).unwrap();
        assert_eq!(loc.topdir.as_deref(), Some(shm));
        fdo::trash_in(&victim, Some(&home)).unwrap();
        assert!(!victim.exists() && !home.exists(), "nothing went to the home Trash");
        let info = std::fs::read_to_string(loc.dir.join("info").join(format!("{name}.trashinfo"))).unwrap();
        assert_eq!(parse_trashinfo(&info).unwrap().0, name.as_bytes(), "relative to the top folder");
        assert_eq!(std::fs::read(loc.dir.join("files").join(&name)).unwrap(), b"v");
        let _ = std::fs::remove_file(loc.dir.join("files").join(&name));
        let _ = std::fs::remove_file(loc.dir.join("info").join(format!("{name}.trashinfo")));
        if made {
            let _ = std::fs::remove_dir_all(&vol);
        }
        let _ = std::fs::remove_dir_all(&home_root);
    }

    /// `mount --bind from to`, where this process may (root, or `unshare -Urm`); false otherwise.
    #[cfg(target_os = "linux")]
    fn bind(from: &Path, to: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        let (a, b) = (c(from), c(to));
        // SAFETY: two NUL-terminated paths; no file system type or data for a bind mount.
        let ok = unsafe { libc::mount(a.as_ptr(), b.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) == 0 };
        // CI runs these tests in a mount namespace and sets this, so a skip there is a failure.
        assert!(ok || std::env::var_os("TMOP_TEST_REQUIRE_MOUNT").is_none(), "bind mount failed: {}", std::io::Error::last_os_error());
        ok
    }

    #[cfg(target_os = "linux")]
    fn unmount(p: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
    }

    /// S1: a folder in the Trash holding a bind mount of the same file system (a file manager
    /// moved it there with the mount inside): emptying the Trash never empties the mounted
    /// folder. Needs mount rights (root or `unshare -Urm`).
    #[cfg(target_os = "linux")]
    #[test]
    fn emptying_never_enters_a_bind_mount_in_the_trash() {
        let root = sandbox("empty-bind");
        let (home, docs) = (root.join("Trash"), root.join("docs"));
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("precious.txt"), b"p").unwrap();
        let proj = home.join("files").join("proj");
        std::fs::create_dir_all(proj.join("data")).unwrap();
        std::fs::create_dir_all(home.join("info")).unwrap();
        if !bind(&docs, &proj.join("data")) {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let e = fdo::empty_in(&home).unwrap_err();
        assert!(e.contains("mount"), "{e}");
        assert!(docs.join("precious.txt").exists(), "the mounted folder was emptied");
        unmount(&proj.join("data"));
        fdo::empty_in(&home).unwrap();
        assert!(!proj.exists() && docs.join("precious.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// T3: an item inside a bind mount of a folder of the home Trash's file system cannot be
    /// renamed into the home Trash (another mount): its Trash is decided at the mount, in advance.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_bind_mount_is_not_the_home_trash_mount() {
        let root = sandbox("bind-home");
        let (home, src, dst) = (root.join("data").join("Trash"), root.join("src"), root.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        if !bind(&src, &dst) {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        std::fs::write(dst.join("f"), b"f").unwrap();
        match fdo::locate(&dst.join("f"), Some(&home), false) {
            Ok(loc) => {
                assert_ne!(loc.dir, home, "{loc:?}");
                assert_eq!(loc.topdir.as_deref(), Some(dst.as_path()));
            }
            Err(why) => assert!(!why.is_empty()),
        }
        let moved = fdo::trash_in(&dst.join("f"), Some(&home));
        assert!(moved.is_ok() || dst.join("f").exists(), "{moved:?}");
        unmount(&dst);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Never another file system: a mount point is refused, and so is a home Trash on another device.
    #[cfg(target_os = "linux")]
    #[test]
    fn mount_points_are_refused() {
        if std::fs::symlink_metadata("/proc").is_ok() {
            assert!(fdo::locate(Path::new("/proc"), None, false).unwrap_err().contains("mount point"));
        }
    }
}
