//! `.tmogskin` model and loader.
//!
//! Format notes:
//! * A skin is either a JSON file named `<Name>.tmogskin`, or a directory named
//!   `<Name>.tmogskin` that contains `skin.json` plus assets referenced by it
//!   (for example `header.logo.file`).
//! * Colours are `#RRGGBB` or `#RRGGBBAA` strings.
//! * A surface is either a colour string or an object `{ "gradient": [c0, c1, ...] }`.
//! * Every field except `format`/`id`/`name` is optional; unknown keys are retained
//!   in `extra` so that skins written for newer TMOG versions still load.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Highest skin `format` version this loader understands.
pub const SUPPORTED_FORMAT: u32 = 1;

// ---------------------------------------------------------------------------
// Colours and fills
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parses `#RGB`, `#RRGGBB` or `#RRGGBBAA` (the `#` is optional).
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(h.get(i..i + 1)?, 16).ok().map(|v| v * 17);
                Some(Self::rgb(n(0)?, n(1)?, n(2)?))
            }
            6 => Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Some(Self::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color::rgba(m(self.r, other.r), m(self.g, other.g), m(self.b, other.b), m(self.a, other.a))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid colour {s:?}")))
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// A surface paint: a flat colour or a (vertical) gradient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fill {
    Solid(Color),
    Gradient { gradient: Vec<Color> },
    /// Any other shape (images, future paint kinds). Kept verbatim.
    Other(Value),
}

impl Fill {
    /// Colour at the top of the surface.
    pub fn top(&self) -> Option<Color> {
        match self {
            Fill::Solid(c) => Some(*c),
            Fill::Gradient { gradient } => gradient.first().copied(),
            Fill::Other(_) => None,
        }
    }
    /// Colour at the bottom of the surface.
    pub fn bottom(&self) -> Option<Color> {
        match self {
            Fill::Solid(c) => Some(*c),
            Fill::Gradient { gradient } => gradient.last().copied(),
            Fill::Other(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Skin sections
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkinMetrics {
    pub corner_radius: Option<f32>,
    pub border_width: Option<f32>,
    pub padding: Option<f32>,
    pub header_bottom_padding: Option<f32>,
    pub header_gap: Option<f32>,
    pub section_spacing: Option<f32>,
    pub tile_corner_radius: Option<f32>,
    pub tile_stroke_width: Option<f32>,
    pub badge_corner_radius: Option<f32>,
    pub frame_inset: Option<f32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Logo {
    pub file: String,
    pub width: Option<f32>,
    pub height: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub title: Option<String>,
    pub hide_title: Option<bool>,
    pub logo: Option<Logo>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Surfaces {
    pub window: Option<Fill>,
    pub window_stroke: Option<Fill>,
    pub header: Option<Fill>,
    pub header_rule: Option<Fill>,
    pub tile: Option<Fill>,
    pub tile_stroke: Option<Fill>,
    pub tile_gloss: Option<Fill>,
    pub badge_stroke: Option<Fill>,
    pub window_image: Option<Value>,
    pub header_image: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontWeight {
    Ultralight,
    Thin,
    Light,
    #[default]
    Regular,
    Medium,
    Semibold,
    Bold,
    Heavy,
    Black,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontDesign {
    #[default]
    Default,
    Monospaced,
    Rounded,
    Serif,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSpec {
    pub size: Option<f32>,
    pub weight: Option<FontWeight>,
    pub design: Option<FontDesign>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Typography {
    pub title: Option<FontSpec>,
    pub heading: Option<FontSpec>,
    pub label: Option<FontSpec>,
    pub value: Option<FontSpec>,
    pub row: Option<FontSpec>,
    pub row_value: Option<FontSpec>,
    pub aux_line: Option<FontSpec>,
    pub title_tracking: Option<f32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ink {
    pub title: Option<Color>,
    pub primary: Option<Color>,
    pub secondary: Option<Color>,
    pub control: Option<Color>,
    pub badge: Option<Color>,
    pub grip: Option<Color>,
    pub rule: Option<Color>,
    pub glow: Option<Color>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeterStyle {
    /// Label + value + horizontal bar per reading, laid out in `columns`.
    Rows,
    /// Segmented horizontal LED bars.
    Bars,
    /// Round dial with needle.
    Gauge,
    /// Scrolling history graph.
    Graph,
    /// Vertical segmented LED meters (classic Windows Task Manager look).
    Vertical,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Insets {
    pub top: Option<f32>,
    pub bottom: Option<f32>,
    pub leading: Option<f32>,
    pub trailing: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSpec {
    pub shows_grid: Option<bool>,
    pub height: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub accent: Option<Color>,
    pub lit: Option<Color>,
    pub unlit: Option<Color>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meters {
    pub style: Option<MeterStyle>,
    pub columns: Option<u32>,
    pub auxiliary: Option<bool>,
    pub positional_palette: Option<bool>,
    pub flat: Option<bool>,
    pub split: Option<u32>,
    pub segments: Option<u32>,
    pub inset: Option<Insets>,
    pub ground: Option<Color>,
    pub graph: Option<GraphSpec>,
    /// Gauge geometry/paint (`face`, `bezel`, `needle`, `startAngle`, `sweepAngle`, ...).
    pub gauge: Option<Value>,
    #[serde(default)]
    pub channels: BTreeMap<String, Channel>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// One of the readings a popout can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Quadrant {
    Cpu,
    Gpu,
    Disk,
    Network,
    Memory,
    Thermal,
    Power,
}

impl Quadrant {
    pub const ALL: [Quadrant; 7] = [
        Quadrant::Cpu,
        Quadrant::Gpu,
        Quadrant::Disk,
        Quadrant::Network,
        Quadrant::Memory,
        Quadrant::Thermal,
        Quadrant::Power,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "cpu" => Self::Cpu,
            "gpu" => Self::Gpu,
            "disk" => Self::Disk,
            "network" => Self::Network,
            "memory" => Self::Memory,
            "thermal" | "thermals" => Self::Thermal,
            "power" => Self::Power,
            _ => return None,
        })
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Memory => "memory",
            Self::Thermal => "thermal",
            Self::Power => "power",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Disk => "DISK",
            Self::Network => "NET",
            Self::Memory => "MEM",
            Self::Thermal => "TEMP",
            Self::Power => "POWER",
        }
    }
}

/// Where a skin came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SkinSource {
    #[default]
    BuiltIn,
    File(PathBuf),
    /// Directory skin; the path is the `.tmogskin` directory (assets are relative to it).
    Directory(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skin {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    /// SF Symbol-style icon name (a macOS heritage of the format); informational only.
    pub symbol: Option<String>,
    pub quadrants: Option<Vec<String>>,
    pub metrics: Option<SkinMetrics>,
    pub header: Option<Header>,
    pub surfaces: Option<Surfaces>,
    pub typography: Option<Typography>,
    pub ink: Option<Ink>,
    pub meters: Option<Meters>,
    /// Window shape (`rectangle`, `circle`, `ellipse`/`oval`, `capsule`/`pill`, `polygon`, ...).
    pub shape: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    #[serde(skip)]
    pub source: SkinSource,
}

/// Quadrants used when a skin does not list any.
/// Design choice: the four channels the shipped quadrant-less skins (Winamp,
/// Windows Media Player) give `lit`/`unlit` colours to.
pub const DEFAULT_QUADRANTS: [Quadrant; 4] =
    [Quadrant::Cpu, Quadrant::Memory, Quadrant::Gpu, Quadrant::Thermal];

impl Skin {
    /// The built-in fallback ("Classic: the popout drawn in the application's own colours").
    pub fn classic() -> Self {
        Skin {
            format: 1,
            id: "classic".into(),
            name: "Classic".into(),
            author: Some("TMOP".into()),
            description: Some("The popout drawn in the application's own colours".into()),
            symbol: Some("rectangle.on.rectangle".into()),
            quadrants: Some(
                ["cpu", "gpu", "disk", "network", "memory", "thermal"].iter().map(|s| s.to_string()).collect(),
            ),
            metrics: Some(SkinMetrics { corner_radius: Some(10.0), tile_corner_radius: Some(0.0), ..Default::default() }),
            header: Some(Header { title: Some("TMOP".into()), ..Default::default() }),
            surfaces: None,
            // Sizes are points (1 pt = 4/3 px): 9 px labels, 11 px values and rows.
            typography: Some(Typography {
                title: Some(FontSpec { size: Some(8.25), weight: Some(FontWeight::Medium), design: None }),
                heading: Some(FontSpec { size: Some(6.75), weight: Some(FontWeight::Regular), design: None }),
                label: Some(FontSpec { size: Some(6.75), weight: Some(FontWeight::Regular), design: None }),
                value: Some(FontSpec { size: Some(8.25), weight: Some(FontWeight::Regular), design: Some(FontDesign::Monospaced) }),
                row: Some(FontSpec { size: Some(8.25), weight: Some(FontWeight::Regular), design: None }),
                row_value: Some(FontSpec { size: Some(8.25), weight: Some(FontWeight::Regular), design: Some(FontDesign::Monospaced) }),
                // 0.14 em of the 11 px title.
                title_tracking: Some(1.16),
                ..Default::default()
            }),
            ink: None,
            meters: Some(Meters { style: Some(MeterStyle::Graph), columns: Some(3), graph: Some(GraphSpec { shows_grid: Some(false), height: Some(20.0) }), ..Default::default() }),
            shape: None,
            extra: BTreeMap::new(),
            source: SkinSource::BuiltIn,
        }
    }

    pub fn from_json(text: &str) -> Result<Self, SkinError> {
        let skin: Skin = serde_json::from_str(text).map_err(|e| SkinError::Parse(e.to_string()))?;
        if skin.format == 0 || skin.format > SUPPORTED_FORMAT {
            return Err(SkinError::UnsupportedFormat(skin.format));
        }
        if skin.id.trim().is_empty() {
            return Err(SkinError::Parse("skin id is empty".into()));
        }
        Ok(skin)
    }

    /// Loads a skin from a `.tmogskin` file or a `.tmogskin` directory.
    pub fn load(path: &Path) -> Result<Self, SkinError> {
        if path.is_dir() {
            let json = path.join("skin.json");
            let text = std::fs::read_to_string(&json)
                .map_err(|e| SkinError::Io(format!("{} has no readable skin.json: {e}", path.display())))?;
            let mut s = Self::from_json(&text)?;
            s.source = SkinSource::Directory(path.to_path_buf());
            Ok(s)
        } else {
            let text = std::fs::read_to_string(path).map_err(|e| SkinError::Io(format!("{}: {e}", path.display())))?;
            let mut s = Self::from_json(&text)?;
            s.source = SkinSource::File(path.to_path_buf());
            Ok(s)
        }
    }

    /// Readings to display, in skin order; unknown names are skipped.
    pub fn resolved_quadrants(&self) -> Vec<Quadrant> {
        match &self.quadrants {
            Some(list) => {
                let mut out = Vec::new();
                for q in list.iter().filter_map(|s| Quadrant::parse(s)) {
                    if !out.contains(&q) {
                        out.push(q);
                    }
                }
                if out.is_empty() {
                    DEFAULT_QUADRANTS.to_vec()
                } else {
                    out
                }
            }
            None => DEFAULT_QUADRANTS.to_vec(),
        }
    }

    /// Effective meter style. Design choice when absent: segmented `bars` if the
    /// skin gives lit/unlit LED colours, otherwise `rows`.
    pub fn meter_style(&self) -> MeterStyle {
        let m = self.meters.as_ref();
        if let Some(style) = m.and_then(|m| m.style) {
            return style;
        }
        let has_leds = m.map(|m| m.channels.values().any(|c| c.lit.is_some())).unwrap_or(false);
        if has_leds {
            MeterStyle::Bars
        } else {
            MeterStyle::Rows
        }
    }

    pub fn channel(&self, q: Quadrant) -> Channel {
        self.meters.as_ref().and_then(|m| m.channels.get(q.key()).cloned()).unwrap_or_default()
    }

    pub fn title(&self) -> String {
        self.header.as_ref().and_then(|h| h.title.clone()).unwrap_or_else(|| self.name.clone())
    }

    /// Absolute path to the header logo, for directory skins.
    pub fn logo_path(&self) -> Option<PathBuf> {
        let logo = self.header.as_ref()?.logo.as_ref()?;
        match &self.source {
            SkinSource::Directory(dir) => {
                let p = dir.join(&logo.file);
                // Refuse to escape the skin directory.
                if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                    None
                } else {
                    Some(p)
                }
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkinError {
    Io(String),
    Parse(String),
    UnsupportedFormat(u32),
}

impl fmt::Display for SkinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkinError::Io(e) => write!(f, "{e}"),
            SkinError::Parse(e) => write!(f, "invalid skin: {e}"),
            SkinError::UnsupportedFormat(v) => write!(f, "unsupported skin format {v}"),
        }
    }
}

impl std::error::Error for SkinError {}

/// Result of scanning skin folders.
#[derive(Debug, Default)]
pub struct SkinCatalog {
    pub skins: Vec<Skin>,
    pub errors: Vec<(PathBuf, SkinError)>,
}

impl SkinCatalog {
    pub fn get(&self, id: &str) -> Option<&Skin> {
        self.skins.iter().find(|s| s.id == id)
    }
}

/// Default directories searched for skins, highest priority first (see [`crate::platform::skin_dirs`]).
pub fn default_skin_dirs() -> Vec<PathBuf> {
    crate::platform::skin_dirs()
}

fn is_skin_entry(path: &Path) -> bool {
    path.extension().map(|e| e.eq_ignore_ascii_case("tmogskin")).unwrap_or(false)
}

/// Skins compiled into the binary, after Classic.
const BUILT_IN: [&str; 5] = [
    include_str!("skins/graph.tmogskin"),
    include_str!("skins/led.tmogskin"),
    include_str!("skins/gauge.tmogskin"),
    include_str!("skins/paper.tmogskin"),
    include_str!("skins/neon.tmogskin"),
];

/// Classic followed by the embedded skins.
pub fn built_in() -> Vec<Skin> {
    let mut skins = vec![Skin::classic()];
    skins.extend(BUILT_IN.iter().map(|text| Skin::from_json(text).expect("built-in skin parses")));
    skins
}

/// WCAG relative luminance.
pub fn luminance(c: Color) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio of `fg` over the opaque `bg` (`fg` alpha is composited first).
pub fn contrast(fg: Color, bg: Color) -> f32 {
    let fg = bg.lerp(Color { a: 255, ..fg }, fg.a as f32 / 255.0);
    let (a, b) = (luminance(fg), luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Scans `dirs` (highest priority first). The built-in skins are always present;
/// a skin id seen earlier wins over the same id seen later, and any installed skin
/// replaces a built-in one with its id.
pub fn scan(dirs: &[PathBuf]) -> SkinCatalog {
    let mut cat = SkinCatalog { skins: built_in(), errors: Vec::new() };
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_skin_entry(p)).collect();
        entries.sort();
        for p in entries {
            match Skin::load(&p) {
                Ok(skin) => {
                    if cat.get(&skin.id).is_none() || cat.get(&skin.id).map(|s| s.source == SkinSource::BuiltIn).unwrap_or(false) {
                        cat.skins.retain(|s| s.id != skin.id);
                        cat.skins.push(skin);
                    }
                }
                Err(e) => cat.errors.push((p, e)),
            }
        }
    }
    cat
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE: &str = r#"{
      "format": 1, "id": "com.tmog.console", "name": "Default", "author": "TMOG",
      "description": "d", "symbol": "rectangle.split.2x1",
      "quadrants": ["cpu", "gpu", "disk", "network", "memory", "thermal"],
      "meters": { "style": "rows", "columns": 2, "auxiliary": false, "positionalPalette": false },
      "typography": { "label": { "size": 10, "weight": "semibold" } }
    }"#;

    #[test]
    fn colour_parsing() {
        assert_eq!(Color::parse("#43505C99"), Some(Color::rgba(0x43, 0x50, 0x5C, 0x99)));
        assert_eq!(Color::parse("#FFF"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(Color::parse("#000080"), Some(Color::rgb(0, 0, 0x80)));
        assert_eq!(Color::parse("nope"), None);
        assert_eq!(Color::rgba(1, 2, 3, 4).to_string(), "#01020304");
    }

    #[test]
    fn parses_console_inline() {
        let s = Skin::from_json(CONSOLE).unwrap();
        assert_eq!(s.id, "com.tmog.console");
        assert_eq!(s.meter_style(), MeterStyle::Rows);
        assert_eq!(s.resolved_quadrants().len(), 6);
        let t = s.typography.unwrap();
        assert_eq!(t.label.unwrap().weight, Some(FontWeight::Semibold));
    }

    #[test]
    fn fills_and_unknown_keys() {
        let s = Skin::from_json(
            r##"{"format":1,"id":"x","name":"X","future":{"a":1},
                "surfaces":{"window":{"gradient":["#101418","#05070A"]},"tile":"#070A0DE6"},
                "meters":{"channels":{"cpu":{"accent":"#1AFF1C","lit":"#1AFF1C","unlit":"#3A363D8C"}}}}"##,
        )
        .unwrap();
        let surf = s.surfaces.as_ref().unwrap();
        assert_eq!(surf.window.as_ref().unwrap().bottom(), Color::parse("#05070A"));
        assert_eq!(surf.tile.as_ref().unwrap().top(), Color::parse("#070A0DE6"));
        assert!(s.extra.contains_key("future"));
        // No style given but LED colours present => bars; no quadrants => defaults.
        assert_eq!(s.meter_style(), MeterStyle::Bars);
        assert_eq!(s.resolved_quadrants(), DEFAULT_QUADRANTS.to_vec());
    }

    #[test]
    fn rejects_bad_skins() {
        assert!(matches!(Skin::from_json(r#"{"format":2,"id":"a","name":"A"}"#), Err(SkinError::UnsupportedFormat(2))));
        assert!(Skin::from_json(r#"{"format":1,"id":"a"}"#).is_err());
        assert!(Skin::from_json(r##"{"format":1,"id":"a","name":"A","ink":{"title":"#GG0000"}}"##).is_err());
    }

    #[test]
    fn directory_skin_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tmop-test-{}", std::process::id()));
        let skin_dir = dir.join("Test.tmogskin");
        std::fs::create_dir_all(&skin_dir).unwrap();
        std::fs::write(
            skin_dir.join("skin.json"),
            r#"{"format":1,"id":"t","name":"T","header":{"title":"Hi","logo":{"file":"l.png","width":16,"height":16}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("Broken.tmogskin"), "{").unwrap();
        let cat = scan(std::slice::from_ref(&dir));
        let t = cat.get("t").unwrap();
        assert_eq!(t.title(), "Hi");
        assert_eq!(t.logo_path().unwrap(), skin_dir.join("l.png"));
        assert_eq!(cat.errors.len(), 1);
        assert!(cat.get("classic").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every built-in skin meets WCAG AA: 4.5:1 for text, 3:1 for meters and graph lines.
    #[test]
    fn built_in_skins_are_readable() {
        let skins = built_in();
        assert_eq!(skins.len(), 6);
        for s in skins.iter().filter(|s| s.id != "classic") {
            let id = &s.id;
            let surf = s.surfaces.as_ref().unwrap_or_else(|| panic!("{id}: no surfaces"));
            let ink = s.ink.as_ref().unwrap_or_else(|| panic!("{id}: no ink"));
            let window = surf.window.as_ref().and_then(Fill::bottom).unwrap();
            let over = |f: &Option<Fill>| -> Vec<Color> {
                let f = f.as_ref().unwrap_or_else(|| panic!("{id}: missing surface"));
                [f.top().unwrap(), f.bottom().unwrap()].into_iter().map(|c| window.lerp(Color { a: 255, ..c }, c.a as f32 / 255.0)).collect()
            };
            let check = |what: &str, fg: Color, bgs: &[Color], min: f32| {
                for bg in bgs {
                    let r = contrast(fg, *bg);
                    assert!(r >= min, "{id}: {what} {fg} on {bg} is {r:.2}:1, needs {min}:1");
                }
            };
            let header = over(&surf.header);
            let tile = over(&surf.tile);
            check("title", ink.title.unwrap(), &header, 4.5);
            check("control", ink.control.unwrap(), &header, 3.0);
            check("primary", ink.primary.unwrap(), &tile, 4.5);
            check("secondary", ink.secondary.unwrap(), &tile, 4.5);
            let meters = s.meters.as_ref().unwrap();
            let face = meters.gauge.as_ref().and_then(|g| g.get("face")).and_then(|v| v.as_str()).and_then(Color::parse);
            for q in s.resolved_quadrants() {
                let ch = s.channel(q);
                let accent = ch.accent.unwrap_or_else(|| panic!("{id}: no accent for {}", q.key()));
                let lit = ch.lit.unwrap_or(accent);
                let what = format!("{} meter", q.key());
                match s.meter_style() {
                    // Labels and values are drawn in the accent.
                    MeterStyle::Rows | MeterStyle::Graph => check(&what, accent, &tile, 4.5),
                    MeterStyle::Bars => check(&what, lit, &tile, 3.0),
                    MeterStyle::Vertical => check(&what, lit, &[meters.ground.unwrap()], 3.0),
                    MeterStyle::Gauge => check(&what, accent, &[face.unwrap()], 3.0),
                }
            }
        }
    }

    #[test]
    fn installed_skin_replaces_built_in() {
        let dir = std::env::temp_dir().join(format!("tmop-test-override-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Mine.tmogskin"), r#"{"format":1,"id":"com.tmop.graph","name":"Mine"}"#).unwrap();
        let cat = scan(std::slice::from_ref(&dir));
        assert_eq!(cat.get("com.tmop.graph").unwrap().name, "Mine");
        assert_eq!(cat.skins.iter().filter(|s| s.id == "com.tmop.graph").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contrast_ratio() {
        assert!((contrast(Color::rgb(0, 0, 0), Color::rgb(255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast(Color::rgb(9, 9, 9), Color::rgb(9, 9, 9)) - 1.0).abs() < 0.001);
        // Fully transparent ink disappears into the background.
        assert!((contrast(Color::rgba(255, 255, 255, 0), Color::rgb(0, 0, 0)) - 1.0).abs() < 0.001);
    }

    /// Parses the skins of an installed TMOG, when present.
    #[test]
    fn parses_installed_tmog_skins() {
        let dir = PathBuf::from(r"C:\Program Files\Task Manager TMOG\Skins");
        if !dir.is_dir() {
            eprintln!("installed TMOG skins not found; skipping");
            return;
        }
        let cat = scan(&[dir]);
        assert!(cat.errors.is_empty(), "skin errors: {:?}", cat.errors);
        let ids: Vec<&str> = cat.skins.iter().map(|s| s.id.as_str()).collect();
        for expected in ["com.tmog.console", "com.tmog.traces", "winamp", "mediaPlayer", "com.tmog.wintaskman"] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
        let wtm = cat.get("com.tmog.wintaskman").unwrap();
        assert!(matches!(wtm.source, SkinSource::Directory(_)));
        assert_eq!(wtm.meter_style(), MeterStyle::Vertical);
        let m = wtm.meters.as_ref().unwrap();
        assert_eq!((m.split, m.segments, m.flat), (Some(2), Some(8), Some(true)));
        assert_eq!(wtm.resolved_quadrants(), vec![Quadrant::Cpu, Quadrant::Memory, Quadrant::Gpu, Quadrant::Thermal]);
        assert!(wtm.logo_path().unwrap().ends_with("taskman.png"));
        let traces = cat.get("com.tmog.traces").unwrap();
        assert_eq!(traces.meter_style(), MeterStyle::Graph);
        assert_eq!(traces.meters.as_ref().unwrap().graph.as_ref().unwrap().height, Some(44.0));
        let winamp = cat.get("winamp").unwrap();
        assert_eq!(winamp.meters.as_ref().unwrap().positional_palette, Some(true));
        assert_eq!(
            winamp.typography.as_ref().unwrap().title.as_ref().unwrap().design,
            Some(FontDesign::Monospaced)
        );
        let wmp = cat.get("mediaPlayer").unwrap();
        assert!(matches!(wmp.surfaces.as_ref().unwrap().tile_gloss, Some(Fill::Gradient { .. })));
    }
}
