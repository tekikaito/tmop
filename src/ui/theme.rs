//! Colours, fonts and egui style (dark and light palettes).

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Margin, Rounding, Stroke, TextStyle, Vec2};
use std::sync::Arc;

pub const MICHROMA: &[u8] = include_bytes!("../../assets/fonts/Michroma-Regular.ttf");

/// Named font families registered by [`install_fonts`].
pub const F_UI: &str = "ui";
pub const F_UI_BOLD: &str = "ui-bold";
pub const F_UI_SEMIBOLD: &str = "ui-semibold";
pub const F_DISPLAY: &str = "michroma";
pub const F_MONO: &str = "mono";

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_DISPLAY.into()))
}
pub fn ui(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(F_UI.into()))
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

/// Installs the application fonts: the embedded Michroma (SIL OFL) for display text and,
/// when present on this machine, Segoe UI / Consolas from the Windows font folder for
/// body text. Missing fonts fall back to egui's bundled ones.
pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    defs.font_data.insert(F_DISPLAY.into(), FontData::from_static(MICHROMA));
    let win = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let fonts = win.join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(fonts.join(file)) {
            Ok(bytes) => {
                defs.font_data.insert(name.into(), FontData::from_owned(bytes));
                true
            }
            Err(_) => false,
        }
    };
    let have_ui = load(F_UI, "segoeui.ttf");
    let have_bold = load(F_UI_BOLD, "segoeuib.ttf");
    let have_semi = load(F_UI_SEMIBOLD, "seguisb.ttf");
    let have_mono = load(F_MONO, "consola.ttf");
    let default_prop = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let default_mono = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let fam = |primary: Option<&str>, rest: &[String]| -> Vec<String> {
        let mut v: Vec<String> = primary.map(|p| vec![p.to_string()]).unwrap_or_default();
        v.extend(rest.iter().cloned());
        v
    };
    let ui_fam = fam(have_ui.then_some(F_UI), &default_prop);
    defs.families.insert(FontFamily::Proportional, ui_fam.clone());
    defs.families.insert(FontFamily::Name(F_UI.into()), ui_fam.clone());
    defs.families.insert(FontFamily::Name(F_UI_BOLD.into()), fam(have_bold.then_some(F_UI_BOLD), &ui_fam));
    defs.families.insert(FontFamily::Name(F_UI_SEMIBOLD.into()), fam(have_semi.then_some(F_UI_SEMIBOLD), &ui_fam));
    defs.families.insert(FontFamily::Name(F_DISPLAY.into()), fam(Some(F_DISPLAY), &ui_fam));
    let mono_fam = fam(have_mono.then_some(F_MONO), &default_mono);
    defs.families.insert(FontFamily::Monospace, mono_fam.clone());
    defs.families.insert(FontFamily::Name(F_MONO.into()), mono_fam);
    ctx.set_fonts(defs);
}

/// Application palette.
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
}

pub const DARK: Palette = Palette {
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
};

pub const LIGHT: Palette = Palette {
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
};

impl Palette {
    /// Card background used behind graphs on the light theme (graphs stay dark-ish for contrast).
    pub fn graph_bg(&self) -> Color32 {
        if self.dark {
            self.card
        } else {
            Color32::from_rgb(0xF2, 0xF4, 0xF6)
        }
    }

    /// Applies the palette to egui's own widgets (combos, sliders, text edits, menus).
    pub fn apply(&self, ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = self.body;
        v.window_fill = self.panel;
        v.window_stroke = Stroke::new(1.0_f32, self.card_stroke);
        v.window_rounding = Rounding::same(6.0);
        v.menu_rounding = Rounding::same(6.0);
        v.extreme_bg_color = self.control;
        v.faint_bg_color = self.hover;
        v.code_bg_color = self.control;
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.noninteractive.bg_fill = self.panel;
        v.widgets.noninteractive.weak_bg_fill = self.panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.panel_stroke);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.text_dim);
        v.widgets.inactive.bg_fill = self.control;
        v.widgets.inactive.weak_bg_fill = self.control;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.control_stroke);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.inactive.rounding = Rounding::same(4.0);
        v.widgets.hovered.bg_fill = self.hover;
        v.widgets.hovered.weak_bg_fill = self.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.hovered.rounding = Rounding::same(4.0);
        v.widgets.active.bg_fill = self.selection;
        v.widgets.active.weak_bg_fill = self.selection;
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.active.rounding = Rounding::same(4.0);
        v.widgets.open.bg_fill = self.control;
        v.widgets.open.weak_bg_fill = self.control;
        v.widgets.open.bg_stroke = Stroke::new(1.0_f32, self.accent);
        v.widgets.open.fg_stroke = Stroke::new(1.0_f32, self.text);
        v.widgets.open.rounding = Rounding::same(4.0);
        v.slider_trailing_fill = true;
        v.popup_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 4.0), blur: 12.0, spread: 0.0, color: Color32::from_black_alpha(120) };
        v.window_shadow = v.popup_shadow;
        v.striped = false;
        style.visuals = v;
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 4.0);
        style.spacing.interact_size = Vec2::new(40.0, 22.0);
        style.spacing.combo_width = 180.0;
        style.spacing.slider_width = 160.0;
        style.spacing.menu_margin = Margin::same(6.0);
        style.spacing.window_margin = Margin::same(10.0);
        style.text_styles = [
            (TextStyle::Small, ui(11.0)),
            (TextStyle::Body, ui(13.0)),
            (TextStyle::Button, ui(13.0)),
            (TextStyle::Heading, display(18.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into_iter()
        .collect();
        ctx.set_style(Arc::new(style));
    }
}

/// Metric accent colours.
pub fn metric_color(key: &str) -> Color32 {
    match key {
        "cpu" | "disk" => Color32::from_rgb(0x1F, 0xC4, 0x48),
        "memory" => Color32::from_rgb(0xC0, 0x40, 0xFF),
        "gpu" | "network" => Color32::from_rgb(0x2F, 0x8B, 0xFF),
        "thermal" => Color32::from_rgb(0xFF, 0x9A, 0x2E),
        "clock" | "power" | "kernel" => Color32::from_rgb(0xFF, 0x3B, 0x3B),
        "yellow" => Color32::from_rgb(0xE8, 0xD0, 0x2A),
        _ => Color32::from_rgb(0x9A, 0x9A, 0x9A),
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

/// Gamma-space mix of two colours.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [r0, g0, b0, a0] = a.to_srgba_unmultiplied();
    let [r1, g1, b1, a1] = b.to_srgba_unmultiplied();
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(r0, r1), m(g0, g1), m(b0, b1), m(a0, a1))
}
