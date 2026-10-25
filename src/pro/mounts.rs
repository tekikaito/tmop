//! Mounted file systems off Windows: the Disk Space volume cards and the size of a file system.
//!
//! Linux reads `/proc/self/mountinfo` (octal-escaped fields, one line per mount, bind mounts and
//! btrfs subvolumes included) and keeps one entry per real device (per subvolume on btrfs); labels come from the
//! `/dev/disk/by-label` links and the removable flag from sysfs. macOS asks `getfsstat` and folds
//! the APFS helper volumes under `/System/Volumes` into the boot volume `/`. The parsers and the
//! filters are pure and tested on every host; only the I/O shells are cfg-gated.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::diskspace::Volume;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mount {
    /// `major:minor` of the file system (st_dev).
    pub dev: (u32, u32),
    /// The directory of the file system that is mounted here (`/` unless a bind mount or subvolume).
    pub root: String,
    /// Where it is mounted.
    pub point: String,
    pub fs: String,
    /// `/dev/sda1`, `server:/export`, `tmpfs`, ...
    pub source: String,
    /// The btrfs subvolume of the mount (`subvol=` of the super options), empty otherwise.
    pub subvol: String,
}

/// Undoes the kernel's `\ooo` octal escapes (space, tab, newline and backslash in paths).
pub fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            if let Ok(v) = u8::try_from(v) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/self/mountinfo`: `id parent major:minor root point options [optional...] - fs source super`.
/// Malformed lines are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            let sep = f.iter().position(|x| *x == "-")?;
            if sep < 6 || f.len() < sep + 3 {
                return None;
            }
            let (maj, min) = f[2].split_once(':')?;
            let subvol = f.get(sep + 3).and_then(|o| o.split(',').find_map(|o| o.strip_prefix("subvol="))).map(unescape).unwrap_or_default();
            Some(Mount { dev: (maj.parse().ok()?, min.parse().ok()?), root: unescape(f[3]), point: unescape(f[4]), fs: f[sep + 1].to_string(), source: unescape(f[sep + 2]), subvol })
        })
        .collect()
}

/// Undoes udev's `\xHH` escapes in `/dev/disk/by-label` names (`My\x20Disk`).
pub fn udev_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'x') {
            if let Some(v) = b.get(i + 2..i + 4).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when `path` is `dir` or inside it (whole components).
fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The volume kind of a Linux mount ("Fixed", "Removable", "Network", "Optical"), or `None` for
/// pseudo and container file systems (proc, tmpfs, overlay, squashfs snaps...) and for mounts under
/// the system's own trees.
pub fn linux_kind(fs: &str, point: &str) -> Option<&'static str> {
    const DISK: &[&str] = &["ext2", "ext3", "ext4", "xfs", "btrfs", "f2fs", "vfat", "msdos", "exfat", "ntfs", "ntfs3", "fuseblk", "zfs", "bcachefs", "jfs", "reiserfs", "hfsplus", "apfs", "nilfs2"];
    const NET: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "9p", "virtiofs", "afs", "ceph", "glusterfs", "fuse.sshfs", "fuse.rclone", "davfs", "fuse.davfs2"];
    const HIDDEN: &[&str] = &["/proc", "/sys", "/dev", "/snap", "/var/snap", "/var/lib/docker", "/var/lib/containers", "/mnt/wsl", "/mnt/wslg", "/usr/lib/wsl"];
    let removable = under(point, "/media") || under(point, "/run/media");
    if (under(point, "/run") && !removable) || HIDDEN.iter().any(|d| under(point, d)) {
        return None;
    }
    if NET.contains(&fs) {
        Some("Network")
    } else if matches!(fs, "iso9660" | "udf") {
        Some("Optical")
    } else if DISK.contains(&fs) {
        Some(if removable { "Removable" } else { "Fixed" })
    } else {
        None
    }
}

/// The btrfs subvolume a mount shows, `None` for other file systems (and kernels without
/// `subvol=` in the options).
fn subvolume(m: &Mount) -> Option<&str> {
    (m.fs == "btrfs" && !m.subvol.is_empty()).then_some(m.subvol.as_str())
}

/// The mounts worth a volume card, with their kind: one per device, and on btrfs one per mounted
/// subvolume (Fedora's `/` and `/home` are two subvolumes of one partition). Bind mounts of a
/// sub-folder are dropped when their file system (or subvolume) already has a card; among the
/// rest the shortest mount point wins. Sorted by mount point so `/` comes first.
pub fn pick_linux(mut mounts: Vec<Mount>) -> Vec<(Mount, &'static str)> {
    let bind = |m: &Mount| m.root != subvolume(m).unwrap_or("/");
    mounts.sort_by(|a, b| bind(a).cmp(&bind(b)).then_with(|| a.point.len().cmp(&b.point.len())).then_with(|| a.point.cmp(&b.point)));
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in mounts {
        let Some(kind) = linux_kind(&m.fs, &m.point) else { continue };
        let key = match (m.source.starts_with("/dev/"), subvolume(&m)) {
            (true, Some(sub)) => format!("{}\0{sub}", m.source),
            (true, None) => m.source.clone(),
            (false, _) => format!("{}:{}", m.dev.0, m.dev.1),
        };
        if seen.insert(key) {
            out.push((m, kind));
        }
    }
    out.sort_by(|a, b| a.0.point.cmp(&b.0.point));
    out
}

/// The other mount points below `path` that belong to the same btrfs file system as `path`
/// itself. Each subvolume has its own `st_dev`, so a scan that stays on one device would skip
/// them although the volume's used space counts them. Empty for other file systems.
pub fn same_fs_points(mounts: &[Mount], path: &str) -> Vec<String> {
    let points: Vec<String> = mounts.iter().map(|m| m.point.clone()).collect();
    let Some(at) = mount_of(path, &points) else { return Vec::new() };
    // The last mount at a point is the one on top.
    let Some(m) = mounts.iter().rev().find(|m| m.point == at) else { return Vec::new() };
    if m.fs != "btrfs" || !m.source.starts_with("/dev/") {
        return Vec::new();
    }
    let mut out: Vec<String> = mounts.iter().filter(|o| o.fs == "btrfs" && o.source == m.source && o.point != m.point && std::path::Path::new(&o.point).starts_with(path)).map(|o| o.point.clone()).collect();
    out.sort();
    out.dedup();
    out
}

/// The kind and label of a macOS mount, or `None` when it gets no card: the APFS helper volumes
/// under `/System/Volumes` (Data shares its container with `/`, which stands for both), hidden
/// (`MNT_DONTBROWSE`) mounts and pseudo file systems.
pub fn macos_kind(point: &str, fs: &str, local: bool, dont_browse: bool) -> Option<(&'static str, String)> {
    if point == "/" {
        return Some(("Fixed", "Macintosh HD".into()));
    }
    if under(point, "/System/Volumes") || dont_browse || matches!(fs, "devfs" | "autofs" | "nullfs" | "fdesc") {
        return None;
    }
    let name = point.rsplit('/').find(|s| !s.is_empty()).unwrap_or(point).to_string();
    let kind = if !local {
        "Network"
    } else if matches!(fs, "cd9660" | "udf") {
        "Optical"
    } else if under(point, "/Volumes") {
        "Removable"
    } else {
        "Fixed"
    };
    Some((kind, name))
}

/// What the delete classifier needs to know about mounts: every mount point (pseudo file systems
/// included), the bind mounts with the folder each one shows, and WSL's Windows drives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MountTable {
    pub points: Vec<String>,
    /// (mount point, the folder it shows) for mounts of a sub-folder of an already mounted file system.
    pub binds: Vec<(String, String)>,
    /// (mount point, drive letter) of WSL's `drvfs` mounts (`/mnt/c` is `C:\`).
    pub drives: Vec<(String, char)>,
}

/// The deepest of `points` that holds the absolute `path`, compared whole components at a time
/// (`/home` holds `/home/me`, not `/homework`).
pub fn mount_of<'a>(path: &str, points: &'a [String]) -> Option<&'a str> {
    let p = std::path::Path::new(path);
    points.iter().filter(|m| m.starts_with('/') && p.starts_with(m.as_str())).max_by_key(|m| m.len()).map(String::as_str)
}

/// The drive letter of a WSL `drvfs` mount (9p with a `C:\` source on WSL 2, `drvfs` on WSL 1).
pub fn wsl_drive(m: &Mount) -> Option<char> {
    if !matches!(m.fs.as_str(), "9p" | "drvfs") {
        return None;
    }
    let b = m.source.as_bytes();
    let letter = (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || (b.len() == 3 && matches!(b[2], b'\\' | b'/')))).then(|| (b[0] as char).to_ascii_lowercase())?;
    Some(letter)
}

/// [`MountTable`] from parsed `/proc/self/mountinfo`. A mount whose root is not `/` shows a
/// sub-folder of its file system; when the same device is also mounted whole, that sub-folder has
/// a real path, which is what the bind mount reaches.
pub fn table_linux(mounts: &[Mount]) -> MountTable {
    let mut t = MountTable::default();
    for m in mounts {
        if !t.points.contains(&m.point) {
            t.points.push(m.point.clone());
        }
        if let Some(l) = wsl_drive(m).filter(|l| !t.drives.contains(&(m.point.clone(), *l))) {
            t.drives.push((m.point.clone(), l));
        }
        if m.root != "/" {
            if let Some(whole) = mounts.iter().find(|o| o.dev == m.dev && o.root == "/") {
                let shown = format!("{}{}", whole.point.trim_end_matches('/'), m.root);
                if shown != m.point && !t.binds.iter().any(|(p, _)| *p == m.point) {
                    t.binds.push((m.point.clone(), shown));
                }
            }
        }
    }
    t
}

/// The mount table from `/proc/self/mountinfo` (empty when it cannot be read).
#[cfg(target_os = "linux")]
pub fn table() -> MountTable {
    std::fs::read_to_string("/proc/self/mountinfo").map(|s| table_linux(&parse_mountinfo(&s))).unwrap_or_default()
}

/// Every mount point from `getfsstat` (macOS has no bind mounts or WSL drives).
#[cfg(target_os = "macos")]
pub fn table() -> MountTable {
    let mut points: Vec<String> = statfs_all().iter().map(|s| c_str(&s.f_mntonname)).filter(|p| !p.is_empty()).collect();
    points.sort();
    points.dedup();
    MountTable { points, ..Default::default() }
}

#[cfg(target_os = "macos")]
fn c_str(b: &[libc::c_char]) -> String {
    let b: Vec<u8> = b.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&b).into_owned()
}

/// Every mounted file system (`getfsstat`, without waiting on unresponsive network mounts).
#[cfg(target_os = "macos")]
fn statfs_all() -> Vec<libc::statfs> {
    // SAFETY: a null buffer asks only for the number of mounts.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    // SAFETY: an all-zero statfs is valid.
    let mut buf: Vec<libc::statfs> = (0..n as usize + 8).map(|_| unsafe { std::mem::zeroed() }).collect();
    let bytes = (buf.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: `buf` holds `bytes` bytes of statfs records; getfsstat fills at most that many.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    buf.truncate(usize::try_from(got).unwrap_or(0));
    buf
}

/// (total, available to the caller) bytes of the file system holding `path`.
#[cfg(target_os = "linux")]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statvfs is valid; statvfs reads the NUL-terminated path and fills `s`.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ between libc targets
    let (blocks, avail, frsize) = (s.f_blocks as u64, s.f_bavail as u64, s.f_frsize as u64);
    Some((blocks.saturating_mul(frsize), avail.saturating_mul(frsize)))
}

/// (total, available to the caller) bytes of the file system holding `path` (`statfs`: macOS's
/// `statvfs` has 32-bit block counts).
#[cfg(target_os = "macos")]
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statfs is valid; statfs reads the NUL-terminated path and fills `s`.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_blocks.saturating_mul(u64::from(s.f_bsize)), s.f_bavail.saturating_mul(u64::from(s.f_bsize))))
}

/// Volume cards from `/proc/self/mountinfo`.
#[cfg(target_os = "linux")]
pub fn volumes() -> Vec<Volume> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else { return Vec::new() };
    let labels = linux_labels();
    pick_linux(parse_mountinfo(&text))
        .into_iter()
        .filter_map(|(m, kind)| {
            // statvfs of a hard-mounted share whose server is gone never returns: such a share
            // gets no card rather than holding up the others.
            let sizes = if kind == "Network" { space_within(&m.point, std::time::Duration::from_secs(2)) } else { space(Path::new(&m.point)) };
            let (total, free) = sizes.filter(|s| s.0 > 0)?;
            let kind = if kind == "Fixed" && linux_removable(m.dev) { "Removable" } else { kind };
            let dev = std::fs::canonicalize(&m.source).unwrap_or_else(|_| m.source.clone().into());
            let label = labels.get(&dev).cloned().unwrap_or_default();
            Some(Volume { root: m.point, label, fs: m.fs, kind, total, free })
        })
        .collect()
}

/// [`space`] on a thread of its own, given up after `wait` (the thread is left behind).
#[cfg(target_os = "linux")]
fn space_within(point: &str, wait: std::time::Duration) -> Option<(u64, u64)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let point = point.to_string();
    std::thread::Builder::new()
        .name("tmop-statvfs".into())
        .spawn(move || {
            let _ = tx.send(space(Path::new(&point)));
        })
        .ok()?;
    rx.recv_timeout(wait).ok().flatten()
}

/// Device node -> file system label, from the `/dev/disk/by-label` links.
#[cfg(target_os = "linux")]
fn linux_labels() -> std::collections::HashMap<std::path::PathBuf, String> {
    let Ok(rd) = std::fs::read_dir("/dev/disk/by-label") else { return Default::default() };
    rd.flatten().filter_map(|e| Some((std::fs::canonicalize(e.path()).ok()?, udev_unescape(&e.file_name().to_string_lossy())))).collect()
}

/// A removable medium (sysfs `removable` of the disk) or a USB disk (most USB drives say 0).
#[cfg(target_os = "linux")]
fn linux_removable(dev: (u32, u32)) -> bool {
    let Ok(dir) = std::fs::canonicalize(format!("/sys/dev/block/{}:{}", dev.0, dev.1)) else { return false };
    let flag = |d: &Path| std::fs::read_to_string(d.join("removable")).is_ok_and(|s| s.trim() == "1");
    dir.to_string_lossy().contains("/usb") || flag(&dir) || dir.parent().is_some_and(flag)
}

/// Volume cards from `getfsstat`.
#[cfg(target_os = "macos")]
pub fn volumes() -> Vec<Volume> {
    let mut out: Vec<Volume> = statfs_all()
        .iter()
        .filter_map(|s| {
            let (point, fs) = (c_str(&s.f_mntonname), c_str(&s.f_fstypename));
            let (kind, label) = macos_kind(&point, &fs, s.f_flags & libc::MNT_LOCAL as u32 != 0, s.f_flags & libc::MNT_DONTBROWSE as u32 != 0)?;
            let bsize = u64::from(s.f_bsize);
            let total = s.f_blocks.saturating_mul(bsize);
            (total > 0).then(|| Volume { root: point, label, fs, kind, total, free: s.f_bavail.saturating_mul(bsize) })
        })
        .collect();
    out.sort_by(|a, b| a.root.cmp(&b.root));
    out.dedup_by(|a, b| a.root == b.root);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop: ext4 root, btrfs-style second subvolume of the same disk, a bind mount, a USB
    /// stick with a space in its label, an NFS share, WSL's drvfs, and the usual pseudo mounts.
    const MOUNTINFO: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw,errors=remount-ro
23 22 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
24 22 0:22 / /sys rw,nosuid,nodev,noexec,relatime shared:2 - sysfs sysfs rw
25 22 0:5 / /dev rw,nosuid,relatime shared:3 - devtmpfs udev rw,size=8000000k
26 22 0:24 / /run rw,nosuid,nodev,noexec,relatime shared:5 - tmpfs tmpfs rw,size=1600000k
27 22 259:1 / /boot/efi rw,relatime shared:7 - vfat /dev/nvme0n1p1 rw,fmask=0077
28 22 259:3 /@home /home rw,relatime shared:8 - btrfs /dev/nvme0n1p3 rw,subvol=/@home
29 22 259:3 /@data /srv/data rw,relatime shared:9 - btrfs /dev/nvme0n1p3 rw,subvol=/@data
30 22 259:2 /var/www /srv/www rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
31 26 8:17 / /run/media/alex/My\\040Stick rw,nosuid,nodev,relatime shared:40 - vfat /dev/sdb1 rw
32 22 0:50 / /mnt/nas rw,relatime shared:41 - nfs4 nas:/export\\134share rw,vers=4.2
33 22 0:51 / /mnt/c rw,noatime - 9p C:\\134 rw,aname=drvfs
34 22 7:3 / /snap/core22/1380 ro,nodev,relatime shared:20 - squashfs /dev/loop3 ro
35 22 0:60 / /var/lib/docker/overlay2/x/merged rw - overlay overlay rw
36 22 0:61 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw
garbage line
37 22 11:0 / /media/cdrom ro,relatime shared:50 - iso9660 /dev/sr0 ro
";

    #[test]
    fn mountinfo_fields_and_escapes() {
        let m = parse_mountinfo(MOUNTINFO);
        assert_eq!(m.len(), 16, "the garbage line is skipped");
        assert_eq!(m[0], Mount { dev: (259, 2), root: "/".into(), point: "/".into(), fs: "ext4".into(), source: "/dev/nvme0n1p2".into(), subvol: String::new() });
        assert_eq!((m[6].root.as_str(), m[6].subvol.as_str()), ("/@home", "/@home"));
        assert_eq!(m[9].point, "/run/media/alex/My Stick");
        assert_eq!(m[10].source, r"nas:/export\share");
        assert_eq!(m[11].source, r"C:\");
        // Optional fields may be absent or many; the `-` separator finds the file system.
        let odd = parse_mountinfo("40 1 8:1 / /x rw - ext4 /dev/sda1 rw\n41 1 8:2 / /y rw shared:1 master:2 propagate_from:3 - xfs /dev/sda2 rw\n");
        assert_eq!((odd[0].fs.as_str(), odd[1].fs.as_str(), odd[1].source.as_str()), ("ext4", "xfs", "/dev/sda2"));
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape(r"a\040b\011c\012d\134e"), "a b\tc\nd\\e");
        assert_eq!(unescape(r"trailing\04"), r"trailing\04", "an incomplete escape stays as is");
        assert_eq!(unescape(r"\777x"), r"\777x", "out of byte range stays as is");
        assert_eq!(udev_unescape(r"My\x20Disk\x2fA"), "My Disk/A");
        assert_eq!(udev_unescape(r"bad\xZZ"), r"bad\xZZ");
    }

    #[test]
    fn one_card_per_real_device() {
        let v = pick_linux(parse_mountinfo(MOUNTINFO));
        let got: Vec<(&str, &str)> = v.iter().map(|(m, k)| (m.point.as_str(), *k)).collect();
        assert_eq!(got, [("/", "Fixed"), ("/boot/efi", "Fixed"), ("/home", "Fixed"), ("/media/cdrom", "Optical"), ("/mnt/c", "Network"), ("/mnt/nas", "Network"), ("/run/media/alex/My Stick", "Removable"), ("/srv/data", "Fixed")]);
    }

    /// Fedora Workstation: `/` and `/home` are subvolumes of one btrfs partition, and a folder of
    /// `/home` is bind-mounted at `/w`.
    const FEDORA: &str = "\
60 1 0:35 /root / rw,relatime shared:1 - btrfs /dev/nvme0n1p3 rw,seclabel,compress=zstd:1,ssd,space_cache=v2,subvolid=257,subvol=/root
61 60 259:2 / /boot rw,relatime shared:2 - ext4 /dev/nvme0n1p2 rw,seclabel
62 60 0:35 /home /home rw,relatime shared:3 - btrfs /dev/nvme0n1p3 rw,seclabel,compress=zstd:1,ssd,space_cache=v2,subvolid=256,subvol=/home
63 60 0:35 /home/alex/www /w rw,relatime shared:3 - btrfs /dev/nvme0n1p3 rw,seclabel,compress=zstd:1,ssd,space_cache=v2,subvolid=256,subvol=/home
64 60 259:5 / /data rw,relatime shared:4 - xfs /dev/sdb1 rw
";

    #[test]
    fn btrfs_subvolumes_get_their_own_cards() {
        let v = pick_linux(parse_mountinfo(FEDORA));
        let got: Vec<&str> = v.iter().map(|(m, _)| m.point.as_str()).collect();
        assert_eq!(got, ["/", "/boot", "/data", "/home"], "the bind mount at /w is not a card although its point is shorter");
    }

    #[test]
    fn a_btrfs_scan_includes_the_other_subvolumes_below_it() {
        let m = parse_mountinfo(FEDORA);
        assert_eq!(same_fs_points(&m, "/"), ["/home", "/w"]);
        assert!(same_fs_points(&m, "/home").is_empty(), "nothing of it is mounted below /home");
        assert!(same_fs_points(&m, "/home/alex").is_empty());
        assert!(same_fs_points(&m, "/data").is_empty(), "not btrfs");
        assert!(same_fs_points(&parse_mountinfo(MOUNTINFO), "/").is_empty(), "ext4 root");
    }

    #[test]
    fn mount_table_for_the_delete_classifier() {
        let t = table_linux(&parse_mountinfo(MOUNTINFO));
        assert_eq!(t.points.len(), 16);
        assert!(["/", "/proc", "/home", "/srv/www", "/run/media/alex/My Stick", "/mnt/c", "/tmp"].iter().all(|p| t.points.iter().any(|q| q == p)));
        // Only the ext4 bind of /var/www has its whole file system mounted too; the btrfs
        // subvolumes do not, so they have no other path.
        assert_eq!(t.binds, [("/srv/www".to_string(), "/var/www".to_string())]);
        assert_eq!(t.drives, [("/mnt/c".to_string(), 'c')]);
        let m = |fs: &str, source: &str| Mount { fs: fs.into(), source: source.into(), ..Default::default() };
        assert_eq!(wsl_drive(&m("drvfs", "D:")), Some('d'));
        assert_eq!(wsl_drive(&m("9p", "drivers")), None);
        assert_eq!(wsl_drive(&m("ext4", "C:\\")), None);
        assert_eq!(wsl_drive(&m("9p", "C:\\x")), None);
    }

    #[test]
    fn mount_of_picks_the_deepest_whole_component_match() {
        let t = table_linux(&parse_mountinfo(MOUNTINFO));
        assert_eq!(mount_of("/home/alex/x", &t.points), Some("/home"));
        assert_eq!(mount_of("/home", &t.points), Some("/home"));
        assert_eq!(mount_of("/homework/x", &t.points), Some("/"));
        assert_eq!(mount_of("/run/media/alex/My Stick/a b", &t.points), Some("/run/media/alex/My Stick"));
        assert_eq!(mount_of("/srv/www/index.html", &t.points), Some("/srv/www"), "a bind mount is its own mount");
        assert_eq!(mount_of("/x", &[]), None);
        assert_eq!(mount_of("relative/x", &t.points), None);
    }

    #[test]
    fn linux_kinds() {
        assert_eq!(linux_kind("ext4", "/media/usb"), Some("Removable"));
        assert_eq!(linux_kind("ext4", "/mediaserver"), Some("Fixed"), "whole components only");
        assert_eq!(linux_kind("ext4", "/run/user/1000/x"), None);
        assert_eq!(linux_kind("tmpfs", "/tmp"), None);
        assert_eq!(linux_kind("cifs", "/mnt/share"), Some("Network"));
        assert_eq!(linux_kind("ext4", "/var/lib/docker/volumes/v"), None);
        assert_eq!(linux_kind("iso9660", "/mnt/wsl/docker-desktop/cli-tools"), None, "WSL internals");
        assert_eq!(linux_kind("9p", "/mnt/c"), Some("Network"));
    }

    #[test]
    fn macos_kinds() {
        assert_eq!(macos_kind("/", "apfs", true, false), Some(("Fixed", "Macintosh HD".into())));
        for helper in ["/System/Volumes/Data", "/System/Volumes/VM", "/System/Volumes/Preboot"] {
            assert_eq!(macos_kind(helper, "apfs", true, true), None, "{helper}");
        }
        assert_eq!(macos_kind("/dev", "devfs", true, false), None);
        assert_eq!(macos_kind("/Volumes/Backup", "apfs", true, false), Some(("Removable", "Backup".into())));
        assert_eq!(macos_kind("/Volumes/share", "smbfs", false, false), Some(("Network", "share".into())));
        assert_eq!(macos_kind("/Volumes/Hidden", "apfs", true, true), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_file_system_has_a_card() {
        // CI runners and WSL mount `/` from a real disk (or overlay in containers, then nothing).
        let v = volumes();
        assert!(v.iter().all(|v| v.total > 0 && v.free <= v.total && !v.root.is_empty()));
        assert!(space(Path::new("/")).is_some_and(|(t, f)| t > 0 && f <= t));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn boot_volume_has_a_card() {
        let v = volumes();
        assert!(v.iter().any(|v| v.root == "/" && v.total > 0), "{v:?}");
        assert!(v.iter().all(|v| !v.root.starts_with("/System/Volumes")));
    }
}
