//! Pure parsers behind the Linux process backend (`linux.rs`): `/proc/<pid>/{stat,status,io,
//! cmdline}`, the ELF header of an image, `.desktop` entries and the `pkexec` argument vectors.
//! No I/O, so the fixture tests run on every host.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};

/// `PF_KTHREAD` in the flags field of `/proc/<pid>/stat`: a kernel thread.
pub const PF_KTHREAD: u64 = 0x0020_0000;
/// 100 ns intervals between 1601-01-01 (FILETIME) and 1970-01-01.
pub const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// The fields of `/proc/<pid>/stat` the process table uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    /// The kernel's name (at most 15 bytes).
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub session: u32,
    pub flags: u64,
    /// Minor plus major page faults.
    pub faults: u64,
    /// utime + stime in clock ticks.
    pub cpu_ticks: u64,
    pub nice: i32,
    pub threads: u32,
    /// Start time in clock ticks after boot.
    pub start_ticks: u64,
}

impl Stat {
    pub fn kernel_thread(&self) -> bool {
        self.flags & PF_KTHREAD != 0
    }
}

/// Parses `/proc/<pid>/stat`. The name sits in parentheses and may itself hold spaces and `)`,
/// so the fields start after the last `)`.
pub fn parse_stat(s: &str) -> Option<Stat> {
    let (open, close) = (s.find('(')?, s.rfind(')')?);
    if close < open {
        return None;
    }
    let f: Vec<&str> = s[close + 1..].split_ascii_whitespace().collect();
    if f.len() < 20 {
        return None;
    }
    let n = |i: usize| f[i].parse::<u64>().ok();
    Some(Stat {
        comm: s[open + 1..close].to_string(),
        state: f[0].chars().next()?,
        ppid: f[1].parse().ok()?,
        session: f[3].parse().ok()?,
        flags: n(6)?,
        faults: n(7)?.saturating_add(n(9)?),
        cpu_ticks: n(11)?.saturating_add(n(12)?),
        nice: f[16].parse().ok()?,
        threads: f[17].parse().ok()?,
        start_ticks: n(19)?,
    })
}

/// The fields of `/proc/<pid>/status` the process table uses; sizes in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The effective uid (what `ps` shows as the user).
    pub euid: Option<u32>,
    pub rss: u64,
    pub peak_rss: u64,
    /// Resident anonymous memory: private to the process (shared memory is `RssShmem`).
    pub rss_anon: u64,
    pub swap: u64,
}

pub fn parse_status(s: &str) -> Status {
    let mut st = Status::default();
    for line in s.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let kb = || value.split_ascii_whitespace().next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        match key {
            "Uid" => st.euid = value.split_ascii_whitespace().nth(1).and_then(|v| v.parse().ok()),
            "VmRSS" => st.rss = kb(),
            "VmHWM" => st.peak_rss = kb(),
            "RssAnon" => st.rss_anon = kb(),
            "VmSwap" => st.swap = kb(),
            _ => {}
        }
    }
    st
}

/// `rchar` and `wchar` of `/proc/<pid>/io`: bytes read and written by any I/O call, like the
/// Windows transfer counts the Disk columns show.
pub fn parse_io(s: &str) -> Option<(u64, u64)> {
    let field = |name: &str| s.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':')?.trim().parse::<u64>().ok());
    Some((field("rchar")?, field("wchar")?))
}

/// `btime` of `/proc/stat`: the boot time in seconds since the epoch.
pub fn parse_btime(s: &str) -> Option<u64> {
    s.lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())
}

/// A process start as a FILETIME (100 ns since 1601), like the Windows creation time: stable
/// for the life of the process, so it keys (pid, start) pairs and shows the start time.
pub fn create_stamp(boot_secs: u64, start_ticks: u64, clk_tck: u64) -> i64 {
    let after_boot = (start_ticks as u128 * 10_000_000 / clk_tck.max(1) as u128) as i64;
    FILETIME_UNIX + boot_secs as i64 * 10_000_000 + after_boot
}

/// Clock ticks in 100 ns units ([`super::ProcessRow::cpu_time`]).
pub fn ticks_to_100ns(ticks: u64, clk_tck: u64) -> u64 {
    (ticks as u128 * 10_000_000 / clk_tck.max(1) as u128) as u64
}

/// The NUL-separated `/proc/<pid>/cmdline` as arguments (empty for kernel threads and zombies).
pub fn cmdline_args(raw: &[u8]) -> Vec<String> {
    let raw = raw.strip_suffix(b"\0").unwrap_or(raw);
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

/// Arguments as one line; arguments with blanks or quotes are double-quoted.
pub fn join_args(args: &[String]) -> Option<String> {
    if args.iter().all(|a| a.is_empty()) {
        return None;
    }
    let quote = |a: &String| {
        if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"') {
            a.clone()
        } else {
            format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
        }
    };
    Some(args.iter().map(quote).collect::<Vec<_>>().join(" "))
}

/// `readlink /proc/<pid>/exe` without the ` (deleted)` the kernel appends once the file is gone.
pub fn clean_exe(link: &str) -> &str {
    link.strip_suffix(" (deleted)").unwrap_or(link)
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The kernel keeps at most this many bytes of a name.
const COMM_MAX: usize = 15;

/// The name shown for a process: the kernel's name, unless it was cut to [`COMM_MAX`] bytes;
/// then the image file name it is a prefix of, else the file name of `argv[0]` or `argv[1]` (a
/// script) on the same terms.
pub fn display_name(comm: &str, exe: Option<&str>, args: &[String]) -> String {
    if comm.len() < COMM_MAX {
        return comm.to_string();
    }
    let first_word = |a: &String| a.split_ascii_whitespace().next().map(str::to_string);
    let args: Vec<String> = args.iter().take(2).filter_map(first_word).collect();
    for cand in exe.into_iter().chain(args.iter().map(String::as_str)).map(base) {
        if !cand.is_empty() && !comm.is_empty() && cand.starts_with(comm) {
            return cand.to_string();
        }
    }
    comm.to_string()
}

/// The architecture in an ELF header (`e_machine` at offset 18, byte order from `EI_DATA`).
pub fn elf_arch(header: &[u8]) -> &'static str {
    if header.len() < 20 || &header[..4] != b"\x7fELF" {
        return "";
    }
    let m = match header[5] {
        2 => u16::from_be_bytes([header[18], header[19]]),
        _ => u16::from_le_bytes([header[18], header[19]]),
    };
    match m {
        0x3E => "x64",
        0x03 => "x86",
        0xB7 => "ARM64",
        0x28 => "ARM",
        0xF3 => "RISC-V",
        _ => "",
    }
}

/// What the process table takes from a `.desktop` entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    /// File name of the program `Exec` starts (behind `env`, or `flatpak run --command=`).
    pub exec: Option<String>,
    pub try_exec: Option<String>,
    pub wm_class: Option<String>,
    /// Runs inside a terminal (no window of its own).
    pub terminal: bool,
    /// `NoDisplay=true`: a helper, not an app with a window of its own.
    pub no_display: bool,
}

/// Parses the `[Desktop Entry]` group of an application entry; `None` for other types, hidden
/// (deleted) entries and entries without a name.
pub fn parse_desktop(s: &str) -> Option<DesktopEntry> {
    let mut e = DesktopEntry::default();
    let (mut in_group, mut app) = (false, false);
    for line in s.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Type" => app = v == "Application",
            "Name" => e.name = v.to_string(),
            "Exec" => e.exec = exec_program(v),
            "TryExec" => e.try_exec = Some(base(v).to_string()).filter(|b| !b.is_empty()),
            "StartupWMClass" => e.wm_class = Some(v.to_string()).filter(|b| !b.is_empty()),
            "Terminal" => e.terminal = v == "true",
            "NoDisplay" => e.no_display = v == "true",
            "Hidden" if v == "true" => return None,
            _ => {}
        }
    }
    (app && !e.name.is_empty()).then_some(e)
}

/// Splits an `Exec` value into arguments: blanks separate, double quotes group, `\` escapes.
fn exec_args(v: &str) -> Vec<String> {
    let (mut args, mut cur, mut quoted, mut escaped, mut any) = (Vec::new(), String::new(), false, false, false);
    for c in v.chars() {
        match c {
            _ if escaped => {
                cur.push(c);
                escaped = false;
            }
            '\\' => escaped = true,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args
}

/// The program file name an `Exec` line starts.
fn exec_program(v: &str) -> Option<String> {
    let args = exec_args(v);
    let mut it = args.iter().skip_while(|a| a.as_str() == "env" || (a.contains('=') && !a.starts_with('/')));
    let prog = base(it.next()?);
    if prog == "flatpak" {
        return args.iter().find_map(|a| a.strip_prefix("--command=")).map(|c| base(c).to_string());
    }
    Some(prog.to_string()).filter(|p| !p.is_empty() && !p.starts_with('%'))
}

/// An app found in the `.desktop` entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopApp {
    pub name: String,
    /// It has a window of its own (not a terminal program, not hidden from menus).
    pub windowed: bool,
}

/// Desktop entries (highest priority first) by lower-case program file name and window class.
/// The first entry for a key wins, unless a later one is a windowed app and the first is not.
pub fn index_desktop(entries: impl IntoIterator<Item = DesktopEntry>) -> HashMap<String, DesktopApp> {
    let mut map: HashMap<String, DesktopApp> = HashMap::new();
    for e in entries {
        let app = DesktopApp { name: e.name.clone(), windowed: !e.terminal && !e.no_display };
        for key in [e.exec, e.try_exec, e.wm_class].into_iter().flatten() {
            let key = key.to_lowercase();
            match map.get(&key) {
                Some(have) if have.windowed || !app.windowed => {}
                _ => {
                    map.insert(key, app.clone());
                }
            }
        }
    }
    map
}

/// The key a process is looked up by in [`index_desktop`]: its image file name, else its name.
pub fn app_key(exe: Option<&str>, comm: &str) -> String {
    exe.map(base).filter(|b| !b.is_empty()).unwrap_or(comm).to_lowercase()
}

/// The window-owner heuristic without a window list (Wayland): processes of `uid` whose program
/// is a windowed desktop app and whose parent is not the same app (its helper processes are).
/// Items are (pid, parent pid, euid, app key when it is a windowed desktop app).
pub fn heuristic_windows(procs: &[(u32, u32, Option<u32>, Option<&str>)], uid: u32) -> HashSet<u32> {
    let app_of: HashMap<u32, Option<&str>> = procs.iter().map(|p| (p.0, p.3)).collect();
    procs
        .iter()
        .filter(|(pid, ppid, euid, app)| *euid == Some(uid) && app.is_some() && app_of.get(ppid).copied().flatten() != *app && *pid != *ppid)
        .map(|p| p.0)
        .collect()
}

/// What a `pkexec` retry runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevated {
    /// `kill -s <SIG> <pid>...` (TERM, STOP, CONT).
    Signal(&'static str),
    /// `renice -n <nice> -p <tid>...`.
    Renice(i32),
    /// `chrt --other --all-tasks --pid 0 <pid>`: back from `SCHED_IDLE` (efficiency mode off).
    NormalPolicy,
}

/// The programs an [`Elevated`] run uses (absolute paths).
pub struct Tools {
    pub pkexec: String,
    pub kill: String,
    pub renice: String,
    pub chrt: Option<String>,
}

/// The argument vector for `pkexec` (never a shell string): `[pkexec, kill|renice|chrt, args...]`;
/// `None` when the program it needs is missing.
pub fn elevated_argv(t: &Tools, op: Elevated, ids: &[u32]) -> Option<Vec<String>> {
    let mut argv = vec![t.pkexec.clone()];
    match op {
        Elevated::Signal(sig) => argv.extend([t.kill.clone(), "-s".into(), sig.into(), "--".into()]),
        Elevated::Renice(n) => argv.extend([t.renice.clone(), "-n".into(), n.to_string(), "-p".into()]),
        Elevated::NormalPolicy => argv.extend([t.chrt.clone()?, "--other".into(), "--all-tasks".into(), "--pid".into(), "0".into()]),
    }
    argv.extend(ids.iter().map(u32::to_string));
    Some(argv)
}

/// What a failed `pkexec` run means: 126 is a dismissed or refused authorization, 127 no way to
/// authenticate (no polkit agent).
pub fn pkexec_error(code: Option<i32>, stderr_line: &str) -> String {
    match code {
        Some(126) => "the authorization prompt was dismissed or refused".into(),
        Some(127) => "pkexec could not ask for authorization (no polkit agent is running)".into(),
        _ if !stderr_line.is_empty() => stderr_line.to_string(),
        Some(c) => format!("the elevated command failed with exit code {c}"),
        None => "the elevated command was terminated".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4242 (Web Content) S 4100 4100 4100 0 -1 4194560 120 0 7 0 250 50 0 0 20 0 31 0 98765 1234567 890 18446744073709551615 0 0 0 0 0 0 0 4096 0 0 0 17 3 0 0 0 0 0";
    const STATUS: &str = "Name:\tWeb Content\nUmask:\t0022\nState:\tS (sleeping)\nUid:\t1000\t1001\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nVmHWM:\t  204800 kB\nVmRSS:\t  102400 kB\nRssAnon:\t   81920 kB\nRssFile:\t   20480 kB\nVmSwap:\t    1024 kB\nThreads:\t31\n";

    #[test]
    fn stat_fields() {
        let s = parse_stat(STAT).unwrap();
        assert_eq!((s.comm.as_str(), s.state, s.ppid, s.session), ("Web Content", 'S', 4100, 4100));
        assert_eq!((s.faults, s.cpu_ticks, s.nice, s.threads, s.start_ticks), (127, 300, 0, 31, 98765));
        assert!(!s.kernel_thread());
        // A name with blanks and parentheses, a negative nice and a kernel thread.
        let odd = parse_stat("7 (a) b (c)) R 2 0 0 0 -1 2129984 0 0 0 0 1 2 0 0 0 -20 1 0 55 0 0").unwrap();
        assert_eq!((odd.comm.as_str(), odd.state, odd.ppid, odd.nice, odd.start_ticks), ("a) b (c)", 'R', 2, -20, 55));
        assert!(odd.kernel_thread());
        assert_eq!(parse_stat("1 (init) S 0 1"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn status_fields() {
        let s = parse_status(STATUS);
        assert_eq!(s, Status { euid: Some(1001), rss: 102_400 * 1024, peak_rss: 204_800 * 1024, rss_anon: 81_920 * 1024, swap: 1024 * 1024 });
        // Kernel threads have no memory lines.
        assert_eq!(parse_status("Name:\tkworker/0:1\nUid:\t0\t0\t0\t0\n"), Status { euid: Some(0), ..Default::default() });
    }

    #[test]
    fn io_and_btime() {
        let io = "rchar: 12345\nwchar: 678\nsyscr: 9\nsyscw: 3\nread_bytes: 4096\nwrite_bytes: 0\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_io(io), Some((12345, 678)));
        assert_eq!(parse_io("syscr: 1\n"), None);
        assert_eq!(parse_btime("cpu  1 2 3\nintr 5\nbtime 1700000000\nprocesses 99\n"), Some(1_700_000_000));
        assert_eq!(parse_btime("cpu 1\n"), None);
    }

    #[test]
    fn times_convert_to_filetime_units() {
        assert_eq!(ticks_to_100ns(250, 100), 25_000_000);
        assert_eq!(ticks_to_100ns(3, 0), 30_000_000);
        // Started 12.5 s after a boot at 2023-11-14 22:13:20 UTC.
        let ft = create_stamp(1_700_000_000, 1250, 100);
        assert_eq!(ft, FILETIME_UNIX + 1_700_000_000 * 10_000_000 + 125_000_000);
        assert!(create_stamp(1_700_000_000, 1251, 100) > ft);
    }

    #[test]
    fn command_lines() {
        let args = cmdline_args(b"/usr/bin/python3\0-m\0http.server\0my dir\0");
        assert_eq!(args, ["/usr/bin/python3", "-m", "http.server", "my dir"]);
        assert_eq!(join_args(&args).unwrap(), "/usr/bin/python3 -m http.server \"my dir\"");
        assert_eq!(join_args(&["a\"b".into(), String::new()]).unwrap(), "\"a\\\"b\" \"\"");
        // Rewritten (one string) and empty command lines.
        assert_eq!(cmdline_args(b"/opt/app --type=renderer   "), ["/opt/app --type=renderer   "]);
        assert!(cmdline_args(b"").is_empty() && join_args(&[]).is_none());
    }

    #[test]
    fn names() {
        assert_eq!(clean_exe("/usr/bin/foo (deleted)"), "/usr/bin/foo");
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(display_name("gnome-shell-cal", Some("/usr/libexec/gnome-shell-calendar-server"), &[]), "gnome-shell-calendar-server");
        // A script: the image is the interpreter, the kernel name is the script's.
        assert_eq!(display_name("myscript", Some("/usr/bin/python3.12"), &a(&["/usr/bin/python3", "./myscript"])), "myscript");
        assert_eq!(display_name("unattended-upgr", None, &a(&["/usr/bin/python3", "/usr/share/unattended-upgrades/unattended-upgrade-shutdown"])), "unattended-upgrade-shutdown");
        // No image (another user's process): argv[0].
        assert_eq!(display_name("systemd-journal", None, &a(&["/usr/lib/systemd/systemd-journald"])), "systemd-journald");
        assert_eq!(display_name("kworker/0:1", None, &[]), "kworker/0:1");
        assert_eq!(display_name("sshd", None, &a(&["sshd: alex [priv]"])), "sshd");
        assert_eq!(display_name("python3", Some("/usr/bin/python3.12"), &[]), "python3");
    }

    #[test]
    fn elf_machines() {
        let mut h = [0u8; 64];
        h[..4].copy_from_slice(b"\x7fELF");
        h[5] = 1;
        h[18] = 0x3E;
        assert_eq!(elf_arch(&h), "x64");
        h[18] = 0xB7;
        assert_eq!(elf_arch(&h), "ARM64");
        h[18] = 0x03;
        assert_eq!(elf_arch(&h), "x86");
        h[5] = 2;
        h[18..20].copy_from_slice(&[0, 0x28]);
        assert_eq!(elf_arch(&h), "ARM");
        assert_eq!(elf_arch(b"#!/bin/sh\n"), "");
        assert_eq!(elf_arch(&h[..10]), "");
    }

    #[test]
    fn desktop_entries() {
        let firefox = "[Desktop Entry]\nVersion=1.0\nName=Firefox Web Browser\nName[de]=Firefox-Webbrowser\nExec=env MOZ_ENABLE_WAYLAND=1 /usr/lib/firefox/firefox %u\nTerminal=false\nType=Application\nStartupWMClass=firefox\n\n[Desktop Action new-window]\nName=New Window\nExec=other --new-window %u\n";
        let e = parse_desktop(firefox).unwrap();
        assert_eq!((e.name.as_str(), e.exec.as_deref(), e.wm_class.as_deref(), e.terminal), ("Firefox Web Browser", Some("firefox"), Some("firefox"), false));
        let quoted = parse_desktop("[Desktop Entry]\nType=Application\nName=My App\nExec=\"/opt/My App/bin/my-app\" --flag\nTryExec=/opt/My App/bin/my-app\n").unwrap();
        assert_eq!((quoted.exec.as_deref(), quoted.try_exec.as_deref()), (Some("my-app"), Some("my-app")));
        let flatpak = parse_desktop("[Desktop Entry]\nType=Application\nName=GIMP\nExec=/usr/bin/flatpak run --branch=stable --command=gimp-2.10 org.gimp.GIMP @@u %U @@\n").unwrap();
        assert_eq!(flatpak.exec.as_deref(), Some("gimp-2.10"));
        let htop = parse_desktop("[Desktop Entry]\nType=Application\nName=Htop\nExec=htop\nTerminal=true\n").unwrap();
        assert!(htop.terminal);
        assert_eq!(parse_desktop("[Desktop Entry]\nType=Link\nName=Docs\nURL=https://x\n"), None);
        assert_eq!(parse_desktop("[Desktop Entry]\nType=Application\nName=Gone\nExec=gone\nHidden=true\n"), None);
        assert_eq!(parse_desktop("[Desktop Entry]\nType=Application\nExec=nameless\n"), None);

        let helper = DesktopEntry { name: "Firefox helper".into(), exec: Some("firefox".into()), no_display: true, ..Default::default() };
        let map = index_desktop([helper, e, htop, flatpak]);
        assert_eq!(map["firefox"], DesktopApp { name: "Firefox Web Browser".into(), windowed: true });
        assert!(!map["htop"].windowed && map["gimp-2.10"].windowed);
        assert_eq!(app_key(Some("/usr/lib/firefox/firefox"), "firefox-bin"), "firefox");
        assert_eq!(app_key(None, "Xorg"), "xorg");
    }

    #[test]
    fn window_heuristic_takes_the_app_root() {
        let procs = [
            (100, 1, Some(1000), Some("firefox")),
            (101, 100, Some(1000), Some("firefox")), // a content process of the same app
            (200, 1, Some(0), Some("firefox")),      // another user's
            (300, 1, Some(1000), None),              // not a desktop app
            (400, 300, Some(1000), Some("gedit")),   // launched from a terminal
        ];
        let mut got: Vec<u32> = heuristic_windows(&procs, 1000).into_iter().collect();
        got.sort();
        assert_eq!(got, [100, 400]);
    }

    #[test]
    fn pkexec_argument_vectors() {
        let mut t = Tools { pkexec: "/usr/bin/pkexec".into(), kill: "/usr/bin/kill".into(), renice: "/usr/bin/renice".into(), chrt: Some("/usr/bin/chrt".into()) };
        assert_eq!(elevated_argv(&t, Elevated::Signal("TERM"), &[42, 43]).unwrap(), ["/usr/bin/pkexec", "/usr/bin/kill", "-s", "TERM", "--", "42", "43"]);
        assert_eq!(elevated_argv(&t, Elevated::Renice(-5), &[7, 8]).unwrap(), ["/usr/bin/pkexec", "/usr/bin/renice", "-n", "-5", "-p", "7", "8"]);
        assert_eq!(elevated_argv(&t, Elevated::NormalPolicy, &[9]).unwrap(), ["/usr/bin/pkexec", "/usr/bin/chrt", "--other", "--all-tasks", "--pid", "0", "9"]);
        t.chrt = None;
        assert_eq!(elevated_argv(&t, Elevated::NormalPolicy, &[9]), None);
        assert!(pkexec_error(Some(126), "").contains("dismissed"));
        assert!(pkexec_error(Some(127), "Error executing command as another user: No authentication agent found.").contains("no polkit agent"));
        assert_eq!(pkexec_error(Some(1), "kill: (42): No such process"), "kill: (42): No such process");
        assert_eq!(pkexec_error(Some(1), ""), "the elevated command failed with exit code 1");
    }
}
