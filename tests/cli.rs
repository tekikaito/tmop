//! `TMOP --version`, `--help` and an unknown option end without a window (#106).

use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_TMOP")).args(args).stdin(Stdio::null()).output().expect("run TMOP")
}

#[test]
fn version_prints_the_version() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("tmop {}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn help_lists_the_user_options() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let stdout = String::from_utf8_lossy(&out.stdout);
        for o in ["Usage:", "--dump", "--screenshot=<file.png>", "--version", "--help"] {
            assert!(stdout.contains(o), "{flag}: no {o} in {stdout}");
        }
    }
}

#[test]
fn unknown_option_exits_with_2_and_points_to_help() {
    let out = run(&["--no-such-option"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--no-such-option") && stderr.contains("--help"), "stderr: {stderr}");
}
