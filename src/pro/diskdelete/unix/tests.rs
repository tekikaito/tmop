//! The Unix rules, table-driven and hermetic: every machine fact (home, uid, mounts, links,
//! owners) is injected, so these run on Windows, Linux and macOS alike.

use super::*;
use std::collections::HashMap;

const ME: u32 = 1000;
const FLAVORS: [Flavor; 2] = [Flavor::Linux, Flavor::MacOs];

/// The home folder of each flavor.
fn home(fl: Flavor) -> &'static str {
    match fl {
        Flavor::Linux => "/home/me",
        Flavor::MacOs => "/Users/me",
    }
}

/// `~` expanded for `fl`.
fn p(fl: Flavor, s: &str) -> String {
    s.replacen('~', home(fl), 1)
}

fn rules(fl: Flavor) -> Rules {
    let n = |s: &str| norm_unix(fl, &p(fl, s));
    let known = DEFAULT_USER_DIRS.iter().map(|(label, dir)| (n(&format!("~/{dir}")), *label)).collect();
    let unix = UnixPaths {
        homes: vec![n("~")],
        uid: Some(ME),
        mounts: ["/", "/boot/efi", "/home", "/media/me/USB", "/Volumes/Backup", "/run/media/me/Stick", "~/nas", "/mnt/c", "/tmp", "/System/Volumes/Data"].iter().map(|m| n(m)).collect(),
        binds: vec![(n("~/etc-bind"), n("/etc"))],
        wsl: vec![WslDrive {
            point: n("/mnt/c"),
            letter: 'c',
            sys: SystemPaths {
                system_root: r"c:\windows".into(),
                program_files: vec![r"c:\program files".into()],
                program_data: r"c:\programdata".into(),
                users_root: r"c:\users".into(),
                profiles: vec![r"c:\users\me".into()],
                appdata: vec![r"c:\users\me\appdata".into()],
                ..Default::default()
            },
        }],
        other_homes: vec![n("/srv/bob"), n("/data/carol")],
        temp: ["/tmp", "/var/tmp", "/private/tmp", "/var/folders/ab/xyz/T", "/private/var/folders/ab/xyz/T"].iter().map(|t| n(t)).collect(),
        protected_ids: vec![(1, 2)],
    };
    Rules { sys: SystemPaths { known, unix, ..Default::default() }, inv: Inventory::default() }
}

/// A pretend file system: symlinks, owners and identities by path.
#[derive(Default)]
struct FakeFs {
    links: HashMap<String, String>,
    owners: HashMap<String, u32>,
    ids: HashMap<String, (u64, u64)>,
    dirs: Vec<String>,
}

impl FakeFs {
    fn link(mut self, at: &str, to: &str) -> Self {
        self.links.insert(at.into(), to.into());
        self
    }
    fn owner(mut self, at: &str, uid: u32) -> Self {
        self.owners.insert(at.into(), uid);
        self
    }
}

impl UnixFs for FakeFs {
    fn lstat(&self, path: &str) -> Option<FsInfo> {
        let path = path.trim_end_matches('/');
        Some(FsInfo {
            owner: self.owners.get(path).copied().unwrap_or(ME),
            id: self.ids.get(path).copied().unwrap_or((9, path.len() as u64 + 1000)),
            is_link: self.links.contains_key(path),
            is_dir: self.dirs.iter().any(|d| d == path) || ["/etc", "/usr", "/home/me", "/Users/me"].contains(&path),
        })
    }
    fn canonical(&self, path: &str) -> Option<String> {
        let mut todo: Vec<String> = path.split('/').filter(|c| !c.is_empty()).rev().map(String::from).collect();
        let mut out = String::new();
        let mut hops = 0;
        while let Some(c) = todo.pop() {
            match c.as_str() {
                "." => {}
                ".." => out.truncate(out.rfind('/').unwrap_or(0)),
                _ => {
                    let cand = format!("{out}/{c}");
                    if let Some(t) = self.links.get(&cand) {
                        hops += 1;
                        if hops > 40 {
                            return None;
                        }
                        if t.starts_with('/') {
                            out.clear();
                        }
                        todo.extend(t.split('/').filter(|c| !c.is_empty()).rev().map(String::from));
                    } else {
                        out = cand;
                    }
                }
            }
        }
        Some(if out.is_empty() { "/".into() } else { out })
    }
}

fn meta(dir: bool) -> ItemMeta {
    ItemMeta { is_dir: dir, ..Default::default() }
}

fn verdict(fl: Flavor, path: &str, dir: bool, r: &Rules, fs: &FakeFs) -> Verdict {
    classify_fs(fl, &p(fl, path), meta(dir), r, fs)
}

#[test]
fn protected_paths_are_blocked() {
    // (path, is a folder): `~` is the flavor's home folder.
    let table: &[(&str, bool)] = &[
        ("~", true),
        ("/", true),
        ("/etc", true),
        ("/etc/passwd", false),
        ("/usr", true),
        ("/usr/lib", true),
        ("/usr/lib/libc.so.6", false),
        ("/bin", true),
        ("/lib64/ld-linux-x86-64.so.2", false),
        ("/boot/vmlinuz", false),
        ("/boot/efi", true),
        ("/proc/1/mem", false),
        ("/dev/sda", false),
        ("/sys/firmware", true),
        ("/run/user/1000", true),
        ("/var", true),
        ("/var/lib/dpkg", true),
        ("/var/log/syslog", false),
        ("/opt/app", true),
        ("/srv/www", true),
        ("/root", true),
        ("/root/.bashrc", false),
        ("/snap/core22", true),
        ("/home", true),
        ("/home/bob", true),
        ("/home/bob/notes.txt", false),
        ("/Users/bob/Documents", true),
        ("/srv/bob/x", false),
        ("/data/carol/thesis.tex", false),
        ("/data", true),
        ("/swapfile", false),
        ("/tmp", true),
        ("/var/tmp", true),
        ("~/.ssh", true),
        ("~/.ssh/id_ed25519", false),
        ("~/.ssh/config", false),
        ("~/.gnupg/private-keys-v1.d", true),
        ("~/.local/share/keyrings/login.keyring", false),
        ("~/.config", true),
        ("~/.local", true),
        ("~/.local/share", true),
        ("~/.cache", true),
        ("~/Desktop", true),
        ("~/Documents", true),
        ("~/Downloads", true),
        ("~/Pictures", true),
        ("~/Music", true),
        ("~/Videos", true),
        ("~/Library", true),
        ("~/Library/Application Support", true),
        ("~/Library/Preferences", true),
        ("~/Library/Keychains/login.keychain-db", false),
        ("~/.local/share/Trash/files/x", false),
        ("~/.Trash/old.zip", false),
        ("/media/me/USB/.Trash-1000/files/x", false),
        ("/Volumes/Backup/.Spotlight-V100", true),
        ("/media/me/USB", true),
        ("/media/me", true),
        ("/Volumes/Backup", true),
        ("~/nas", true),
        ("/System", true),
        ("/System/Library", true),
        ("/System/Library/Kernels/kernel", false),
        ("/Library/LaunchDaemons/x.plist", false),
        ("/Applications", true),
        ("/private/etc/hosts", false),
        ("/private/var/db", true),
        ("/System/Volumes/Data/Users/bob", true),
        ("/cores", true),
        ("/var/folders/ab", true),
        ("/var/folders/ab/xyz/T", true),
        ("/mnt/c", true),
        ("/mnt/c/Windows/System32", true),
        ("/mnt/c/Users/me", true),
        ("/mnt/c/Program Files", true),
        ("~/etc-bind/passwd", false),
        ("~/x/../../bob", true),
        ("relative/path", false),
    ];
    let fs = FakeFs::default();
    for fl in FLAVORS {
        let r = rules(fl);
        for (path, dir) in table {
            let v = verdict(fl, path, *dir, &r, &fs);
            assert!(matches!(v, Verdict::Blocked(_)), "{fl:?} {path} should be blocked, got {v:?}");
        }
    }
}

#[test]
fn ordinary_items_are_allowed() {
    let table: &[(&str, bool)] = &[
        ("/tmp/t1/a.txt", false),
        ("/tmp/t1/sub", true),
        ("/tmp/t1/sub/deeper/x.bin", false),
        ("/var/tmp/build", true),
        ("/var/folders/ab/xyz/T/x.tmp", false),
        ("/private/var/folders/ab/xyz/T/sub", true),
        ("~/Documents/report.pdf", false),
        ("~/Documents/Projects", true),
        ("~/Downloads/old.iso", false),
        ("~/.cache/thumbnails", true),
        ("~/Library/Caches/com.example.app", true),
        ("~/src/proj/target", true),
        ("~/src/proj/node_modules", true),
        ("~/nas/movies/a.mkv", false),
        ("/media/me/USB/photos", true),
        ("/run/media/me/Stick/a.jpg", false),
        ("/Volumes/Backup/old", true),
        ("/mnt/c/Users/me/Downloads/setup.exe", false),
        ("/data/scratch/x", false),
        ("/Users/Shared/x", false),
    ];
    let fs = FakeFs::default();
    for fl in FLAVORS {
        let r = rules(fl);
        for (path, dir) in table {
            assert_eq!(verdict(fl, path, *dir, &r, &fs), Verdict::Allowed, "{fl:?} {path}");
        }
    }
}

#[test]
fn symlinks_are_judged_by_where_they_lead() {
    for fl in FLAVORS {
        let r = rules(fl);
        let fs = FakeFs::default().link("/tmp/t1/etc-link", "/etc").link("/tmp/t1/up", "../..").link(&p(fl, "~/proj/home-link"), home(fl)).link("/tmp/t1/ok-link", "/tmp/t1/data");
        // The link itself (only the link would go, but a link to /etc is refused all the same)
        // and anything reached through it.
        for path in ["/tmp/t1/etc-link", "/tmp/t1/etc-link/passwd", "/tmp/t1/etc-link/ssh", "/tmp/t1/up/etc/shadow", "~/proj/home-link", "~/proj/home-link/.ssh/id_rsa"] {
            assert!(matches!(verdict(fl, path, false, &r, &fs), Verdict::Blocked(_)), "{fl:?} {path}");
            assert!(matches!(verdict(fl, path, true, &r, &fs), Verdict::Blocked(_)), "{fl:?} {path} (dir)");
        }
        let v = verdict(fl, "/tmp/t1/etc-link/passwd", false, &r, &fs);
        assert!(matches!(&v, Verdict::Blocked(why) if why.contains("/etc/passwd")), "{v:?}");
        // A link to an ordinary place stays an ordinary item; through-link paths that land in
        // the home folder get the home folder's rules.
        assert_eq!(verdict(fl, "/tmp/t1/ok-link", false, &r, &fs), Verdict::Allowed);
        assert_eq!(verdict(fl, "~/proj/home-link/Documents/a.txt", false, &r, &fs), Verdict::Allowed);
    }
}

#[test]
fn owners_identities_and_binds() {
    for fl in FLAVORS {
        let mut r = rules(fl);
        let fs = FakeFs::default().owner("/tmp/t1/rootfile", 0).owner("/tmp/t1/bobs", 1001).owner(&p(fl, "~/build/out.o"), 0);
        for path in ["/tmp/t1/rootfile", "~/build/out.o"] {
            assert!(matches!(verdict(fl, path, false, &r, &fs), Verdict::Blocked(why) if why.contains("root")), "{fl:?} {path}");
        }
        assert!(matches!(verdict(fl, "/tmp/t1/bobs", false, &r, &fs), Verdict::Warn(w) if w.iter().any(|x| x.contains("another user"))));
        // Running as root: root's files are deletable, the protected places still are not.
        r.sys.unix.uid = Some(0);
        assert_eq!(verdict(fl, "/tmp/t1/rootfile", false, &r, &fs), Verdict::Allowed);
        assert!(matches!(verdict(fl, "/etc/passwd", false, &r, &fs), Verdict::Blocked(_)));
        assert!(matches!(verdict(fl, "~/.ssh/id_ed25519", false, &r, &fs), Verdict::Blocked(_)));
        // Unknown uid counts as not root.
        r.sys.unix.uid = None;
        assert!(matches!(verdict(fl, "/tmp/t1/rootfile", false, &r, &fs), Verdict::Blocked(_)));
        // A protected folder under another name (bind mount, NFD/NFC spelling...).
        let mut fs = FakeFs::default();
        fs.ids.insert("/tmp/t1/alias".into(), (1, 2));
        assert!(matches!(verdict(fl, "/tmp/t1/alias", true, &r, &fs), Verdict::Blocked(_)));
        // A bind mount shows /etc: its contents are /etc's.
        assert!(matches!(verdict(fl, "~/etc-bind/hosts", false, &r, &FakeFs::default()), Verdict::Blocked(why) if why.contains("bind")));
    }
}

#[test]
fn hostile_environment_opens_nothing() {
    for fl in FLAVORS {
        let fs = FakeFs::default();
        let mut r = rules(fl);
        // $HOME=/etc and $TMPDIR=/var/lib must not make /etc or /var/lib deletable.
        r.sys.unix.homes = vec!["/etc".into(), "/".into()];
        r.sys.unix.temp = vec![norm_unix(fl, "/var/lib"), "/".into(), norm_unix(fl, "/var/folders")];
        for path in ["/etc/passwd", "/var/lib/dpkg/status", "/var/folders/ab/c", "/usr/bin/ls"] {
            assert!(matches!(verdict(fl, path, false, &r, &fs), Verdict::Blocked(_)), "{fl:?} {path}");
        }
        // No home known at all: every home folder is someone's.
        r.sys.unix.homes.clear();
        assert!(matches!(verdict(fl, &format!("{}/notes.txt", home(fl)), false, &r, &fs), Verdict::Blocked(_)));
        // A passwd entry with `/` as its home protects nothing extra but breaks nothing either.
        let mut r = rules(fl);
        r.sys.unix.other_homes.push("/".into());
        assert_eq!(verdict(fl, "/tmp/t1/a.txt", false, &r, &fs), Verdict::Allowed);
    }
}

#[test]
fn folders_holding_protected_places_are_blocked() {
    for fl in FLAVORS {
        let fs = FakeFs::default();
        let mut r = rules(fl);
        r.sys.known.push((norm_unix(fl, &p(fl, "~/work/docs")), "Documents"));
        r.sys.unix.mounts.push(norm_unix(fl, &p(fl, "~/mnt/phone")));
        for path in ["~/work", "~/mnt", "/media"] {
            assert!(matches!(verdict(fl, path, true, &r, &fs), Verdict::Blocked(_)), "{fl:?} {path}");
        }
        // As a file name the same path holds nothing.
        assert_eq!(verdict(fl, "~/work", false, &r, &fs), Verdict::Allowed);
    }
}

#[test]
fn warnings_in_and_out_of_home() {
    let fs = FakeFs::default();
    for fl in FLAVORS {
        let mut r = rules(fl);
        r.inv.process_exes = vec![(norm_unix(fl, &p(fl, "~/apps/tool/bin/tool")), 42, "tool".into())];
        r.inv.app_dirs = vec![(norm_unix(fl, "/usr"), "Too broad".into()), (norm_unix(fl, &p(fl, "~")), "Home".into())];
        let warns = |path: &str, dir: bool, needle: &str| {
            let v = verdict(fl, path, dir, &r, &fs);
            assert!(matches!(&v, Verdict::Warn(w) if w.iter().any(|x| x.contains(needle))), "{fl:?} {path}: {v:?}");
        };
        warns("~/.config/app/settings.toml", false, "settings");
        warns("~/.config/google-chrome/Default", true, "browser profile");
        warns("~/.mozilla/firefox/abc.default", true, "browser profile");
        warns("~/.local/share/app/db", false, "application data");
        warns("~/Library/Mobile Documents/com~apple~CloudDocs/a.txt", false, "iCloud");
        warns("~/.bashrc", false, "hidden");
        warns("~/src/proj/.git/objects", true, "Git");
        warns("~/vm/disk.qcow2", false, "virtual machine");
        warns("~/vault.kdbx", false, "password");
        warns("~/.aws/credentials", false, "credentials");
        warns("~/apps/tool", true, "running process");
        warns("~/apps/tool/bin/tool", false, "PID 42");
        warns("/Applications/Foo.app", true, "installed application");
        warns("/Applications/Foo.app/Contents/MacOS/foo", false, "application bundle");
        // Broad install locations (the home folder, /usr) never warn about everything below.
        assert_eq!(verdict(fl, "~/Documents/a.txt", false, &r, &fs), Verdict::Allowed);
    }
}

#[test]
fn macos_folds_case_and_reads_firmlinks() {
    let fs = FakeFs::default();
    let r = rules(Flavor::MacOs);
    for path in ["/ETC/hosts", "/users/ME/.SSH/id_rsa", "/Users/me/LIBRARY", "/system/library/x", "/System/Volumes/Data/Users/me/Documents"] {
        assert!(matches!(classify_fs(Flavor::MacOs, path, meta(true), &r, &fs), Verdict::Blocked(_)), "{path}");
    }
    assert_eq!(classify_fs(Flavor::MacOs, "/System/Volumes/Data/Users/me/Documents/a.txt", meta(false), &r, &fs), Verdict::Allowed);
    // Linux compares exactly: /ETC is not /etc, though a top-level folder is still never deletable.
    let r = rules(Flavor::Linux);
    assert_eq!(classify_fs(Flavor::Linux, "/ETC/hosts", meta(false), &r, &fs), Verdict::Allowed);
    assert!(matches!(classify_fs(Flavor::Linux, "/ETC", meta(true), &r, &fs), Verdict::Blocked(_)));
    assert!(matches!(classify_fs(Flavor::Linux, "/home/me/.SSH", meta(true), &r, &fs), Verdict::Warn(_)));
}

#[test]
fn norm_unix_forms() {
    assert_eq!(norm_unix(Flavor::Linux, "/a//b/./c/"), "/a/b/c");
    assert_eq!(norm_unix(Flavor::Linux, "/A/b"), "/A/b");
    assert_eq!(norm_unix(Flavor::Linux, "/a/../b"), "/a/../b", "'..' is never resolved lexically");
    assert_eq!(norm_unix(Flavor::Linux, "rel/x"), "rel/x");
    assert_eq!(norm_unix(Flavor::Linux, "/"), "/");
    assert_eq!(norm_unix(Flavor::Linux, r"/a\b"), r"/a\b", "a backslash is part of a name");
    assert_eq!(norm_unix(Flavor::MacOs, "/Users/Me/"), "/users/me");
    assert_eq!(norm_unix(Flavor::MacOs, "/System/Volumes/Data"), "/");
    assert_eq!(norm_unix(Flavor::MacOs, "/System/Volumes/Data/Users/x"), "/users/x");
    assert_eq!(norm_unix(Flavor::MacOs, "/System/Volumes/DataX"), "/system/volumes/datax");
    assert!(under("/a/b", "/a") && under("/a", "/a") && under("/a", "/") && !under("/ab", "/a") && !under("/a", ""));
}

#[test]
fn passwd_and_user_dirs() {
    let pw = "root:x:0:0:root:/root:/bin/bash\n# comment\nbob:x:1001:1001::/home/bob:/bin/sh\nbroken:x\nweird:x:abc:1::/x:/bin/sh\n";
    assert_eq!(parse_passwd(pw), [(0, "/root".to_string()), (1001, "/home/bob".to_string())]);
    let dirs = "# generated\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_DOWNLOAD_DIR=\"$HOME/\"\nXDG_DOCUMENTS_DIR=\"/data/docs\"\nXDG_MUSIC_DIR=\"relative\"\nXDG_FOO_DIR=\"$HOME/Foo\"\n";
    assert_eq!(parse_user_dirs(dirs, "/home/me"), [("Desktop", "/home/me/Schreibtisch".to_string()), ("Documents", "/data/docs".to_string())]);
}

/// The real rules of this machine against the real file system (Linux and macOS only).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn real_machine_blocks_the_basics() {
    let r = Rules { sys: crate::pro::diskdelete::system_paths(), inv: Inventory::default() };
    let v = |path: &std::path::Path, dir: bool| crate::pro::diskdelete::classify_resolved(path, meta(dir), &r);
    let home = crate::platform::home().expect("a home folder");
    for path in [std::path::PathBuf::from("/"), "/etc".into(), "/usr/lib".into(), home.clone(), home.join(".ssh"), home.join(".ssh").join("id_ed25519")] {
        assert!(matches!(v(&path, true), Verdict::Blocked(_)), "{}", path.display());
    }
    // Inside WSL the Windows drive gets the Windows rules.
    if std::path::Path::new("/mnt/c/Windows/System32").is_dir() {
        for path in ["/mnt/c", "/mnt/c/Windows/System32", "/mnt/c/Users", "/mnt/c/Program Files"] {
            assert!(matches!(v(std::path::Path::new(path), true), Verdict::Blocked(_)), "{path}");
        }
    }
    let base = std::env::temp_dir().join(format!("tmop-del-unix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub").join("deeper")).unwrap();
    std::fs::write(base.join("a.txt"), b"x").unwrap();
    std::os::unix::fs::symlink("/etc", base.join("etc-link")).unwrap();
    assert_eq!(v(&base.join("a.txt"), false), Verdict::Allowed);
    assert_eq!(v(&base.join("sub"), true), Verdict::Allowed);
    assert!(matches!(v(&base.join("etc-link"), false), Verdict::Blocked(_)));
    assert!(matches!(v(&base.join("etc-link").join("hosts"), false), Verdict::Blocked(_)));
    // Deleting the folder removes the link, never /etc.
    assert!(crate::pro::diskdelete::delete_permanent(&base).is_ok());
    assert!(std::path::Path::new("/etc").is_dir() && !base.exists());
}
