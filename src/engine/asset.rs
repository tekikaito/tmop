//! Engine release assets: names, download URLs, `.sha256` parsing, SHA-256 verification,
//! gzip import and version comparison. File IO is limited to the functions that take paths.

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::engine::RELEASE_REPO;

/// Read size for hashing and copying.
const CHUNK: usize = 64 * 1024;

/// `tmop-engine-<tag>-<arch>.tar.gz`
pub fn asset_name(tag: &str, arch: &str) -> String {
    format!("tmop-engine-{tag}-{arch}.tar.gz")
}

/// Checksum file name: the asset name plus `.sha256`.
pub fn sha_name(tag: &str, arch: &str) -> String {
    format!("{}.sha256", asset_name(tag, arch))
}

/// Download URL of the engine tarball on the release page.
pub fn asset_url(tag: &str, arch: &str) -> String {
    release_url(tag, &asset_name(tag, arch))
}

/// Download URL of the checksum file.
pub fn sha_url(tag: &str, arch: &str) -> String {
    release_url(tag, &sha_name(tag, arch))
}

fn release_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/{tag}/{name}")
}

/// Reads the digest from a `.sha256` file (`<hex>  <name>`, optional BOM, CRLF or `*name`).
pub fn parse_sha256_file(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let token = line.split(|c: char| c.is_whitespace() || c == '*').next()?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(token.to_ascii_lowercase())
}

/// SHA-256 of a stream as lowercase hex. `progress` receives the total bytes read so far.
pub fn sha256_hex(mut reader: impl Read, mut progress: impl FnMut(u64)) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks a file against an expected digest (case-insensitive).
pub fn verify_file(path: &Path, expected_hex: &str, progress: impl FnMut(u64)) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let actual = sha256_hex(file, progress).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let expected = expected_hex.trim().to_ascii_lowercase();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("checksum mismatch for {}: expected {expected}, got {actual}", path.display()))
    }
}

/// Gunzips `src` into `dst` (a plain copy when `src` does not end with `.gz`).
///
/// Writes `<dst>.part` first and renames it into place. `progress` receives the compressed bytes read.
pub fn gunzip_to(src: &Path, dst: &Path, mut progress: impl FnMut(u64)) -> io::Result<()> {
    let part = suffixed(dst, ".part");
    if let Err(e) = write_part(src, &part, &mut progress) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, dst)
}

fn write_part(src: &Path, part: &Path, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    let file = File::open(src)?;
    let mut out = File::create(part)?;
    let mut input = Counting { inner: file, progress, count: 0 };
    let gz = src
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".gz"));
    if gz {
        io::copy(&mut flate2::read::GzDecoder::new(&mut input), &mut out)?;
    } else {
        io::copy(&mut input, &mut out)?;
    }
    out.sync_all()
}

/// Reader wrapper that reports the bytes read through it.
struct Counting<'a, R> {
    inner: R,
    progress: &'a mut dyn FnMut(u64),
    count: u64,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        (self.progress)(self.count);
        Ok(n)
    }
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A `major.minor.patch` version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Parses `v0.1.0` or `0.1.0`; a `-suffix` is ignored.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let core = s.split('-').next()?;
        let mut parts = core.split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = Version(next()?, next()?, next()?);
        if parts.next().is_some() {
            return None;
        }
        Some(v)
    }
}

/// True when `installed` parses and is older than `current`.
pub fn update_available(installed: &str, current: &str) -> bool {
    match (Version::parse(installed), Version::parse(current)) {
        (Some(i), Some(c)) => i < c,
        _ => false,
    }
}

/// True when both versions parse and are equal (suffixes ignored).
pub fn installed_matches(installed: &str, current: &str) -> bool {
    matches!((Version::parse(installed), Version::parse(current)), (Some(i), Some(c)) if i == c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmop-asset-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_and_urls() {
        assert_eq!(asset_name("v0.1.0", "x64"), "tmop-engine-v0.1.0-x64.tar.gz");
        assert_eq!(asset_name("v0.1.0", "arm64"), "tmop-engine-v0.1.0-arm64.tar.gz");
        assert_eq!(sha_name("v0.1.0", "arm64"), "tmop-engine-v0.1.0-arm64.tar.gz.sha256");
        assert_eq!(
            asset_url("v0.1.0", "x64"),
            "https://github.com/tekikaito/tmop/releases/download/v0.1.0/tmop-engine-v0.1.0-x64.tar.gz"
        );
        assert_eq!(
            sha_url("v0.1.0", "x64"),
            "https://github.com/tekikaito/tmop/releases/download/v0.1.0/tmop-engine-v0.1.0-x64.tar.gz.sha256"
        );
    }

    #[test]
    fn sha_file_variants() {
        let upper = ABC_SHA256.to_ascii_uppercase();
        assert_eq!(parse_sha256_file(&format!("{ABC_SHA256}\n")).as_deref(), Some(ABC_SHA256));
        assert_eq!(parse_sha256_file(&format!("{upper}  tmop.tar.gz\n")).as_deref(), Some(ABC_SHA256));
        assert_eq!(parse_sha256_file(&format!("{ABC_SHA256} *tmop.tar.gz\r\n")).as_deref(), Some(ABC_SHA256));
        assert_eq!(parse_sha256_file(&format!("\u{feff}{ABC_SHA256}  f\r\n")).as_deref(), Some(ABC_SHA256));
        assert_eq!(parse_sha256_file(&format!("\n\n  {ABC_SHA256} f\nsecond line")).as_deref(), Some(ABC_SHA256));
        assert_eq!(parse_sha256_file("not a checksum"), None);
        assert_eq!(parse_sha256_file(""), None);
        assert_eq!(parse_sha256_file("abcd  short"), None);
        assert_eq!(parse_sha256_file(&format!("{}g  f", &ABC_SHA256[1..])), None);
    }

    #[test]
    fn sha256_of_known_input() {
        let mut seen = 0;
        let digest = sha256_hex(&b"abc"[..], |n| seen = n).unwrap();
        assert_eq!(digest, ABC_SHA256);
        assert_eq!(seen, 3);
    }

    #[test]
    fn sha256_of_multi_chunk_input() {
        let data = vec![7u8; CHUNK * 2 + 5];
        let mut last = 0;
        let digest = sha256_hex(&data[..], |n| last = n).unwrap();
        assert_eq!(last, data.len() as u64);
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn verify_file_compares_digests() {
        let dir = temp_dir("verify");
        let path = dir.join("abc.bin");
        fs::write(&path, b"abc").unwrap();
        assert!(verify_file(&path, ABC_SHA256, |_| {}).is_ok());
        assert!(verify_file(&path, &ABC_SHA256.to_ascii_uppercase(), |_| {}).is_ok());
        let err = verify_file(&path, &"0".repeat(64), |_| {}).unwrap_err();
        assert!(err.contains("abc.bin"), "{err}");
        assert!(err.contains(ABC_SHA256), "{err}");
        assert!(err.contains(&"0".repeat(64)), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gunzip_roundtrip() {
        let dir = temp_dir("gunzip");
        let payload = b"tar bytes go here".repeat(1000);
        let src = dir.join("engine.tar.gz");
        let mut enc = GzEncoder::new(fs::File::create(&src).unwrap(), Compression::default());
        enc.write_all(&payload).unwrap();
        enc.finish().unwrap();

        let dst = dir.join("engine.tar");
        let mut read = 0;
        gunzip_to(&src, &dst, |n| read = n).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), payload);
        assert!(read > 0);
        assert!(!dir.join("engine.tar.part").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_gz_source_is_copied() {
        let dir = temp_dir("copy");
        let src = dir.join("engine.tar");
        fs::write(&src, b"plain").unwrap();
        let dst = dir.join("out.tar");
        gunzip_to(&src, &dst, |_| {}).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"plain");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gunzip_error_leaves_no_part_file() {
        let dir = temp_dir("badgz");
        let src = dir.join("bad.tar.gz");
        fs::write(&src, b"definitely not gzip").unwrap();
        let dst = dir.join("bad.tar");
        assert!(gunzip_to(&src, &dst, |_| {}).is_err());
        assert!(!dst.exists());
        assert!(!dir.join("bad.tar.part").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_parse_and_order() {
        assert_eq!(Version::parse("v0.1.0"), Some(Version(0, 1, 0)));
        assert_eq!(Version::parse("0.1.0"), Some(Version(0, 1, 0)));
        assert_eq!(Version::parse("v1.2.3-beta.1"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("1.x.3"), None);
        assert_eq!(Version::parse(""), None);
        assert!(Version(0, 1, 9) < Version(0, 2, 0));
        assert!(Version(0, 2, 0) < Version(1, 0, 0));
    }

    #[test]
    fn update_and_match_checks() {
        assert!(update_available("v0.1.0", "0.2.0"));
        assert!(!update_available("0.2.0", "0.2.0"));
        assert!(!update_available("0.3.0", "0.2.0"));
        assert!(!update_available("garbage", "0.2.0"));
        assert!(!update_available("0.1.0", "garbage"));
        assert!(installed_matches("v0.2.0", "0.2.0"));
        assert!(installed_matches("0.2.0-rc1", "0.2.0"));
        assert!(!installed_matches("0.1.0", "0.2.0"));
        assert!(!installed_matches("", "0.2.0"));
    }
}
