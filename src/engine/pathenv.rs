//! Windows user PATH list helpers and command strings for autostart and the firewall rule.
//!
//! Pure string functions; the registry write lives in `winreg.rs`.

use std::path::Path;

/// Comparison form: trimmed, no trailing separator, lowercase.
fn norm(entry: &str) -> String {
    entry.trim().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase()
}

/// Non-empty `;`-separated items, verbatim.
fn items(path_value: &str) -> impl Iterator<Item = &str> {
    path_value.split(';').filter(|s| !s.trim().is_empty())
}

/// True when an equal entry (case-insensitive, trailing-separator-insensitive) is in the list.
pub fn contains_path_entry(path_value: &str, entry: &str) -> bool {
    let want = norm(entry);
    !want.is_empty() && items(path_value).any(|item| norm(item) == want)
}

/// Prepends `entry` unless an equal entry exists. Returns the new value and whether it changed.
pub fn add_path_entry(path_value: &str, entry: &str) -> (String, bool) {
    let entry = entry.trim();
    if entry.is_empty() || contains_path_entry(path_value, entry) {
        return (path_value.to_string(), false);
    }
    let mut out = entry.to_string();
    for item in items(path_value) {
        out.push(';');
        out.push_str(item);
    }
    (out, true)
}

/// Removes every equal entry. Returns the new value and whether it changed.
pub fn remove_path_entry(path_value: &str, entry: &str) -> (String, bool) {
    let want = norm(entry);
    if want.is_empty() {
        return (path_value.to_string(), false);
    }
    let all: Vec<&str> = items(path_value).collect();
    let kept: Vec<&str> = all.iter().copied().filter(|item| norm(item) != want).collect();
    if kept.len() == all.len() {
        return (path_value.to_string(), false);
    }
    (kept.join(";"), true)
}

/// Value for `HKCU\...\Run`: the quoted exe with the relay switch.
pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --engine-relay", exe.display())
}

/// `netsh` arguments for the inbound firewall rule. Joined with spaces they form the command line.
pub fn firewall_rule_args(exe: &Path) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        "name=\"TMOP engine ports\"".into(),
        "dir=in".into(),
        "action=allow".into(),
        format!("program=\"{}\"", exe.display()),
        "enable=yes".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_ignores_case_and_trailing_separator() {
        let p = r"C:\Windows;C:\Tools\Bin\;D:\x";
        assert!(contains_path_entry(p, r"c:\tools\bin"));
        assert!(contains_path_entry(p, r"C:\TOOLS\BIN\"));
        assert!(contains_path_entry(p, "D:/x/"));
        assert!(!contains_path_entry(p, r"C:\Tools"));
        assert!(!contains_path_entry(p, ""));
        assert!(!contains_path_entry("", r"C:\x"));
    }

    #[test]
    fn add_prepends_and_keeps_others() {
        let (out, changed) = add_path_entry(r"C:\Windows;D:\x", r"C:\TMOP\bin");
        assert!(changed);
        assert_eq!(out, r"C:\TMOP\bin;C:\Windows;D:\x");
    }

    #[test]
    fn add_is_idempotent() {
        let (once, c1) = add_path_entry(r"C:\Windows", r"C:\TMOP\bin");
        let (twice, c2) = add_path_entry(&once, r"c:\tmop\bin\");
        assert!(c1);
        assert!(!c2);
        assert_eq!(once, twice);
    }

    #[test]
    fn add_to_empty_path() {
        assert_eq!(add_path_entry("", r"C:\TMOP\bin"), (r"C:\TMOP\bin".to_string(), true));
        assert_eq!(add_path_entry("", ""), (String::new(), false));
    }

    #[test]
    fn add_does_not_introduce_empty_items() {
        let (out, changed) = add_path_entry(r"C:\A;;C:\B;", r"C:\new");
        assert!(changed);
        assert_eq!(out, r"C:\new;C:\A;C:\B");
    }

    #[test]
    fn remove_drops_all_equal_entries() {
        let p = r"C:\TMOP\bin;C:\Windows;c:\tmop\bin\;D:\x";
        let (out, changed) = remove_path_entry(p, r"C:\TMOP\bin");
        assert!(changed);
        assert_eq!(out, r"C:\Windows;D:\x");
        assert!(!contains_path_entry(&out, r"C:\TMOP\bin"));
    }

    #[test]
    fn remove_absent_or_empty_is_noop() {
        assert_eq!(remove_path_entry(r"C:\A", r"C:\B"), (r"C:\A".to_string(), false));
        assert_eq!(remove_path_entry("", r"C:\B"), (String::new(), false));
        assert_eq!(remove_path_entry(r"C:\A", ""), (r"C:\A".to_string(), false));
    }

    #[test]
    fn remove_then_add_round_trips() {
        let base = r"C:\Windows;D:\x";
        let (added, _) = add_path_entry(base, r"C:\TMOP\bin");
        let (removed, changed) = remove_path_entry(&added, r"C:\TMOP\bin");
        assert!(changed);
        assert_eq!(removed, base);
    }

    #[test]
    fn autostart_command_is_quoted() {
        assert_eq!(
            autostart_command(Path::new(r"C:\Program Files\TMOP\TMOP.exe")),
            r#""C:\Program Files\TMOP\TMOP.exe" --engine-relay"#
        );
    }

    #[test]
    fn firewall_args_form_the_documented_command() {
        let args = firewall_rule_args(Path::new(r"C:\TMOP\TMOP.exe"));
        assert_eq!(
            args.join(" "),
            r#"advfirewall firewall add rule name="TMOP engine ports" dir=in action=allow program="C:\TMOP\TMOP.exe" enable=yes"#
        );
    }
}
