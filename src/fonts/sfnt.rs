//! The font-folder scanner behind the Display font picker off Windows (Linux, macOS): the font
//! folders of the OS (and, on Linux, those fontconfig names), and of every `.ttf`, `.otf`, `.ttc`
//! and `.otc` in them the family name (OpenType `name` table), weight and slant (`OS/2`, else
//! `head`). Only the table directory and those tables are read, not whole files.
//!
//! The parsers take a reader of byte ranges, so the tests feed them built fonts.
#![cfg_attr(windows, allow(dead_code))]

use crate::platform::xdg::Env;
use std::path::{Path, PathBuf};

/// Reads `len` bytes at `offset` of a font file (`None` past its end or on an I/O error).
pub type ReadAt<'a> = &'a mut dyn FnMut(u64, usize) -> Option<Vec<u8>>;

/// Most faces read from one collection, tables read from one face, and bytes of a `name` table.
const MAX_FACES: u32 = 256;
const MAX_TABLES: u16 = 512;
const MAX_NAME: usize = 1 << 20;
/// How deep the scan descends below a font folder.
const MAX_DEPTH: usize = 8;

/// One face of a font file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceInfo {
    /// Typographic family (`name` ID 16), else the family (ID 1).
    pub family: String,
    /// `OS/2` `usWeightClass` (400 when there is no `OS/2` table).
    pub weight: u16,
    pub italic: bool,
    /// Index of the face in a collection (0 for a single font).
    pub index: u32,
}

/// A face found by [`scan`], with the file it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedFace {
    pub face: FaceInfo,
    pub path: PathBuf,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// TrueType outlines (`0x00010000`, Apple `true`) or CFF (`OTTO`).
fn is_sfnt(version: u32) -> bool {
    matches!(version, 0x0001_0000 | 0x7472_7565 | 0x4F54_544F)
}

/// The faces of a font file: one for a `.ttf`/`.otf`, one per member of a collection. Faces
/// without a family name are left out; an unreadable file has none.
pub fn faces(read: ReadAt) -> Vec<FaceInfo> {
    let Some(head) = read(0, 12) else { return Vec::new() };
    let offsets: Vec<u32> = if head.get(..4) == Some(b"ttcf") {
        let n = be32(&head, 8).unwrap_or(0).min(MAX_FACES);
        match read(12, 4 * n as usize) {
            Some(b) => (0..n as usize).filter_map(|i| be32(&b, 4 * i)).collect(),
            None => Vec::new(),
        }
    } else {
        vec![0]
    };
    offsets.into_iter().enumerate().filter_map(|(i, off)| face_at(read, off as u64, i as u32)).collect()
}

/// The face whose table directory is at `off`.
fn face_at(read: ReadAt, off: u64, index: u32) -> Option<FaceInfo> {
    let dir = read(off, 12)?;
    if !is_sfnt(be32(&dir, 0)?) {
        return None;
    }
    let tables = be16(&dir, 4)?.min(MAX_TABLES) as usize;
    let recs = read(off + 12, 16 * tables)?;
    let table = |tag: &[u8]| (0..tables).find(|t| recs.get(16 * t..16 * t + 4) == Some(tag)).and_then(|t| Some((be32(&recs, 16 * t + 8)? as u64, be32(&recs, 16 * t + 12)? as usize)));
    let (name_off, name_len) = table(b"name")?;
    let family = family_name(&read(name_off, name_len.min(MAX_NAME))?)?;
    let (mut weight, mut italic) = (400, false);
    if let Some(os2) = table(b"OS/2").and_then(|(o, l)| read(o, l.min(64))) {
        weight = be16(&os2, 4).filter(|w| (1..=1000).contains(w)).unwrap_or(400);
        // fsSelection: bit 0 ITALIC, bit 9 OBLIQUE.
        italic = be16(&os2, 62).is_some_and(|f| f & 0x0201 != 0);
    } else if let Some(head) = table(b"head").and_then(|(o, l)| read(o, l.min(54))) {
        // macStyle: bit 0 bold, bit 1 italic.
        let style = be16(&head, 44).unwrap_or(0);
        italic = style & 2 != 0;
        weight = if style & 1 != 0 { 700 } else { 400 };
    }
    Some(FaceInfo { family, weight, italic, index })
}

/// The family of a `name` table: the typographic family (ID 16), else the family (ID 1). English
/// Windows names come first, then any Windows name, then Unicode, then Mac Roman ones.
pub fn family_name(table: &[u8]) -> Option<String> {
    let count = be16(table, 2)? as usize;
    let strings = be16(table, 4)? as usize;
    let mut best: Option<(u8, String)> = None;
    for id in [16u16, 1] {
        for r in 0..count {
            let rec = 6 + 12 * r;
            let (Some(platform), Some(encoding), Some(lang), Some(name_id), Some(len), Some(off)) =
                (be16(table, rec), be16(table, rec + 2), be16(table, rec + 4), be16(table, rec + 6), be16(table, rec + 8), be16(table, rec + 10))
            else {
                break;
            };
            if name_id != id {
                continue;
            }
            let Some(bytes) = table.get(strings + off as usize..strings + off as usize + len as usize) else { continue };
            let (rank, text) = match (platform, encoding) {
                (3, 0 | 1 | 10) => (if lang == 0x0409 { 0 } else { 1 }, utf16be(bytes)),
                (0, _) => (2, utf16be(bytes)),
                (1, 0) if lang == 0 => (3, bytes.iter().map(|&b| if b.is_ascii() { b as char } else { '\u{fffd}' }).collect()),
                _ => continue,
            };
            let text = text.trim().to_string();
            if !text.is_empty() && best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, text));
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|(_, t)| t)
}

fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// Whether the picker lists `family`: hidden system families (macOS names them with a leading
/// dot) and Apple's LastResort are left out.
pub fn listed(family: &str) -> bool {
    !family.is_empty() && !family.starts_with('.') && family != "LastResort"
}

/// The picker's families from scanned faces: those with an upright face, each once (any case),
/// sorted, without Michroma (listed first as the built-in font).
pub fn families(faces: &[ScannedFace]) -> Vec<String> {
    let mut out: Vec<String> = faces.iter().filter(|f| !f.face.italic && listed(&f.face.family) && !super::is_built_in(&f.face.family)).map(|f| f.face.family.clone()).collect();
    out.sort_by_key(|n| n.to_lowercase());
    out.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    out
}

/// The face [`super::load`] reads for `family`: upright before italic, then the weight nearest
/// regular (400), then the first found.
pub fn pick<'a>(faces: &'a [ScannedFace], family: &str) -> Option<&'a ScannedFace> {
    let family = family.trim();
    faces.iter().filter(|f| f.face.family.eq_ignore_ascii_case(family)).min_by_key(|f| (f.face.italic, (f.face.weight as i32 - 400).abs()))
}

/// The `<dir>` folders of a fontconfig file. `prefix="xdg"` is relative to `$XDG_DATA_HOME`
/// (`xdg_data`), a leading `~` to `home`; relative folders are skipped.
pub fn fontconfig_dirs(xml: &str, home: Option<&Path>, xdg_data: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<dir") {
        rest = &rest[start + 4..];
        // `<dir>` or `<dir attr=...>`, not `<directory>` or `<dirs>`.
        if !rest.starts_with(['>', ' ', '\t', '\n', '\r']) {
            continue;
        }
        let Some(close) = rest.find('>') else { break };
        let attrs = &rest[..close];
        rest = &rest[close + 1..];
        if attrs.ends_with('/') {
            continue;
        }
        let Some(end) = rest.find("</dir>") else { break };
        let text = rest[..end].trim().replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'");
        rest = &rest[end..];
        let xdg = attrs.contains("prefix=\"xdg\"") || attrs.contains("prefix='xdg'");
        let dir = if xdg {
            xdg_data.map(|d| d.join(&text))
        } else if text == "~" {
            home.map(Path::to_path_buf)
        } else if let Some(sub) = text.strip_prefix("~/") {
            home.map(|h| h.join(sub))
        } else {
            Some(PathBuf::from(&text)).filter(|p| p.has_root())
        };
        if let Some(d) = dir.filter(|d| !out.contains(d)) {
            out.push(d);
        }
    }
    out
}

/// The font folders of `os` before fontconfig's: Linux `$XDG_DATA_HOME/fonts`, `~/.fonts` and
/// `fonts` under each `$XDG_DATA_DIRS` entry; macOS the system, local and user `Fonts` folders.
pub fn base_dirs(os: crate::platform::Os, env: Env) -> Vec<PathBuf> {
    use crate::platform::Os;
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).filter(|h| h.has_root());
    let mut dirs: Vec<PathBuf> = Vec::new();
    match os {
        Os::MacOs => {
            dirs.extend([PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts")]);
            dirs.extend(home.map(|h| h.join("Library/Fonts")));
        }
        Os::Linux | Os::Other => {
            let data = env("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.has_root()).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
            dirs.extend(data.map(|d| d.join("fonts")));
            dirs.extend(home.map(|h| h.join(".fonts")));
            let system = env("XDG_DATA_DIRS").filter(|v| !v.is_empty()).map(|v| v.to_string_lossy().into_owned()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            dirs.extend(system.split(':').map(PathBuf::from).filter(|p| p.has_root()).map(|p| p.join("fonts")));
        }
        Os::Windows => {}
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs
}

/// This machine's font folders: [`base_dirs`], then on Linux the `<dir>`s of fontconfig's own,
/// conf.d and user files.
pub fn font_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k);
    let os = crate::platform::OS;
    let mut dirs = base_dirs(os, &env);
    if os == crate::platform::Os::Linux {
        let home = env("HOME").map(PathBuf::from).filter(|h| h.has_root());
        let xdg_data = env("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.has_root()).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let config = env("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.has_root()).or_else(|| home.as_ref().map(|h| h.join(".config")));
        let mut files = vec![PathBuf::from("/etc/fonts/fonts.conf"), PathBuf::from("/etc/fonts/local.conf")];
        if let Ok(rd) = std::fs::read_dir("/etc/fonts/conf.d") {
            let mut confs: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "conf")).collect();
            confs.sort();
            files.extend(confs);
        }
        files.extend(config.map(|c| c.join("fontconfig/fonts.conf")));
        for f in files {
            if let Ok(xml) = std::fs::read_to_string(&f) {
                for d in fontconfig_dirs(&xml, home.as_deref(), xdg_data.as_deref()) {
                    if !dirs.contains(&d) {
                        dirs.push(d);
                    }
                }
            }
        }
    }
    dirs
}

/// Whether `path` names an OpenType font or collection.
fn is_font_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "otf", "ttc", "otc"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The faces of one font file on disk.
pub fn file_faces(path: &Path) -> Vec<FaceInfo> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> {
        file.seek(SeekFrom::Start(off)).ok()?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    faces(&mut read)
}

/// Every face in the font files under `dirs` (recursively, each file once, symlinked folders
/// followed without looping).
pub fn scan(dirs: &[PathBuf]) -> Vec<ScannedFace> {
    let mut out = Vec::new();
    let mut seen_dirs = std::collections::HashSet::new();
    let mut seen_files = std::collections::HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    stack.reverse();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(real) = std::fs::canonicalize(&dir) else { continue };
        if !seen_dirs.insert(real) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((p, depth + 1));
                }
            } else if meta.is_file() && is_font_file(&p) && seen_files.insert(std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone())) {
                out.extend(file_faces(&p).into_iter().map(|face| ScannedFace { face, path: p.clone() }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A `name` table with `(platform, encoding, language, name id, text)` records; the text is
    /// stored UTF-16BE except on the Mac platform.
    fn name_table(records: &[(u16, u16, u16, u16, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut recs = Vec::new();
        for &(platform, encoding, lang, id, text) in records {
            let bytes: Vec<u8> = if platform == 1 { text.as_bytes().to_vec() } else { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
            for v in [platform, encoding, lang, id, bytes.len() as u16, strings.len() as u16] {
                recs.extend(v.to_be_bytes());
            }
            strings.extend(bytes);
        }
        let mut t = Vec::new();
        t.extend(0u16.to_be_bytes());
        t.extend((records.len() as u16).to_be_bytes());
        t.extend((6 + recs.len() as u16).to_be_bytes());
        t.extend(recs);
        t.extend(strings);
        t
    }

    /// An `OS/2` table (version 4 size) with `weight` and `fs_selection`.
    fn os2(weight: u16, fs_selection: u16) -> Vec<u8> {
        let mut t = vec![0u8; 96];
        t[0..2].copy_from_slice(&4u16.to_be_bytes());
        t[4..6].copy_from_slice(&weight.to_be_bytes());
        t[62..64].copy_from_slice(&fs_selection.to_be_bytes());
        t
    }

    /// A single-font file: an sfnt header and directory, then the `tables` in order.
    fn font(version: u32, tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        sfnt_at(0, version, tables)
    }

    fn sfnt_at(base: usize, version: u32, tables: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(version.to_be_bytes());
        out.extend((tables.len() as u16).to_be_bytes());
        out.extend([0u8; 6]);
        let mut off = base + 12 + 16 * tables.len();
        for (tag, data) in tables {
            out.extend(tag.iter());
            out.extend(0u32.to_be_bytes());
            out.extend((off as u32).to_be_bytes());
            out.extend((data.len() as u32).to_be_bytes());
            off += data.len();
        }
        for (_, data) in tables {
            out.extend(data);
        }
        out
    }

    fn read_slice(data: &[u8]) -> impl FnMut(u64, usize) -> Option<Vec<u8>> + '_ {
        move |off, len| data.get(off as usize..(off as usize).checked_add(len)?).map(<[u8]>::to_vec)
    }

    #[test]
    fn family_prefers_typographic_english_windows_names() {
        let t = name_table(&[(1, 0, 0, 1, "Mac Family"), (3, 1, 0x0407, 1, "Deutsch"), (3, 1, 0x0409, 1, "Inter Light"), (3, 1, 0x0409, 16, "Inter"), (3, 1, 0x0409, 2, "Light")]);
        assert_eq!(family_name(&t).as_deref(), Some("Inter"));
        let t = name_table(&[(1, 0, 0, 1, "Mac Family"), (3, 1, 0x0407, 1, "Deutsch"), (3, 1, 0x0409, 1, "  English  ")]);
        assert_eq!(family_name(&t).as_deref(), Some("English"));
        let t = name_table(&[(1, 0, 0, 1, "Mac Family"), (3, 1, 0x0407, 1, "Deutsch")]);
        assert_eq!(family_name(&t).as_deref(), Some("Deutsch"));
        let t = name_table(&[(1, 0, 0, 1, "Mac Family"), (0, 3, 0, 1, "Unicode \u{e9}")]);
        assert_eq!(family_name(&t).as_deref(), Some("Unicode \u{e9}"));
        assert_eq!(family_name(&name_table(&[(1, 0, 0, 1, "Mac Family")])).as_deref(), Some("Mac Family"));
        // Only a style name, an empty name, an unknown platform, or a truncated table: no family.
        assert_eq!(family_name(&name_table(&[(3, 1, 0x0409, 2, "Regular"), (3, 1, 0x0409, 1, " "), (2, 0, 0, 1, "ISO")])), None);
        let t = name_table(&[(3, 1, 0x0409, 1, "Cut")]);
        assert_eq!(family_name(&t[..t.len() - 2]), None);
        assert_eq!(family_name(&[0, 0]), None);
    }

    #[test]
    fn reads_weight_and_slant() {
        let name = name_table(&[(3, 1, 0x0409, 1, "Test Sans")]);
        let f = font(0x0001_0000, &[(b"OS/2", os2(300, 0x0001)), (b"name", name.clone())]);
        assert_eq!(faces(&mut read_slice(&f)), [FaceInfo { family: "Test Sans".into(), weight: 300, italic: true, index: 0 }]);
        let f = font(u32::from_be_bytes(*b"OTTO"), &[(b"OS/2", os2(400, 0x0040)), (b"name", name.clone())]);
        assert_eq!(faces(&mut read_slice(&f))[0], FaceInfo { family: "Test Sans".into(), weight: 400, italic: false, index: 0 });
        // No OS/2: the head table's macStyle (bold + italic).
        let mut head = vec![0u8; 54];
        head[44..46].copy_from_slice(&3u16.to_be_bytes());
        let f = font(0x0001_0000, &[(b"head", head), (b"name", name.clone())]);
        assert_eq!(faces(&mut read_slice(&f))[0], FaceInfo { family: "Test Sans".into(), weight: 700, italic: true, index: 0 });
        // Neither: regular.
        let f = font(0x0001_0000, &[(b"name", name.clone())]);
        assert_eq!((faces(&mut read_slice(&f))[0].weight, faces(&mut read_slice(&f))[0].italic), (400, false));
        // Not an sfnt (a Type 1 or WOFF file), no name table, or cut short: nothing.
        assert!(faces(&mut read_slice(&font(u32::from_be_bytes(*b"wOFF"), &[(b"name", name.clone())]))).is_empty());
        assert!(faces(&mut read_slice(&font(0x0001_0000, &[(b"OS/2", os2(400, 0))]))).is_empty());
        let f = font(0x0001_0000, &[(b"name", name)]);
        assert!(faces(&mut read_slice(&f[..20])).is_empty());
        assert!(faces(&mut read_slice(&[])).is_empty());
    }

    #[test]
    fn lists_every_face_of_a_collection() {
        let regular = [(b"OS/2", os2(400, 0x40)), (b"name", name_table(&[(3, 1, 0x0409, 1, "Coll")]))];
        let bold = [(b"OS/2", os2(700, 0x20)), (b"name", name_table(&[(3, 1, 0x0409, 1, "Coll")]))];
        let mut ttc = b"ttcf".to_vec();
        ttc.extend(0x0001_0000u32.to_be_bytes());
        ttc.extend(2u32.to_be_bytes());
        let first = 20usize;
        let a = sfnt_at(first, 0x0001_0000, &regular);
        let second = first + a.len();
        ttc.extend((first as u32).to_be_bytes());
        ttc.extend((second as u32).to_be_bytes());
        ttc.extend(a);
        ttc.extend(sfnt_at(second, 0x0001_0000, &bold));
        let got = faces(&mut read_slice(&ttc));
        assert_eq!(got.iter().map(|f| (f.index, f.weight)).collect::<Vec<_>>(), [(0, 400), (1, 700)]);
        // A face's index matches what collection_index finds from its name table.
        assert_eq!(super::super::collection_index(&ttc, &name_table(&[(3, 1, 0x0409, 1, "Coll")])), Some(0));
    }

    fn scanned(family: &str, weight: u16, italic: bool, path: &str) -> ScannedFace {
        ScannedFace { face: FaceInfo { family: family.into(), weight, italic, index: 0 }, path: PathBuf::from(path) }
    }

    #[test]
    fn families_and_the_face_to_load() {
        let all = [
            scanned("Noto Sans", 700, false, "/f/NotoSans-Bold.ttf"),
            scanned("Noto Sans", 400, true, "/f/NotoSans-Italic.ttf"),
            scanned("Noto Sans", 400, false, "/f/NotoSans-Regular.ttf"),
            scanned("noto sans", 300, false, "/f/NotoSans-Light.ttf"),
            scanned("Only Italic", 400, true, "/f/OI.ttf"),
            scanned(".SF NS", 400, false, "/f/SF.ttf"),
            scanned("LastResort", 400, false, "/f/LR.otf"),
            scanned("Michroma", 400, false, "/f/Michroma.ttf"),
            scanned("DejaVu Sans", 400, false, "/f/DejaVuSans.ttf"),
        ];
        assert_eq!(families(&all), ["DejaVu Sans", "Noto Sans"]);
        assert_eq!(pick(&all, " NOTO SANS ").map(|f| f.path.clone()), Some(PathBuf::from("/f/NotoSans-Regular.ttf")));
        assert_eq!(pick(&all[..2], "Noto Sans").map(|f| f.face.weight), Some(700), "upright before italic");
        assert_eq!(pick(&all, "Only Italic").map(|f| f.face.italic), Some(true));
        assert_eq!(pick(&all, "Nope"), None);
    }

    #[test]
    fn fontconfig_dir_entries() {
        let xml = r#"<?xml version="1.0"?>
            <fontconfig>
              <dir>/usr/share/fonts</dir>
              <dir>/usr/local/share/fonts</dir>
              <dir prefix="xdg">fonts</dir>
              <!-- the deprecated user folder -->
              <dir>~/.fonts</dir>
              <dir>relative/fonts</dir>
              <dir/>
              <directory>/not/this</directory>
              <dir salt="x" >/opt/Fonts &amp; More</dir>
              <dir>/usr/share/fonts</dir>
              <cachedir>/var/cache/fontconfig</cachedir>
            </fontconfig>"#;
        let got = fontconfig_dirs(xml, Some(Path::new("/home/u")), Some(Path::new("/home/u/.local/share")));
        let want = ["/usr/share/fonts", "/usr/local/share/fonts", "/home/u/.local/share/fonts", "/home/u/.fonts", "/opt/Fonts & More"].map(PathBuf::from);
        assert_eq!(got, want);
        assert_eq!(fontconfig_dirs("<dir>~/.fonts</dir><dir prefix=\"xdg\">fonts</dir>", None, None), Vec::<PathBuf>::new());
        assert!(fontconfig_dirs("<dir>/unterminated", None, None).is_empty());
    }

    fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn base_font_folders_per_os() {
        use crate::platform::Os;
        let env = env_of(&[("HOME", "/home/u")]);
        assert_eq!(base_dirs(Os::Linux, &env), ["/home/u/.local/share/fonts", "/home/u/.fonts", "/usr/local/share/fonts", "/usr/share/fonts"].map(PathBuf::from));
        let env = env_of(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data"), ("XDG_DATA_DIRS", "/usr/share:relative:/usr/share")]);
        assert_eq!(base_dirs(Os::Linux, &env), ["/data/fonts", "/home/u/.fonts", "/usr/share/fonts"].map(PathBuf::from));
        assert_eq!(base_dirs(Os::MacOs, &env_of(&[("HOME", "/Users/u")])), ["/System/Library/Fonts", "/Library/Fonts", "/Users/u/Library/Fonts"].map(PathBuf::from));
        assert_eq!(base_dirs(Os::MacOs, &env_of(&[])).len(), 2);
        assert!(base_dirs(Os::Windows, &env).is_empty());
    }

    /// Scans a folder tree with a nested font, a collection-less junk file, a non-font and a
    /// duplicate path, and reads each font file once.
    #[test]
    fn scans_a_folder_tree() {
        let root = std::env::temp_dir().join(format!("tmop-fontscan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        let name = |n: &str| name_table(&[(3, 1, 0x0409, 1, n)]);
        std::fs::write(root.join("A.ttf"), font(0x0001_0000, &[(b"OS/2", os2(400, 0x40)), (b"name", name("Alpha"))])).unwrap();
        std::fs::write(root.join("sub/deeper/B.OTF"), font(u32::from_be_bytes(*b"OTTO"), &[(b"name", name("Beta"))])).unwrap();
        std::fs::write(root.join("sub/junk.ttf"), b"not a font").unwrap();
        std::fs::write(root.join("sub/readme.txt"), font(0x0001_0000, &[(b"name", name("Text"))])).unwrap();
        let got = scan(&[root.clone(), root.join("sub"), root.join("missing")]);
        let fams: Vec<&str> = got.iter().map(|f| f.face.family.as_str()).collect();
        assert_eq!(fams, ["Alpha", "Beta"]);
        assert!(got[1].path.ends_with("B.OTF"));
        assert_eq!(families(&got), ["Alpha", "Beta"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
