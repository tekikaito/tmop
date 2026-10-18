//! `.tmoptheme` palettes: the built-in ones and the custom ones in `%LOCALAPPDATA%\TMOP\themes`.
//!
//! Format notes:
//! * a palette is a JSON file `<id>.tmoptheme`:
//!   `{ "format": 1, "id": "nord-copy", "name": "Nord copy", "dark": true, "colors": { "chrome": "#343A48", ... } }`;
//! * `colors` holds all 22 roles of [`Palette`] by their [`ROLES`] keys; colours are `#RRGGBB` or
//!   `#RRGGBBAA` strings;
//! * a missing role, an unknown key or a bad colour rejects the file;
//! * built-in palettes are read-only. A custom palette starts as a duplicate of another one, and
//!   is found by a scan like the one used for skins.

use crate::skin::Color;
use crate::ui::theme::{self, Palette, ROLES};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};

/// Highest palette `format` version this loader understands.
pub const SUPPORTED_FORMAT: u32 = 1;
/// File extension of a palette.
pub const EXTENSION: &str = "tmoptheme";
/// `appearance.theme` value that follows Windows (built-in Dark or Light).
pub const SYSTEM: &str = "system";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Doc {
    format: u32,
    id: String,
    name: String,
    dark: bool,
    colors: Map<String, Value>,
}

/// Where a palette came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PaletteSource {
    #[default]
    BuiltIn,
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteFile {
    pub id: String,
    pub name: String,
    pub palette: Palette,
    pub source: PaletteSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteError {
    Io(String),
    Parse(String),
    UnsupportedFormat(u32),
}

impl fmt::Display for PaletteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteError::Io(e) => write!(f, "{e}"),
            PaletteError::Parse(e) => write!(f, "invalid palette: {e}"),
            PaletteError::UnsupportedFormat(v) => write!(f, "unsupported palette format {v}"),
        }
    }
}

impl std::error::Error for PaletteError {}

impl PaletteFile {
    pub fn from_json(text: &str) -> Result<Self, PaletteError> {
        let doc: Doc = serde_json::from_str(text).map_err(|e| PaletteError::Parse(e.to_string()))?;
        if doc.format == 0 || doc.format > SUPPORTED_FORMAT {
            return Err(PaletteError::UnsupportedFormat(doc.format));
        }
        if doc.id.trim().is_empty() {
            return Err(PaletteError::Parse("palette id is empty".into()));
        }
        let mut palette = theme::DARK;
        palette.dark = doc.dark;
        for key in doc.colors.keys() {
            if palette.role(key).is_none() {
                return Err(PaletteError::Parse(format!("unknown colour role {key:?}")));
            }
        }
        for r in ROLES {
            let v = doc.colors.get(r.key).ok_or_else(|| PaletteError::Parse(format!("missing colour role {:?}", r.key)))?;
            let c = v.as_str().and_then(Color::parse).ok_or_else(|| PaletteError::Parse(format!("{}: invalid colour {v}", r.key)))?;
            *palette.role_mut(r.key).expect("role") = theme::from_skin(c);
        }
        Ok(Self { id: doc.id, name: doc.name, palette, source: PaletteSource::BuiltIn })
    }

    pub fn to_json(&self) -> String {
        let colors = ROLES.iter().map(|r| (r.key.to_string(), Value::String(theme::to_skin(self.palette.role(r.key).expect("role")).to_string()))).collect();
        let doc = Doc { format: SUPPORTED_FORMAT, id: self.id.clone(), name: self.name.clone(), dark: self.palette.dark, colors };
        serde_json::to_string_pretty(&doc).expect("palette serialize")
    }

    pub fn load(path: &Path) -> Result<Self, PaletteError> {
        let text = std::fs::read_to_string(path).map_err(|e| PaletteError::Io(format!("{}: {e}", path.display())))?;
        let mut p = Self::from_json(&text)?;
        p.source = PaletteSource::File(path.to_path_buf());
        Ok(p)
    }

    pub fn is_built_in(&self) -> bool {
        self.source == PaletteSource::BuiltIn
    }

    /// Writes a custom palette to its file (atomically). Built-in palettes are read-only.
    pub fn save(&self) -> std::io::Result<()> {
        let PaletteSource::File(path) = &self.source else {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "built-in palettes are read-only"));
        };
        write_atomic(path, &self.to_json())
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The built-in palettes: Dark, Light, High Contrast, Nord and Solarized Light.
pub fn built_in() -> Vec<PaletteFile> {
    [
        ("dark", "Dark", theme::DARK),
        ("light", "Light", theme::LIGHT),
        ("high-contrast", "High Contrast", theme::HIGH_CONTRAST),
        ("nord", "Nord", theme::NORD),
        ("solarized-light", "Solarized Light", theme::SOLARIZED_LIGHT),
    ]
    .into_iter()
    .map(|(id, name, palette)| PaletteFile { id: id.into(), name: name.into(), palette, source: PaletteSource::BuiltIn })
    .collect()
}

/// `%LOCALAPPDATA%\TMOP\themes`
pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(crate::APP_DIR).join("themes")
}

fn is_palette_file(path: &Path) -> bool {
    path.is_file() && path.extension().map(|e| e.eq_ignore_ascii_case(EXTENSION)).unwrap_or(false)
}

/// A file-name-safe id: lower case letters, digits, `-`, `_` and `.`; anything else becomes `-`.
pub fn slug(s: &str) -> String {
    let s: String = s.trim().chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() {
        "palette".into()
    } else {
        s
    }
}

/// Result of scanning the palette folder.
#[derive(Debug, Default)]
pub struct PaletteCatalog {
    pub palettes: Vec<PaletteFile>,
    pub errors: Vec<(PathBuf, PaletteError)>,
    /// Where custom palettes are written.
    pub dir: PathBuf,
}

impl PaletteCatalog {
    pub fn get(&self, id: &str) -> Option<&PaletteFile> {
        self.palettes.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut PaletteFile> {
        self.palettes.iter_mut().find(|p| p.id == id)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.{EXTENSION}"))
    }

    /// `base` if no palette (or file) has that id yet, otherwise `base-2`, `base-3`, ...
    pub fn unique_id(&self, base: &str) -> String {
        let taken = |id: &str| id == SYSTEM || self.get(id).is_some() || self.path_for(id).exists();
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).expect("a free id")
    }

    /// Writes `p` as a new custom palette under a free id derived from `id`, never replacing a
    /// file. Returns the id it got.
    fn add(&mut self, mut p: PaletteFile, id: &str) -> Result<String, PaletteError> {
        use std::io::Write;
        p.id = self.unique_id(&slug(id));
        let path = self.path_for(&p.id);
        std::fs::create_dir_all(&self.dir).map_err(|e| PaletteError::Io(format!("{}: {e}", self.dir.display())))?;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| PaletteError::Io(format!("{}: {e}", path.display())))?;
        f.write_all(p.to_json().as_bytes()).map_err(|e| {
            let _ = std::fs::remove_file(&path);
            PaletteError::Io(format!("{}: {e}", path.display()))
        })?;
        p.source = PaletteSource::File(path);
        let id = p.id.clone();
        self.palettes.push(p);
        Ok(id)
    }

    /// Copies palette `id` to a new custom palette named "<name> copy". Returns the new id.
    pub fn duplicate(&mut self, id: &str) -> Result<String, PaletteError> {
        let src = self.get(id).ok_or_else(|| PaletteError::Io(format!("no palette {id:?}")))?;
        let name = format!("{} copy", src.name);
        let p = PaletteFile { id: String::new(), name: name.clone(), palette: src.palette, source: PaletteSource::BuiltIn };
        self.add(p, &name)
    }

    /// Imports a `.tmoptheme` file. An id that is already taken gets a suffix (`nord-2`);
    /// nothing is overwritten. Returns the id it got.
    pub fn import(&mut self, path: &Path) -> Result<String, PaletteError> {
        let p = PaletteFile::load(path)?;
        let id = p.id.clone();
        self.add(p, &id)
    }

    /// Renames a custom palette (its id and file name stay).
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), PaletteError> {
        let p = self.get_mut(id).filter(|p| !p.is_built_in()).ok_or_else(|| PaletteError::Io(format!("{id:?} is not a custom palette")))?;
        let old = std::mem::replace(&mut p.name, name.trim().to_string());
        p.save().map_err(|e| {
            p.name = old;
            PaletteError::Io(e.to_string())
        })
    }

    /// Deletes a custom palette and its file.
    pub fn delete(&mut self, id: &str) -> Result<(), PaletteError> {
        let Some(PaletteFile { source: PaletteSource::File(path), .. }) = self.get(id) else {
            return Err(PaletteError::Io(format!("{id:?} is not a custom palette")));
        };
        std::fs::remove_file(path).map_err(|e| PaletteError::Io(format!("{}: {e}", path.display())))?;
        self.palettes.retain(|p| p.id != id);
        Ok(())
    }
}

/// Writes palette `p` to `path` (export); unlike [`PaletteCatalog::import`] this replaces `path`.
pub fn export(p: &PaletteFile, path: &Path) -> std::io::Result<()> {
    write_atomic(path, &p.to_json())
}

/// Scans `dir` for `*.tmoptheme` files. The built-in palettes always come first and can't be
/// replaced: a file whose id is already taken is reported as an error.
pub fn scan(dir: &Path) -> PaletteCatalog {
    let mut cat = PaletteCatalog { palettes: built_in(), errors: Vec::new(), dir: dir.to_path_buf() };
    let Ok(rd) = std::fs::read_dir(dir) else { return cat };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_palette_file(p)).collect();
    entries.sort();
    for p in entries {
        match PaletteFile::load(&p) {
            Ok(pal) if pal.id == SYSTEM || cat.get(&pal.id).is_some() => cat.errors.push((p, PaletteError::Parse(format!("palette id {:?} is already taken", pal.id)))),
            Ok(pal) => cat.palettes.push(pal),
            Err(e) => cat.errors.push((p, e)),
        }
    }
    cat
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmop-palettes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn json_round_trips_all_roles() {
        let mut p = PaletteFile { id: "x".into(), name: "X".into(), palette: theme::NORD, source: PaletteSource::BuiltIn };
        p.palette.grid = Color32::from_rgb(1, 2, 3);
        let text = p.to_json();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["colors"].as_object().unwrap().len(), 22);
        assert_eq!(v["colors"]["grid"], "#010203");
        assert_eq!(v["dark"], true);
        assert_eq!(PaletteFile::from_json(&text).unwrap(), p);
    }

    #[test]
    fn rejects_bad_files() {
        let good = PaletteFile { id: "x".into(), name: "X".into(), palette: theme::DARK, source: PaletteSource::BuiltIn }.to_json();
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["colors"].as_object_mut().unwrap().remove("ok");
        assert!(PaletteFile::from_json(&v.to_string()).unwrap_err().to_string().contains("missing colour role \"ok\""));
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["colors"]["sparkle"] = "#FFFFFF".into();
        assert!(PaletteFile::from_json(&v.to_string()).is_err());
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["colors"]["ok"] = "green".into();
        assert!(PaletteFile::from_json(&v.to_string()).is_err());
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["format"] = 2.into();
        assert_eq!(PaletteFile::from_json(&v.to_string()), Err(PaletteError::UnsupportedFormat(2)));
        let mut v: Value = serde_json::from_str(&good).unwrap();
        v["extra"] = 1.into();
        assert!(PaletteFile::from_json(&v.to_string()).is_err());
        assert!(PaletteFile::from_json("{").is_err());
    }

    /// Every built-in palette keeps text readable (4.5:1) on every surface it is drawn on.
    #[test]
    fn built_in_palettes_are_readable() {
        let all = built_in();
        assert_eq!(all.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["dark", "light", "high-contrast", "nord", "solarized-light"]);
        for p in &all {
            assert!(p.palette.contrast_warnings().is_empty(), "{}: {:?}", p.id, p.palette.contrast_warnings());
            // Hint text is not held to 4.5:1, but must stay legible.
            for bg in [p.palette.body, p.palette.card, p.palette.panel] {
                let r = theme::contrast(p.palette.text_faint, bg);
                assert!(r >= 2.5, "{}: textFaint is {r:.2}:1", p.id);
            }
        }
    }

    #[test]
    fn derived_roles_follow_the_core_roles() {
        for p in built_in() {
            let d = p.palette.derived();
            for r in theme::CORE_ROLES {
                assert_eq!(d.role(r.key), p.palette.role(r.key), "{}: core role {} changed", p.id, r.key);
            }
            assert_eq!(d.dark, p.palette.dark);
        }
        // Dark's derived roles stay close to the hand-tuned ones.
        let d = theme::DARK.derived();
        let close = |a: Color32, b: Color32| a.to_array().iter().zip(b.to_array()).all(|(x, y)| (*x as i32 - y as i32).abs() <= 16);
        for (a, b) in [(d.chrome, theme::DARK.chrome), (d.card_stroke, theme::DARK.card_stroke), (d.text_dim, theme::DARK.text_dim), (d.control, theme::DARK.control)] {
            assert!(close(a, b), "{a:?} vs {b:?}");
        }
        assert!(d.contrast_warnings().is_empty(), "{:?}", d.contrast_warnings());
        assert!(theme::LIGHT.derived().contrast_warnings().is_empty());
    }

    #[test]
    fn contrast_warnings_name_the_role() {
        let mut p = theme::DARK;
        p.text_dim = Color32::from_rgb(0x40, 0x40, 0x40);
        let w = p.contrast_warnings();
        assert!(w.iter().any(|(k, _)| *k == "card"));
        assert!(w.iter().any(|(k, c)| *k == "textDim" && *c < 4.5));
        assert!(p.worst_contrast("grid").is_none());
    }

    #[test]
    fn scan_duplicate_rename_delete() {
        let dir = temp_dir("scan");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Broken.tmoptheme"), "{").unwrap();
        let clash = PaletteFile { id: "nord".into(), name: "Not Nord".into(), palette: theme::DARK, source: PaletteSource::BuiltIn };
        std::fs::write(dir.join("clash.tmoptheme"), clash.to_json()).unwrap();
        let mut cat = scan(&dir);
        assert_eq!(cat.palettes.len(), 5);
        assert_eq!(cat.errors.len(), 2);
        assert_eq!(cat.get("nord").unwrap().name, "Nord", "built-ins can't be replaced");

        let id = cat.duplicate("nord").unwrap();
        assert_eq!(id, "nord-copy");
        assert_eq!(cat.duplicate("nord").unwrap(), "nord-copy-2");
        let copy = cat.get(&id).unwrap();
        assert_eq!((copy.name.as_str(), copy.palette), ("Nord copy", theme::NORD));
        assert!(dir.join("nord-copy.tmoptheme").is_file());

        cat.rename(&id, "Frost").unwrap();
        assert!(cat.rename("nord", "Nope").is_err(), "built-ins are read-only");
        let mut edited = cat.get(&id).unwrap().clone();
        edited.palette.accent = Color32::from_rgb(1, 2, 3);
        edited.save().unwrap();

        let again = scan(&dir);
        let p = again.get(&id).unwrap();
        assert_eq!((p.name.as_str(), p.palette.accent), ("Frost", Color32::from_rgb(1, 2, 3)));
        assert!(built_in()[0].save().is_err());

        cat.delete(&id).unwrap();
        assert!(cat.get(&id).is_none() && !dir.join("nord-copy.tmoptheme").exists());
        assert!(cat.delete("dark").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_never_overwrites() {
        let dir = temp_dir("import");
        let src = temp_dir("import-src");
        std::fs::create_dir_all(&src).unwrap();
        let mut cat = scan(&dir);
        let file = src.join("nord.tmoptheme");
        let mut nord = built_in().remove(3);
        nord.palette.accent = Color32::from_rgb(9, 9, 9);
        export(&nord, &file).unwrap();
        assert_eq!(cat.import(&file).unwrap(), "nord-2");
        assert_eq!(cat.import(&file).unwrap(), "nord-3");
        assert_eq!(cat.get("nord").unwrap().palette, theme::NORD);
        assert_eq!(cat.get("nord-2").unwrap().palette.accent, Color32::from_rgb(9, 9, 9));
        // A file with an unsafe id is stored under a safe one.
        let odd = PaletteFile { id: "My Theme/..\\x".into(), name: "Odd".into(), palette: theme::LIGHT, source: PaletteSource::BuiltIn };
        export(&odd, &src.join("odd.tmoptheme")).unwrap();
        assert_eq!(cat.import(&src.join("odd.tmoptheme")).unwrap(), "my-theme-..-x");
        std::fs::write(src.join("bad.tmoptheme"), "{\"format\":1}").unwrap();
        assert!(matches!(cat.import(&src.join("bad.tmoptheme")), Err(PaletteError::Parse(_))));
        assert_eq!(scan(&dir).palettes.len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&src);
    }
}
