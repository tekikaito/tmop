//! The optional check for a newer TMOP release (off by default, see `general.checkForUpdates`),
//! and the in-place update on Windows.
//!
//! The check is a single GET of GitHub's "latest release" API. It sends nothing beyond what any
//! HTTP request carries (IP address, User-Agent `TMOP/<version>` on Windows, curl's elsewhere)
//! and runs at most once per [`CHECK_INTERVAL`] on start, or when asked from Settings. Windows
//! uses WinHTTP ([`crate::engine::http`]); Linux and macOS run `curl`, which has no TLS stack to
//! bundle and is what the install command already needs.
//!
//! On Windows, "Update now" downloads `TMOP-windows-<arch>.exe` of that release next to the
//! running exe, checks its `.sha256`, renames the running exe to `<name>.old` (Windows allows
//! renaming a running program, not overwriting it) and puts the new one in its place.
//! [`remove_leftover`] deletes the `.old` file on the next start. Linux and macOS show the
//! install command (or `brew upgrade tmop`) instead, because TMOP is installed in several ways
//! there (AppImage, tar.gz, install command, Homebrew cask).

use crate::engine::RELEASE_REPO;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// At most one start-up check per day.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// The largest API response accepted (a release with its notes and asset list is ~30 KB).
const MAX_RESPONSE: usize = 1024 * 1024;
/// The download page.
pub const DOWNLOAD_PAGE: &str = "https://tekikaito.github.io/tmop/";

/// This build's version, `X.Y.Z`.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn latest_api() -> String {
    format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest")
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// `vX.Y.Z`.
    pub tag: String,
    /// `X.Y.Z`.
    pub version: String,
    /// The release notes (Markdown).
    pub notes: String,
    /// The release page.
    pub page: String,
    /// (file name, download URL) of each attached file.
    pub assets: Vec<(String, String)>,
}

impl Release {
    pub fn asset_url(&self, name: &str) -> Option<&str> {
        self.assets.iter().find(|(n, _)| n == name).map(|(_, u)| u.as_str())
    }
}

/// `X.Y.Z` or `vX.Y.Z` (a `-suffix` is ignored) as numbers.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let s = s.split(['-', '+']).next()?;
    let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// True when `candidate` is a higher version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// True when a start-up check is due: never checked, a day has passed, or the clock went back.
pub fn due(last_check: u64, now: u64) -> bool {
    last_check == 0 || now < last_check || now - last_check >= CHECK_INTERVAL_SECS
}

/// Seconds since 1970.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads GitHub's release JSON. Drafts and pre-releases are refused.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return Err("the newest release is a pre-release".into());
    }
    let tag = v["tag_name"].as_str().ok_or("unexpected answer from GitHub: no tag_name")?.to_string();
    let (a, b, c) = parse_version(&tag).ok_or_else(|| format!("unexpected release tag {tag}"))?;
    let assets = v["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version: format!("{a}.{b}.{c}"),
        tag,
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        page: v["html_url"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://github.com/{RELEASE_REPO}/releases/latest")),
        assets,
    })
}

/// Asks GitHub for the newest release. `Ok(None)` when this build is up to date.
pub fn check() -> Result<Option<Release>, String> {
    let release = parse_release(&http_get(&latest_api())?)?;
    Ok(is_newer(&release.version, current_version()).then_some(release))
}

#[cfg(windows)]
fn http_get(url: &str) -> Result<String, String> {
    crate::engine::http::fetch_text(url, MAX_RESPONSE)
}

#[cfg(not(windows))]
fn http_get(url: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let agent = format!("TMOP/{}", current_version());
    let out = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--max-time", "30", "-A", &agent, "-H", "Accept: application/vnd.github+json", url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "curl is not installed".to_string() } else { format!("could not run curl: {e}") })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("curl failed ({})", out.status) } else { err });
    }
    if out.stdout.len() > MAX_RESPONSE {
        return Err(format!("response from {url} is larger than {MAX_RESPONSE} bytes"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The release notes as plain text for the dialog: Markdown marks removed, links reduced to
/// their text, and everything from the "Downloads" heading on dropped (file list and install
/// commands, which the dialog shows its own way).
pub fn plain_notes(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in markdown.lines() {
        let t = line.trim_end();
        let heading = t.trim_start_matches('#');
        if heading.len() != t.len() && heading.trim().eq_ignore_ascii_case("downloads") {
            break;
        }
        let mut s = if heading.len() != t.len() { heading.trim().to_string() } else { t.to_string() };
        if let Some(rest) = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")) {
            s = format!("\u{2022} {rest}");
        }
        out.push(strip_inline(&s));
    }
    let text = out.join("\n");
    // At most one empty line in a row, none at the ends.
    let mut tidy = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !tidy.is_empty() {
            tidy.push_str("\n\n");
        }
        tidy.push_str(para);
    }
    tidy
}

/// Removes `**`, `*`, `_`-free emphasis marks and backticks, and turns `[text](url)` into `text`.
fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find("](").and_then(|j| after[j + 2..].find(')').map(|k| (j, j + 2 + k))) {
            Some((j, end)) if !after[..j].contains('[') => {
                out.push_str(&after[..j]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // A leading bullet "• " stays; the remaining asterisks and backticks are emphasis/code marks.
    // Emoji (outside the Basic Multilingual Plane) aren't in TMOP's fonts.
    out.chars().filter(|c| *c != '*' && *c != '`' && (*c as u32) < 0x10000).collect::<String>().trim().to_string()
}

/// How this copy of TMOP can be updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Windows: download and swap the exe in place.
    InPlace,
    /// Linux and macOS: run the install command again (or download by hand).
    InstallCommand,
    /// macOS: installed with the Homebrew cask.
    Homebrew,
}

impl Method {
    /// The command that updates TMOP; none for [`Method::InPlace`].
    pub fn command(self) -> Option<&'static str> {
        match self {
            Method::InPlace => None,
            Method::InstallCommand => Some("curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh"),
            Method::Homebrew => Some("brew upgrade --cask tmop"),
        }
    }

    /// What to do with [`Method::command`], for the dialog.
    pub fn instructions(self) -> &'static str {
        match self {
            Method::InPlace => "",
            Method::InstallCommand => "Installed with the install command? Run it again in a terminal to update. Otherwise download the new version from the release page.",
            Method::Homebrew => "TMOP was installed with Homebrew. To update, run this in a terminal:",
        }
    }
}

/// How to update the running copy.
pub fn method() -> Method {
    if cfg!(windows) {
        Method::InPlace
    } else if ["/opt/homebrew/Caskroom/tmop", "/usr/local/Caskroom/tmop"].iter().any(|d| Path::new(d).is_dir()) {
        Method::Homebrew
    } else {
        Method::InstallCommand
    }
}

/// The Windows download for this build's processor.
pub fn windows_asset() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "TMOP-windows-arm64.exe"
    } else {
        "TMOP-windows-x64.exe"
    }
}

/// `<exe>.old`: where the replaced exe goes.
fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// `<exe>.new`: the download before it is moved into place.
fn new_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".new");
    PathBuf::from(s)
}

/// Deletes the exe an earlier update replaced, and a download it left behind, in the background:
/// the old copy that started this one is still exiting, so its file is retried for a few seconds.
pub fn remove_leftover() {
    let Ok(exe) = std::env::current_exe() else { return };
    let files: Vec<PathBuf> = [old_path(&exe), new_path(&exe)].into_iter().filter(|p| p.exists()).collect();
    if files.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("tmop-update-cleanup".into()).spawn(move || {
        for _ in 0..20 {
            if files.iter().all(|p| !p.exists() || std::fs::remove_file(p).is_ok()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

/// Puts `new` in place of `exe`, keeping the old one as `<exe>.old`. On failure `exe` is as before.
pub fn swap_in(exe: &Path, new: &Path) -> Result<(), String> {
    let old = old_path(exe);
    if old.exists() {
        std::fs::remove_file(&old).map_err(|e| format!("cannot remove {}: {e}", old.display()))?;
    }
    std::fs::rename(exe, &old).map_err(|e| format!("cannot rename {}: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("cannot put the new version in place: {e}"));
    }
    Ok(())
}

/// Downloads `release`'s exe for this processor, checks it and swaps it in. Returns the exe to start.
/// `progress(done, total)` follows the download.
#[cfg(windows)]
pub fn install(release: &Release, progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find TMOP.exe: {e}"))?;
    install_at(&exe, release, progress)?;
    set_installed_version(exe.parent().unwrap_or(Path::new("")), &release.version);
    Ok(exe)
}

/// [`install`] for the exe at `exe`.
#[cfg(windows)]
fn install_at(exe: &Path, release: &Release, progress: impl FnMut(u64, u64)) -> Result<(), String> {
    use crate::engine::{asset, http};
    use std::sync::atomic::AtomicBool;
    let name = windows_asset();
    let url = release.asset_url(name).ok_or_else(|| format!("release {} has no {name}", release.tag))?;
    let sha_url = release.asset_url(&format!("{name}.sha256")).ok_or_else(|| format!("release {} has no checksum for {name}", release.tag))?;
    let expected = http::fetch_text(sha_url, 4096).and_then(|t| asset::parse_sha256_file(&t).ok_or_else(|| "the checksum file is malformed".to_string()))?;
    let new = new_path(exe);
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    // A folder TMOP may not write to (Program Files without administrator rights) fails here,
    // before anything is downloaded.
    std::fs::File::create(&new).map_err(|e| format!("TMOP cannot write to {} ({e}). Download the new version from the release page instead.", dir.display()))?;
    http::download_to_file(url, &new, &AtomicBool::new(false), progress).inspect_err(|_| {
        let _ = std::fs::remove_file(&new);
    })?;
    if let Err(e) = asset::verify_file(&new, &expected, |_| {}) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("the download is damaged, nothing was changed ({e})"));
    }
    swap_in(exe, &new).inspect_err(|_| {
        let _ = std::fs::remove_file(&new);
    })
}

/// Updates the version that Settings → Apps shows for a copy installed by `install.ps1`.
#[cfg(windows)]
fn set_installed_version(dir: &Path, version: &str) {
    use crate::pro::winutil::{Key, View};
    use windows::Win32::System::Registry::HKEY_CURRENT_USER;
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\TMOP";
    let ours = Key::open(HKEY_CURRENT_USER, KEY, View::Default)
        .and_then(|k| k.string("InstallLocation"))
        .is_some_and(|l| Path::new(l.trim_end_matches('\\')).eq(dir));
    if ours {
        let mut cmd = std::process::Command::new("reg.exe");
        cmd.args(["add", &format!(r"HKCU\{KEY}"), "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version, "/f"]);
        crate::platform::hide_console(&mut cmd);
        let _ = cmd.status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "tag_name": "v0.8.0", "draft": false, "prerelease": false,
        "html_url": "https://github.com/tekikaito/tmop/releases/tag/v0.8.0",
        "body": "TMOP 0.8.0 adds things.\r\n\r\n## 🐞 New\r\n- **Update check** in *Settings*, see [the README](https://x.y/z).\r\n\r\n## Downloads\r\n| File | For |",
        "assets": [
            {"name": "TMOP-windows-x64.exe", "browser_download_url": "https://example/x64.exe"},
            {"name": "TMOP-windows-x64.exe.sha256", "browser_download_url": "https://example/x64.exe.sha256"}
        ]
    }"#;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.7.3"), Some((0, 7, 3)));
        assert_eq!(parse_version("1.10.0-beta.1"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("0.7.4", "0.7.3"));
        assert!(!is_newer("v0.7.3", "0.7.3"));
        assert!(!is_newer("v0.7.2", "0.7.3"));
        assert!(!is_newer("garbage", "0.7.3"));
    }

    #[test]
    fn checks_at_most_daily() {
        assert!(due(0, 1_000));
        assert!(!due(1_000, 1_000 + CHECK_INTERVAL_SECS - 1));
        assert!(due(1_000, 1_000 + CHECK_INTERVAL_SECS));
        assert!(due(5_000, 1_000), "a clock set back checks again");
    }

    #[test]
    fn reads_a_release() {
        let r = parse_release(SAMPLE).unwrap();
        assert_eq!((r.tag.as_str(), r.version.as_str()), ("v0.8.0", "0.8.0"));
        assert_eq!(r.asset_url("TMOP-windows-x64.exe"), Some("https://example/x64.exe"));
        assert_eq!(r.asset_url("TMOP-windows-arm64.exe"), None);
        assert!(parse_release(&SAMPLE.replace(r#""prerelease": false"#, r#""prerelease": true"#)).is_err());
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn notes_become_plain_text() {
        let r = parse_release(SAMPLE).unwrap();
        assert_eq!(plain_notes(&r.notes), "TMOP 0.8.0 adds things.\n\nNew\n\u{2022} Update check in Settings, see the README.");
        assert_eq!(strip_inline("a [b] c [d](e) `f`"), "a [b] c d f");
    }

    #[test]
    fn update_commands() {
        assert_eq!(Method::InPlace.command(), None);
        assert_eq!(Method::Homebrew.command(), Some("brew upgrade --cask tmop"));
        assert!(Method::InstallCommand.command().unwrap().ends_with("install.sh | sh"));
        assert_eq!(method() == Method::InPlace, cfg!(windows));
    }

    #[test]
    fn swap_keeps_the_old_exe_aside() {
        let dir = std::env::temp_dir().join(format!("tmop-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("TMOP.exe");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(old_path(&exe), b"older").unwrap();
        let new = new_path(&exe);
        std::fs::write(&new, b"new").unwrap();
        swap_in(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!new.exists());
        // A missing download leaves the exe as it was.
        assert!(swap_in(&exe, &new).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asks GitHub for the newest release and installs its exe over a stand-in file.
    /// Needs the network: `cargo test --lib update::tests::live -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_check_and_install() {
        let release = parse_release(&http_get(&latest_api()).unwrap()).unwrap();
        assert!(!is_newer(current_version(), &release.version), "this build is not newer than the latest release");
        let dir = std::env::temp_dir().join(format!("tmop-update-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("TMOP.exe");
        std::fs::write(&exe, b"stand-in").unwrap();
        let mut last = (0, 0);
        install_at(&exe, &release, |d, t| last = (d, t)).unwrap();
        assert!(last.0 > 1_000_000 && last.0 == last.1, "progress reached the full size: {last:?}");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"stand-in");
        assert!(std::fs::metadata(&exe).unwrap().len() == last.0 && !new_path(&exe).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
