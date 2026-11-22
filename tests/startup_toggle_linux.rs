//! Startup apps Enable / Disable through the real `set_enabled` and `startup_apps` on Linux, with
//! the XDG folders pointed at temporary ones. It sets environment variables, so it is a test
//! binary of its own with a single test. It runs only on GitHub Actions runners.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::Path;
use tmop::pro::inventory::toggle::{set_enabled, Switch};
use tmop::pro::inventory::{startup_apps, StartupApp};

/// Written past `cargo test`'s capture (it only captures `print!`), so the CI log shows it.
fn report(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn row(name: &str) -> StartupApp {
    let all = startup_apps().unwrap();
    all.into_iter().find(|a| a.name == name).unwrap_or_else(|| panic!("{name} is not listed"))
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).map(|rd| rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    v.sort();
    v
}

#[test]
fn ci_only_autostart_disable_and_enable() {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        report("ci_only_autostart_disable_and_enable: skipped, it only runs on GitHub Actions runners");
        return;
    }
    let root = std::env::temp_dir().join(format!("tmop-ci-autostart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (config, etc) = (root.join("config"), root.join("etc"));
    let (user, system) = (config.join("autostart"), etc.join("autostart"));
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &config);
    std::env::set_var("XDG_CONFIG_DIRS", &etc);
    std::env::remove_var("XDG_CURRENT_DESKTOP");

    let user_text = b"[Desktop Entry]\r\nType=Application\r\nName=TMOP CI user\r\nName[de]=TMOP CI Benutzer\r\nExec=true --user\r\n# a comment\r\n".to_vec();
    let system_text = b"[Desktop Entry]\nType=Application\nName=TMOP CI system\nExec=true --system".to_vec();
    std::fs::write(user.join("tmop-ci-user.desktop"), &user_text).unwrap();
    std::fs::write(system.join("tmop-ci-system.desktop"), &system_text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(user.join("tmop-ci-user.desktop"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // The user's own file: switched in place, restored byte for byte.
    let a = row("TMOP CI user");
    assert!(a.enabled);
    assert!(matches!(&a.switch, Switch::Desktop { user: true, .. }), "{:?}", a.switch);
    set_enabled(&a, false).unwrap();
    let a = row("TMOP CI user");
    report(&format!("autostart: user entry after disable: enabled={} at {}", a.enabled, a.location));
    assert!(!a.enabled);
    assert!(String::from_utf8_lossy(&std::fs::read(user.join("tmop-ci-user.desktop")).unwrap()).contains("\r\nHidden=true\r\n"));
    set_enabled(&a, true).unwrap();
    assert!(row("TMOP CI user").enabled);
    assert_eq!(std::fs::read(user.join("tmop-ci-user.desktop")).unwrap(), user_text);
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(user.join("tmop-ci-user.desktop")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // The all-users file: a copy in the user's folder turns it off, Enable deletes the copy.
    let s = row("TMOP CI system");
    assert!(s.enabled && s.location == "Autostart: all users", "{s:?}");
    set_enabled(&s, false).unwrap();
    let s = row("TMOP CI system");
    report(&format!("autostart: all-users entry after disable: enabled={} at {}", s.enabled, s.location));
    assert!(!s.enabled);
    assert_eq!(s.location, "Autostart: current user");
    assert_eq!(std::fs::read(system.join("tmop-ci-system.desktop")).unwrap(), system_text, "the all-users file is untouched");
    set_enabled(&s, true).unwrap();
    let s = row("TMOP CI system");
    assert!(s.enabled && s.location == "Autostart: all users", "{s:?}");
    assert_eq!(names(&user), ["tmop-ci-user.desktop"], "the copy is gone and no temporary file is left");
    assert_eq!(std::fs::read(system.join("tmop-ci-system.desktop")).unwrap(), system_text);
    report("autostart: user and all-users round trips passed");
    let _ = std::fs::remove_dir_all(&root);
}
