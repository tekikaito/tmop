//! Display fonts (`appearance.displayFont`): the design default (Inter Light), the embedded
//! Michroma or an installed family.
//!
//! * installed families are listed with GDI `EnumFontFamiliesExW`: regular-weight, upright
//!   TrueType and OpenType families only; vertical (`@`) variants, bitmap, vector and Type 1
//!   fonts are left out (see [`Face::listed`]);
//! * a family's bytes are read with `GetFontData`; a face in a TrueType collection is read as
//!   the whole collection (the `ttcf` table) plus its index in it (see [`collection_index`]);
//! * a saved family that isn't installed (or can't be parsed) falls back to the default; the
//!   saved value is kept (see [`resolve`]).

/// `appearance.displayFont` value of the default display font, and the settings default. It
/// names Michroma for compatibility with settings saved before the redesign; it now means
/// "the design default", Inter Light (see [`is_built_in`]).
pub const BUILT_IN: &str = "Michroma";
/// The default's entry in the picker.
pub const BUILT_IN_LABEL: &str = "Inter (default)";
/// `appearance.displayFont` value of the embedded Michroma, the previous look.
pub const CLASSIC: &str = "Michroma (classic)";
/// Michroma's entry in the picker.
pub const CLASSIC_LABEL: &str = "Michroma (built-in)";

// GDI's values (wingdi.h), kept here so the filter is the same on every platform.
const RASTER_FONTTYPE: u32 = 0x1;
const TRUETYPE_FONTTYPE: u32 = 0x4;
const NTM_PS_OPENTYPE: u32 = 0x2_0000;
const NTM_TT_OPENTYPE: u32 = 0x4_0000;
const NTM_TYPE1: u32 = 0x10_0000;
const FW_NORMAL: i32 = 400;

/// Whether `name` means the default display font (an empty value does too).
pub fn is_built_in(name: &str) -> bool {
    let name = name.trim();
    name.is_empty() || name.eq_ignore_ascii_case(BUILT_IN)
}

/// Whether `name` means the embedded Michroma.
pub fn is_classic(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(CLASSIC)
}

/// The picker's text for `saved`; `missing` when it fell back to the default.
pub fn picker_label(saved: &str, missing: bool) -> String {
    if is_built_in(saved) {
        BUILT_IN_LABEL.into()
    } else if is_classic(saved) {
        CLASSIC_LABEL.into()
    } else if missing {
        format!("{saved} (not installed)")
    } else {
        saved.into()
    }
}

/// One face reported by `EnumFontFamiliesExW`.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// `lfFaceName`, the family name.
    pub name: String,
    /// `FontType` (`RASTER_FONTTYPE`, `TRUETYPE_FONTTYPE`, ...).
    pub font_type: u32,
    /// `ntmFlags` (`NTM_PS_OPENTYPE`, ...); 0 for bitmap fonts.
    pub ntm_flags: u32,
    pub weight: i32,
    pub italic: bool,
}

impl Face {
    /// Whether the picker lists this face's family: a regular-weight, upright TrueType or OpenType
    /// face that isn't a vertical (`@`) variant.
    pub fn listed(&self) -> bool {
        let outline = self.font_type & TRUETYPE_FONTTYPE != 0 || self.ntm_flags & (NTM_PS_OPENTYPE | NTM_TT_OPENTYPE) != 0;
        !self.name.is_empty()
            && !self.name.starts_with('@')
            && self.font_type & RASTER_FONTTYPE == 0
            && self.ntm_flags & NTM_TYPE1 == 0
            && outline
            && self.weight == FW_NORMAL
            && !self.italic
    }
}

/// The picker's families from enumerated faces: [`Face::listed`] ones, each name once (GDI
/// reports a family once per character set), sorted, without Michroma (it is listed first as
/// the built-in font).
pub fn families(faces: impl IntoIterator<Item = Face>) -> Vec<String> {
    let mut out: Vec<String> = faces.into_iter().filter(|f| f.listed() && !is_built_in(&f.name)).map(|f| f.name).collect();
    out.sort_by_key(|n| n.to_lowercase());
    out.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    out
}

/// An installed font's file: a `.ttf`/`.otf` or a whole collection, and the face's index in it.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFile {
    pub family: String,
    pub bytes: Vec<u8>,
    pub index: u32,
}

impl FontFile {
    /// Whether egui can use it (egui panics on a font it can't parse).
    pub fn parses(&self) -> bool {
        use ab_glyph::Font as _;
        ab_glyph::FontRef::try_from_slice_and_index(&self.bytes, self.index).is_ok_and(|f| f.units_per_em().is_some())
    }
}

/// The font for `appearance.displayFont`: Michroma's embedded bytes for [`CLASSIC`], an installed
/// font (read with `load`), or `None` for the default (Inter Light): when `saved` is the default,
/// or isn't installed or can't be parsed. The second value is true when `saved` names a font
/// that fell back to the default.
pub fn resolve(saved: &str, load: impl FnOnce(&str) -> Option<FontFile>) -> (Option<FontFile>, bool) {
    if is_built_in(saved) {
        return (None, false);
    }
    if is_classic(saved) {
        return (Some(FontFile { family: "Michroma".into(), bytes: crate::ui::theme::MICHROMA.to_vec(), index: 0 }), false);
    }
    match load(saved.trim()).filter(FontFile::parses) {
        Some(f) => (Some(f), false),
        None => (None, true),
    }
}

/// Index of the face whose `name` table is `name_table` in the TrueType collection `ttc`
/// (`None` if `ttc` isn't a collection or no face matches).
pub fn collection_index(ttc: &[u8], name_table: &[u8]) -> Option<u32> {
    let u32_at = |at: usize| ttc.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |at: usize| ttc.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    if ttc.get(..4)? != b"ttcf" {
        return None;
    }
    let count = u32_at(8)?;
    (0..count).find(|i| {
        // Each face: an offset to its table directory, whose 16-byte records follow a 12-byte header.
        let face = || -> Option<bool> {
            let dir = u32_at(12 + 4 * *i as usize)? as usize;
            let tables = u16_at(dir + 4)? as usize;
            for t in 0..tables {
                let rec = dir + 12 + 16 * t;
                if ttc.get(rec..rec + 4)? == b"name" {
                    let (off, len) = (u32_at(rec + 8)? as usize, u32_at(rec + 12)? as usize);
                    return Some(ttc.get(off..off.checked_add(len)?)? == name_table);
                }
            }
            Some(false)
        };
        face().unwrap_or(false)
    })
}

/// The installed families for the picker (see [`families`]). Slow (hundreds of fonts): call it
/// off the UI thread. Empty off Windows.
pub fn installed() -> Vec<String> {
    #[cfg(windows)]
    {
        families(gdi::faces())
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Reads installed family `family` (its regular face). `None` if it isn't installed: GDI would
/// substitute another font, which is detected by the face it actually selects.
pub fn load(family: &str) -> Option<FontFile> {
    #[cfg(windows)]
    {
        gdi::load(family)
    }
    #[cfg(not(windows))]
    {
        let _ = family;
        None
    }
}

#[cfg(windows)]
mod gdi {
    use super::{Face, FontFile};
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateFontIndirectW, DeleteDC, DeleteObject, EnumFontFamiliesExW, GetFontData, GetTextFaceW, SelectObject, DEFAULT_CHARSET, FW_NORMAL, GDI_ERROR, HDC, LOGFONTW,
        NEWTEXTMETRICW, OUT_OUTLINE_PRECIS, RASTER_FONTTYPE, TEXTMETRICW,
    };

    /// `GetFontData` table that returns a whole TrueType collection.
    const TTCF: u32 = u32::from_le_bytes(*b"ttcf");
    const NAME: u32 = u32::from_le_bytes(*b"name");

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    unsafe extern "system" fn on_face(lf: *const LOGFONTW, tm: *const TEXTMETRICW, font_type: u32, out: LPARAM) -> i32 {
        // SAFETY: `out` is the `Vec<Face>` passed by `faces`; GDI passes valid structures for the
        // duration of the call. Every font but a bitmap one gets a NEWTEXTMETRICEXW, which
        // starts with the NEWTEXTMETRICW read here.
        let (faces, lf) = (&mut *(out.0 as *mut Vec<Face>), &*lf);
        let ntm_flags = if font_type & RASTER_FONTTYPE == 0 { (*(tm as *const NEWTEXTMETRICW)).ntmFlags } else { 0 };
        faces.push(Face { name: from_wide(&lf.lfFaceName), font_type, ntm_flags, weight: lf.lfWeight, italic: lf.lfItalic != 0 });
        1
    }

    /// One face per family and character set.
    pub fn faces() -> Vec<Face> {
        let mut faces: Vec<Face> = Vec::new();
        // SAFETY: a memory DC is created and deleted here; the callback only writes to `faces`,
        // which outlives the call.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return faces;
            }
            let lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
            EnumFontFamiliesExW(dc, &lf, Some(on_face), LPARAM(&mut faces as *mut Vec<Face> as isize), 0);
            let _ = DeleteDC(dc);
        }
        faces
    }

    /// `GetFontData` of table `table` (0: the whole font file) of the font selected in `dc`.
    unsafe fn font_data(dc: HDC, table: u32) -> Option<Vec<u8>> {
        let size = GetFontData(dc, table, 0, None, 0);
        if size == GDI_ERROR as u32 || size == 0 {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        (GetFontData(dc, table, 0, Some(bytes.as_mut_ptr().cast()), size) == size).then_some(bytes)
    }

    pub fn load(family: &str) -> Option<FontFile> {
        let name: Vec<u16> = family.encode_utf16().collect();
        let mut lf = LOGFONTW { lfWeight: FW_NORMAL.0 as i32, lfCharSet: DEFAULT_CHARSET, lfOutPrecision: OUT_OUTLINE_PRECIS, ..Default::default() };
        if name.is_empty() || name.len() >= lf.lfFaceName.len() {
            return None;
        }
        lf.lfFaceName[..name.len()].copy_from_slice(&name);
        // SAFETY: the font is selected into a memory DC created here, and both are released
        // before returning; GetFontData and GetTextFaceW write only within the buffers given.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let font = CreateFontIndirectW(&lf);
            let old = SelectObject(dc, font);
            let mut face = [0u16; 64];
            GetTextFaceW(dc, Some(&mut face));
            let file = if from_wide(&face).to_lowercase() != family.to_lowercase() {
                None
            } else if let Some(ttc) = font_data(dc, TTCF) {
                let index = font_data(dc, NAME).and_then(|n| super::collection_index(&ttc, &n)).unwrap_or(0);
                Some(FontFile { family: family.into(), bytes: ttc, index })
            } else {
                font_data(dc, 0).map(|bytes| FontFile { family: family.into(), bytes, index: 0 })
            };
            SelectObject(dc, old);
            let _ = DeleteObject(font);
            let _ = DeleteDC(dc);
            file
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(name: &str, font_type: u32, ntm_flags: u32, weight: i32, italic: bool) -> Face {
        Face { name: name.into(), font_type, ntm_flags, weight, italic }
    }

    /// Regular upright TrueType/OpenType families are listed; `@` variants, bitmap, vector and
    /// Type 1 fonts and other weights or italics aren't.
    #[test]
    fn lists_regular_outline_families_only() {
        assert!(face("Arial", TRUETYPE_FONTTYPE, NTM_TT_OPENTYPE, FW_NORMAL, false).listed());
        assert!(face("Source Sans", 0, NTM_PS_OPENTYPE, FW_NORMAL, false).listed(), "OpenType with PostScript outlines");
        assert!(!face("@MS Gothic", TRUETYPE_FONTTYPE, 0, FW_NORMAL, false).listed(), "vertical variant");
        assert!(!face("Courier", RASTER_FONTTYPE, 0, FW_NORMAL, false).listed(), "bitmap font");
        assert!(!face("Modern", 0, 0, FW_NORMAL, false).listed(), "vector font");
        assert!(!face("Old PS", 0, NTM_TYPE1, FW_NORMAL, false).listed(), "Type 1");
        assert!(!face("Segoe UI Light", TRUETYPE_FONTTYPE, 0, 300, false).listed());
        assert!(!face("Segoe UI Black", TRUETYPE_FONTTYPE, 0, 900, false).listed());
        assert!(!face("Some Italic", TRUETYPE_FONTTYPE, 0, FW_NORMAL, true).listed());
        assert!(!face("", TRUETYPE_FONTTYPE, 0, FW_NORMAL, false).listed());
    }

    /// One entry per family (GDI repeats a family per character set), sorted without regard to
    /// case, with Michroma left to the built-in entry.
    #[test]
    fn families_are_unique_and_sorted() {
        let tt = |n: &str| face(n, TRUETYPE_FONTTYPE, 0, FW_NORMAL, false);
        let got = families([tt("Segoe UI"), tt("arial"), tt("Segoe UI"), tt("@Segoe UI"), tt("Michroma"), tt("Consolas"), face("Fixedsys", RASTER_FONTTYPE, 0, FW_NORMAL, false)]);
        assert_eq!(got, ["arial", "Consolas", "Segoe UI"]);
    }

    /// The default (or nothing saved) and the classic Michroma never read an installed font; a
    /// missing or unusable font falls back to the default and says so.
    #[test]
    fn resolve_falls_back_to_the_default() {
        for saved in ["Michroma", "michroma", "", "  "] {
            assert_eq!(resolve(saved, |_| panic!("loaded {saved:?}")), (None, false));
        }
        let (classic, missing) = resolve(" michroma (classic) ", |_| panic!("loaded Michroma"));
        let classic = classic.expect("embedded Michroma");
        assert!(!missing && classic.parses() && classic.bytes == crate::ui::theme::MICHROMA && classic.family == "Michroma");
        assert_eq!(resolve("Nope Sans", |_| None), (None, true));
        let junk = |f: &str| Some(FontFile { family: f.into(), bytes: vec![1, 2, 3], index: 0 });
        assert_eq!(resolve("Broken", junk), (None, true));
        let good = |f: &str| Some(FontFile { family: f.into(), bytes: crate::ui::theme::MICHROMA.to_vec(), index: 0 });
        let (file, missing) = resolve(" Good ", good);
        assert_eq!((file.map(|f| f.family), missing), (Some("Good".into()), false));
        let bad_index = |f: &str| Some(FontFile { family: f.into(), bytes: crate::ui::theme::MICHROMA.to_vec(), index: 3 });
        assert_eq!(resolve("Good", bad_index), (None, true));
    }

    #[test]
    fn picker_labels() {
        assert_eq!(picker_label("Michroma", false), BUILT_IN_LABEL);
        assert_eq!(picker_label(CLASSIC, false), "Michroma (built-in)");
        assert_eq!(picker_label("", true), BUILT_IN_LABEL);
        assert_eq!(picker_label("Consolas", false), "Consolas");
        assert_eq!(picker_label("Nope Sans", true), "Nope Sans (not installed)");
    }

    /// Against this machine's fonts: Arial is listed and loads, a face in a collection loads
    /// with its index (Cambria Math is the second face of cambria.ttc), and an unknown family
    /// isn't substituted.
    #[cfg(windows)]
    #[test]
    fn reads_installed_fonts() {
        let list = installed();
        assert!(list.iter().any(|f| f == "Arial"), "{list:?}");
        assert!(!list.iter().any(|f| f.starts_with('@') || is_built_in(f)));
        let arial = load("Arial").expect("Arial");
        assert!(arial.parses() && arial.index == 0);
        let mut egui_gets = arial;
        if list.iter().any(|f| f == "Cambria Math") {
            let math = load("Cambria Math").expect("Cambria Math");
            assert_eq!((&math.bytes[..4], math.index), (&b"ttcf"[..], 1));
            assert!(math.parses());
            egui_gets = math;
        }
        assert_eq!(load("No Such Font 4f1c"), None);
        // egui lays out display text in it.
        use crate::ui::theme;
        use eframe::egui;
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx, Some(egui_gets));
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| ui.label(egui::RichText::new("TMOP 42%").font(theme::display(20.0))));
            });
        }
    }

    /// A two-face collection: each face is found by its `name` table; anything else isn't.
    #[test]
    fn finds_a_face_in_a_collection() {
        let names: [&[u8]; 2] = [b"first face name", b"second face"];
        // Header (12) + 2 offsets (8), then per face a directory (12) with one record (16).
        let dirs = [20usize, 48];
        let data = 76usize;
        let mut ttc = b"ttcf".to_vec();
        ttc.extend(0x0001_0000u32.to_be_bytes());
        ttc.extend(2u32.to_be_bytes());
        ttc.extend((dirs[0] as u32).to_be_bytes());
        ttc.extend((dirs[1] as u32).to_be_bytes());
        let mut off = data;
        for n in names {
            ttc.extend(0x0001_0000u32.to_be_bytes());
            ttc.extend(1u16.to_be_bytes());
            ttc.extend([0u8; 6]);
            ttc.extend(b"name");
            ttc.extend(0u32.to_be_bytes());
            ttc.extend((off as u32).to_be_bytes());
            ttc.extend((n.len() as u32).to_be_bytes());
            off += n.len();
        }
        assert_eq!(ttc.len(), data);
        for n in names {
            ttc.extend(n);
        }
        assert_eq!(collection_index(&ttc, names[0]), Some(0));
        assert_eq!(collection_index(&ttc, names[1]), Some(1));
        assert_eq!(collection_index(&ttc, b"other"), None);
        assert_eq!(collection_index(&ttc[..60], names[1]), None, "truncated");
        assert_eq!(collection_index(crate::ui::theme::MICHROMA, names[0]), None, "not a collection");
    }
}
