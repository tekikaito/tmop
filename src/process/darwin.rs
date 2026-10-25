//! The pure parts of the macOS process backend (`macos.rs`): unit conversions, the
//! `KERN_PROCARGS2` layout and the bundle name. Built on every target for the tests.

/// FILETIME of the Unix epoch (100 ns units since 1601).
const FILETIME_UNIX: i64 = 116_444_736_000_000_000;

/// A start time in seconds and microseconds since the Unix epoch as a creation FILETIME, the
/// unit of [`super::ProcessRow::create_time`].
pub(crate) fn filetime(secs: u64, usecs: u64) -> i64 {
    FILETIME_UNIX.saturating_add((secs as i64).saturating_mul(10_000_000)).saturating_add((usecs.min(999_999) * 10) as i64)
}

/// Mach absolute time units (CPU times of `proc_taskinfo` and `rusage_info`) in 100 ns, given
/// `mach_timebase_info` (1/1 on Intel, 125/3 on Apple Silicon).
pub(crate) fn mach_to_100ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    let ns = ticks as u128 * numer.max(1) as u128 / denom.max(1) as u128;
    (ns / 100).min(u64::MAX as u128) as u64
}

/// The argument vector in a `KERN_PROCARGS2` buffer: `argc` (a native `int`), the executable
/// path, NUL padding, then `argc` NUL-terminated arguments (the environment follows). None
/// when the buffer is cut short.
pub(crate) fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if !(0..=65_536).contains(&argc) {
        return None;
    }
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    if argc == 0 {
        return Some(Vec::new());
    }
    let mut at = exec_end + rest[exec_end..].iter().position(|&b| b != 0)?;
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        let len = rest[at..].iter().position(|&b| b == 0)?;
        args.push(String::from_utf8_lossy(&rest[at..at + len]).into_owned());
        at += len + 1;
    }
    Some(args)
}

/// `args` as one line, with arguments that are empty or hold blanks or quotes in double quotes.
pub(crate) fn join_args(args: &[String]) -> Option<String> {
    let quote = |a: &String| {
        if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || c == '"') {
            a.clone()
        } else {
            format!("\"{}\"", a.replace('"', "\\\""))
        }
    };
    (!args.is_empty()).then(|| args.iter().map(quote).collect::<Vec<_>>().join(" "))
}

/// The name of the outermost `.app` bundle holding `path` (`Google Chrome` for its helpers), the
/// macOS counterpart of a Windows file description.
pub(crate) fn bundle_name(path: &str) -> Option<String> {
    // Only a real bundle: the path continues into its Contents folder.
    let parts: Vec<&str> = path.split('/').collect();
    parts.windows(2).find_map(|w| w[0].strip_suffix(".app").filter(|n| !n.is_empty() && w[1] == "Contents")).map(str::to_string)
}

/// The fields TMOP reads from `struct extern_proc`, the head of a `struct kinfo_proc` (`sysctl`
/// `KERN_PROC_PID`), which libc does not declare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternProc {
    /// `p_starttime` (seconds, microseconds since the Unix epoch).
    pub start: (u64, u64),
    /// `p_flag` (`P_*`).
    pub flags: i32,
    /// `p_stat` (`SRUN`, `SSTOP`, `SZOMB` ...).
    pub stat: u8,
    /// `p_nice`.
    pub nice: i32,
}

/// `sizeof(struct kinfo_proc)` on 64-bit macOS.
pub(crate) const KINFO_PROC_SIZE: usize = 648;

/// Reads [`ExternProc`] from a `kinfo_proc` of `pid`. LP64 offsets: `p_starttime` (a timeval in
/// the leading union) 0, `p_flag` 32, `p_stat` 36, `p_pid` 40, `p_nice` 242. None unless the
/// buffer has the full size and names `pid`.
pub(crate) fn parse_extern_proc(buf: &[u8], pid: i32) -> Option<ExternProc> {
    if buf.len() != KINFO_PROC_SIZE {
        return None;
    }
    let i64_at = |o: usize| i64::from_ne_bytes(buf[o..o + 8].try_into().unwrap_or_default());
    let i32_at = |o: usize| i32::from_ne_bytes(buf[o..o + 4].try_into().unwrap_or_default());
    if i32_at(40) != pid {
        return None;
    }
    Some(ExternProc { start: (i64_at(0).max(0) as u64, i32_at(8).max(0) as u64), flags: i32_at(32), stat: buf[36], nice: buf[242] as i8 as i32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinfo_proc_fields() {
        let mut buf = vec![0u8; KINFO_PROC_SIZE];
        buf[0..8].copy_from_slice(&1_704_067_200i64.to_ne_bytes());
        buf[8..12].copy_from_slice(&250_000i32.to_ne_bytes());
        buf[32..36].copy_from_slice(&0x0002_0004i32.to_ne_bytes());
        buf[36] = 4;
        buf[40..44].copy_from_slice(&4242i32.to_ne_bytes());
        buf[242] = (-5i8) as u8;
        let p = parse_extern_proc(&buf, 4242).unwrap();
        assert_eq!(p, ExternProc { start: (1_704_067_200, 250_000), flags: 0x0002_0004, stat: 4, nice: -5 });
        assert_eq!(parse_extern_proc(&buf, 4243), None);
        assert_eq!(parse_extern_proc(&buf[..600], 4242), None);
    }

    #[test]
    fn start_times_and_cpu_times() {
        assert_eq!(filetime(0, 0), FILETIME_UNIX);
        // 2024-01-01T00:00:00.5Z
        assert_eq!(filetime(1_704_067_200, 500_000), 133_485_408_005_000_000);
        // Intel counts nanoseconds; Apple Silicon 24 MHz ticks (125/3 ns each).
        assert_eq!(mach_to_100ns(1_000_000_000, 1, 1), 10_000_000);
        assert_eq!(mach_to_100ns(24_000_000, 125, 3), 10_000_000);
        assert_eq!(mach_to_100ns(u64::MAX, 1000, 1), u64::MAX);
        assert_eq!(mach_to_100ns(500, 0, 0), 5);
    }

    fn procargs(argc: i32, body: &[u8]) -> Vec<u8> {
        let mut v = argc.to_ne_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn procargs2_layout() {
        let buf = procargs(3, b"/bin/sleep\0\0\0\0sleep\x0030\0two words\0HOME=/Users/a\0");
        let args = parse_procargs2(&buf).unwrap();
        assert_eq!(args, ["sleep", "30", "two words"]);
        assert_eq!(join_args(&args).unwrap(), "sleep 30 \"two words\"");
        assert_eq!(parse_procargs2(&procargs(0, b"/sbin/launchd\0\0")), Some(vec![]));
        assert_eq!(join_args(&[]), None);
        assert_eq!(join_args(&["a\"b".into(), String::new()]).unwrap(), "\"a\\\"b\" \"\"");
        // Cut short, a negative count, nothing at all.
        assert_eq!(parse_procargs2(&procargs(2, b"/bin/x\0\0x\0half")), None);
        assert_eq!(parse_procargs2(&procargs(-1, b"/bin/x\0\0x\0")), None);
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&procargs(1, b"/bin/x\0\0\0")), None);
    }

    #[test]
    fn bundle_names() {
        assert_eq!(bundle_name("/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder").as_deref(), Some("Finder"));
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)";
        assert_eq!(bundle_name(helper).as_deref(), Some("Google Chrome"));
        assert_eq!(bundle_name("/usr/bin/top"), None);
        assert_eq!(bundle_name("/Users/a/odd.app"), None);
        assert_eq!(bundle_name("/x/.app/Contents/MacOS/y"), None);
        assert_eq!(bundle_name("/Volumes/Tools.app/x/Bar.app/Contents/MacOS/bar").as_deref(), Some("Bar"));
    }
}
