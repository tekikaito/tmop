//! Deleting off Windows through descriptors, so what was judged is what goes.
//!
//! [`Anchor::new`] resolves the parent folder once, one component at a time from `/`, never
//! following a link owned by anyone but root or this user (another user could swap it), and keeps
//! that folder open. Everything after that is relative to the open folder: a later rename or a
//! symlink swapped into the path cannot redirect the delete. [`remove`] walks the tree itself
//! (`openat` with `O_NOFOLLOW`, `unlinkat`) and refuses to enter another file system or another
//! mount, bind mounts of the same file system included (their mount id differs). It checks the
//! whole tree before removing anything, and checks again while removing.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// The parent folder of an item, open, and the item's name in it.
pub struct Anchor {
    dir: OwnedFd,
    /// The parent folder as resolved (no links).
    pub parent: PathBuf,
    pub name: OsString,
    cname: CString,
}

fn cstr(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// Flags for opening a folder that is only walked through, never listed.
#[cfg(target_os = "linux")]
const WALK: libc::c_int = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const WALK: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
/// Flags for opening a folder whose entries are read.
const LIST: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_at(dir: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: a NUL-terminated name relative to an open folder (or AT_FDCWD); the result is a new fd we own.
    let fd = cvt(unsafe { libc::openat(dir, name.as_ptr(), flags) })?;
    // SAFETY: `fd` was just returned by openat and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `fstatat` without following a link; an empty name stats `dir` itself.
fn stat_at(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    // SAFETY: an all-zero stat is valid; fstatat fills it.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if name.to_bytes().is_empty() {
        // SAFETY: `dir` is an open descriptor; fstat fills `st`.
        cvt(unsafe { libc::fstat(dir, &mut st) })?;
    } else {
        // SAFETY: as above; `name` is NUL-terminated.
        cvt(unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    }
    Ok(st)
}

/// The mount id of `name` in `dir` (or of `dir` itself for an empty name), where the kernel says
/// (Linux 5.8+). Differs between a bind mount and the folder holding it, on the same file system too.
#[cfg(target_os = "linux")]
fn mount_id(dir: RawFd, name: &CStr) -> Option<u64> {
    // SAFETY: an all-zero statx is valid; statx fills it.
    let mut sx: libc::statx = unsafe { std::mem::zeroed() };
    let flags = libc::AT_SYMLINK_NOFOLLOW | if name.to_bytes().is_empty() { libc::AT_EMPTY_PATH } else { 0 };
    // SAFETY: as above; `name` is NUL-terminated and `dir` is open.
    let rc = unsafe { libc::statx(dir, name.as_ptr(), flags, libc::STATX_MNT_ID, &mut sx) };
    (rc == 0 && sx.stx_mask & libc::STATX_MNT_ID != 0).then_some(sx.stx_mnt_id)
}

#[cfg(not(target_os = "linux"))]
fn mount_id(_dir: RawFd, _name: &CStr) -> Option<u64> {
    None
}

/// The mount id of `path` (links not followed), where the kernel says.
#[cfg(target_os = "linux")]
pub fn mount_id_of(path: &Path) -> Option<u64> {
    let c = cstr(path.as_os_str().as_bytes()).ok()?;
    mount_id(libc::AT_FDCWD, &c)
}

fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn is_link(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFLNK
}

/// st_dev as u64 (i32 on macOS).
#[allow(clippy::unnecessary_cast)]
fn dev_of(st: &libc::stat) -> u64 {
    st.st_dev as u64
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn short(e: &io::Error) -> String {
    super::short_error(e)
}

impl Anchor {
    /// Opens the parent folder of the absolute `path` (see the module docs). Refuses a relative
    /// path, `..`, a name of `.` or `..` and a link in the parent chain owned by another user.
    pub fn new(path: &Path) -> Result<Anchor, String> {
        if !path.has_root() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("refusing a relative path or a path with '..'".into());
        }
        let name = path.file_name().ok_or("the path has no file name")?.to_os_string();
        let cname = cstr(name.as_bytes()).map_err(|e| short(&e))?;
        let me = euid();
        let root = cstr(b"/").map_err(|e| short(&e))?;
        let mut cur = open_at(libc::AT_FDCWD, &root, WALK).map_err(|e| short(&e))?;
        let mut stack: Vec<OsString> = Vec::new();
        let parent = path.parent().unwrap_or(Path::new("/"));
        let mut todo: Vec<OsString> = parent.components().filter(|c| matches!(c, std::path::Component::Normal(_))).map(|c| c.as_os_str().to_os_string()).rev().collect();
        let mut hops = 0;
        while let Some(c) = todo.pop() {
            if c == "." {
                continue;
            }
            let cc = cstr(c.as_bytes()).map_err(|e| short(&e))?;
            if c == ".." {
                cur = open_at(cur.as_raw_fd(), &cc, WALK).map_err(|e| short(&e))?;
                stack.pop();
                continue;
            }
            match open_at(cur.as_raw_fd(), &cc, WALK) {
                Ok(fd) => {
                    cur = fd;
                    stack.push(c);
                }
                Err(e) => {
                    let st = stat_at(cur.as_raw_fd(), &cc).map_err(|_| short(&e))?;
                    if !is_link(&st) {
                        return Err(short(&e));
                    }
                    let here = Path::new("/").join(stack.iter().collect::<PathBuf>()).join(&c);
                    if st.st_uid != 0 && st.st_uid != me {
                        return Err(format!("the path goes through a link owned by another user ({}); nothing was changed", here.display()));
                    }
                    hops += 1;
                    if hops > 40 {
                        return Err("too many links in the path".into());
                    }
                    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
                    // SAFETY: the buffer is as long as passed; readlinkat does not NUL-terminate, the length is returned.
                    let n = unsafe { libc::readlinkat(cur.as_raw_fd(), cc.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
                    let again = stat_at(cur.as_raw_fd(), &cc).map_err(|e| short(&e))?;
                    if n < 0 || n as usize >= buf.len() || (again.st_dev, again.st_ino, again.st_uid) != (st.st_dev, st.st_ino, st.st_uid) {
                        return Err(format!("the link {} changed while it was read; nothing was changed", here.display()));
                    }
                    buf.truncate(n as usize);
                    let target = PathBuf::from(OsString::from_vec(buf));
                    if target.has_root() {
                        cur = open_at(libc::AT_FDCWD, &root, WALK).map_err(|e| short(&e))?;
                        stack.clear();
                    }
                    todo.extend(target.components().filter_map(|c| match c {
                        std::path::Component::Normal(n) => Some(n.to_os_string()),
                        std::path::Component::ParentDir => Some(OsString::from("..")),
                        _ => None,
                    }).collect::<Vec<_>>().into_iter().rev());
                }
            }
        }
        let parent = Path::new("/").join(stack.iter().collect::<PathBuf>());
        Ok(Anchor { dir: cur, parent, name, cname })
    }

    /// The item as resolved: the parent folder without links, and the name.
    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    /// The open parent folder.
    pub fn dir_fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }

    /// The name as a C string.
    pub fn cname(&self) -> &CStr {
        &self.cname
    }

    /// The item itself (a link is not followed): Ok(Some(is a folder)), Ok(None) for a link.
    pub fn kind(&self) -> Result<Option<bool>, String> {
        let st = stat_at(self.dir_fd(), &self.cname).map_err(|e| short(&e))?;
        Ok((!is_link(&st)).then(|| is_dir(&st)))
    }

    /// (st_dev, st_ino) of the item (link not followed).
    pub fn id(&self) -> Option<(u64, u64)> {
        stat_at(self.dir_fd(), &self.cname).ok().map(|st| (dev_of(&st), st.st_ino))
    }
}

/// What stops a removal.
enum Stop {
    /// Another file system or mount at this path: nothing below it is entered.
    Mount(PathBuf),
    /// Another account's home folder (the path, the home as that account's entry lists it).
    Home(PathBuf, String),
    /// Folders nested deeper than [`MAX_DEPTH`].
    Deep,
    Io(io::Error),
}

/// Deeper trees are refused (one descriptor and one stack frame per level).
const MAX_DEPTH: usize = 512;

struct Walk<'a> {
    dev: u64,
    mnt: Option<u64>,
    removed: u64,
    /// This user (folders of other accounts are checked against their home folders).
    me: u32,
    /// The home folder of an account (the user database).
    home_of: &'a dyn Fn(u32) -> Option<String>,
    homes: std::collections::HashMap<u32, Option<(String, Vec<String>)>>,
}

impl Walk<'_> {
    /// `path` (a folder owned by `uid`) is that account's home folder or inside it, where the
    /// account is neither root nor this user: the home as listed, when so.
    fn foreign_home(&mut self, uid: u32, path: &Path) -> Option<String> {
        use super::unix::{norm_unix, under, FLAVOR};
        if uid == 0 || uid == self.me {
            return None;
        }
        let home_of = self.home_of;
        // (as listed, its normalised forms as given and resolved), once per account.
        let (h, hs) = self
            .homes
            .entry(uid)
            .or_insert_with(|| {
                let h = home_of(uid)?;
                let real = std::fs::canonicalize(&h).ok().map(|c| c.to_string_lossy().into_owned());
                let hs = [Some(h.clone()), real].into_iter().flatten().map(|x| norm_unix(FLAVOR, &x)).filter(|x| x.starts_with('/') && x != "/" && !x.split('/').any(|c| c == "..")).collect();
                Some((h, hs))
            })
            .as_ref()?;
        let p = norm_unix(FLAVOR, &path.to_string_lossy());
        hs.iter().any(|x: &String| under(&p, x)).then(|| h.clone())
    }

    /// The folder `name` in `dir` (`path` for messages, `depth` levels below the item), checked
    /// and, unless `dry`, removed.
    fn dir(&mut self, dir: RawFd, name: &CStr, path: &Path, depth: usize, dry: bool) -> Result<(), Stop> {
        if depth > MAX_DEPTH {
            return Err(Stop::Deep);
        }
        let sub = match open_at(dir, name, LIST) {
            Ok(fd) => fd,
            // Replaced by a link or a file since it was listed: remove that, never follow it.
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)) => return self.leaf(dir, name, dry),
            Err(e) => return Err(Stop::Io(e)),
        };
        let st = stat_at(sub.as_raw_fd(), c"").map_err(Stop::Io)?;
        if dev_of(&st) != self.dev || (self.mnt.is_some() && mount_id(sub.as_raw_fd(), c"") != self.mnt) {
            return Err(Stop::Mount(path.to_path_buf()));
        }
        if let Some(h) = self.foreign_home(st.st_uid, path) {
            return Err(Stop::Home(path.to_path_buf(), h));
        }
        // The entries first, with the listing stream closed before going deeper (one fd per level).
        let mut names: Vec<CString> = Vec::new();
        {
            let dup = open_at(sub.as_raw_fd(), c".", LIST).map_err(Stop::Io)?;
            let raw = dup.as_raw_fd();
            // SAFETY: fdopendir takes over `raw` (forgotten below) and is closed with closedir.
            let d = unsafe { libc::fdopendir(raw) };
            if d.is_null() {
                return Err(Stop::Io(io::Error::last_os_error()));
            }
            std::mem::forget(dup);
            loop {
                // SAFETY: `d` is a valid open DIR stream; the entry is copied before the next call.
                let e = unsafe { libc::readdir(d) };
                if e.is_null() {
                    break;
                }
                // SAFETY: d_name is NUL-terminated within the entry.
                let n = unsafe { CStr::from_ptr((*e).d_name.as_ptr()) };
                if n.to_bytes() != b"." && n.to_bytes() != b".." {
                    names.push(n.to_owned());
                }
            }
            // SAFETY: closes the stream opened above (and its fd).
            unsafe { libc::closedir(d) };
        }
        // Files and links first, then the folders.
        let mut dirs: Vec<&CString> = Vec::new();
        for n in &names {
            match stat_at(sub.as_raw_fd(), n) {
                Ok(st) if is_dir(&st) => dirs.push(n),
                Ok(_) => self.leaf(sub.as_raw_fd(), n, dry)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(Stop::Io(e)),
            }
        }
        for n in dirs {
            self.dir(sub.as_raw_fd(), n, &path.join(OsStr::from_bytes(n.to_bytes())), depth + 1, dry)?;
        }
        if !dry {
            // SAFETY: a NUL-terminated name in an open folder.
            cvt(unsafe { libc::unlinkat(dir, name.as_ptr(), libc::AT_REMOVEDIR) }).map_err(Stop::Io)?;
            self.removed += 1;
        }
        Ok(())
    }

    fn leaf(&mut self, dir: RawFd, name: &CStr, dry: bool) -> Result<(), Stop> {
        if !dry {
            // SAFETY: a NUL-terminated name in an open folder; a link is removed, never followed.
            match cvt(unsafe { libc::unlinkat(dir, name.as_ptr(), 0) }) {
                Ok(_) => self.removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(Stop::Io(e)),
            }
        }
        Ok(())
    }
}

/// Mount points listed by the system at or below `item` (absolute, parents resolved).
fn mounts_at_or_below(item: &Path) -> Option<String> {
    use super::unix::{norm_unix, under, FLAVOR};
    let n = norm_unix(FLAVOR, &item.to_string_lossy());
    super::super::mounts::table().points.into_iter().find(|m| under(&norm_unix(FLAVOR, m), &n))
}

/// Deletes the item for good (a link itself, never its target). A folder is refused when it is
/// or holds a mount point (by the mount table, the device and the mount id of every folder) or
/// another account's home folder (every folder owned by another account is looked up in the user
/// database), or is nested deeper than [`MAX_DEPTH`]; then nothing is deleted. When the removal
/// fails part way, the error says so.
pub fn remove(a: &Anchor) -> Result<(), String> {
    use super::unix::UnixFs;
    remove_with(a, euid(), &|uid| super::unix::RealFs.home_of(uid))
}

fn remove_with(a: &Anchor, me: u32, home_of: &dyn Fn(u32) -> Option<String>) -> Result<(), String> {
    let st = stat_at(a.dir_fd(), a.cname()).map_err(|e| short(&e))?;
    if !is_dir(&st) {
        // SAFETY: a NUL-terminated name in the open parent; a link is removed, never followed.
        return cvt(unsafe { libc::unlinkat(a.dir_fd(), a.cname().as_ptr(), 0) }).map(|_| ()).map_err(|e| short(&e));
    }
    if let Some(m) = mounts_at_or_below(&a.path()) {
        return Err(format!("{} mount point ({m}); nothing was deleted", if Path::new(&m) == a.path() { "it is a" } else { "contains a" }));
    }
    // The parent's device and mount: the item and everything below must share both.
    let parent = stat_at(a.dir_fd(), c"").map_err(|e| short(&e))?;
    let mut w = Walk { dev: dev_of(&parent), mnt: mount_id(a.dir_fd(), c""), removed: 0, me, home_of, homes: Default::default() };
    let stop = |s: Stop, removed: u64| -> String {
        let why = match s {
            Stop::Mount(p) => format!("{} is another file system or mount", p.display()),
            Stop::Home(p, h) => format!("{} is in another user's home folder ({h})", p.display()),
            Stop::Deep => format!("folders are nested more than {MAX_DEPTH} levels deep"),
            Stop::Io(e) => short(&e),
        };
        if removed > 0 {
            format!("partly deleted ({removed} items removed): {why}")
        } else {
            format!("{why}; nothing was deleted")
        }
    };
    let path = a.path();
    w.dir(a.dir_fd(), a.cname(), &path, 0, true).map_err(|s| stop(s, 0))?;
    w.dir(a.dir_fd(), a.cname(), &path, 0, false).map_err(|s| stop(s, w.removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn sandbox(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("tmop-anchor-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(&p).unwrap()
    }

    /// `mount --bind from to`, where this process may (root, or `unshare -Urm`); false otherwise.
    #[cfg(target_os = "linux")]
    fn bind(from: &Path, to: &Path) -> bool {
        let (a, b) = (cstr(from.as_os_str().as_bytes()).unwrap(), cstr(to.as_os_str().as_bytes()).unwrap());
        // SAFETY: two NUL-terminated paths; no file system type or data for a bind mount.
        let ok = unsafe { libc::mount(a.as_ptr(), b.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) == 0 };
        // CI runs these tests in a mount namespace and sets this, so a skip there is a failure.
        assert!(ok || std::env::var_os("TMOP_TEST_REQUIRE_MOUNT").is_none(), "bind mount failed: {}", std::io::Error::last_os_error());
        ok
    }

    #[cfg(target_os = "linux")]
    fn unmount(p: &Path) {
        let c = cstr(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        unsafe { libc::umount2(c.as_ptr(), libc::MNT_DETACH) };
    }

    /// S3: the parent is resolved and opened once; a link swapped into its place afterwards
    /// (the other user's folder renamed away, a link to the victim's home put there) changes
    /// nothing: the item that was judged goes, the victim's folder of the same name stays.
    #[test]
    fn a_link_swapped_in_after_the_check_is_not_followed() {
        let base = sandbox("swap");
        std::fs::create_dir_all(base.join("shared").join("project")).unwrap();
        std::fs::write(base.join("shared").join("project").join("junk"), b"j").unwrap();
        std::fs::create_dir_all(base.join("victim").join("project")).unwrap();
        std::fs::write(base.join("victim").join("project").join("precious"), b"p").unwrap();
        let a = Anchor::new(&base.join("shared").join("project")).unwrap();
        assert_eq!(a.path(), base.join("shared").join("project"));
        std::fs::rename(base.join("shared"), base.join("moved")).unwrap();
        symlink(base.join("victim"), base.join("shared")).unwrap();
        remove(&a).unwrap();
        assert!(base.join("victim").join("project").join("precious").exists(), "the victim's folder was followed");
        assert!(!base.join("moved").join("project").exists());
        // A path through a link (this user's own) is resolved before anything is judged.
        let b = Anchor::new(&base.join("shared").join("project").join("precious")).unwrap();
        assert_eq!(b.path(), base.join("victim").join("project").join("precious"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S3: a link in the parent chain owned by another user is never followed (only checkable
    /// as root, which can give a link away).
    #[test]
    fn a_link_owned_by_another_user_is_refused() {
        let base = sandbox("owner");
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::fs::write(base.join("real").join("x"), b"x").unwrap();
        symlink(base.join("real"), base.join("theirs")).unwrap();
        let c = cstr(base.join("theirs").as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path; lchown changes the link itself.
        if unsafe { libc::lchown(c.as_ptr(), 1, 1) } == 0 {
            let e = Anchor::new(&base.join("theirs").join("x")).err().unwrap();
            assert!(e.contains("owned by another user"), "{e}");
            assert!(base.join("real").join("x").exists());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S1/S2/U8: a folder holding a bind mount of the same file system (same st_dev) is refused
    /// with nothing deleted, by the mount table and, without it, by the mount id of every folder;
    /// a mount point itself is refused too. Needs mount rights (root or `unshare -Urm`).
    #[cfg(target_os = "linux")]
    #[test]
    fn bind_mounts_of_the_same_file_system_are_never_entered() {
        let base = sandbox("bind");
        let (docs, proj) = (base.join("docs"), base.join("proj"));
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("precious.txt"), b"p").unwrap();
        std::fs::create_dir_all(proj.join("data")).unwrap();
        std::fs::write(proj.join("a.txt"), b"a").unwrap();
        if !bind(&docs, &proj.join("data")) {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let e = remove(&Anchor::new(&proj).unwrap()).unwrap_err();
        assert!(e.contains("mount") && e.contains("nothing was deleted"), "{e}");
        assert!(docs.join("precious.txt").exists() && proj.join("a.txt").exists());
        // The per-folder check alone (as when the mount appears after the table was read).
        let a = Anchor::new(&proj).unwrap();
        let parent = stat_at(a.dir_fd(), c"").unwrap();
        let none = |_: u32| None;
        let mut w = Walk { dev: dev_of(&parent), mnt: mount_id(a.dir_fd(), c""), removed: 0, me: euid(), home_of: &none, homes: Default::default() };
        assert!(w.mnt.is_some(), "statx gives mount ids on this kernel");
        assert!(matches!(w.dir(a.dir_fd(), a.cname(), &proj, 0, true), Err(Stop::Mount(p)) if p == proj.join("data")));
        assert!(docs.join("precious.txt").exists());
        // The mount point itself.
        let e = remove(&Anchor::new(&proj.join("data")).unwrap()).unwrap_err();
        assert!(e.contains("mount"), "{e}");
        assert!(docs.join("precious.txt").exists());
        unmount(&proj.join("data"));
        remove(&Anchor::new(&proj).unwrap()).unwrap();
        assert!(!proj.exists() && docs.join("precious.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// S5: a removal stopped by a read-only folder says that part of it is gone.
    #[test]
    fn a_partial_removal_says_so() {
        let base = sandbox("partial");
        let v = base.join("v");
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(v.join("f"), b"f").unwrap();
        std::fs::create_dir_all(v.join("ro").join("sub")).unwrap();
        std::fs::write(v.join("ro").join("sub").join("g"), b"g").unwrap();
        std::fs::set_permissions(v.join("ro").join("sub"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = remove(&Anchor::new(&v).unwrap());
        if euid() != 0 {
            let e = r.unwrap_err();
            assert!(e.starts_with("partly deleted"), "{e}");
            assert!(v.join("ro").join("sub").join("g").exists());
        }
        let _ = std::fs::set_permissions(v.join("ro").join("sub"), std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// UX1/E1/R1: a folder holding another account's home (an LDAP account in no passwd file, its
    /// home under a plain `/export/home`) is refused with nothing deleted, as is that home itself.
    /// The files here are this test's; the walk is told it runs as someone else, so they are
    /// "alice's" and the user database says her home is `export/home/alice`.
    #[test]
    fn another_users_home_inside_is_never_deleted() {
        let base = sandbox("homes");
        let alice = base.join("export").join("home").join("alice");
        std::fs::create_dir_all(alice.join(".ssh")).unwrap();
        std::fs::write(alice.join(".ssh").join("id_ed25519"), b"k").unwrap();
        std::fs::write(alice.join("thesis.tex"), b"t").unwrap();
        // Root's own folders are never another user's: as root, the files are given away first.
        let me = if euid() == 0 {
            for p in [alice.clone(), alice.join(".ssh"), alice.join(".ssh").join("id_ed25519"), alice.join("thesis.tex")] {
                std::os::unix::fs::lchown(&p, Some(4_000_123), Some(4_000_123)).unwrap();
            }
            4_000_123
        } else {
            euid()
        };
        let other = me.wrapping_add(7919);
        let home = alice.to_string_lossy().into_owned();
        let home_of = |uid: u32| (uid == me).then(|| home.clone());
        for item in [base.join("export"), base.join("export").join("home"), alice.clone(), alice.join(".ssh")] {
            let e = remove_with(&Anchor::new(&item).unwrap(), other, &home_of).unwrap_err();
            assert!(e.contains("another user's home folder") && e.contains("nothing was deleted"), "{}: {e}", item.display());
            assert!(alice.join(".ssh").join("id_ed25519").exists() && alice.join("thesis.tex").exists());
        }
        // A folder of that account outside its home is not refused here (the rules warn about it).
        std::fs::create_dir_all(base.join("scratch").join("x")).unwrap();
        remove_with(&Anchor::new(&base.join("scratch")).unwrap(), other, &home_of).unwrap();
        assert!(!base.join("scratch").exists());
        // As the account itself, its own folders go.
        remove_with(&Anchor::new(&alice).unwrap(), me, &home_of).unwrap();
        assert!(!alice.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// E2/R2: a tree nested deeper than MAX_DEPTH is refused before anything is removed (no
    /// stack overflow, no running out of descriptors part way).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_too_deep_tree_is_refused_whole() {
        let base = sandbox("deep");
        let mut p = base.join("v");
        for _ in 0..MAX_DEPTH + 5 {
            p.push("d");
        }
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("f"), b"f").unwrap();
        let e = remove(&Anchor::new(&base.join("v")).unwrap()).unwrap_err();
        assert!(e.contains("nested") && e.contains("nothing was deleted"), "{e}");
        assert!(p.join("f").exists());
        // Within the limit it goes.
        let mut q = base.join("w");
        for _ in 0..MAX_DEPTH - 1 {
            q.push("d");
        }
        std::fs::create_dir_all(&q).unwrap();
        remove(&Anchor::new(&base.join("w")).unwrap()).unwrap();
        assert!(!base.join("w").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn links_are_removed_never_followed() {
        let base = sandbox("links");
        std::fs::create_dir_all(base.join("keep")).unwrap();
        std::fs::write(base.join("keep").join("k"), b"k").unwrap();
        std::fs::create_dir_all(base.join("v")).unwrap();
        symlink(base.join("keep"), base.join("v").join("l")).unwrap();
        symlink(base.join("keep"), base.join("lone")).unwrap();
        let a = Anchor::new(&base.join("lone")).unwrap();
        assert_eq!(a.kind(), Ok(None));
        remove(&a).unwrap();
        remove(&Anchor::new(&base.join("v")).unwrap()).unwrap();
        assert!(base.join("keep").join("k").exists() && !base.join("v").exists() && std::fs::symlink_metadata(base.join("lone")).is_err());
        assert!(Anchor::new(Path::new("rel/x")).is_err() && Anchor::new(Path::new("/tmp/../etc/x")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
