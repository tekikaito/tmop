//! Colours, fonts and egui style (dark and light palettes).

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Margin, Rounding, Stroke, TextStyle, Vec2};
use std::sync::Arc;

pub const MICHROMA: &[u8] = include_bytes!("../../assets/fonts/Michroma-Regular.ttf");
pub const INTER_LIGHT: &[u8] = include_bytes!("../../assets/fonts/Inter-Light.ttf");
pub const INTER: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
pub const JBMONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
pub const JBMONO_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf");

/// Named font families registered by [`install_fonts`].
pub const F_UI: &str = "ui";
pub const F_UI_LIGHT: &str = "ui-light";
/// Inter has no heavier weight embedded: bold and semibold are both Medium.
pub const F_UI_BOLD: &str = "ui-bold";
pub const F_UI_SEMIBOLD: &str = "ui-semibold";
/// The display font: Inter Light or the `appearance.displayFont` family (see [`crate::fonts`]).
pub const F_DISPLAY: &str = "display";
pub const F_MONO: &str = "mono";
pub const F_MONO_MEDIUM: &str = "mono-medium";

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_DISPLAY.into()))
}
pub fn ui(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_UI.into()))
}
pub fn light(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_UI_LIGHT.into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_UI_BOLD.into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_UI_SEMIBOLD.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_MONO.into()))
}
pub fn mono_medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_MONO_MEDIUM.into()))
}

/// Installs the application fonts (see [`font_definitions`]). Takes effect from the next frame.
pub fn install_fonts(ctx: &egui::Context, display: Option<crate::fonts::FontFile>) {
    ctx.set_fonts(font_definitions(display));
}

/// The application fonts, all embedded (SIL OFL): Inter for text, JetBrains Mono for numbers and
/// code, and `display` (an installed font, see [`crate::fonts::resolve`]) or else Inter Light for
/// display text. egui's bundled fonts stay behind them for symbols and emoji.
pub fn font_definitions(display: Option<crate::fonts::FontFile>) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let display = match display {
        Some(f) => FontData { index: f.index, ..FontData::from_owned(f.bytes) },
        None => FontData::from_static(INTER_LIGHT),
    };
    defs.font_data.insert(F_DISPLAY.into(), display);
    for (name, bytes) in [("inter-light", INTER_LIGHT), ("inter", INTER), ("inter-medium", INTER_MEDIUM), ("jbmono", JBMONO), ("jbmono-medium", JBMONO_MEDIUM)] {
        defs.font_data.insert(name.into(), FontData::from_static(bytes));
    }
    let default_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let default_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let fam = |primary: &str, rest: &[String]| -> Vec<String> {
        let mut v = vec![primary.to_string()];
        v.extend(rest.iter().cloned());
        v
    };
    let ui_fam = fam("inter", &default_prop);
    defs.families.insert(FontFamily::Proportional, ui_fam.clone());
    defs.families.insert(FontFamily::Name(F_UI.into()), ui_fam.clone());
    defs.families.insert(FontFamily::Name(F_UI_LIGHT.into()), fam("inter-light", &ui_fam));
    defs.families.insert(FontFamily::Name(F_UI_BOLD.into()), fam("inter-medium", &ui_fam));
    defs.families.insert(FontFamily::Name(F_UI_SEMIBOLD.into()), fam("inter-medium", &ui_fam));
    defs.families.insert(FontFamily::Name(F_DISPLAY.into()), fam(F_DISPLAY, &ui_fam));
    let mono_fam = fam("jbmono", &default_mono);
    defs.families.insert(FontFamily::Monospace, mono_fam.clone());
    defs.families.insert(FontFamily::Name(F_MONO.into()), mono_fam.clone());
    defs.families.insert(FontFamily::Name(F_MONO_MEDIUM.into()), fam("jbmono-medium", &mono_fam));
    defs
}

/// Application palette: 22 colour roles (see [`ROLES`]). Built-in and custom palettes
/// (`.tmoptheme` files) are listed by [`crate::palette`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Title/status chrome.
    pub chrome: Color32,
    /// Page body behind the cards.
    pub body: Color32,
    pub sidebar: Color32,
    pub sidebar_rule: Color32,
    pub card: Color32,
    pub card_stroke: Color32,
    /// Settings cards, table headers.
    pub panel: Color32,
    pub panel_stroke: Color32,
    pub hover: Color32,
    /// Selected rows, tabs and nav entries. Text on it uses [`Palette::on_selection`], not `text`.
    pub selection: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub grid: Color32,
    pub led_bg: Color32,
    pub led_dim: Color32,
    pub led_lit: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub control: Color32,
    pub control_stroke: Color32,
    /// Window background gradient stops (highlight, mid, edge). Not a palette-file role: only the
    /// built-in Dark and Light have one, every other palette is flat `body`.
    pub backdrop: Option<[Color32; 3]>,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The default dark palette (the redesign): navy surfaces, teal accent.
pub const DARK: Palette = Palette {
    dark: true,
    chrome: rgb(0x0B0F1C),
    body: rgb(0x0B0F1C),
    sidebar: rgb(0x0B0F1C),
    sidebar_rule: rgb(0x1F222E),
    card: rgb(0x141724),
    card_stroke: rgb(0x1C202C),
    panel: rgb(0x0E1322),
    panel_stroke: rgb(0x1F222E),
    hover: rgb(0x1A1D2A),
    selection: rgb(0x152932),
    accent: rgb(0x5EEAD4),
    text: rgb(0xE8ECF5),
    text_dim: rgb(0xA7B2C9),
    text_faint: rgb(0x7C8AA5),
    grid: rgb(0x1C202C),
    led_bg: rgb(0x070A12),
    led_dim: rgb(0x1C463F),
    led_lit: rgb(0x5EEAD4),
    ok: rgb(0x6EE7A8),
    warn: rgb(0xF5B86B),
    control: rgb(0x171B27),
    control_stroke: rgb(0x1F222E),
    backdrop: Some([rgb(0x16213A), rgb(0x0B0F1C), rgb(0x070A12)]),
};

/// The default light palette: the same structure as [`DARK`] on pale surfaces.
pub const LIGHT: Palette = Palette {
    dark: false,
    chrome: rgb(0xEEF1F7),
    body: rgb(0xEEF1F7),
    sidebar: rgb(0xEEF1F7),
    sidebar_rule: rgb(0xD5DAE5),
    card: rgb(0xFAFBFD),
    card_stroke: rgb(0xDCE1EA),
    panel: rgb(0xF3F5F9),
    panel_stroke: rgb(0xD5DAE5),
    hover: rgb(0xE3E7EF),
    selection: rgb(0xCFEFEA),
    accent: rgb(0x0B7F75),
    text: rgb(0x141A2B),
    text_dim: rgb(0x4A5670),
    text_faint: rgb(0x6F7A92),
    grid: rgb(0xDCE1EA),
    led_bg: rgb(0x141A2B),
    led_dim: rgb(0x1F3A3A),
    led_lit: rgb(0x0B7F75),
    ok: rgb(0x1A8F5B),
    warn: rgb(0xB5721C),
    control: rgb(0xFFFFFF),
    control_stroke: rgb(0xD5DAE5),
    backdrop: Some([rgb(0xFFFFFF), rgb(0xEEF1F7), rgb(0xE4E8F0)]),
};

/// The previous Dark palette, kept as a built-in.
pub const CLASSIC_DARK: Palette = Palette {
    dark: true,
    chrome: Color32::from_rgb(0x20, 0x20, 0x20),
    body: Color32::from_rgb(0x18, 0x18, 0x18),
    sidebar: Color32::from_rgb(0x1A, 0x1A, 0x1A),
    sidebar_rule: Color32::from_rgb(0x37, 0x37, 0x37),
    card: Color32::from_rgb(0x26, 0x26, 0x26),
    card_stroke: Color32::from_rgb(0x60, 0x60, 0x60),
    panel: Color32::from_rgb(0x2B, 0x2B, 0x2B),
    panel_stroke: Color32::from_rgb(0x3C, 0x3C, 0x3C),
    hover: Color32::from_rgb(0x2E, 0x2E, 0x2E),
    selection: Color32::from_rgb(0x2D, 0x4A, 0x8B),
    accent: Color32::from_rgb(0x60, 0x82, 0xEB),
    text: Color32::from_rgb(0xF5, 0xF5, 0xF5),
    text_dim: Color32::from_rgb(0xB4, 0xB4, 0xB4),
    text_faint: Color32::from_rgb(0x80, 0x80, 0x80),
    grid: Color32::from_rgb(0x33, 0x33, 0x33),
    led_bg: Color32::from_rgb(0x00, 0x00, 0x00),
    led_dim: Color32::from_rgb(0x05, 0x2A, 0x57),
    led_lit: Color32::from_rgb(0x33, 0xEB, 0xFF),
    ok: Color32::from_rgb(0x4C, 0xD9, 0x64),
    warn: Color32::from_rgb(0xE0, 0x9A, 0x3C),
    control: Color32::from_rgb(0x33, 0x33, 0x33),
    control_stroke: Color32::from_rgb(0x4A, 0x4A, 0x4A),
    backdrop: None,
};

/// The previous Light palette, kept as a built-in.
pub const CLASSIC_LIGHT: Palette = Palette {
    dark: false,
    chrome: Color32::from_rgb(0xF0, 0xF0, 0xF0),
    body: Color32::from_rgb(0xE9, 0xE9, 0xE9),
    sidebar: Color32::from_rgb(0xF3, 0xF3, 0xF3),
    sidebar_rule: Color32::from_rgb(0xD0, 0xD0, 0xD0),
    card: Color32::from_rgb(0xFB, 0xFB, 0xFB),
    card_stroke: Color32::from_rgb(0xC4, 0xC4, 0xC4),
    panel: Color32::from_rgb(0xF6, 0xF6, 0xF6),
    panel_stroke: Color32::from_rgb(0xD4, 0xD4, 0xD4),
    hover: Color32::from_rgb(0xE2, 0xE2, 0xE2),
    selection: Color32::from_rgb(0x2D, 0x4A, 0x8B),
    accent: Color32::from_rgb(0x3A, 0x62, 0xD8),
    text: Color32::from_rgb(0x16, 0x16, 0x16),
    text_dim: Color32::from_rgb(0x4A, 0x4A, 0x4A),
    text_faint: Color32::from_rgb(0x80, 0x80, 0x80),
    grid: Color32::from_rgb(0xDA, 0xDA, 0xDA),
    led_bg: Color32::from_rgb(0x10, 0x10, 0x10),
    led_dim: Color32::from_rgb(0x08, 0x30, 0x60),
    led_lit: Color32::from_rgb(0x33, 0xEB, 0xFF),
    ok: Color32::from_rgb(0x1E, 0x9E, 0x3C),
    warn: Color32::from_rgb(0xB8, 0x70, 0x10),
    control: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    control_stroke: Color32::from_rgb(0xBE, 0xBE, 0xBE),
    backdrop: None,
};

pub const HIGH_CONTRAST: Palette = Palette {
    dark: true,
    chrome: Color32::from_rgb(0x00, 0x00, 0x00),
    body: Color32::from_rgb(0x00, 0x00, 0x00),
    sidebar: Color32::from_rgb(0x00, 0x00, 0x00),
    sidebar_rule: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    card: Color32::from_rgb(0x0A, 0x0A, 0x0A),
    card_stroke: Color32::from_rgb(0xC8, 0xC8, 0xC8),
    panel: Color32::from_rgb(0x0A, 0x0A, 0x0A),
    panel_stroke: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    hover: Color32::from_rgb(0x30, 0x30, 0x30),
    selection: Color32::from_rgb(0x1A, 0x4F, 0xBF),
    accent: Color32::from_rgb(0xFF, 0xFF, 0x00),
    text: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    text_dim: Color32::from_rgb(0xE6, 0xE6, 0xE6),
    text_faint: Color32::from_rgb(0xB4, 0xB4, 0xB4),
    grid: Color32::from_rgb(0x5A, 0x5A, 0x5A),
    led_bg: Color32::from_rgb(0x00, 0x00, 0x00),
    led_dim: Color32::from_rgb(0x00, 0x33, 0x3A),
    led_lit: Color32::from_rgb(0x00, 0xFF, 0xFF),
    ok: Color32::from_rgb(0x3F, 0xF2, 0x3F),
    warn: Color32::from_rgb(0xFF, 0xB0, 0x00),
    control: Color32::from_rgb(0x00, 0x00, 0x00),
    control_stroke: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    backdrop: None,
};

/// Nord (Arctic Ice Studio): polar night surfaces, snow storm text, frost accent.
pub const NORD: Palette = Palette {
    dark: true,
    chrome: Color32::from_rgb(0x34, 0x3A, 0x48),
    body: Color32::from_rgb(0x2E, 0x34, 0x40),
    sidebar: Color32::from_rgb(0x2A, 0x2F, 0x3A),
    sidebar_rule: Color32::from_rgb(0x43, 0x4C, 0x5E),
    card: Color32::from_rgb(0x3B, 0x42, 0x52),
    card_stroke: Color32::from_rgb(0x4C, 0x56, 0x6A),
    panel: Color32::from_rgb(0x3F, 0x47, 0x58),
    panel_stroke: Color32::from_rgb(0x4C, 0x56, 0x6A),
    hover: Color32::from_rgb(0x43, 0x4C, 0x5E),
    selection: Color32::from_rgb(0x4C, 0x6A, 0x92),
    accent: Color32::from_rgb(0x88, 0xC0, 0xD0),
    text: Color32::from_rgb(0xEC, 0xEF, 0xF4),
    text_dim: Color32::from_rgb(0xD8, 0xDE, 0xE9),
    text_faint: Color32::from_rgb(0x9A, 0xA3, 0xB5),
    grid: Color32::from_rgb(0x43, 0x4C, 0x5E),
    led_bg: Color32::from_rgb(0x24, 0x29, 0x33),
    led_dim: Color32::from_rgb(0x2F, 0x40, 0x4E),
    led_lit: Color32::from_rgb(0x8F, 0xBC, 0xBB),
    ok: Color32::from_rgb(0xA3, 0xBE, 0x8C),
    warn: Color32::from_rgb(0xD0, 0x87, 0x70),
    control: Color32::from_rgb(0x43, 0x4C, 0x5E),
    control_stroke: Color32::from_rgb(0x4C, 0x56, 0x6A),
    backdrop: None,
};

/// Solarized Light (Ethan Schoonover): base3/base2 surfaces, base02 text.
pub const SOLARIZED_LIGHT: Palette = Palette {
    dark: false,
    chrome: Color32::from_rgb(0xF6, 0xF0, 0xDD),
    body: Color32::from_rgb(0xEE, 0xE8, 0xD5),
    sidebar: Color32::from_rgb(0xF4, 0xEE, 0xDB),
    sidebar_rule: Color32::from_rgb(0xD9, 0xD2, 0xBC),
    card: Color32::from_rgb(0xFD, 0xF6, 0xE3),
    card_stroke: Color32::from_rgb(0xC9, 0xC2, 0xAC),
    panel: Color32::from_rgb(0xFA, 0xF3, 0xE0),
    panel_stroke: Color32::from_rgb(0xDD, 0xD6, 0xC1),
    hover: Color32::from_rgb(0xE8, 0xE2, 0xCF),
    selection: Color32::from_rgb(0x1F, 0x6F, 0xA8),
    accent: Color32::from_rgb(0x26, 0x8B, 0xD2),
    text: Color32::from_rgb(0x07, 0x36, 0x42),
    text_dim: Color32::from_rgb(0x4A, 0x5F, 0x66),
    text_faint: Color32::from_rgb(0x83, 0x94, 0x96),
    grid: Color32::from_rgb(0xDD, 0xD6, 0xC1),
    led_bg: Color32::from_rgb(0x00, 0x2B, 0x36),
    led_dim: Color32::from_rgb(0x0B, 0x3B, 0x46),
    led_lit: Color32::from_rgb(0x2A, 0xA1, 0x98),
    ok: Color32::from_rgb(0x85, 0x99, 0x00),
    warn: Color32::from_rgb(0xCB, 0x4B, 0x16),
    control: Color32::from_rgb(0xFF, 0xFB, 0xEF),
    control_stroke: Color32::from_rgb(0xC9, 0xC2, 0xAC),
    backdrop: None,
};

/// A colour role of a [`Palette`]: its `.tmoptheme` key and its label in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Role {
    pub key: &'static str,
    pub label: &'static str,
}

const fn role(key: &'static str, label: &'static str) -> Role {
    Role { key, label }
}

/// All 22 roles, in file order.
pub const ROLES: [Role; 22] = [
    role("chrome", "Title and status bars"),
    role("body", "Background"),
    role("sidebar", "Sidebar"),
    role("sidebarRule", "Sidebar rule"),
    role("card", "Surface"),
    role("cardStroke", "Surface border"),
    role("panel", "Panel"),
    role("panelStroke", "Panel border"),
    role("hover", "Hover"),
    role("selection", "Selection"),
    role("accent", "Accent"),
    role("text", "Text"),
    role("textDim", "Secondary text"),
    role("textFaint", "Hint text"),
    role("grid", "Grid"),
    role("ledBg", "LED background"),
    role("ledDim", "LED unlit"),
    role("ledLit", "LED lit"),
    role("ok", "OK"),
    role("warn", "Warning"),
    role("control", "Control"),
    role("controlStroke", "Control border"),
];

/// The roles the editor shows by default; [`Palette::derived`] computes the others from them.
pub const CORE_ROLES: [Role; 8] = [
    role("body", "Background"),
    role("card", "Surface"),
    role("sidebar", "Sidebar"),
    role("accent", "Accent"),
    role("text", "Text"),
    role("ok", "OK"),
    role("warn", "Warning"),
    role("grid", "Grid"),
];

/// Text roles that must stay readable (WCAG AA, 4.5:1) on every [`SURFACE_ROLES`] colour.
pub const TEXT_ROLES: [&str; 2] = ["text", "textDim"];
/// Roles text is drawn on.
pub const SURFACE_ROLES: [&str; 7] = ["chrome", "body", "sidebar", "card", "panel", "hover", "control"];
/// Minimum text contrast (WCAG AA).
pub const MIN_CONTRAST: f32 = 4.5;

/// The recorder's seven-segment display (status bar and Flight Recorder page).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lcd {
    pub bg: Color32,
    pub rim: Color32,
    /// Segments while not recording (recording uses red).
    pub lit: Color32,
    pub unlit: Color32,
}

impl Palette {
    /// The colour of role `key` (a [`ROLES`] key).
    pub fn role(&self, key: &str) -> Option<Color32> {
        let mut p = *self;
        p.role_mut(key).map(|c| *c)
    }

    pub fn role_mut(&mut self, key: &str) -> Option<&mut Color32> {
        Some(match key {
            "chrome" => &mut self.chrome,
            "body" => &mut self.body,
            "sidebar" => &mut self.sidebar,
            "sidebarRule" => &mut self.sidebar_rule,
            "card" => &mut self.card,
            "cardStroke" => &mut self.card_stroke,
            "panel" => &mut self.panel,
            "panelStroke" => &mut self.panel_stroke,
            "hover" => &mut self.hover,
            "selection" => &mut self.selection,
            "accent" => &mut self.accent,
            "text" => &mut self.text,
            "textDim" => &mut self.text_dim,
            "textFaint" => &mut self.text_faint,
            "grid" => &mut self.grid,
            "ledBg" => &mut self.led_bg,
            "ledDim" => &mut self.led_dim,
            "ledLit" => &mut self.led_lit,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "control" => &mut self.control,
            "controlStroke" => &mut self.control_stroke,
            _ => return None,
        })
    }

    /// This palette with every non-core role recomputed from the [`CORE_ROLES`] and `dark`.
    pub fn derived(&self) -> Palette {
        let (body, card, text) = (self.body, self.card, self.text);
        let control = if self.dark { mix(card, text, 0.06) } else { lighten(card, 0.8) };
        Palette {
            chrome: mix(body, card, 0.5),
            sidebar_rule: mix(self.sidebar, text, 0.15),
            card_stroke: mix(card, text, 0.28),
            panel: mix(card, text, 0.025),
            panel_stroke: mix(card, text, 0.1),
            hover: mix(card, text, 0.06),
            selection: dim(self.accent, 0.6),
            text_dim: mix(text, body, 0.3),
            text_faint: mix(text, body, 0.5),
            led_bg: dim(body, 0.1),
            led_dim: dim(self.accent, 0.3),
            led_lit: lighten(self.accent, 0.35),
            control,
            control_stroke: mix(control, text, 0.15),
            ..*self
        }
    }

    /// Text on [`Palette::selection`]: white or black, whichever reads better.
    pub fn on_selection(&self) -> Color32 {
        if contrast(Color32::WHITE, self.selection) >= contrast(Color32::BLACK, self.selection) {
            Color32::WHITE
        } else {
            Color32::BLACK
        }
    }

    /// Table body text (between `text` and `text_dim`).
    pub fn text_table(&self) -> Color32 {
        mix(self.text, self.text_dim, 0.3)
    }

    /// Inactive navigation icons and labels.
    pub fn text_rail(&self) -> Color32 {
        mix(self.text_faint, self.text_dim, 0.35)
    }

    /// Code and log text.
    pub fn text_code(&self) -> Color32 {
        mix(self.text, self.text_dim, 0.5)
    }

    /// Inset boxes (paths, command lines, per-core cells, log panes).
    pub fn well(&self) -> Color32 {
        with_alpha(self.led_bg, 0.3)
    }

    /// A hairline wash of `text` over whatever is below: secondary buttons (0.06), their border
    /// (0.10..0.12), segmented-control highlights (0.12).
    pub fn raise(&self, alpha: f32) -> Color32 {
        with_alpha(self.text, alpha)
    }

    /// Rule between table rows and list entries.
    pub fn row_rule(&self) -> Color32 {
        with_alpha(self.text, 0.06)
    }

    /// Fill of a selected table row (the row also gets a 1 px accent inset).
    pub fn selected_row(&self) -> Color32 {
        with_alpha(self.accent, 0.07)
    }

    /// The lowest text contrast that involves role `key`, if it is a text or surface role:
    /// a text role over every surface, or every text role over a surface.
    pub fn worst_contrast(&self, key: &str) -> Option<f32> {
        let pairs: Vec<(&str, &str)> = if TEXT_ROLES.contains(&key) {
            SURFACE_ROLES.iter().map(|s| (key, *s)).collect()
        } else if SURFACE_ROLES.contains(&key) {
            TEXT_ROLES.iter().map(|t| (*t, key)).collect()
        } else {
            return None;
        };
        let mut worst = f32::MAX;
        for (fg, bg) in pairs {
            worst = worst.min(contrast(self.role(fg)?, self.role(bg)?));
        }
        if key == "selection" {
            worst = worst.min(contrast(self.on_selection(), self.selection));
        }
        Some(worst)
    }

    /// Roles whose [`Palette::worst_contrast`] is below [`MIN_CONTRAST`], with that contrast.
    pub fn contrast_warnings(&self) -> Vec<(&'static str, f32)> {
        ROLES.iter().filter_map(|r| self.worst_contrast(r.key).filter(|c| *c < MIN_CONTRAST).map(|c| (r.key, c))).collect()
    }

    /// Process-row heat shading (busy rows tinted towards it).
    pub fn heat(&self) -> Color32 {
        mix(self.ok, self.body, 0.4)
    }

    /// Tuned so Dark reproduces the old fixed LCD (#0A1626, rim #1A2C44, lit #9CB8D8, unlit #16263A).
    pub fn lcd(&self) -> Lcd {
        let tint = |t: f32, haze: f32| lighten(mix(self.led_bg, self.led_dim, t), haze);
        Lcd { bg: tint(0.35, 0.035), rim: tint(0.55, 0.09), lit: lighten(mix(self.led_dim, self.accent, 0.65), 0.51), unlit: tint(0.48, 0.08) }
    }

    /// Card background used behind graphs on the light theme (graphs stay dark-ish for contrast).
    pub fn graph_bg(&self) -> Color32 {
        if self.dark {
            self.card
        } else {
            mix(self.card, self.body, 0.5)
        }
    }

    /// Applies the palette to egui's own widgets (combos, sliders, text edits, menus). egui keeps
    /// one style per OS theme; both get this palette, so a Windows theme change can't swap it out.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = self.body;
        // Glass: dialogs, menus and popups let the backdrop show through a little.
        v.window_fill = with_alpha(mix(self.body, self.card, 0.6), 0.92);
        v.window_stroke = Stroke::new(1.0_f32, with_alpha(self.text, 0.10));
        v.window_rounding = Rounding::same(10.0);
        v.menu_rounding = Rounding::same(8.0);
        v.extreme_bg_color = self.control;
        v.faint_bg_color = self.hover;
        v.code_bg_color = self.control;
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(1.0_f32, self.on_selection());
        v.widgets.noninteractive.bg_fill = self.panel;
        v.widgets.noninteractive.weak_bg_fill = self.panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.panel_stroke);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text_dim);
        v.widgets.inactive.bg_fill = self.control;
        v.widgets.inactive.weak_bg_fill = self.control;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.control_stroke);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.inactive.rounding = Rounding::same(6.0);
        v.widgets.hovered.bg_fill = self.hover;
        v.widgets.hovered.weak_bg_fill = self.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.hovered.rounding = Rounding::same(6.0);
        v.widgets.active.bg_fill = self.selection;
        v.widgets.active.weak_bg_fill = self.selection;
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0_f32, self.on_selection());
        v.widgets.active.rounding = Rounding::same(6.0);
        v.widgets.open.bg_fill = self.control;
        v.widgets.open.weak_bg_fill = self.control;
        v.widgets.open.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.open.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.open.rounding = Rounding::same(6.0);
        v.slider_trailing_fill = true;
        v.popup_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 16.0), blur: 48.0, spread: 0.0, color: Color32::from_black_alpha(140) };
        v.window_shadow = v.popup_shadow;
        v.striped = false;
        style.visuals = v;
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size = Vec2::new(40.0, 30.0);
        style.spacing.combo_width = 180.0;
        style.spacing.slider_width = 160.0;
        style.spacing.menu_margin = Margin::same(6.0);
        style.spacing.window_margin = Margin::same(10.0);
        style.text_styles = [
            (TextStyle::Small, ui(11.0)),
            (TextStyle::Body, ui(13.0)),
            (TextStyle::Button, semibold(13.0)),
            (TextStyle::Heading, light(20.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into_iter()
        .collect();
        let style = Arc::new(style);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
    }
}

/// Metrics with a colour of their own (`graphs.metricColors` keys), with their labels in
/// Settings → Graphs.
pub const METRICS: [(&str, &str); 10] = [
    ("cpu", "CPU"),
    ("disk", "Disk"),
    ("memory", "Memory"),
    ("network", "Network"),
    ("gpu", "GPU"),
    ("thermal", "Thermals"),
    ("clock", "Clock"),
    ("power", "Power"),
    ("kernel", "Kernel"),
    ("docker", "Docker"),
];

// Fixed accent colours: the built-in metric colours, plus yellow for the middle of the
// green/yellow/red ramps. Status and severity colours (ok/warning/danger ramps, process,
// connection and service states, summary-card accents) use these rather than a metric's colour,
// so `graphs.metricColors` overrides never reach them. They are pastels meant for dark surfaces:
// on light palettes read them through [`status`].
pub const GREEN: Color32 = rgb(0x6EE7A8);
pub const YELLOW: Color32 = rgb(0xF5D28F);
pub const RED: Color32 = rgb(0xFF7A8A);
pub const ORANGE: Color32 = rgb(0xF5B86B);
pub const BLUE: Color32 = rgb(0x8FB4FF);
pub const PURPLE: Color32 = rgb(0xC9A6FF);
pub const INDIGO: Color32 = rgb(0x8B9CFF);
pub const PINK: Color32 = rgb(0xFFB4C8);
/// Danger text (labels, "Remove…" buttons).
pub const RED_TEXT: Color32 = rgb(0xFF9AA6);
/// Fill of the primary danger button, and the text on it.
pub const DANGER_FILL: Color32 = rgb(0xFF6B7E);
pub const ON_DANGER: Color32 = rgb(0x1A0E12);

/// Severity colours that read on `pal`: the pastel constants on dark palettes, darkened on light ones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub ok: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub danger_text: Color32,
    pub info: Color32,
    pub yellow: Color32,
}

/// The severity colours for `pal` (see [`Status`]).
pub fn status(pal: &Palette) -> Status {
    let c = |c: Color32| darken_for(pal, c);
    Status { ok: c(GREEN), warn: c(ORANGE), danger: c(RED), danger_text: c(RED_TEXT), info: c(BLUE), yellow: c(YELLOW) }
}

/// `c` as is on a dark palette, darkened to stay readable on a light one.
pub fn darken_for(pal: &Palette, c: Color32) -> Color32 {
    if pal.dark {
        c
    } else {
        dim(c, 0.62)
    }
}

/// Built-in metric colours. Unknown keys are grey.
pub fn default_metric_color(key: &str) -> Color32 {
    match key {
        "cpu" => rgb(0x5EEAD4),
        "memory" => INDIGO,
        "disk" => PURPLE,
        "gpu" => PINK,
        "network" => rgb(0xE8ECF5),
        "thermal" | "clock" => ORANGE,
        "power" => YELLOW,
        "kernel" => rgb(0x7C8AA5),
        "docker" => BLUE,
        _ => Color32::from_rgb(0x9A, 0x9A, 0x9A),
    }
}

/// The colours that identify a metric:[`default_metric_color`] with the `graphs.metricColors`
/// overrides applied. Resolved once per frame (see [`super::FrameData::metric`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MetricColors {
    /// Overrides, indexed like [`METRICS`].
    custom: [Option<Color32>; METRICS.len()],
}

impl MetricColors {
    /// Overrides from the settings. Without colour keying there are none (graphs use one blue
    /// and every other accent keeps its built-in colour); values that aren't colours are ignored.
    pub fn from_settings(g: &crate::settings::Graphs) -> Self {
        if g.color_keyed {
            Self::from_map(&g.metric_colors)
        } else {
            Self::default()
        }
    }

    /// Overrides from a `graphs.metricColors` map, whether or not colour keying is on.
    pub fn from_map(map: &std::collections::BTreeMap<String, String>) -> Self {
        let mut m = Self::default();
        for (i, (key, _)) in METRICS.iter().enumerate() {
            m.custom[i] = map.get(*key).and_then(|v| crate::skin::Color::parse(v)).map(from_skin);
        }
        m
    }

    /// The colour of metric `key`.
    pub fn get(&self, key: &str) -> Color32 {
        self.custom(key).unwrap_or_else(|| default_metric_color(key))
    }

    /// The override for `key`, if there is one.
    pub fn custom(&self, key: &str) -> Option<Color32> {
        METRICS.iter().position(|(k, _)| *k == key).and_then(|i| self.custom[i])
    }
}

/// `c` with its alpha replaced.
pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// `c` scaled towards black (alpha preserved).
pub fn dim(c: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let m = |v: u8| (v as f32 * factor.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(m(r), m(g), m(b), a)
}

/// `c` mixed towards white by `t`.
pub fn lighten(c: Color32, t: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let m = |v: u8| (v as f32 + (255.0 - v as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(m(r), m(g), m(b), a)
}

/// WCAG contrast ratio of `fg` over the opaque `bg` (see [`crate::skin::contrast`]).
pub fn contrast(fg: Color32, bg: Color32) -> f32 {
    crate::skin::contrast(to_skin(fg), to_skin(bg))
}

pub fn to_skin(c: Color32) -> crate::skin::Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    crate::skin::Color::rgba(r, g, b, a)
}

pub fn from_skin(c: crate::skin::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Gamma-space mix of two colours.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [r0, g0, b0, a0] = a.to_srgba_unmultiplied();
    let [r1, g1, b1, a1] = b.to_srgba_unmultiplied();
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(r0, r1), m(g0, g1), m(b0, b1), m(a0, a1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorder's LCD stays readable on every built-in look: lit segments against the glass.
    #[test]
    fn lcd_stays_readable() {
        for (name, p) in [("dark", DARK), ("light", LIGHT), ("classic dark", CLASSIC_DARK), ("classic light", CLASSIC_LIGHT), ("nord", NORD)] {
            let l = p.lcd();
            let r = contrast(l.lit, l.bg);
            assert!(r >= 3.0, "{name}: lit vs bg is {r:.2}:1");
        }
    }

    /// Without overrides every metric has its built-in colour; each key has one of its own.
    #[test]
    fn metric_colors_default_to_the_built_in_ones() {
        let m = MetricColors::from_settings(&crate::settings::Graphs::default());
        assert_eq!(m, MetricColors::default());
        let hex = Color32::from_rgb;
        for (k, c) in [
            ("cpu", hex(0x5E, 0xEA, 0xD4)),
            ("memory", hex(0x8B, 0x9C, 0xFF)),
            ("disk", hex(0xC9, 0xA6, 0xFF)),
            ("gpu", hex(0xFF, 0xB4, 0xC8)),
            ("network", hex(0xE8, 0xEC, 0xF5)),
            ("thermal", hex(0xF5, 0xB8, 0x6B)),
            ("clock", hex(0xF5, 0xB8, 0x6B)),
            ("power", hex(0xF5, 0xD2, 0x8F)),
            ("kernel", hex(0x7C, 0x8A, 0xA5)),
            ("docker", hex(0x8F, 0xB4, 0xFF)),
        ] {
            assert_eq!(m.get(k), c, "{k}");
            assert_eq!(m.custom(k), None);
        }
        assert_ne!(m.get("cpu"), m.get("disk"));
        assert_eq!(m.get("yellow"), default_metric_color("yellow"));
        assert_eq!(m.get("nope"), default_metric_color("nope"));
    }

    /// Severity colours are the pastel constants on dark palettes and darker on light ones, where
    /// they must stay readable on a card.
    #[test]
    fn status_colors_follow_the_palette() {
        assert_eq!(status(&DARK).ok, GREEN);
        assert_eq!(status(&NORD).danger, RED);
        for pal in [LIGHT, CLASSIC_LIGHT, SOLARIZED_LIGHT] {
            let s = status(&pal);
            assert_ne!(s.ok, GREEN);
            for (name, c) in [("ok", s.ok), ("warn", s.warn), ("danger", s.danger), ("danger_text", s.danger_text), ("info", s.info), ("yellow", s.yellow)] {
                let r = contrast(c, pal.card);
                assert!(r >= 3.0, "{name} on a light card is {r:.2}:1");
            }
        }
    }

    /// Each key is its own override: changing CPU leaves Disk (which shared its colour) alone.
    #[test]
    fn metric_color_overrides_are_per_key() {
        let mut g = crate::settings::Graphs::default();
        g.metric_colors.insert("cpu".into(), "#112233".into());
        g.metric_colors.insert("kernel".into(), "abc".into());
        g.metric_colors.insert("gpu".into(), "not a colour".into());
        g.metric_colors.insert("yellow".into(), "#000000".into());
        let m = MetricColors::from_settings(&g);
        assert_eq!(m.get("cpu"), Color32::from_rgb(0x11, 0x22, 0x33));
        assert_eq!(m.custom("cpu"), Some(Color32::from_rgb(0x11, 0x22, 0x33)));
        assert_eq!(m.get("disk"), default_metric_color("disk"));
        assert_eq!(m.get("kernel"), Color32::from_rgb(0xAA, 0xBB, 0xCC));
        assert_eq!(m.get("clock"), default_metric_color("clock"));
        assert_eq!((m.get("gpu"), m.custom("gpu")), (default_metric_color("gpu"), None), "an invalid value is ignored");
        assert_eq!(m.get("yellow"), default_metric_color("yellow"), "only metric keys can be overridden");
        // Colour keying off: overrides are kept in the settings but not used.
        g.color_keyed = false;
        assert_eq!(MetricColors::from_settings(&g), MetricColors::default());
        assert_eq!(MetricColors::from_map(&g.metric_colors), m);
    }

    /// The display font is Inter Light, Michroma or the installed one, falling back to the UI fonts for
    /// missing glyphs; body text never uses it.
    #[test]
    fn display_font_only_changes_the_display_family() {
        let family = |d: &FontDefinitions, f: FontFamily| d.families.get(&f).cloned().unwrap_or_default();
        let builtin = font_definitions(None);
        assert_eq!(builtin.font_data[F_DISPLAY].font.as_ref(), INTER_LIGHT);
        let classic = font_definitions(crate::fonts::resolve(crate::fonts::CLASSIC, crate::fonts::load).0);
        assert_eq!(classic.font_data[F_DISPLAY].font.as_ref(), MICHROMA);
        let file = crate::fonts::FontFile { family: "Other".into(), bytes: vec![7; 16], index: 2 };
        let custom = font_definitions(Some(file));
        assert_eq!((custom.font_data[F_DISPLAY].font.as_ref(), custom.font_data[F_DISPLAY].index), (&[7u8; 16][..], 2));
        for d in [&builtin, &classic, &custom] {
            let display = family(d, FontFamily::Name(F_DISPLAY.into()));
            assert_eq!(display[0], F_DISPLAY);
            assert_eq!(display[1..], family(d, FontFamily::Name(F_UI.into())));
            for f in [FontFamily::Proportional, FontFamily::Name(F_UI.into()), FontFamily::Name(F_UI_LIGHT.into()), FontFamily::Name(F_UI_BOLD.into()), FontFamily::Name(F_MONO.into()), FontFamily::Monospace] {
                assert!(!family(d, f.clone()).iter().any(|n| n == F_DISPLAY), "{f:?}");
            }
        }
    }

    /// egui keeps a style per OS theme; a palette restyles both, so Windows' theme can't undo it.
    #[test]
    fn apply_styles_both_os_themes() {
        let ctx = egui::Context::default();
        NORD.apply(&ctx);
        for t in [egui::Theme::Dark, egui::Theme::Light] {
            let s = ctx.style_of(t);
            assert_eq!((s.visuals.panel_fill, s.visuals.dark_mode), (NORD.body, true), "{t:?}");
        }
    }
}
