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
//! there (AppImage, tar.gz, install command, Homebrew cask). A Windows copy installed with Scoop
//! ([`is_scoop_exe`]) shows `scoop update tmop` instead, so Scoop keeps owning its version folders.
//!
//! Where updates come from: the GitHub repository this build was released from
//! ([`RELEASE_REPO`], set by a fork's release workflow), or the one named in
//! `general.updateRepo`, so a fork's users can follow it (see [`repo`]).

pub use crate::engine::RELEASE_REPO;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// At most one start-up check per day.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// The largest API response accepted (a release with its notes and asset list is ~30 KB).
const MAX_RESPONSE: usize = 1024 * 1024;

/// True for a GitHub repository name `owner/name` (letters, digits, `-`, `_`, `.`).
pub fn valid_repo(s: &str) -> bool {
    let ok = |p: &str| !p.is_empty() && p != "." && p != ".." && p.len() <= 100 && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    matches!(s.split_once('/'), Some((owner, name)) if ok(owner) && ok(name) && !name.contains('/'))
}

/// The repository updates come from: `setting` (`general.updateRepo`) when it names one,
/// else the one this build was released from.
pub fn repo(setting: &str) -> &str {
    let s = setting.trim();
    if valid_repo(s) {
        s
    } else {
        RELEASE_REPO
    }
}

/// The repository's GitHub Pages site (`https://owner.github.io/name/`), where the download
/// page and the install command live.
pub fn pages_url(repo: &str) -> String {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    format!("https://{}.github.io/{name}/", owner.to_ascii_lowercase())
}

/// This build's version, `X.Y.Z`.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn latest_api(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
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
    /// The repository (`owner/name`) it was published in.
    pub repo: String,
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

/// Reads GitHub's release JSON of `repo`. Drafts and pre-releases are refused.
pub fn parse_release(json: &str, repo: &str) -> Result<Release, String> {
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
        page: v["html_url"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://github.com/{repo}/releases/latest")),
        assets,
        repo: repo.to_string(),
    })
}

/// Asks GitHub for the newest release of `repo`. `Ok(None)` when this build is up to date.
pub fn check(repo: &str) -> Result<Option<Release>, String> {
    let release = parse_release(&http_get(&latest_api(repo))?, repo)?;
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
    /// Windows: installed with Scoop, which owns the version folders.
    Scoop,
    /// Linux: run from the Nix store; `nix` owns the files, so nothing is downloaded here.
    Nix,
    /// Linux: the `.deb` or `.rpm` package (`/usr/bin/tmop`); nothing is downloaded here.
    SystemPackage,
}

impl Method {
    /// The command that updates TMOP from `repo`; none for [`Method::InPlace`].
    pub fn command(self, repo: &str) -> Option<String> {
        match self {
            Method::InPlace | Method::Nix | Method::SystemPackage => None,
            Method::InstallCommand => Some(format!("curl -fsSL {}install.sh | sh", pages_url(repo))),
            Method::Homebrew => Some("brew upgrade --cask tmop".into()),
            Method::Scoop => Some("scoop update tmop".into()),
        }
    }

    /// What to do with [`Method::command`], for the dialog.
    pub fn instructions(self, repo: &str) -> String {
        match self {
            Method::InPlace => String::new(),
            Method::InstallCommand => "Installed with the install command? Run it again in a terminal to update. Otherwise download the new version from the release page.".into(),
            Method::Homebrew => "TMOP was installed with Homebrew. To update, run this in a terminal:".into(),
            Method::Scoop => "TMOP was installed with Scoop. To update, run this in a terminal:".into(),
            Method::Nix => format!("TMOP runs from the Nix store. To update, run `nix flake update tmop` if it is an input of your flake, or `nix run github:{repo} --refresh`. TMOP does not replace its own files here."),
            Method::SystemPackage => "TMOP was installed from a .deb or .rpm package. To update, download the new .deb or .rpm from the release page and install it with your package manager. TMOP does not replace its own files here.".into(),
        }
    }

    /// True when the dialog only shows [`Method::instructions`] and has nothing to copy or run.
    pub fn hint_only(self) -> bool {
        matches!(self, Method::Nix | Method::SystemPackage)
    }
}

/// The repository the Scoop bucket and the Homebrew cask (`tekikaito/tmop`) install from.
const SCOOP_REPO: &str = "tekikaito/tmop";

/// The repository the Homebrew cask (`tekikaito/tmop` tap) installs from.
const HOMEBREW_REPO: &str = "tekikaito/tmop";

/// Splits a Windows or POSIX path into its components, without a `\\?\` prefix or empty parts.
fn path_parts(p: &str) -> Vec<&str> {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.split(['\\', '/']).filter(|s| !s.is_empty()).collect()
}

/// True when `exe` is `<root>\apps\tmop\<version or current>\<file>` of a Scoop root: `root`
/// is one of `env_roots` (`SCOOP`, `SCOOP_GLOBAL`), or `is_dir` finds both `<root>\shims` and
/// `<root>\apps`. Case-insensitive, either path separator, `\\?\` prefix ignored. A folder
/// that is merely called `apps\tmop` is not a Scoop install.
pub fn is_scoop_exe(exe: &str, env_roots: &[String], is_dir: impl Fn(&str) -> bool) -> bool {
    let parts = path_parts(exe);
    let n = parts.len();
    if n < 5 || !parts[n - 3].eq_ignore_ascii_case("tmop") || !parts[n - 4].eq_ignore_ascii_case("apps") {
        return false;
    }
    let root = &parts[..n - 4];
    let same = |a: &[&str], b: &[&str]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y));
    if env_roots.iter().any(|r| same(&path_parts(r), root)) {
        return true;
    }
    let joined = root.join("\\");
    // `\\?\UNC\server\share` (a canonicalized network path) is `\\server\share`.
    let base = if exe.to_ascii_uppercase().starts_with(r"\\?\UNC\") { format!(r"\\{}", joined.get(4..).unwrap_or("")) } else { joined };
    is_dir(&format!(r"{base}\shims")) && is_dir(&format!(r"{base}\apps"))
}

/// True when the running exe lives in a Scoop app folder (checked once). The path is tried as
/// given and with junctions resolved (`apps\tmop\current` points at the version folder).
pub fn is_scoop_install() -> bool {
    static SCOOP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SCOOP.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else { return false };
        let roots: Vec<String> = ["SCOOP", "SCOOP_GLOBAL"].iter().filter_map(|k| std::env::var(k).ok()).filter(|v| !v.trim().is_empty()).collect();
        let is_dir = |p: &str| Path::new(p).is_dir();
        [Some(exe.clone()), std::fs::canonicalize(&exe).ok()].into_iter().flatten().any(|p| is_scoop_exe(&p.to_string_lossy(), &roots, is_dir))
    })
}

/// The update method of a Linux or macOS install, from facts the caller gathered: `exe` is the
/// executable's canonical path (symlinks resolved by the caller, see [`method`]), `home` is
/// `$HOME`, `appimage` is whether `$APPIMAGE` is set, `cask` whether a Homebrew Caskroom folder
/// of TMOP exists. Pure: no file system or environment access.
pub fn unix_method(repo: &str, exe: &str, home: Option<&str>, appimage: bool, cask: bool) -> Method {
    if repo.eq_ignore_ascii_case(HOMEBREW_REPO) && cask {
        return Method::Homebrew;
    }
    if appimage {
        return Method::InstallCommand;
    }
    if exe.starts_with("/nix/store/") {
        return Method::Nix;
    }
    let in_home = home.map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty()).is_some_and(|h| exe.strip_prefix(h).is_some_and(|r| r.starts_with('/')));
    // The `.deb` and `.rpm` (packaging/linux/nfpm.yaml) install exactly `/usr/bin/tmop`.
    if exe == "/usr/bin/tmop" && !in_home {
        return Method::SystemPackage;
    }
    Method::InstallCommand
}

/// How to update the running copy from `repo`. The Homebrew cask only follows its own repository.
pub fn method(repo: &str) -> Method {
    if cfg!(windows) {
        if is_scoop_install() && repo.eq_ignore_ascii_case(SCOOP_REPO) {
            Method::Scoop
        } else {
            Method::InPlace
        }
    } else {
        let exe = std::env::current_exe().ok().map(|e| std::fs::canonicalize(&e).unwrap_or(e).to_string_lossy().into_owned()).unwrap_or_default();
        let cask = ["/opt/homebrew/Caskroom/tmop", "/usr/local/Caskroom/tmop"].iter().any(|d| Path::new(d).is_dir());
        unix_method(repo, &exe, std::env::var("HOME").ok().as_deref(), std::env::var_os("APPIMAGE").is_some(), cask)
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
        let r = parse_release(SAMPLE, "tekikaito/tmop").unwrap();
        assert_eq!((r.tag.as_str(), r.version.as_str()), ("v0.8.0", "0.8.0"));
        assert_eq!(r.asset_url("TMOP-windows-x64.exe"), Some("https://example/x64.exe"));
        assert_eq!(r.asset_url("TMOP-windows-arm64.exe"), None);
        assert!(parse_release(&SAMPLE.replace(r#""prerelease": false"#, r#""prerelease": true"#), "a/b").is_err());
        assert!(parse_release("{}", "a/b").is_err());
        assert!(parse_release("not json", "a/b").is_err());
    }

    #[test]
    fn notes_become_plain_text() {
        let r = parse_release(SAMPLE, "tekikaito/tmop").unwrap();
        assert_eq!(plain_notes(&r.notes), "TMOP 0.8.0 adds things.\n\nNew\n\u{2022} Update check in Settings, see the README.");
        assert_eq!(strip_inline("a [b] c [d](e) `f`"), "a [b] c d f");
    }

    #[test]
    fn update_commands() {
        assert_eq!(Method::InPlace.command("tekikaito/tmop"), None);
        assert_eq!(Method::Homebrew.command("tekikaito/tmop").as_deref(), Some("brew upgrade --cask tmop"));
        assert_eq!(Method::InstallCommand.command("tekikaito/tmop").as_deref(), Some("curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh"));
        assert_eq!(Method::InstallCommand.command("Someone/tmop-fork").as_deref(), Some("curl -fsSL https://someone.github.io/tmop-fork/install.sh | sh"));
        assert_eq!(method("tekikaito/tmop") == Method::InPlace, cfg!(windows));
        assert_ne!(method("someone/fork"), Method::Homebrew, "the cask only updates from its own repository");
        assert_eq!(Method::Scoop.command("tekikaito/tmop").as_deref(), Some("scoop update tmop"));
    }

    #[test]
    fn scoop_installs_are_recognised() {
        let dirs = |known: &'static [&'static str]| move |p: &str| known.iter().any(|k| k.eq_ignore_ascii_case(p));
        let user = dirs(&[r"C:\Users\Ann\scoop\shims", r"C:\Users\Ann\scoop\apps"]);
        let no_env: Vec<String> = vec![];
        assert!(is_scoop_exe(r"C:\Users\Ann\scoop\apps\tmop\0.11.1\TMOP.exe", &no_env, user));
        assert!(is_scoop_exe(r"C:\Users\Ann\scoop\apps\tmop\current\TMOP.exe", &no_env, user));
        // Case, slashes and the prefix canonicalize() adds.
        assert!(is_scoop_exe(r"\\?\c:\users\ann\SCOOP\Apps\TMOP\0.11.1\tmop.exe", &no_env, user));
        assert!(is_scoop_exe("C:/Users/Ann/scoop/apps/tmop/0.11.1/TMOP.exe", &no_env, user));
        // Global install, found through SCOOP_GLOBAL even if the folders cannot be listed.
        let global = vec![r"C:\ProgramData\scoop".to_string()];
        assert!(is_scoop_exe(r"C:\ProgramData\scoop\apps\tmop\0.11.1\TMOP.exe", &global, dirs(&[])));
        assert!(is_scoop_exe(r"C:\programdata\SCOOP\apps\tmop\current\TMOP.exe", &[r"C:\ProgramData\scoop\".to_string()], dirs(&[])));
        // A custom root with shims and apps beside each other.
        assert!(is_scoop_exe(r"D:\tools\apps\tmop\0.11.1\TMOP.exe", &no_env, dirs(&[r"D:\tools\shims", r"D:\tools\apps"])));
        // A canonicalized network root.
        assert!(is_scoop_exe(r"\\?\UNC\srv\share\scoop\apps\tmop\1.0.0\TMOP.exe", &no_env, dirs(&[r"\\srv\share\scoop\shims", r"\\srv\share\scoop\apps"])));
    }

    #[test]
    fn other_folders_are_not_scoop() {
        let none = |_: &str| false;
        let no_env: Vec<String> = vec![];
        // Merely called apps\tmop: no shims folder next to apps.
        assert!(!is_scoop_exe(r"C:\apps\tmop\0.11.1\TMOP.exe", &no_env, |p| p == r"C:\apps"));
        assert!(!is_scoop_exe(r"C:\Users\Ann\scoop\apps\tmop\0.11.1\TMOP.exe", &no_env, none));
        // Wrong depth or names.
        assert!(!is_scoop_exe(r"C:\Users\Ann\scoop\apps\tmop\TMOP.exe", &["C:\\Users\\Ann\\scoop".into()], none));
        assert!(!is_scoop_exe(r"C:\Users\Ann\scoop\apps\tmop\0.11.1\bin\TMOP.exe", &["C:\\Users\\Ann\\scoop".into()], none));
        assert!(!is_scoop_exe(r"C:\Users\Ann\scoop\apps\other\0.11.1\TMOP.exe", &["C:\\Users\\Ann\\scoop".into()], none));
        assert!(!is_scoop_exe(r"C:\Users\Ann\scoop\app\tmop\0.11.1\TMOP.exe", &["C:\\Users\\Ann\\scoop".into()], none));
        // A different SCOOP root than the one the exe is in.
        assert!(!is_scoop_exe(r"C:\Users\Ann\apps\tmop\0.11.1\TMOP.exe", &["C:\\Users\\Ann\\scoop".into()], none));
        // Ordinary install locations.
        assert!(!is_scoop_exe(r"C:\Program Files\TMOP\TMOP.exe", &no_env, none));
        assert!(!is_scoop_exe("", &no_env, none));
    }

    #[test]
    fn unix_methods() {
        let r = "tekikaito/tmop";
        let home = Some("/home/me");
        let m = |exe: &str, appimage, cask| unix_method(r, exe, home, appimage, cask);
        // Canonicalisation happens in `method`, outside this function: a symlink such as
        // /run/current-system/sw/bin/tmop arrives here as its /nix/store target.
        assert_eq!(m("/nix/store/abc-tmop-0.8.0/bin/tmop", false, false), Method::Nix);
        assert_eq!(m("/home/me/.nix-profile/bin/tmop", false, false), Method::InstallCommand, "an unresolved symlink is not Nix");
        assert_eq!(m("/home/me/nix/store/x/tmop", false, false), Method::InstallCommand);
        // .deb / .rpm
        assert_eq!(m("/usr/bin/tmop", false, false), Method::SystemPackage);
        assert_eq!(unix_method(r, "/usr/bin/tmop", Some("/usr"), false, false), Method::InstallCommand, "under $HOME");
        assert_eq!(m("/usr/local/bin/tmop", false, false), Method::InstallCommand);
        assert_eq!(m("/usr/bin/tmop-other", false, false), Method::InstallCommand);
        // install.sh, AppImage, tar.gz: unchanged
        assert_eq!(m("/home/me/.local/bin/tmop", false, false), Method::InstallCommand);
        assert_eq!(m("/tmp/.mount_TMOPab/usr/bin/tmop", true, false), Method::InstallCommand);
        assert_eq!(m("/usr/bin/tmop", true, false), Method::InstallCommand, "APPIMAGE set");
        assert_eq!(unix_method(r, "/nix/store/a/bin/tmop", None, false, false), Method::Nix);
        assert_eq!(unix_method(r, "", None, false, false), Method::InstallCommand);
        // macOS
        assert_eq!(m("/Applications/TMOP.app/Contents/MacOS/tmop", false, true), Method::Homebrew);
        assert_eq!(m("/Applications/TMOP.app/Contents/MacOS/tmop", false, false), Method::InstallCommand);
        assert_eq!(unix_method("someone/fork", "/x/tmop", home, false, true), Method::InstallCommand, "cask follows only tekikaito/tmop");
    }

    #[test]
    fn hints_of_packaged_installs() {
        let r = "tekikaito/tmop";
        assert!(Method::Nix.hint_only() && Method::SystemPackage.hint_only());
        assert!(!Method::InstallCommand.hint_only() && !Method::InPlace.hint_only() && !Method::Homebrew.hint_only() && !Method::Scoop.hint_only());
        assert_eq!(Method::Nix.command(r), None);
        assert_eq!(Method::SystemPackage.command(r), None);
        let nix = Method::Nix.instructions(r);
        assert!(nix.contains("nix flake update tmop") && nix.contains("nix run github:tekikaito/tmop --refresh"));
        let pkg = Method::SystemPackage.instructions(r);
        assert!(pkg.contains(".deb") && pkg.contains(".rpm") && pkg.contains("release page"));
        assert_eq!(Method::InstallCommand.instructions(r), "Installed with the install command? Run it again in a terminal to update. Otherwise download the new version from the release page.");
    }

    #[test]
    fn update_repository_setting() {
        assert!(valid_repo("tekikaito/tmop") && valid_repo("A-b_c.d/x.y-z_9"));
        for bad in ["", "tmop", "/tmop", "owner/", "a/b/c", "a b/c", "../x", "a/..", "https://github.com/a/b", "a/b?x"] {
            assert!(!valid_repo(bad), "{bad:?}");
        }
        assert_eq!(repo(""), RELEASE_REPO);
        assert_eq!(repo("  someone/fork "), "someone/fork");
        assert_eq!(repo("not a repo"), RELEASE_REPO, "an invalid entry falls back to the built-in repository");
        assert_eq!(pages_url("Tekikaito/tmop"), "https://tekikaito.github.io/tmop/");
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
        let release = parse_release(&http_get(&latest_api(RELEASE_REPO)).unwrap(), RELEASE_REPO).unwrap();
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
