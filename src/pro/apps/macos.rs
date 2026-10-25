//! macOS installed apps: `.app` bundles in `/Applications` (and its folders) and
//! `~/Applications`, plus Homebrew formulae from the Cellar.

use super::parse;
use super::App;
use crate::pro::inventory::unix_date;
use crate::pro::sysfmt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

fn mdate(path: &Path) -> String {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| unix_date(d.as_secs())).unwrap_or_default()
}

/// Allocated bytes under `root`, not following symlinks.
fn dir_size(root: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                total += m.blocks() * 512;
            }
        }
    }
    total
}

/// `.app` bundles in `dir` and (one level down) in its plain folders, like `Utilities`.
fn bundles(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "app") {
            out.push(p);
        } else if e.file_type().is_ok_and(|t| t.is_dir()) {
            if let Ok(sub) = std::fs::read_dir(&p) {
                out.extend(sub.flatten().map(|s| s.path()).filter(|s| s.extension().is_some_and(|x| x == "app")));
            }
        }
    }
    out
}

pub fn read() -> Result<Vec<App>, String> {
    let mut out: Vec<App> = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute());
    let mut roots = vec![(PathBuf::from("/Applications"), "Application")];
    if let Some(h) = &home {
        roots.push((h.join("Applications"), "User"));
    }
    for (root, scope) in roots {
        for b in bundles(&root) {
            let stem = b.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut a = match sysfmt::read_plist(&b.join("Contents/Info.plist")) {
                Some(p) => parse::app_bundle(&p, &stem),
                None => App { name: stem.clone(), key: stem, ..Default::default() },
            };
            a.scope = scope;
            a.install_date = mdate(&b);
            a.size = dir_size(&b);
            a.location = b.display().to_string();
            out.push(a);
        }
    }
    for cellar in ["/opt/homebrew/Cellar", "/usr/local/Cellar"] {
        let Ok(rd) = std::fs::read_dir(cellar) else { continue };
        for formula in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
            let name = formula.file_name().to_string_lossy().into_owned();
            // The newest installed version.
            let Some(ver) = std::fs::read_dir(formula.path()).ok().and_then(|v| v.flatten().filter(|e| e.path().is_dir()).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())) else { continue };
            let (tap, time) = std::fs::read_to_string(ver.path().join("INSTALL_RECEIPT.json")).map(|j| parse::brew_receipt(&j)).unwrap_or_default();
            out.push(App {
                name: name.clone(),
                publisher: tap,
                version: ver.file_name().to_string_lossy().into_owned(),
                install_date: time.map(unix_date).unwrap_or_else(|| mdate(&ver.path())),
                size: dir_size(&ver.path()),
                location: ver.path().display().to_string(),
                scope: "Homebrew",
                key: name,
            });
        }
    }
    if out.is_empty() {
        return Err("no applications found in /Applications, ~/Applications or the Homebrew Cellar".into());
    }
    super::finish(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_some_apps() {
        let apps = super::read().unwrap();
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|a| !a.name.is_empty() && !a.scope.is_empty()));
    }
}
