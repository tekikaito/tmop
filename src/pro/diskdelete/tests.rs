use super::*;

fn sys() -> SystemPaths {
    SystemPaths {
        system_root: r"d:\windows".into(),
        program_files: vec![r"d:\program files".into(), r"d:\program files (x86)".into()],
        program_data: r"d:\programdata".into(),
        users_root: r"d:\users".into(),
        profiles: vec![r"d:\users\me".into(), r"d:\users\public".into()],
        appdata: vec![r"d:\users\me\appdata".into(), r"d:\users\public\appdata".into()],
        known: vec![
            (r"d:\users\me\documents".into(), "Documents"),
            (r"d:\users\me\pictures".into(), "Pictures"),
            (r"d:\users\me\desktop".into(), "Desktop"),
            (r"d:\users\me\downloads".into(), "Downloads"),
            (r"d:\users\me\music".into(), "Music"),
            (r"d:\users\me\videos".into(), "Videos"),
            (r"d:\users\me\saved games".into(), "Saved Games"),
        ],
        onedrive: vec![r"d:\onedrive".into()],
        temp: r"d:\users\me\appdata\local\temp".into(),
        local_appdata: r"d:\users\me\appdata\local".into(),
    }
}

fn rules() -> Rules {
    Rules { sys: sys(), inv: Inventory::default() }
}

fn file(p: &str, r: &Rules) -> Verdict {
    classify(Path::new(p), ItemMeta::default(), r)
}

fn dir(p: &str, r: &Rules) -> Verdict {
    classify(Path::new(p), ItemMeta { is_dir: true, ..Default::default() }, r)
}

fn is_blocked(v: &Verdict) -> bool {
    matches!(v, Verdict::Blocked(_))
}

fn reasons(v: &Verdict) -> Vec<String> {
    match v {
        Verdict::Warn(r) => r.clone(),
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[test]
fn norm_paths() {
    assert_eq!(norm_path(Path::new(r"\\?\C:\A\B\")), r"c:\a\b");
    assert_eq!(norm_path(Path::new("C:/x/")), r"c:\x");
    assert_eq!(norm_path(Path::new(r"C:\")), r"c:\");
    assert_eq!(norm_path(Path::new("C:")), r"c:\");
    assert_eq!(norm_path(Path::new(r"\\?\UNC\srv\share\f")), r"\\srv\share\f");
    assert!(is_drive_root(r"c:\") && is_drive_root(r"\\srv\share") && !is_drive_root(r"c:\x") && !is_drive_root(r"\\srv\share\f"));
    assert!(!under(r"c:\a\bc", r"c:\a\b"));
    assert!(under(r"c:\a\b", r"c:\a\b") && under(r"c:\a\b\c", r"c:\a\b") && under(r"c:\a", r"c:\"));
}

#[test]
fn classify_blocked() {
    let r = rules();
    for p in [
        r"D:\",
        r"D:\Windows\System32\kernel32.dll",
        r"D:\Windows",
        r"D:\Program Files",
        r"D:\Program Files (x86)",
        r"D:\ProgramData",
        r"D:\Program Files\WindowsApps\x",
        r"D:\Program Files\WindowsApps",
        r"D:\Users",
        r"D:\Users\me",
        r"D:\Users\Public",
        r"D:\Users\me\AppData",
        r"D:\Users\me\AppData\Local",
        r"D:\Users\me\NTUSER.DAT",
        r"D:\Users\me\ntuser.dat.LOG1",
        r"D:\Users\me\AppData\Local\Microsoft\Windows\UsrClass.dat",
        r"D:\Users\me\AppData\Roaming\Microsoft\Credentials\abc",
        r"D:\Users\me\AppData\Local\Microsoft\Credentials",
        r"D:\Users\me\AppData\Roaming\Microsoft\Protect\S-1-5-21",
        r"D:\Users\me\AppData\Local\Microsoft\Protect\S-1-5-21",
        r"D:\System Volume Information",
        r"D:\System Volume Information\x",
        r"D:\$Recycle.Bin\S-1-5",
        r"D:\Recovery",
        r"D:\pagefile.sys",
        r"D:\hiberfil.sys",
        r"D:\swapfile.sys",
        r"\\?\D:\WINDOWS\System32\",
    ] {
        assert!(is_blocked(&file(p, &r)), "{p} should be blocked");
        assert!(is_blocked(&dir(p, &r)), "{p} (dir) should be blocked");
    }
    // Not blocked: look-alikes.
    for p in [r"D:\Windows2\x", r"D:\Program Files\App\x", r"D:\Users\me\Downloads\ntuser.txt", r"D:\data\Recovery", r"D:\data\pagefile.sys", r"D:\Users\me\AppData\Local\Temp\x"] {
        assert!(!is_blocked(&file(p, &r)), "{p} should not be blocked");
    }
    // Another drive layout: Windows on C:, profile elsewhere.
    let mut r2 = rules();
    r2.sys.system_root = r"c:\windows".into();
    assert!(is_blocked(&file(r"C:\Windows\System32", &r2)));
    assert!(!is_blocked(&file(r"D:\Windows\System32", &r2)));
}

#[test]
fn classify_system_root_exceptions() {
    let r = rules();
    for p in [r"D:\Windows\Temp\x.tmp", r"D:\Windows\Logs\CBS\a.log", r"D:\Windows\Prefetch\a.pf", r"D:\Windows\Minidump\1.dmp", r"D:\Windows\MEMORY.DMP"] {
        assert_eq!(file(p, &r), Verdict::Allowed, "{p}");
    }
    let v = file(r"D:\Windows\SoftwareDistribution\Download\abc", &r);
    let rs = reasons(&v);
    assert_eq!(rs.len(), 1);
    assert!(rs[0].contains("Windows Update"));
    assert!(is_blocked(&file(r"D:\Windows\SoftwareDistribution\DataStore", &r)));
    assert!(is_blocked(&file(r"D:\Windows\Tempx", &r)));
}

#[test]
fn cleanup_locations_never_blocked() {
    let r = rules();
    for p in [
        r"D:\Users\me\AppData\Local\Temp\foo",
        r"D:\Windows\Temp\foo",
        r"D:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Cache\data_0",
        r"D:\Users\me\AppData\Local\Microsoft\Edge\User Data\Default\Code Cache",
        r"D:\Users\me\AppData\Local\Mozilla\Firefox\Profiles\x.default\cache2",
        r"D:\ProgramData\Microsoft\Windows\WER\ReportArchive\r1",
        r"D:\ProgramData\Microsoft\Windows\WER\ReportQueue\r1",
        r"D:\Users\me\AppData\Local\Microsoft\Windows\WER\ReportArchive",
        r"D:\Users\me\AppData\Local\CrashDumps\a.dmp",
        r"D:\Windows\Minidump\a.dmp",
        r"D:\Windows\MEMORY.DMP",
        r"D:\Users\me\AppData\Local\Microsoft\Windows\Explorer\thumbcache_32.db",
        r"D:\Windows\SoftwareDistribution\Download\x",
        r"D:\Windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache\x",
        r"D:\Users\me\Downloads\old.zip",
        r"D:\Users\me\src\proj\node_modules",
        r"D:\Users\me\src\proj\target",
        r"D:\Users\me\.gradle\caches",
        r"D:\Users\me\AppData\Local\npm-cache",
        r"D:\Users\me\AppData\Local\pip\cache",
        r"D:\Users\me\AppData\Local\Yarn\Cache",
    ] {
        assert!(!is_blocked(&dir(p, &r)), "{p}");
    }
}

#[test]
fn classify_warn_inventory() {
    let mut r = rules();
    r.inv.app_dirs = vec![(r"d:\apps\foo".into(), "Foo".into())];
    r.inv.service_images = vec![(r"d:\svc\s.exe".into(), "Svc".into())];
    r.inv.startup_targets = vec![(r"d:\start\a.exe".into(), "Starter".into())];
    r.inv.process_exes = vec![(r"d:\run\r.exe".into(), 42, "r.exe".into())];
    assert!(reasons(&dir(r"D:\apps\foo\bin", &r))[0].contains("install folder of Foo"));
    assert!(reasons(&dir(r"D:\apps\foo", &r))[0].contains("Foo"));
    assert!(reasons(&dir(r"D:\apps", &r))[0].contains("contains the install folder of Foo"));
    assert!(reasons(&dir(r"D:\svc", &r))[0].contains("Svc"));
    assert!(reasons(&file(r"D:\svc\s.exe", &r))[0].contains("Svc"));
    assert!(reasons(&file(r"D:\start\a.exe", &r))[0].contains("Starter"));
    assert!(reasons(&dir(r"D:\run", &r))[0].contains("running process"));
    assert!(reasons(&file(r"D:\run\r.exe", &r))[0].contains("PID 42"));
    assert_eq!(file(r"D:\run\other.txt", &r), Verdict::Allowed);
    assert_eq!(file(r"D:\other\x.txt", &r), Verdict::Allowed);
    // Two matches, two reasons.
    r.inv.startup_targets.push((r"d:\apps\foo\bin\x.exe".into(), "X".into()));
    assert_eq!(reasons(&file(r"D:\apps\foo\bin\x.exe", &r)).len(), 2);
    // Broad install locations are ignored.
    r.inv.app_dirs.push((r"d:\".into(), "Weird".into()));
    r.inv.app_dirs.push((r"d:\program files".into(), "Weird2".into()));
    assert_eq!(file(r"D:\other\x.txt", &r), Verdict::Allowed);
}

#[test]
fn classify_warn_free() {
    let r = rules();
    let warn = |p: &str, d: bool| {
        let v = classify(Path::new(p), ItemMeta { is_dir: d, ..Default::default() }, &r);
        assert!(matches!(v, Verdict::Warn(_)), "{p}: {v:?}");
        let up = classify(Path::new(&p.to_uppercase()), ItemMeta { is_dir: d, ..Default::default() }, &r);
        assert_eq!(v, up, "case: {p}");
    };
    for p in [
        r"D:\vm\disk.vhdx",
        r"D:\vm\disk.vmdk",
        r"D:\vm\disk.vdi",
        r"D:\Users\me\vault.kdbx",
        r"D:\Users\me\mail.pst",
        r"D:\Users\me\.ssh\id_rsa",
        r"D:\Users\me\.aws\credentials",
        r"D:\Users\me\saved games\x",
        r"D:\Users\me\Documents\My Games\x",
    ] {
        warn(p, false);
    }
    for p in [
        r"D:\src\proj\.git\objects",
        r"D:\Users\me\.kube",
        r"D:\Users\me\.gnupg",
        r"D:\Users\me\Documents",
        r"D:\Users\me\Pictures",
        r"D:\Users\me\Desktop",
        r"D:\Users\me\Downloads",
        r"D:\Users\me\Music",
        r"D:\Users\me\Videos",
        r"D:\OneDrive",
        r"D:\Windows.old\x",
        r"D:\ProgramData\Package Cache\{guid}",
        r"D:\Users\me\AppData\Local\Google\Chrome\User Data\Default",
        r"D:\Users\me\AppData\Local\Google\Chrome\User Data\Profile 1",
        r"D:\Users\me\AppData\Local\Google\Chrome\User Data",
        r"D:\Users\me\AppData\Local\Microsoft\Edge\User Data\Default",
        r"D:\Users\me\AppData\Local\Microsoft\Edge\User Data\Profile 2",
        r"D:\Users\me\AppData\Roaming\Mozilla\Firefox\Profiles\abcd.default-release",
        r"D:\Users\me\AppData\Roaming\Thunderbird\Profiles\abcd.default",
    ] {
        warn(p, true);
    }
    assert_eq!(file(r"D:\Users\me\Documents\report.docx", &r), Verdict::Allowed);
    assert_eq!(dir(r"D:\Users\me\Documents\Projects", &r), Verdict::Allowed);
    assert_eq!(dir(r"D:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Cache", &r), Verdict::Allowed);
}

#[test]
fn log_line_format() {
    let p = Path::new(r"D:\x\y.txt");
    assert_eq!(log_line("2025-01-02 03:04:05", Mode::Permanent, 12, &Ok(()), p), "2025-01-02 03:04:05\tpermanent\t12\tok\tD:\\x\\y.txt");
    assert_eq!(log_line("t", Mode::Recycle, 0, &Err("access denied".into()), p), "t\trecycle\t0\tfailed: access denied\tD:\\x\\y.txt");
}

#[test]
fn relaunch_args_quote_the_root() {
    assert_eq!(relaunch_args(Path::new(r"C:\Program Files")), r#"--page=disk-space --disk-root="C:\Program Files""#);
}

fn target(p: &Path, size: u64, is_dir: bool) -> Target {
    Target { path: p.to_path_buf(), size, is_dir, node: None, file: None }
}

#[test]
fn permanent_delete_in_temp() {
    let base = std::env::temp_dir().join(format!("tmop-del-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (victim, keep) = (base.join("victim"), base.join("keep"));
    std::fs::create_dir_all(victim.join("sub")).unwrap();
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::write(victim.join("sub").join("a.bin"), [1u8; 100]).unwrap();
    std::fs::write(keep.join("precious.txt"), b"x").unwrap();
    let lone = base.join("lone.txt");
    std::fs::write(&lone, [0u8; 7]).unwrap();
    // A directory link inside the victim pointing at `keep`: it must go, the target must stay.
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&keep, victim.join("link")).is_ok();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&keep, victim.join("link")).is_ok();
    let log = base.join("deletions.log");
    let missing = base.join("missing");
    let rep = run_with_log(vec![target(&victim, 100, true), target(&lone, 7, false), target(&missing, 5, false)], Mode::Permanent, Some(&log));
    assert_eq!(rep.deleted.len(), 2);
    assert_eq!(rep.freed, 107);
    assert_eq!(rep.skipped.len(), 1);
    assert_eq!(rep.skipped[0].1, "no longer exists");
    assert!(!victim.exists() && !lone.exists());
    assert!(keep.join("precious.txt").exists(), "link target must survive (linked: {linked})");
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(text.lines().any(|l| l.contains("\tpermanent\t100\tok\t")) && text.contains("failed: no longer exists"));
    // A link on its own is removed without touching its target.
    if linked {
        std::fs::create_dir_all(base.join("x")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&keep, base.join("x").join("l2")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&keep, base.join("x").join("l2")).unwrap();
        delete_permanent(&base.join("x").join("l2")).unwrap();
        assert!(keep.join("precious.txt").exists() && std::fs::symlink_metadata(base.join("x").join("l2")).is_err());
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_refuses_drive_roots() {
    let root = if cfg!(windows) { PathBuf::from(r"C:\") } else { PathBuf::from("/") };
    let rep = run_with_log(vec![target(&root, 1, true)], Mode::Permanent, None);
    assert!(rep.deleted.is_empty() && rep.skipped.len() == 1);
}

#[test]
fn assess_marks_blocked_and_allowed() {
    let r = rules();
    let a = assess(vec![target(Path::new(r"D:\Windows\System32"), 1, true), target(Path::new(r"D:\nonexistent-tmop\x.txt"), 1, false)], &r);
    assert!(is_blocked(&a[0].verdict));
    assert_eq!(a[1].verdict, Verdict::Allowed);
}

#[cfg(windows)]
#[test]
#[ignore = "moves a temp file to the real Recycle Bin"]
fn recycle_delete_in_temp() {
    let f = std::env::temp_dir().join(format!("tmop-recycle-{}.txt", std::process::id()));
    std::fs::write(&f, b"x").unwrap();
    let rep = run_with_log(vec![target(&f, 1, false)], Mode::Recycle, None);
    assert_eq!(rep.deleted.len(), 1, "{:?}", rep.skipped);
    assert!(!f.exists());
}

#[cfg(windows)]
#[test]
fn system_paths_resolve() {
    let s = system_paths();
    assert!(!s.system_root.is_empty() && !s.program_data.is_empty() && !s.users_root.is_empty());
    assert!(!s.profiles.is_empty() && s.known.len() >= 6, "{s:?}");
    let me = norm_path(Path::new(&std::env::var_os("USERPROFILE").unwrap()));
    assert!(s.profiles.contains(&me));
    assert!(is_blocked(&dir(&me, &Rules { sys: s, inv: Inventory::default() })));
}

#[cfg(windows)]
#[test]
fn has_recycle_bin_for_system_drive() {
    let d = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    assert!(has_recycle_bin(Path::new(&format!(r"{d}\"))));
    assert!(!has_recycle_bin(Path::new(r"\\server\share\x")));
}
