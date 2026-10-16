//! `.wslconfig` merge: sets or removes `memory` and `processors` in `[wsl2]`, keeping everything else.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const SECTION: &str = "wsl2";

/// Merges the `[wsl2]` memory/processors settings into `existing`.
///
/// `None` removes the key. Line endings follow the input (CRLF when undetectable). A leading UTF-8
/// BOM is dropped, so the first section header is still recognised and no BOM is written back.
pub fn merge(existing: &str, memory_gb: Option<u32>, processors: Option<u32>) -> String {
    let existing = existing.strip_prefix('\u{feff}').unwrap_or(existing);
    let nl = if existing.contains('\n') && !existing.contains("\r\n") { "\n" } else { "\r\n" };
    let wanted: [(&str, Option<String>); 2] = [
        ("memory", memory_gb.map(|gb| format!("{gb}GB"))),
        ("processors", processors.map(|n| n.to_string())),
    ];
    let mut seen = [false; 2];
    let mut out: Vec<String> = Vec::new();
    let mut in_wsl2 = false;
    // Index after the last content line of the [wsl2] section; new keys go there.
    let mut anchor: Option<usize> = None;

    for line in existing.lines() {
        let t = line.trim();
        if let Some(name) = section_name(t) {
            in_wsl2 = name.eq_ignore_ascii_case(SECTION);
            out.push(line.to_string());
            if in_wsl2 {
                anchor = Some(out.len());
            }
            continue;
        }
        if in_wsl2 {
            if let Some(idx) = wanted.iter().position(|(key, _)| key_matches(t, key)) {
                if !seen[idx] {
                    seen[idx] = true;
                    if let Some(value) = &wanted[idx].1 {
                        out.push(format!("{}={value}", wanted[idx].0));
                        anchor = Some(out.len());
                    }
                }
                continue;
            }
        }
        out.push(line.to_string());
        if in_wsl2 && !t.is_empty() {
            anchor = Some(out.len());
        }
    }

    let missing: Vec<String> = wanted
        .iter()
        .enumerate()
        .filter_map(|(i, (key, value))| match value {
            Some(v) if !seen[i] => Some(format!("{key}={v}")),
            _ => None,
        })
        .collect();
    if !missing.is_empty() {
        match anchor {
            Some(at) => {
                let tail = out.split_off(at);
                out.extend(missing);
                out.extend(tail);
            }
            None => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push(format!("[{SECTION}]"));
                out.extend(missing);
            }
        }
    }

    if out.is_empty() {
        return String::new();
    }
    let mut text = out.join(nl);
    text.push_str(nl);
    text
}

fn section_name(trimmed: &str) -> Option<&str> {
    trimmed.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn key_matches(trimmed: &str, key: &str) -> bool {
    if trimmed.starts_with('#') || trimmed.starts_with(';') {
        return false;
    }
    trimmed.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
}

/// `<path>.tmop-backup-<now>`
pub fn backup_name(path: &Path, now_unix: u64) -> PathBuf {
    suffixed(path, &format!(".tmop-backup-{now_unix}"))
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Reads `path` (if present), backs it up, and writes the merged content.
///
/// Returns the backup path when the file existed.
pub fn apply(
    path: &Path,
    memory_gb: Option<u32>,
    processors: Option<u32>,
    now_unix: u64,
) -> Result<Option<PathBuf>, String> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let backup = match &existing {
        Some(_) => {
            let dst = backup_name(path, now_unix);
            fs::copy(path, &dst).map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
            Some(dst)
        }
        None => None,
    };
    let merged = merge(existing.as_deref().unwrap_or(""), memory_gb, processors);
    let tmp = suffixed(path, ".tmop-tmp");
    fs::write(&tmp, merged).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmop-wslcfg-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_file_gets_section() {
        assert_eq!(merge("", Some(8), Some(4)), "[wsl2]\r\nmemory=8GB\r\nprocessors=4\r\n");
        assert_eq!(merge("", None, None), "");
    }

    #[test]
    fn file_without_wsl2_gets_section_at_end() {
        let out = merge("[experimental]\nautoMemoryReclaim=gradual\n", Some(4), None);
        assert_eq!(out, "[experimental]\nautoMemoryReclaim=gradual\n\n[wsl2]\nmemory=4GB\n");
    }

    #[test]
    fn other_sections_and_comments_are_kept() {
        let input = "# top comment\n[wsl2]\nswap=2GB\n; keep me\n\n[user]\nnetworkingMode=mirrored\n";
        let out = merge(input, Some(6), Some(2));
        assert_eq!(
            out,
            "# top comment\n[wsl2]\nswap=2GB\n; keep me\nmemory=6GB\nprocessors=2\n\n[user]\nnetworkingMode=mirrored\n"
        );
    }

    #[test]
    fn existing_keys_are_replaced_and_not_duplicated() {
        let input = "[wsl2]\nMemory = 2GB\n  processors= 1\nmemory=3GB\nprocessors=9\n";
        let out = merge(input, Some(8), Some(4));
        assert_eq!(out, "[wsl2]\nmemory=8GB\nprocessors=4\n");
    }

    #[test]
    fn none_removes_keys() {
        let input = "[wsl2]\nmemory=2GB\nprocessors=1\nswap=1GB\n";
        assert_eq!(merge(input, None, None), "[wsl2]\nswap=1GB\n");
        assert_eq!(merge(input, Some(5), None), "[wsl2]\nmemory=5GB\nswap=1GB\n");
    }

    #[test]
    fn crlf_is_preserved() {
        let input = "[user]\r\nname=a\r\n\r\n[wsl2]\r\nprocessors=1\r\n";
        let out = merge(input, Some(3), Some(2));
        assert_eq!(out, "[user]\r\nname=a\r\n\r\n[wsl2]\r\nprocessors=2\r\nmemory=3GB\r\n");
        assert!(!out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn leading_bom_is_ignored_and_not_written_back() {
        let out = merge("\u{feff}[wsl2]\r\nmemory=1GB\r\n", Some(2), None);
        assert_eq!(out, "[wsl2]\r\nmemory=2GB\r\n");
        assert_eq!(merge("\u{feff}[experimental]\n", Some(4), None), "[experimental]\n\n[wsl2]\nmemory=4GB\n");
    }

    #[test]
    fn lf_is_preserved() {
        assert_eq!(merge("[wsl2]\nmemory=1GB\n", Some(2), None), "[wsl2]\nmemory=2GB\n");
    }

    #[test]
    fn backup_name_appends_suffix() {
        assert_eq!(
            backup_name(Path::new(r"C:\Users\me\.wslconfig"), 1700000000),
            PathBuf::from(r"C:\Users\me\.wslconfig.tmop-backup-1700000000")
        );
    }

    #[test]
    fn apply_creates_backup_and_merged_file() {
        let dir = temp_dir("apply");
        let path = dir.join(".wslconfig");
        fs::write(&path, "[wsl2]\r\nmemory=2GB\r\n").unwrap();
        let backup = apply(&path, Some(8), Some(4), 42).unwrap().expect("backup");
        assert_eq!(backup, backup_name(&path, 42));
        assert_eq!(fs::read_to_string(&backup).unwrap(), "[wsl2]\r\nmemory=2GB\r\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "[wsl2]\r\nmemory=8GB\r\nprocessors=4\r\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_without_existing_file_writes_only() {
        let dir = temp_dir("fresh");
        let path = dir.join(".wslconfig");
        assert_eq!(apply(&path, Some(4), None, 1).unwrap(), None);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[wsl2]\r\nmemory=4GB\r\n");
        assert!(!backup_name(&path, 1).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
