//! *Group by app* on the Processes page: which parent a process is folded under.
//!
//! A group is a process with everything started below it, the parent/child links of the snapshot
//! decide. Two kinds of links are not used:
//!
//! - A windowed process always starts its own entry (an app), on every system.
//! - Off Windows, a process started by a *launcher* is the root of its own group instead of a member
//!   of the launcher's. A launcher is the init process (PID 1: systemd, launchd, OpenRC's or
//!   SysV's `init`, a container's init) and, on Linux, a service manager below it: the systemd user
//!   manager (`systemd --user`, also a container's systemd), `runsvdir` (runit) and `s6-svscan`.
//!   Every service, desktop program, daemon and container shim is a child of one of them, so
//!   folding them in would put the whole system into one row. Daemons that fork twice end up there
//!   too (the kernel hands an orphan to the nearest subreaper, else to PID 1), so each is a group of
//!   its own.
//!
//! On Windows nothing is a launcher: there, the processes that start the others (`smss.exe`, the
//! session `winlogon.exe` and `userinit.exe`) exit or are few, and the list was already useful.
//!
//! Kernel threads stay folded into one group under `kthreadd` (PID 2, which is no launcher).
//!
//! The *Tree view* does not use this: it shows every real link ([`super::tree`]).

use super::ProcessRow;
use crate::platform::Os;
use std::collections::HashMap;

/// Linux service managers that start the processes of one service, program or session each.
const LINUX_LAUNCHERS: [&str; 3] = ["systemd", "runsvdir", "s6-svscan"];

/// The parent of row `i` among the processes in `by_pid` (pid to index), or `None`: the parent
/// must not have started after the child (else the PID was reused), PID 0 (the Windows System Idle
/// Process, macOS `kernel_task`) is no parent, and neither is a process of itself. On Windows the
/// System process (PID 4) has no parent either.
pub fn real_parent(os: Os, rows: &[ProcessRow], by_pid: &HashMap<u32, usize>, i: usize) -> Option<usize> {
    let r = &rows[i];
    if r.pid == 0 || (os == Os::Windows && r.pid == 4) || r.parent_pid == r.pid {
        return None;
    }
    by_pid.get(&r.parent_pid).copied().filter(|p| rows[*p].create_time <= r.create_time && rows[*p].pid != 0)
}

/// Whether the processes `r` starts are groups of their own rather than members of its group.
pub fn is_launcher(os: Os, r: &ProcessRow) -> bool {
    match os {
        Os::Windows => false,
        Os::Linux => r.pid == 1 || LINUX_LAUNCHERS.contains(&r.name.as_str()),
        Os::MacOs | Os::Other => r.pid == 1,
    }
}

/// The process whose group row `i` is folded into, or `None` when `i` heads a group itself: a
/// windowed process, a process without a [`real_parent`], and one started by a launcher.
pub fn group_parent(os: Os, rows: &[ProcessRow], by_pid: &HashMap<u32, usize>, i: usize) -> Option<usize> {
    if rows[i].has_window {
        return None;
    }
    real_parent(os, rows, by_pid, i).filter(|p| !is_launcher(os, &rows[*p]))
}

/// The groups of `members` (indices into `rows`): each head with the number of processes in its
/// group (itself included), largest first, then by PID. A process in a loop of links (possible only
/// with equal start times) belongs to no group.
pub fn groups(os: Os, rows: &[ProcessRow], members: &[usize]) -> Vec<(usize, usize)> {
    let mut by_pid: HashMap<u32, usize> = HashMap::with_capacity(members.len());
    for &i in members {
        by_pid.entry(rows[i].pid).or_insert(i);
    }
    let up: HashMap<usize, Option<usize>> = members.iter().map(|&i| (i, group_parent(os, rows, &by_pid, i))).collect();
    let mut size: HashMap<usize, usize> = HashMap::new();
    for &i in members {
        let mut cur = i;
        for _ in 0..=members.len() {
            match up.get(&cur).copied().flatten() {
                Some(p) => cur = p,
                None => {
                    *size.entry(cur).or_default() += 1;
                    break;
                }
            }
        }
    }
    let mut out: Vec<(usize, usize)> = size.into_iter().collect();
    out.sort_by_key(|&(i, n)| (std::cmp::Reverse(n), rows[i].pid));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process: pid, parent, start time, name.
    fn p(pid: u32, ppid: u32, ct: i64, name: &str) -> ProcessRow {
        ProcessRow { pid, parent_pid: ppid, create_time: ct, name: name.into(), ..Default::default() }
    }

    fn windowed(mut r: ProcessRow) -> ProcessRow {
        r.has_window = true;
        r
    }

    /// (head name, size) of every group.
    fn shape(os: Os, rows: &[ProcessRow]) -> Vec<(String, usize)> {
        let all: Vec<usize> = (0..rows.len()).collect();
        groups(os, rows, &all).into_iter().map(|(i, n)| (rows[i].name.clone(), n)).collect()
    }

    fn named(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(s, n)| (s.to_string(), *n)).collect()
    }

    /// The group head of every process, by pid (the head of a head is itself).
    fn heads(os: Os, rows: &[ProcessRow]) -> HashMap<u32, u32> {
        let by_pid: HashMap<u32, usize> = rows.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        (0..rows.len())
            .map(|i| {
                let mut cur = i;
                while let Some(p) = group_parent(os, rows, &by_pid, cur) {
                    cur = p;
                }
                (rows[i].pid, rows[cur].pid)
            })
            .collect()
    }

    fn kernel(rows: &mut Vec<ProcessRow>, n: u32) {
        rows.push(p(2, 0, 1, "kthreadd"));
        for k in 0..n {
            // PID 4 is a kernel thread on Linux (the System process is PID 4 on Windows).
            let name = if k == 1 { "kworker/R-rcu_gp".to_string() } else { format!("kworker/{k}:0") };
            rows.push(p(3 + k, 2, 1, &name));
        }
    }

    /// A headless server: sshd with two sessions, Docker with two containers, cron, journald,
    /// and kernel threads.
    fn server() -> Vec<ProcessRow> {
        let mut rows = vec![p(1, 0, 1, "systemd")];
        kernel(&mut rows, 40);
        rows.extend([
            p(300, 1, 10, "systemd-journald"),
            p(310, 1, 10, "systemd-udevd"),
            p(500, 1, 20, "sshd"),
            p(900, 500, 90, "sshd"),
            p(905, 900, 91, "sshd"),
            p(906, 905, 92, "bash"),
            p(907, 906, 93, "top"),
            p(950, 500, 95, "sshd"),
            p(955, 950, 96, "sshd"),
            p(956, 955, 97, "bash"),
            p(600, 1, 30, "containerd"),
            p(610, 1, 31, "dockerd"),
            p(620, 610, 32, "docker-proxy"),
            // runc shims daemonize: their parent is init.
            p(700, 1, 40, "containerd-shim-runc-v2"),
            p(710, 700, 41, "nginx"),
            p(711, 710, 42, "nginx"),
            p(712, 710, 42, "nginx"),
            p(800, 1, 50, "containerd-shim-runc-v2"),
            p(810, 800, 51, "postgres"),
            p(811, 810, 52, "postgres"),
            p(400, 1, 15, "cron"),
            // The user manager of the signed-in user, with its own services.
            p(880, 1, 85, "systemd"),
            p(881, 880, 86, "(sd-pam)"),
            p(882, 880, 86, "dbus-daemon"),
        ]);
        rows
    }

    #[test]
    fn a_server_has_one_group_per_service_and_container() {
        let rows = server();
        let g = shape(Os::Linux, &rows);
        assert_eq!(
            g,
            named(&[
                ("kthreadd", 41),
                ("sshd", 8),
                ("containerd-shim-runc-v2", 4),
                ("containerd-shim-runc-v2", 3),
                ("dockerd", 2),
                ("systemd", 1),
                ("systemd-journald", 1),
                ("systemd-udevd", 1),
                ("cron", 1),
                ("containerd", 1),
                ("systemd", 1),
                ("(sd-pam)", 1),
                ("dbus-daemon", 1),
            ])
        );
        let h = heads(Os::Linux, &rows);
        // Every kernel thread, PID 4 too, sits under kthreadd.
        assert!((3..43).all(|k| h[&k] == 2));
        assert_eq!((h[&907], h[&956], h[&712], h[&811], h[&620]), (500, 500, 700, 800, 610));
        // Every process is in exactly one group.
        assert_eq!(g.iter().map(|x| x.1).sum::<usize>(), rows.len());
    }

    #[test]
    fn before_the_rule_init_and_kthreadd_took_everything() {
        // The same table on the Windows path (no launchers) folds into init and kthreadd, which is
        // what Linux showed before (plus PID 4, then always parentless).
        let rows = server();
        let g = shape(Os::Windows, &rows);
        assert_eq!(g[..2], named(&[("kthreadd", 40), ("systemd", 25)])[..]);
        assert_eq!(g.len(), 3);
        assert_eq!(g[2], ("kworker/R-rcu_gp".to_string(), 1));
    }

    /// A GNOME desktop: gdm, the user manager with the shell and the session services, apps
    /// launched into app-*.scope (their parent is the user manager), a browser with helpers, and
    /// a terminal running a shell.
    fn desktop() -> Vec<ProcessRow> {
        let mut rows = vec![p(1, 0, 1, "systemd")];
        kernel(&mut rows, 4);
        rows.extend([
            p(700, 1, 30, "gdm3"),
            p(720, 700, 31, "gdm-session-wor"),
            p(730, 720, 32, "gdm-wayland-ses"),
            p(731, 730, 33, "gnome-session-binary"),
            p(1500, 1, 40, "systemd"),
            p(1510, 1500, 41, "pipewire"),
            p(1511, 1500, 41, "wireplumber"),
            p(1520, 1500, 42, "dbus-daemon"),
            p(1600, 1500, 50, "gnome-shell"),
            p(1610, 1600, 51, "Xwayland"),
            p(1620, 1600, 52, "ibus-daemon"),
            p(1621, 1620, 53, "ibus-engine-simple"),
            windowed(p(2000, 1500, 60, "firefox")),
            p(2010, 2000, 61, "Socket Process"),
            p(2020, 2000, 62, "Privileged Cont"),
            p(2030, 2000, 63, "Isolated Web Co"),
            p(2031, 2000, 63, "Isolated Web Co"),
            p(2040, 2000, 64, "Web Content"),
            p(2100, 1500, 70, "gnome-terminal-server"),
            p(2110, 2100, 71, "bash"),
            p(2111, 2110, 72, "cargo"),
            p(2112, 2111, 73, "rustc"),
            // A program started from the terminal opens a window: its own app.
            windowed(p(2120, 2110, 74, "gedit")),
            // An app moved into its scope after a double fork: reparented to the user manager.
            windowed(p(2200, 1500, 80, "code")),
            p(2210, 2200, 81, "code"),
            p(2211, 2200, 81, "code"),
        ]);
        rows
    }

    #[test]
    fn a_desktop_has_one_group_per_app_and_session_service() {
        let rows = desktop();
        let h = heads(Os::Linux, &rows);
        // The browser with its helpers, the editor with its helpers.
        assert!([2010, 2020, 2030, 2031, 2040].iter().all(|k| h[k] == 2000));
        assert_eq!((h[&2210], h[&2211]), (2200, 2200));
        // The terminal with its shell and what runs in it, but a windowed program is its own.
        assert_eq!((h[&2111], h[&2112], h[&2120]), (2100, 2100, 2120));
        // The session services are separate entries, not one "systemd --user" row.
        assert_eq!((h[&1510], h[&1511], h[&1520], h[&1600], h[&1621]), (1510, 1511, 1520, 1600, 1600));
        assert_eq!((h[&731], h[&1500]), (700, 1500));
        let g = shape(Os::Linux, &rows);
        assert_eq!(g[0], ("firefox".to_string(), 6));
        assert_eq!(g.iter().find(|x| x.0 == "kthreadd").map(|x| x.1), Some(5));
        assert_eq!(g.iter().map(|x| x.1).sum::<usize>(), rows.len());
    }

    #[test]
    fn runit_and_sysv_init() {
        // runit: runsvdir starts one runsv per service, which runs the service and its logger.
        let rows = vec![
            p(1, 0, 1, "runit"),
            p(50, 1, 2, "runsvdir"),
            p(60, 50, 3, "runsv"),
            p(61, 60, 4, "sshd"),
            p(62, 60, 4, "svlogd"),
            p(70, 50, 3, "runsv"),
            p(71, 70, 4, "nginx"),
            p(72, 71, 5, "nginx"),
            p(80, 1, 6, "agetty"),
        ];
        let h = heads(Os::Linux, &rows);
        assert_eq!((h[&61], h[&62], h[&72], h[&50], h[&80]), (60, 60, 70, 50, 80));
        // SysV / OpenRC: double-forked daemons are children of init.
        let rows = vec![p(1, 0, 1, "init"), p(300, 1, 5, "syslogd"), p(310, 1, 6, "crond"), p(320, 1, 7, "sshd"), p(330, 320, 8, "sshd"), p(331, 330, 9, "sh")];
        let g = shape(Os::Linux, &rows);
        assert_eq!(g, named(&[("sshd", 3), ("init", 1), ("syslogd", 1), ("crond", 1)]));
    }

    #[test]
    fn reused_pids_and_orphans() {
        let rows = vec![
            p(1, 0, 1, "systemd"),
            // 40 names 30 as its parent, but this 30 started later: the PID was reused.
            p(30, 1, 50, "new holder of 30"),
            p(40, 30, 20, "old child"),
            // The parent of 60 is gone: it heads a group with its child.
            p(60, 999, 30, "orphan"),
            p(61, 60, 31, "kid"),
            // A daemon that forked twice: its parent is init.
            p(70, 1, 40, "daemon"),
            p(71, 70, 41, "worker"),
        ];
        let h = heads(Os::Linux, &rows);
        assert_eq!((h[&40], h[&30], h[&61], h[&71]), (40, 30, 60, 70));
    }

    #[test]
    fn macos_launchd_starts_groups() {
        let rows = vec![
            p(0, 0, 0, "kernel_task"),
            p(1, 0, 1, "launchd"),
            p(90, 1, 2, "logd"),
            p(100, 1, 3, "WindowServer"),
            windowed(p(400, 1, 9, "Safari")),
            p(410, 1, 10, "com.apple.WebKit.WebContent"),
            windowed(p(500, 1, 11, "Terminal")),
            p(510, 500, 12, "login"),
            p(511, 510, 13, "zsh"),
            p(4, 1, 14, "some daemon with pid 4"),
            p(5, 4, 15, "its child"),
        ];
        let h = heads(Os::MacOs, &rows);
        assert_eq!((h[&90], h[&100], h[&410], h[&511], h[&1], h[&0]), (90, 100, 410, 500, 1, 0));
        // PID 4 is an ordinary process off Windows.
        assert_eq!(h[&5], 4);
        assert_eq!(shape(Os::MacOs, &rows)[0], ("Terminal".to_string(), 3));
    }

    #[test]
    fn windows_keeps_every_link() {
        // Nothing is a launcher on Windows, not even a PID 1 or a process called systemd.
        assert!(!is_launcher(Os::Windows, &p(1, 0, 1, "systemd")));
        let rows = vec![
            p(0, 0, 0, "System Idle Process"),
            p(4, 0, 0, "System"),
            p(120, 4, 1, "Registry"),
            p(500, 4, 2, "smss.exe"),
            // wininit's parent (an smss instance) exited.
            p(600, 480, 3, "wininit.exe"),
            p(700, 600, 4, "services.exe"),
            p(800, 700, 5, "svchost.exe"),
            p(810, 800, 6, "dllhost.exe"),
            p(900, 600, 4, "lsass.exe"),
            windowed(p(3000, 2900, 20, "explorer.exe")),
            windowed(p(3100, 3000, 21, "WindowsTerminal.exe")),
            p(3110, 3100, 22, "OpenConsole.exe"),
            p(3120, 3100, 22, "pwsh.exe"),
            // A PID 1 is an ordinary process on Windows.
            p(1, 4, 30, "pid 1"),
        ];
        let h = heads(Os::Windows, &rows);
        assert_eq!((h[&120], h[&500], h[&810], h[&900], h[&4], h[&0]), (4, 4, 600, 600, 4, 0));
        assert_eq!((h[&3100], h[&3110], h[&3120]), (3100, 3100, 3100));
        assert_eq!(h[&1], 4);
        // The same as the rule before this module: the parent link unless the process is windowed
        // or one of PID 0 and 4.
        let by_pid: HashMap<u32, usize> = rows.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        for i in 0..rows.len() {
            let r = &rows[i];
            let old = if r.pid == 0 || r.pid == 4 || r.parent_pid == r.pid || r.has_window {
                None
            } else {
                by_pid.get(&r.parent_pid).copied().filter(|p| rows[*p].create_time <= r.create_time && rows[*p].pid != 0)
            };
            assert_eq!(group_parent(Os::Windows, &rows, &by_pid, i), old, "{}", r.name);
        }
    }

    #[test]
    fn real_parent_ignores_launchers() {
        // The parent shown in the details is the real one, launcher or not.
        let rows = vec![p(1, 0, 1, "systemd"), p(4, 2, 1, "kworker/R-rcu_gp"), p(2, 0, 1, "kthreadd"), p(50, 1, 2, "sshd")];
        let by_pid: HashMap<u32, usize> = rows.iter().enumerate().map(|(i, r)| (r.pid, i)).collect();
        assert_eq!(real_parent(Os::Linux, &rows, &by_pid, 3), Some(0));
        assert_eq!(group_parent(Os::Linux, &rows, &by_pid, 3), None);
        assert_eq!(real_parent(Os::Linux, &rows, &by_pid, 1), Some(2));
        assert_eq!(real_parent(Os::Windows, &rows, &by_pid, 1), None);
    }

    #[test]
    fn loops_do_not_hang() {
        let rows = vec![p(8, 9, 1, "x"), p(9, 8, 1, "y"), p(10, 1, 2, "z")];
        let g = groups(Os::Linux, &rows, &[0, 1, 2]);
        assert_eq!(g, vec![(2, 1)]);
    }
}
