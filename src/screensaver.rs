//! Windows screensaver command-line conventions.
//!
//! Handles the documented Windows conventions (`/s`, `/p <hwnd>`, `/c`) when built with
//! the `screensaver` feature (see `main.rs`). `--manual` marks a start from the app.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrMode {
    /// `/s`: run full-screen. `manual` = started by the app (F12), not by Windows.
    Run { manual: bool },
    /// `/p <hwnd>`: render into the Control Panel preview window.
    Preview(isize),
    /// `/c[:hwnd]` or no arguments: configuration dialog.
    Configure,
}

/// Parses screensaver arguments (excluding argv[0]). Returns `None` when the
/// arguments are not screensaver arguments at all.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Option<ScrMode> {
    let args: Vec<String> = args.into_iter().collect();
    let manual = args.iter().any(|a| a.eq_ignore_ascii_case("--manual"));
    let mut it = args.iter().filter(|a| !a.eq_ignore_ascii_case("--manual"));
    let first = match it.next() {
        Some(a) => a.clone(),
        None => return Some(ScrMode::Configure),
    };
    let lower = first.to_ascii_lowercase();
    let body = lower.strip_prefix('/').or_else(|| lower.strip_prefix('-'))?;
    let (flag, inline) = match body.split_once(':') {
        Some((f, v)) => (f.to_string(), Some(v.to_string())),
        None => (body.to_string(), None),
    };
    match flag.as_str() {
        "s" => Some(ScrMode::Run { manual }),
        "c" => Some(ScrMode::Configure),
        "p" | "l" => {
            let raw = inline.or_else(|| it.next().cloned())?;
            raw.trim().parse::<isize>().ok().map(ScrMode::Preview)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Option<ScrMode> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn conventions() {
        assert_eq!(p(&[]), Some(ScrMode::Configure));
        assert_eq!(p(&["/s"]), Some(ScrMode::Run { manual: false }));
        assert_eq!(p(&["/S", "--manual"]), Some(ScrMode::Run { manual: true }));
        assert_eq!(p(&["/p", "1234"]), Some(ScrMode::Preview(1234)));
        assert_eq!(p(&["/p:42"]), Some(ScrMode::Preview(42)));
        assert_eq!(p(&["/c:99"]), Some(ScrMode::Configure));
        assert_eq!(p(&["-c"]), Some(ScrMode::Configure));
        assert_eq!(p(&["/p"]), None);
        assert_eq!(p(&["--popout"]), None);
    }
}
