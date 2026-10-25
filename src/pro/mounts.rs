//! Mounted file systems off Windows: the Disk Space volume cards and the size of a file system.
//!
//! Linux reads `/proc/self/mountinfo` (octal-escaped fields, one line per mount, bind mounts and
//! btrfs subvolumes included) and keeps one entry per real device; labels come from the
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
            Some(Mount { dev: (maj.parse().ok()?, min.parse().ok()?), root: unescape(f[3]), point: unescape(f[4]), fs: f[sep + 1].to_string(), source: unescape(f[sep + 2]) })
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

/// The mounts worth a volume card, with their kind: one per device (bind mounts and further btrfs
/// subvolumes of a device already shown are dropped, the shortest mount point wins), sorted by
/// mount point so `/` comes first.
pub fn pick_linux(mut mounts: Vec<Mount>) -> Vec<(Mount, &'static str)> {
    mounts.sort_by(|a, b| a.point.len().cmp(&b.point.len()).then_with(|| a.point.cmp(&b.point)));
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in mounts {
        let Some(kind) = linux_kind(&m.fs, &m.point) else { continue };
        let key = if m.source.starts_with("/dev/") { m.source.clone() } else { format!("{}:{}", m.dev.0, m.dev.1) };
        if seen.insert(key) {
            out.push((m, kind));
        }
    }
    out.sort_by(|a, b| a.0.point.cmp(&b.0.point));
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
            let (total, free) = space(Path::new(&m.point)).filter(|s| s.0 > 0)?;
            let kind = if kind == "Fixed" && linux_removable(m.dev) { "Removable" } else { kind };
            let dev = std::fs::canonicalize(&m.source).unwrap_or_else(|_| m.source.clone().into());
            let label = labels.get(&dev).cloned().unwrap_or_default();
            Some(Volume { root: m.point, label, fs: m.fs, kind, total, free })
        })
        .collect()
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
    let cstr = |b: &[libc::c_char]| {
        let b: Vec<u8> = b.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        String::from_utf8_lossy(&b).into_owned()
    };
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
    let got = usize::try_from(got).unwrap_or(0).min(buf.len());
    let mut out: Vec<Volume> = buf[..got]
        .iter()
        .filter_map(|s| {
            let (point, fs) = (cstr(&s.f_mntonname), cstr(&s.f_fstypename));
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
        assert_eq!(m[0], Mount { dev: (259, 2), root: "/".into(), point: "/".into(), fs: "ext4".into(), source: "/dev/nvme0n1p2".into() });
        assert_eq!(m[6].root, "/@home");
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
        assert_eq!(got, [("/", "Fixed"), ("/boot/efi", "Fixed"), ("/home", "Fixed"), ("/media/cdrom", "Optical"), ("/mnt/c", "Network"), ("/mnt/nas", "Network"), ("/run/media/alex/My Stick", "Removable")]);
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
