//! Skin-driven popout renderer: header with badge and controls, meter tile (rows / bars /
//! vertical / gauge / graph), per-core strip and the top-processes list, painted with the
//! skin's surfaces, ink, typography, metrics, shape and images.
//!
//! Skin typography sizes are points (1 pt = 4/3 px).

use super::icons::IconCache;
use super::summary::proc_icon;
use super::theme::{self, dim, lighten, mix, with_alpha, Palette};
use super::widgets::{self, draw_graph, draw_icon, ellipse_points, fill_vertical_gradient, polygon_points, rounded_rect_points, GraphLine, Icon, LedStyle};
use super::FrameData;
use crate::metrics::Series;
use crate::process::format_rate;
use crate::skin::{Channel, Color, Fill, FontDesign, FontSpec, FontWeight, Ink, MeterStyle, Quadrant, Skin, SkinSource, Surfaces};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Rounding, Sense, Shape, Stroke, TextureHandle, TextureOptions, Vec2};
use std::path::{Path, PathBuf};

const WIDTH: f32 = 320.0;
const PAD: f32 = 10.0;
/// Points to pixels at 96 DPI.
const PT: f32 = 4.0 / 3.0;
/// Header title used when a skin does not set `header.title`.
pub const DEFAULT_TITLE: &str = "TMOP Popout";

pub fn c32(c: Color, opacity: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, (c.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8)
}

/// Default channel colours of the popout.
pub fn channel_default(q: Quadrant) -> Color32 {
    match q {
        Quadrant::Cpu | Quadrant::Disk => Color32::from_rgb(0x1F, 0xC4, 0x52),
        Quadrant::Memory => Color32::from_rgb(0xE1, 0x29, 0xFF),
        Quadrant::Gpu | Quadrant::Network => Color32::from_rgb(0x29, 0xB5, 0xFF),
        Quadrant::Thermal => Color32::from_rgb(0xFF, 0x9F, 0x0A),
        Quadrant::Power => Color32::from_rgb(0xFF, 0xD6, 0x0A),
    }
}

/// One reading prepared for display.
pub struct Reading {
    pub quadrant: Quadrant,
    pub fraction: Option<f32>,
    pub text: String,
    pub aux: String,
}

pub fn reading(q: Quadrant, fd: &FrameData<'_>, fahrenheit: bool) -> Reading {
    let s = fd.snap;
    let sm = fd.smooth;
    let pct = |v: f32| Some((v / 100.0).clamp(0.0, 1.0));
    match q {
        Quadrant::Cpu => Reading { quadrant: q, fraction: pct(sm.cpu), text: format!("{:.0}%", s.cpu.total_pct), aux: s.cpu.current_mhz.map(|m| format!("{:.2} GHz", m as f32 / 1000.0)).unwrap_or_default() },
        Quadrant::Memory => Reading {
            quadrant: q,
            fraction: pct(sm.memory),
            text: format!("{:.0}%", s.memory.used_pct),
            aux: format!("{:.1}/{:.1} GB", (s.memory.total - s.memory.available) as f64 / 1073741824.0, s.memory.total as f64 / 1073741824.0),
        },
        Quadrant::Gpu => Reading {
            quadrant: q,
            fraction: if s.gpu.available { pct(sm.gpu) } else { None },
            text: if s.gpu.available { format!("{:.0}%", s.gpu.utilization_pct) } else { "N/A".into() },
            aux: if s.gpu.dedicated_used > 0 { format!("{:.1} GB", s.gpu.dedicated_used as f64 / 1073741824.0) } else { String::new() },
        },
        Quadrant::Disk => Reading { quadrant: q, fraction: pct(sm.disk), text: format!("{:.0}%", s.disk.active_pct), aux: format!("R {}  W {}", format_rate(s.disk.read_bps), format_rate(s.disk.write_bps)) },
        Quadrant::Network => Reading {
            quadrant: q,
            fraction: pct(sm.net),
            text: format_rate(s.network.rx_bps + s.network.tx_bps),
            aux: format!("\u{2193}{} \u{2191}{}", format_rate(s.network.rx_bps), format_rate(s.network.tx_bps)),
        },
        Quadrant::Thermal => match s.thermal.max_c() {
            Some(c) => Reading {
                quadrant: q,
                fraction: Some((sm.thermal / 100.0).clamp(0.0, 1.0)),
                text: if fahrenheit { format!("{:.0}\u{b0}F", c * 9.0 / 5.0 + 32.0) } else { format!("{c:.0}\u{b0}C") },
                aux: "ACPI zone".into(),
            },
            None => Reading { quadrant: q, fraction: None, text: "N/A".into(), aux: "no sensor".into() },
        },
        Quadrant::Power => Reading { quadrant: q, fraction: None, text: "N/A".into(), aux: String::new() },
    }
}

fn series_for<'a>(fd: &'a FrameData<'_>, q: Quadrant, scratch: &'a mut Series) -> (&'a Series, f32) {
    let h = fd.hist;
    match q {
        Quadrant::Cpu => (&h.cpu, 100.0),
        Quadrant::Memory => (&h.memory, 100.0),
        Quadrant::Gpu => (&h.gpu, 100.0),
        Quadrant::Disk => (&h.disk, 100.0),
        Quadrant::Thermal => (&h.thermal, 100.0),
        Quadrant::Network | Quadrant::Power => {
            *scratch = Series::new(h.net_rx.capacity);
            for (a, b) in h.net_rx.values.iter().zip(&h.net_tx.values) {
                scratch.push(a + b);
            }
            scratch.pushed = h.net_rx.pushed;
            let m = scratch.max().max(1024.0);
            (scratch, m)
        }
    }
}

/// Font for a skin typography slot; `default_pt` is in points.
fn font(spec: Option<&FontSpec>, default_pt: f32, default_weight: FontWeight) -> FontId {
    let size = spec.and_then(|f| f.size).unwrap_or(default_pt) * PT;
    let weight = spec.and_then(|f| f.weight).unwrap_or(default_weight);
    match spec.and_then(|f| f.design) {
        Some(FontDesign::Monospaced) => theme::mono(size),
        _ => match weight {
            FontWeight::Bold | FontWeight::Heavy | FontWeight::Black => theme::bold(size),
            FontWeight::Semibold | FontWeight::Medium => theme::semibold(size),
            _ => theme::ui(size),
        },
    }
}

/// Loaded skin images (textures live in the egui context).
#[derive(Default)]
pub struct Textures {
    loaded_for: Option<String>,
    logo: Option<(TextureHandle, Vec2)>,
    window: Option<(TextureHandle, Vec2)>,
    header: Option<(TextureHandle, Vec2)>,
}

fn load_png(ctx: &egui::Context, path: &Path, name: &str) -> Option<(TextureHandle, Vec2)> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::Grayscale => buf[..w * h].iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Indexed => return None,
    };
    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    Some((ctx.load_texture(name, image, TextureOptions::NEAREST), vec2(w as f32, h as f32)))
}

fn skin_file(skin: &Skin, file: &str) -> Option<PathBuf> {
    let dir = match &skin.source {
        SkinSource::Directory(d) => d.clone(),
        SkinSource::File(f) => f.parent()?.to_path_buf(),
        SkinSource::BuiltIn => return None,
    };
    let p = dir.join(file);
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    p.is_file().then_some(p)
}

struct ImageSpec {
    file: String,
    mode: String,
    opacity: f32,
}

fn image_spec(v: Option<&serde_json::Value>) -> Option<ImageSpec> {
    let v = v?;
    let file = v.get("file")?.as_str()?.to_string();
    let mode = v.get("mode").or(v.get("fit")).and_then(|m| m.as_str()).unwrap_or("fill").to_string();
    let opacity = v.get("opacity").and_then(|o| o.as_f64()).unwrap_or(1.0) as f32;
    Some(ImageSpec { file, mode, opacity })
}

impl Textures {
    fn ensure(&mut self, ctx: &egui::Context, skin: &Skin) {
        if self.loaded_for.as_deref() == Some(&skin.id) {
            return;
        }
        self.loaded_for = Some(skin.id.clone());
        self.logo = skin.logo_path().filter(|p| p.is_file()).and_then(|p| load_png(ctx, &p, "popout-logo"));
        let surf = skin.surfaces.clone().unwrap_or_default();
        self.window = image_spec(surf.window_image.as_ref()).and_then(|s| skin_file(skin, &s.file)).and_then(|p| load_png(ctx, &p, "popout-window-image"));
        self.header = image_spec(surf.header_image.as_ref()).and_then(|s| skin_file(skin, &s.file)).and_then(|p| load_png(ctx, &p, "popout-header-image"));
    }
}

fn draw_image(painter: &egui::Painter, tex: &(TextureHandle, Vec2), rect: Rect, mode: &str, opacity: f32) {
    let (handle, size) = tex;
    let tint = Color32::from_white_alpha((opacity.clamp(0.0, 1.0) * 255.0) as u8);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let p = painter.with_clip_rect(rect);
    match mode {
        "stretch" => {
            p.image(handle.id(), rect, uv, tint);
        }
        "tile" => {
            let mut y = rect.top();
            while y < rect.bottom() {
                let mut x = rect.left();
                while x < rect.right() {
                    p.image(handle.id(), Rect::from_min_size(pos2(x, y), *size), uv, tint);
                    x += size.x.max(1.0);
                }
                y += size.y.max(1.0);
            }
        }
        "fit" => {
            let s = (rect.width() / size.x).min(rect.height() / size.y);
            p.image(handle.id(), Rect::from_center_size(rect.center(), *size * s), uv, tint);
        }
        _ => {
            let s = (rect.width() / size.x).max(rect.height() / size.y);
            p.image(handle.id(), Rect::from_center_size(rect.center(), *size * s), uv, tint);
        }
    }
}

/// Resolved paint for a skin with defaults from the app's own popout colours.
struct Paint {
    opacity: f32,
    window: (Color32, Color32),
    window_stroke: Option<Color32>,
    header: (Color32, Color32),
    header_rule: Option<Color32>,
    tile: (Color32, Color32),
    tile_stroke: Option<Color32>,
    tile_gloss: Option<(Color32, Color32)>,
    badge_stroke: Color32,
    title: Color32,
    primary: Color32,
    secondary: Color32,
    control: Color32,
    badge: Color32,
    rule: Color32,
    glow: Option<Color32>,
}

fn fill_pair(f: Option<&Fill>, op: f32) -> Option<(Color32, Color32)> {
    let f = f?;
    Some((c32(f.top()?, op), c32(f.bottom()?, op)))
}

fn paint(skin: &Skin, op: f32) -> Paint {
    let s = skin.surfaces.clone().unwrap_or_default();
    let ink = skin.ink.clone().unwrap_or_default();
    let win = with_alpha(Color32::from_rgb(0x1E, 0x1E, 0x1E), op);
    let tile = with_alpha(Color32::from_rgb(0x1F, 0x1F, 0x1F), op);
    let ic = |c: Option<Color>, d: Color32| c.map(|c| c32(c, op)).unwrap_or(with_alpha(d, op));
    let stroke = |f: Option<&Fill>| f.and_then(|f| f.top()).map(|c| c32(c, op));
    Paint {
        opacity: op,
        window: fill_pair(s.window.as_ref(), op).unwrap_or((win, win)),
        window_stroke: stroke(s.window_stroke.as_ref()).or(Some(with_alpha(Color32::from_rgb(0x4A, 0x4A, 0x4C), op))),
        header: fill_pair(s.header.as_ref(), op).unwrap_or((win, win)),
        header_rule: stroke(s.header_rule.as_ref()),
        tile: fill_pair(s.tile.as_ref(), op).unwrap_or((tile, tile)),
        tile_stroke: stroke(s.tile_stroke.as_ref()).or(Some(with_alpha(Color32::from_rgb(0x3C, 0x3C, 0x3E), op))),
        tile_gloss: fill_pair(s.tile_gloss.as_ref(), op),
        badge_stroke: stroke(s.badge_stroke.as_ref()).unwrap_or(with_alpha(Color32::from_rgb(0x5B, 0x5B, 0x60), op)),
        title: ic(ink.title, Color32::from_rgb(0xEB, 0xEB, 0xF0)),
        primary: ic(ink.primary, Color32::from_rgb(0xEB, 0xEB, 0xF0)),
        secondary: ic(ink.secondary, Color32::from_rgb(0xA0, 0xA0, 0xA8)),
        control: ic(ink.control, Color32::from_rgb(0xD0, 0xD0, 0xD6)),
        badge: ic(ink.badge, Color32::from_rgb(0x30, 0xE1, 0x67)),
        rule: ic(ink.rule, Color32::from_rgb(0x43, 0x43, 0x43)),
        glow: ink.glow.map(|c| c32(c, op)),
    }
}

/// (accent, lit, unlit) for a quadrant.
fn accent(skin: &Skin, q: Quadrant, op: f32) -> (Color32, Color32, Color32) {
    let ch = skin.channel(q);
    let fallback = channel_default(q);
    let accent = ch.accent.map(|c| c32(c, op)).unwrap_or(with_alpha(fallback, op));
    let lit = ch.lit.map(|c| c32(c, op)).unwrap_or(accent);
    let unlit = ch.unlit.map(|c| c32(c, op)).unwrap_or(dim(lit, 0.21));
    (accent, lit, unlit)
}

/// Top-process heat band for a row at `t` (0..1 of the busiest): the tile tinted towards the
/// badge colour, so row text keeps the contrast it has on the tile.
fn proc_band(p: &Paint, t: f32) -> Color32 {
    let [r, g, b, _] = p.tile.1.to_srgba_unmultiplied();
    mix(Color32::from_rgb(r, g, b), p.badge, 0.10 + 0.14 * t)
}

/// Positional palette: lit segments shade towards the skin's `alert` accent near the top.
fn positional(skin: &Skin, lit: Color32, op: f32) -> [Color32; 3] {
    let alert = skin.meters.as_ref().and_then(|m| m.channels.get("alert")).and_then(|c| c.accent).map(|c| c32(c, op)).unwrap_or(Color32::from_rgb(0xE8, 0x5A, 0x50));
    [lit, mix(lit, alert, 0.35), alert]
}

/// Layout of the popout for a skin.
pub struct Layout {
    pub columns: usize,
    pub tile: Vec2,
    pub header_h: f32,
    pub header_gap: f32,
    pub pad: f32,
    pub gap: f32,
    pub meters_h: f32,
    pub cores_h: f32,
    pub procs_rows: usize,
    pub row_h: f32,
    pub size: Vec2,
    pub content: Rect,
}

fn window_kind(skin: &Skin) -> (String, u32, f32) {
    let sh = skin.shape.as_ref();
    let kind = sh.and_then(|s| s.get("kind")).and_then(|k| k.as_str()).unwrap_or("rectangle").to_ascii_lowercase();
    let sides = sh.and_then(|s| s.get("sides")).and_then(|v| v.as_u64()).unwrap_or(6) as u32;
    let rot = sh.and_then(|s| s.get("rotation")).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    (kind, sides, rot)
}

pub fn layout(skin: &Skin) -> Layout {
    let m = skin.metrics.clone().unwrap_or_default();
    let typo = skin.typography.clone().unwrap_or_default();
    let style = skin.meter_style();
    let n = skin.resolved_quadrants().len().max(1);
    let pad = m.padding.unwrap_or(PAD);
    let columns = skin
        .meters
        .as_ref()
        .and_then(|m| m.columns)
        .map(|c| c as usize)
        .unwrap_or(match style {
            MeterStyle::Vertical => n.min(4),
            MeterStyle::Gauge => 2,
            MeterStyle::Graph | MeterStyle::Bars => 1,
            MeterStyle::Rows => 2,
        })
        .clamp(1, n);
    let label_px = typo.label.as_ref().and_then(|f| f.size).unwrap_or(10.0) * PT;
    let graph_h = skin.meters.as_ref().and_then(|m| m.graph.as_ref()).and_then(|g| g.height).unwrap_or(44.0);
    let inner_w = WIDTH - pad * 2.0 - 12.0;
    let tile = match style {
        MeterStyle::Rows => vec2(inner_w / columns as f32, (label_px + 3.0).max(16.0)),
        MeterStyle::Bars => vec2(inner_w / columns as f32, label_px + 18.0),
        MeterStyle::Vertical => vec2(inner_w / columns as f32, 118.0),
        MeterStyle::Gauge => vec2(inner_w / columns as f32, 96.0),
        MeterStyle::Graph => vec2(inner_w / columns as f32, graph_h + label_px + 10.0),
    };
    let rows = n.div_ceil(columns);
    let meters_h = rows as f32 * tile.y + 14.0;
    let cores_h = if skin.resolved_quadrants().contains(&Quadrant::Cpu) { 14.0 } else { 0.0 };
    let row_px = typo.row.as_ref().and_then(|f| f.size).unwrap_or(10.0) * PT;
    let row_h = (row_px + 7.0).max(20.0);
    let procs_rows = 9;
    let heading_px = typo.heading.as_ref().and_then(|f| f.size).unwrap_or(11.0) * PT;
    let procs_h = heading_px + 16.0 + procs_rows as f32 * row_h + 10.0;
    let gap = m.section_spacing.unwrap_or(8.0);
    let header_h = 26.0 + m.header_bottom_padding.unwrap_or(7.0);
    let header_gap = m.header_gap.unwrap_or(6.0);
    let mut h = header_h + header_gap + meters_h + gap + procs_h + pad;
    if cores_h > 0.0 {
        h += cores_h + gap;
    }
    let h = h.round();
    let (kind, _, _) = window_kind(skin);
    let size = match kind.as_str() {
        "circle" | "ellipse" | "oval" => vec2(WIDTH * 1.35, h * 1.35),
        "polygon" | "capsule" | "pill" => vec2(WIDTH * 1.15, h * 1.15),
        _ => vec2(WIDTH, h),
    };
    let content = match kind.as_str() {
        "circle" | "ellipse" | "oval" | "polygon" | "capsule" | "pill" => Rect::from_center_size(pos2(size.x / 2.0, size.y / 2.0), vec2(WIDTH, h)),
        _ => Rect::from_min_size(Pos2::ZERO, size),
    };
    Layout { columns, tile, header_h, header_gap, pad, gap, meters_h, cores_h, procs_rows, row_h, size, content }
}

pub fn desired_size(skin: &Skin) -> Vec2 {
    layout(skin).size
}

/// What the user did in the popout chrome.
#[derive(Default)]
pub struct PopoutEvents {
    pub close: bool,
    pub drag: bool,
    pub menu: bool,
    /// The chevron, for anchoring the skin menu.
    pub chevron: Option<egui::Response>,
}

/// Draws the whole popout into `rect`.
#[allow(clippy::too_many_arguments)]
pub fn draw(ui: &mut egui::Ui, rect: Rect, skin: &Skin, fd: &FrameData<'_>, opacity: f32, fahrenheit: bool, tex: &mut Textures, icons: &mut IconCache) -> PopoutEvents {
    tex.ensure(ui.ctx(), skin);
    let mut ev = PopoutEvents::default();
    let painter = ui.painter_at(rect);
    let p = paint(skin, opacity);
    let m = skin.metrics.clone().unwrap_or_default();
    let typo = skin.typography.clone().unwrap_or_default();
    let l = layout(skin);
    let radius = m.corner_radius.unwrap_or(8.0);
    let border = m.border_width.unwrap_or(1.0);
    let pad = l.pad;
    let bloom = fd.bloom;
    let surf = skin.surfaces.clone().unwrap_or_default();

    // Window shape.
    let (kind, sides, rot) = window_kind(skin);
    let win_pts = match kind.as_str() {
        "circle" => ellipse_points(Rect::from_center_size(rect.center(), Vec2::splat(rect.width().min(rect.height()))), 96),
        "ellipse" | "oval" => ellipse_points(rect, 96),
        "capsule" | "pill" => rounded_rect_points(rect, Rounding::same(rect.height().min(rect.width()) / 2.0)),
        "polygon" => polygon_points(rect, sides as usize, rot),
        _ => rounded_rect_points(rect, Rounding::same(radius)),
    };
    fill_vertical_gradient(&painter, &win_pts, p.window.0, p.window.1);
    if let (Some(img), Some(spec)) = (&tex.window, image_spec(surf.window_image.as_ref())) {
        draw_image(&painter, img, rect, &spec.mode, spec.opacity * opacity);
    }
    let content = Rect::from_min_size(rect.min + l.content.min.to_vec2(), l.content.size());
    // Header band.
    let header = Rect::from_min_size(content.min, vec2(content.width(), l.header_h));
    let rounded_rect = matches!(kind.as_str(), "rectangle");
    if p.header != p.window || tex.header.is_some() {
        let hr = if rounded_rect { header.shrink2(vec2(border, 0.0)).with_min_y(header.top() + border) } else { header.shrink(4.0) };
        let r = if rounded_rect { Rounding { nw: (radius - border).max(0.0), ne: (radius - border).max(0.0), sw: 0.0, se: 0.0 } } else { Rounding::same(6.0) };
        fill_vertical_gradient(&painter, &rounded_rect_points(hr, r), p.header.0, p.header.1);
    }
    if let (Some(img), Some(spec)) = (&tex.header, image_spec(surf.header_image.as_ref())) {
        draw_image(&painter, img, header, &spec.mode, spec.opacity * opacity);
    }
    if let Some(rule) = p.header_rule {
        painter.line_segment([pos2(header.left() + border, header.bottom() - 0.5), pos2(header.right() - border, header.bottom() - 0.5)], Stroke::new(1.0_f32, rule));
    }
    // Badge (logo box) + title.
    let mut x = header.left() + pad;
    let hc = header.center().y - (m.header_bottom_padding.unwrap_or(7.0) - 7.0) / 2.0;
    match &tex.logo {
        Some(img) => {
            let lw = skin.header.as_ref().and_then(|h| h.logo.as_ref()).and_then(|l| l.width).unwrap_or(16.0);
            let lh = skin.header.as_ref().and_then(|h| h.logo.as_ref()).and_then(|l| l.height).unwrap_or(16.0);
            painter.image(img.0.id(), Rect::from_min_size(pos2(x, hc - lh / 2.0), vec2(lw, lh)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha((opacity * 255.0) as u8));
            x += lw + 6.0;
        }
        None => {
            let badge = Rect::from_min_size(pos2(x, hc - 11.5), vec2(23.0, 23.0));
            let br = m.badge_corner_radius.unwrap_or(3.0);
            painter.rect_stroke(badge, br, Stroke::new(1.0_f32, p.badge_stroke));
            draw_icon(&painter, badge.shrink2(vec2(3.5, 6.0)), Icon::Pulse, p.title);
            x = badge.right() + 10.0;
        }
    }
    let hide_title = skin.header.as_ref().and_then(|h| h.hide_title).unwrap_or(false);
    if !hide_title {
        let title = skin.header.as_ref().and_then(|h| h.title.clone()).unwrap_or_else(|| DEFAULT_TITLE.to_string());
        let tf = font(typo.title.as_ref(), 11.0, FontWeight::Bold);
        let tracking = typo.title_tracking.unwrap_or(0.0);
        if tracking > 0.05 {
            let mut cx = x;
            for ch in title.chars() {
                let r = painter.text(pos2(cx, hc), Align2::LEFT_CENTER, ch, tf.clone(), p.title);
                cx += r.width() + tracking * PT;
            }
        } else {
            painter.text(pos2(x, hc), Align2::LEFT_CENTER, title, tf, p.title);
        }
    }
    // Controls: chevron (skin menu) and close.
    let close = Rect::from_center_size(pos2(header.right() - pad - 8.0, hc), Vec2::splat(20.0));
    let chev = Rect::from_center_size(close.center() - vec2(26.0, 0.0), Vec2::splat(20.0));
    let close_resp = ui.interact(close, ui.id().with("popout-close"), Sense::click());
    let chev_resp = ui.interact(chev, ui.id().with("popout-chevron"), Sense::click());
    let ctl = |hovered: bool| if hovered { p.title } else { p.control };
    painter.line_segment([close.left_top() + vec2(4.0, 4.0), close.right_bottom() - vec2(4.0, 4.0)], Stroke::new(1.6_f32, ctl(close_resp.hovered())));
    painter.line_segment([close.right_top() + vec2(-4.0, 4.0), close.left_bottom() + vec2(4.0, -4.0)], Stroke::new(1.6_f32, ctl(close_resp.hovered())));
    painter.add(Shape::line(vec![chev.left_center() + vec2(3.0, -3.5), chev.center() + vec2(0.0, 3.5), chev.right_center() + vec2(-3.0, -3.5)], Stroke::new(1.4_f32, ctl(chev_resp.hovered()))));
    ev.close = close_resp.clicked();
    ev.menu = chev_resp.clicked();
    ev.chevron = Some(chev_resp);
    let drag_rect = Rect::from_min_max(header.min, pos2(chev.left() - 2.0, header.bottom()));
    let drag = ui.interact(drag_rect, ui.id().with("popout-drag"), egui::Sense::click_and_drag());
    ev.drag = drag.drag_started();

    // Tiles.
    let tile_r = m.tile_corner_radius.unwrap_or(6.0);
    let tile_sw = m.tile_stroke_width.unwrap_or(1.0);
    let tile_bg = |r: Rect| {
        fill_vertical_gradient(&painter, &rounded_rect_points(r, Rounding::same(tile_r)), p.tile.0, p.tile.1);
        if let Some((g0, g1)) = p.tile_gloss {
            let gloss = Rect::from_min_size(r.min, vec2(r.width(), r.height() * 0.5));
            fill_vertical_gradient(&painter, &rounded_rect_points(gloss, Rounding { nw: tile_r, ne: tile_r, sw: 0.0, se: 0.0 }), g0, g1);
        }
        if let Some(ts) = p.tile_stroke {
            painter.rect_stroke(r, tile_r, Stroke::new(tile_sw, ts));
        }
    };
    let mut y = header.bottom() + l.header_gap;
    let tile_x0 = content.left() + pad;
    let tile_w = content.width() - pad * 2.0;
    // Meter tile.
    let meters = Rect::from_min_size(pos2(tile_x0, y), vec2(tile_w, l.meters_h));
    tile_bg(meters);
    let readings: Vec<Reading> = skin.resolved_quadrants().into_iter().map(|q| reading(q, fd, fahrenheit)).collect();
    let style = skin.meter_style();
    let mut scratch = Series::new(2);
    let positional_on = skin.meters.as_ref().and_then(|m| m.positional_palette).unwrap_or(false);
    let label_f = font(typo.label.as_ref(), 10.0, FontWeight::Semibold);
    let value_f = font(typo.value.as_ref(), 10.0, FontWeight::Semibold);
    let aux_f = font(typo.aux_line.as_ref(), 8.0, FontWeight::Regular);
    let show_aux = skin.meters.as_ref().and_then(|m| m.auxiliary).unwrap_or(false);
    // Rows: one label column width for every row so the bars line up.
    let label_w = readings.iter().map(|r| widgets::text_width(&painter, r.quadrant.label(), label_f.clone())).fold(0.0, f32::max) + 12.0;
    for (i, r) in readings.iter().enumerate() {
        let col = i % l.columns;
        let row = i / l.columns;
        let cell = Rect::from_min_size(pos2(meters.left() + 8.0 + col as f32 * l.tile.x, meters.top() + 7.0 + row as f32 * l.tile.y), l.tile);
        let (acc, lit, unlit) = accent(skin, r.quadrant, opacity);
        let mut led = LedStyle::new(lit, bloom);
        led.unlit_factor = 1.0;
        led.rim = true;
        led.rounding = 1.5;
        led.gap = 2.5;
        if positional_on {
            led.positional = Some(positional(skin, lit, opacity));
        }
        match style {
            MeterStyle::Rows => {
                painter.text(pos2(cell.left() + 2.0, cell.center().y), Align2::LEFT_CENTER, r.quadrant.label(), label_f.clone(), acc);
                let bar = Rect::from_min_max(pos2(cell.left() + label_w, cell.center().y - 3.5), pos2(cell.right() - 10.0, cell.center().y + 3.5));
                led_bar(&painter, bar, 9, r.fraction.unwrap_or(0.0), &led, unlit);
            }
            MeterStyle::Bars => {
                painter.text(cell.left_top(), Align2::LEFT_TOP, r.quadrant.label(), label_f.clone(), p.secondary);
                painter.text(pos2(cell.right() - 8.0, cell.top()), Align2::RIGHT_TOP, &r.text, value_f.clone(), p.primary);
                if show_aux && !r.aux.is_empty() {
                    painter.text(pos2(cell.left() + label_w, cell.top() + 1.0), Align2::LEFT_TOP, &r.aux, aux_f.clone(), p.secondary);
                }
                let top = cell.top() + label_f.size + 3.0;
                let bar = Rect::from_min_max(pos2(cell.left(), top), pos2(cell.right() - 8.0, top + 10.0));
                led_bar(&painter, bar, 24, r.fraction.unwrap_or(0.0), &led, unlit);
            }
            MeterStyle::Vertical => draw_vertical(skin, &painter, cell, r, lit, unlit, &p, &label_f, &led),
            MeterStyle::Graph => {
                painter.text(cell.left_top(), Align2::LEFT_TOP, r.quadrant.label(), label_f.clone(), p.secondary);
                painter.text(pos2(cell.right() - 8.0, cell.top() - 1.0), Align2::RIGHT_TOP, &r.text, value_f.clone(), acc);
                let gr = Rect::from_min_max(pos2(cell.left(), cell.top() + label_f.size + 4.0), pos2(cell.right() - 8.0, cell.bottom() - 4.0));
                let shows_grid = skin.meters.as_ref().and_then(|m| m.graph.as_ref()).and_then(|g| g.shows_grid).unwrap_or(true);
                let (series, max) = series_for(fd, r.quadrant, &mut scratch);
                let max = widgets::eased(painter.ctx(), ("popout-scale", r.quadrant.label()), max);
                let mut g = widgets::Graph::new(vec![GraphLine { series, color: acc, range: (0.0, max) }]);
                g.px_per_update = 3.0;
                g.phase = fd.phase;
                g.glide = fd.glide;
                g.regions = 1;
                g.bloom = bloom;
                g.grid = shows_grid.then(|| with_alpha(p.rule, 0.9));
                g.border = Some(with_alpha(p.rule, 0.9));
                g.fill = 0.18;
                g.line_width = 1.5;
                g.markers = false;
                draw_graph(&painter, gr, &g);
            }
            MeterStyle::Gauge => draw_gauge(skin, &painter, cell, r, acc, unlit, &p, &label_f, &value_f, bloom),
        }
    }
    y += l.meters_h + l.gap;
    // Per-core strip.
    if l.cores_h > 0.0 {
        let strip = Rect::from_min_size(pos2(tile_x0, y), vec2(tile_w, l.cores_h));
        tile_bg(strip);
        let cores = &fd.smooth.per_core;
        if !cores.is_empty() {
            let (acc, lit, unlit) = accent(skin, Quadrant::Cpu, opacity);
            let n = cores.len();
            let w = 4.5;
            let pitch = 6.0;
            let x0 = strip.center().x - (n as f32 * pitch - (pitch - w)) / 2.0;
            for (i, v) in cores.iter().enumerate() {
                let r = Rect::from_min_size(pos2(x0 + i as f32 * pitch, strip.center().y - w / 2.0), vec2(w, w));
                let t = (v / 100.0).clamp(0.0, 1.0);
                let c = if t > 0.03 { mix(dim(lit, 0.45), lit, (t * 3.0).min(1.0)) } else { mix(unlit, Color32::BLACK, 0.5) };
                painter.rect_filled(r, 0.5, c);
                if t > 0.4 && bloom > 0.0 {
                    painter.rect_filled(r.expand(1.5), 1.5, with_alpha(acc, 0.18 * bloom));
                }
            }
        }
        y += l.cores_h + l.gap;
    }
    // Top processes.
    let procs = Rect::from_min_max(pos2(tile_x0, y), pos2(tile_x0 + tile_w, content.bottom() - pad));
    tile_bg(procs);
    let heading_f = font(typo.heading.as_ref(), 11.0, FontWeight::Bold);
    let row_f = font(typo.row.as_ref(), 10.0, FontWeight::Regular);
    let rowv_f = font(typo.row_value.as_ref(), 10.0, FontWeight::Bold);
    let hy = procs.top() + 6.0 + heading_f.size * 0.6;
    painter.text(pos2(procs.left() + 8.0, hy), Align2::LEFT_CENTER, format!("TOP {} PROCESSES", l.procs_rows), heading_f.clone(), p.badge);
    painter.text(pos2(procs.right() - 8.0, hy), Align2::RIGHT_CENTER, "CPU", heading_f.clone(), p.secondary);
    let rule_y = (procs.top() + heading_f.size + 14.0).round() + 0.5;
    painter.line_segment([pos2(procs.left() + 8.0, rule_y), pos2(procs.right() - 8.0, rule_y)], Stroke::new(1.0_f32, p.rule));
    let me = std::process::id();
    let mut rows: Vec<&crate::process::ProcessRow> = fd.rows.iter().filter(|r| r.pid != 0 && (fd.show_self || r.pid != me)).collect();
    rows.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal));
    let top = rows.first().map(|r| r.cpu_pct).unwrap_or(0.0).max(0.5);
    for (i, r) in rows.iter().take(l.procs_rows).enumerate() {
        let rr = Rect::from_min_size(pos2(procs.left() + 6.0, rule_y + 5.0 + i as f32 * l.row_h), vec2(procs.width() - 12.0, l.row_h - 2.0));
        if r.cpu_pct > 0.0 {
            painter.rect_filled(rr, 2.0, with_alpha(proc_band(&p, (r.cpu_pct / top).min(1.0)), opacity));
        }
        let ic = Rect::from_center_size(pos2(rr.left() + 11.0, rr.center().y), vec2(16.0, 16.0));
        match icons.for_process(r.pid, r.path.as_deref()) {
            Some(t) => {
                painter.image(t, ic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha((opacity * 255.0) as u8));
            }
            None => proc_icon(&painter, ic.shrink(2.0), r.has_window),
        }
        widgets::text_fit(&painter, pos2(rr.left() + 24.0, rr.center().y), Align2::LEFT_CENTER, &r.name, row_f.clone(), p.primary, rr.width() - 80.0);
        painter.text(pos2(rr.right() - 3.0, rr.center().y), Align2::RIGHT_CENTER, format!("{:.1}%", r.cpu_pct), rowv_f.clone(), if r.cpu_pct > 0.0 { p.badge } else { p.secondary });
    }
    // Window outline (and optional glow).
    if let Some(g) = p.glow {
        if bloom > 0.0 {
            let mut pts = win_pts.clone();
            pts.push(win_pts[0]);
            painter.add(Shape::line(pts, Stroke::new(6.0_f32, with_alpha(g, 0.18 * bloom))));
        }
    }
    if let Some(ws) = p.window_stroke {
        let mut pts = win_pts.clone();
        pts.push(win_pts[0]);
        painter.add(Shape::line(pts, Stroke::new(border, ws)));
    }
    let _ = p.opacity;
    ev
}

/// Segmented LED bar with explicit unlit colour (skins give `unlit` directly), a soft
/// glow under the lit run and a lighter top edge on lit segments.
fn led_bar(painter: &egui::Painter, bar: Rect, n: usize, frac: f32, style: &LedStyle, unlit: Color32) {
    let seg_w = (bar.width() - style.gap * (n as f32 - 1.0)) / n as f32;
    let lit_n = ((frac.clamp(0.0, 1.0) * n as f32).round() as usize).min(n);
    if lit_n > 0 && style.bloom > 0.0 {
        let right = bar.left() + lit_n as f32 * (seg_w + style.gap) - style.gap;
        let span = Rect::from_min_max(bar.min, pos2(right, bar.bottom()));
        let c = led_color_at(style, 0.0);
        painter.rect_filled(span.expand(5.0 * style.bloom), 5.0, with_alpha(c, 0.06 * style.bloom));
        painter.rect_filled(span.expand(2.5 * style.bloom), 3.0, with_alpha(c, 0.12 * style.bloom));
    }
    for i in 0..n {
        let x0 = bar.left() + i as f32 * (seg_w + style.gap);
        let r = Rect::from_min_max(pos2(x0, bar.top()), pos2(x0 + seg_w, bar.bottom()));
        if i < lit_n {
            let c = led_color_at(style, (i as f32 + 0.5) / n as f32);
            painter.rect_filled(r, style.rounding, c);
            if style.rim && r.height() >= 4.0 {
                painter.line_segment([pos2(r.left() + 1.0, r.top() + 0.5), pos2(r.right() - 1.0, r.top() + 0.5)], Stroke::new(1.0_f32, lighten(c, 0.45)));
            }
        } else {
            painter.rect_filled(r, style.rounding, unlit);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_vertical(skin: &Skin, painter: &egui::Painter, cell: Rect, r: &Reading, lit: Color32, unlit: Color32, p: &Paint, label_f: &FontId, led: &LedStyle) {
    let meters = skin.meters.clone().unwrap_or_default();
    let op = p.opacity;
    let ground = meters.ground.map(|c| c32(c, op)).unwrap_or(with_alpha(Color32::BLACK, op));
    let inset = meters.inset.clone().unwrap_or_default();
    let inner = cell.shrink2(vec2(4.0, 0.0));
    painter.text(pos2(inner.center().x, inner.top()), Align2::CENTER_TOP, r.quadrant.label(), label_f.clone(), p.primary);
    let well = Rect::from_min_max(pos2(inner.left(), inner.top() + label_f.size + 4.0), pos2(inner.right(), inner.bottom() - label_f.size - 6.0));
    painter.rect_filled(well, 0.0, ground);
    if meters.flat != Some(true) {
        painter.rect_stroke(well, 0.0, Stroke::new(1.0_f32, with_alpha(Color32::WHITE, 0.15 * op)));
    } else {
        // Etched: dark top-left, light bottom-right (the classic sunken box).
        painter.line_segment([well.left_bottom(), well.left_top()], Stroke::new(1.0_f32, p.rule));
        painter.line_segment([well.left_top(), well.right_top()], Stroke::new(1.0_f32, p.rule));
        painter.line_segment([well.right_top(), well.right_bottom()], Stroke::new(1.0_f32, with_alpha(Color32::WHITE, op)));
        painter.line_segment([well.right_bottom(), well.left_bottom()], Stroke::new(1.0_f32, with_alpha(Color32::WHITE, op)));
    }
    let leds = Rect::from_min_max(well.min + vec2(inset.leading.unwrap_or(8.0), inset.top.unwrap_or(6.0)), well.max - vec2(inset.trailing.unwrap_or(8.0), inset.bottom.unwrap_or(3.0)));
    let split = meters.split.unwrap_or(2).clamp(1, 8) as usize;
    let segs = meters.segments.unwrap_or(10).clamp(2, 64) as usize;
    let frac = r.fraction.unwrap_or(0.0);
    let col_w = leds.width() / split as f32;
    let lit_n = ((frac * segs as f32).round() as usize).min(segs);
    let seg_h = (leds.height() - 2.0 * (segs as f32 - 1.0)) / segs as f32;
    for c in 0..split {
        let col = Rect::from_min_max(pos2(leds.left() + c as f32 * col_w + 1.0, leds.top()), pos2(leds.left() + (c + 1) as f32 * col_w - 1.0, leds.bottom()));
        for s in 0..segs {
            let y1 = col.bottom() - s as f32 * (seg_h + 2.0);
            let rr = Rect::from_min_max(pos2(col.left(), y1 - seg_h), pos2(col.right(), y1));
            let t = (s as f32 + 0.5) / segs as f32;
            let color = if s < lit_n { led.positional.map(|_| led_color_at(led, t)).unwrap_or(lit) } else { unlit };
            painter.rect_filled(rr, 0.0, color);
        }
    }
    painter.text(pos2(well.center().x, inner.bottom()), Align2::CENTER_BOTTOM, &r.text, label_f.clone(), p.primary);
}

fn led_color_at(led: &LedStyle, t: f32) -> Color32 {
    match led.positional {
        Some([a, b, c]) => {
            if t < 0.5 {
                mix(a, b, t * 2.0)
            } else {
                mix(b, c, (t - 0.5) * 2.0)
            }
        }
        None => led.lit,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_gauge(skin: &Skin, painter: &egui::Painter, cell: Rect, r: &Reading, acc: Color32, unlit: Color32, p: &Paint, label_f: &FontId, value_f: &FontId, bloom: f32) {
    let g = skin.meters.as_ref().and_then(|m| m.gauge.as_ref());
    let num = |k: &str, d: f32| g.and_then(|g| g.get(k)).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(d);
    let col = |k: &str, d: Color32| g.and_then(|g| g.get(k)).and_then(|v| v.as_str()).and_then(Color::parse).map(|c| c32(c, p.opacity)).unwrap_or(d);
    let inner = cell.shrink(4.0);
    let center = inner.center() + vec2(0.0, 6.0);
    let radius = num("radius", inner.width().min(inner.height()) * 0.42);
    let start = num("startAngle", 135.0);
    let sweep = num("sweepAngle", 270.0);
    let arc = |from: f32, to: f32, rad: f32| -> Vec<Pos2> {
        (0..=40)
            .map(|i| {
                let a = (start + sweep * (from + (to - from) * i as f32 / 40.0)).to_radians();
                center + vec2(a.cos(), a.sin()) * rad
            })
            .collect()
    };
    painter.circle_filled(center, radius + num("bezelWidth", 4.0), col("bezel", with_alpha(p.rule, 0.8)));
    painter.circle_filled(center, radius + 1.0, col("face", with_alpha(Color32::from_rgb(0x14, 0x14, 0x16), p.opacity)));
    painter.add(Shape::line(arc(0.0, 1.0, radius - 4.0), Stroke::new(num("trackWidth", 5.0), col("track", unlit))));
    let frac = r.fraction.unwrap_or(0.0);
    if frac > 0.0 {
        if bloom > 0.0 {
            painter.add(Shape::line(arc(0.0, frac, radius - 4.0), Stroke::new(num("arcWidth", 5.0) + 6.0 * bloom, with_alpha(acc, 0.18 * bloom))));
        }
        painter.add(Shape::line(arc(0.0, frac, radius - 4.0), Stroke::new(num("arcWidth", 5.0), col("arc", acc))));
    }
    let majors = num("majorTicks", 5.0).max(2.0) as usize;
    for i in 0..majors {
        let a = (start + sweep * i as f32 / (majors - 1) as f32).to_radians();
        let d = vec2(a.cos(), a.sin());
        painter.line_segment([center + d * (radius - 10.0), center + d * (radius - 7.0)], Stroke::new(1.0_f32, col("majorTick", p.secondary)));
    }
    let a = (start + sweep * frac).to_radians();
    painter.line_segment([center, center + vec2(a.cos(), a.sin()) * (radius - 9.0)], Stroke::new(2.0_f32, col("needle", p.primary)));
    painter.circle_filled(center, 3.0, col("cap", p.primary));
    painter.text(inner.center_top(), Align2::CENTER_TOP, r.quadrant.label(), label_f.clone(), p.secondary);
    painter.text(inner.center_bottom(), Align2::CENTER_BOTTOM, &r.text, value_f.clone(), col("text", p.primary));
}

fn skin_color(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

/// Meter accent for Classic: the app's metric colour, shifted for text contrast on `pal`
/// (further, in steps, until it reads at 4.5:1 on the palette's surface).
fn classic_accent(pal: &Palette, q: Quadrant) -> Color32 {
    let c = theme::metric_color(q.key());
    let shift = |t: f32| if pal.dark { lighten(c, 0.35 + t) } else { dim(c, 0.55 - t) };
    (0..=6).map(|i| shift(i as f32 * 0.1)).find(|a| theme::contrast(*a, pal.card) >= theme::MIN_CONTRAST).unwrap_or_else(|| shift(0.6))
}

/// Classic drawn in the application's own colours (`pal`).
pub fn classic_skin(pal: &Palette) -> Skin {
    let mut skin = Skin::classic();
    let solid = |c: Color32| Some(Fill::Solid(skin_color(c)));
    skin.surfaces = Some(Surfaces {
        window: solid(pal.body),
        window_stroke: solid(pal.card_stroke),
        header: solid(pal.chrome),
        header_rule: solid(pal.panel_stroke),
        tile: solid(pal.card),
        tile_stroke: solid(pal.panel_stroke),
        badge_stroke: solid(pal.card_stroke),
        ..Default::default()
    });
    skin.ink = Some(Ink {
        title: Some(skin_color(pal.text)),
        primary: Some(skin_color(pal.text)),
        secondary: Some(skin_color(pal.text_dim)),
        control: Some(skin_color(pal.text_dim)),
        badge: Some(skin_color(if pal.dark { pal.ok } else { dim(pal.ok, 0.7) })),
        rule: Some(skin_color(pal.grid)),
        ..Default::default()
    });
    if let Some(m) = skin.meters.as_mut() {
        for q in Quadrant::ALL {
            let accent = skin_color(classic_accent(pal, q));
            let unlit = skin_color(pal.grid);
            m.channels.insert(q.key().into(), Channel { accent: Some(accent), lit: None, unlit: Some(unlit) });
        }
    }
    skin
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::contrast;

    /// Process rows stay readable on their heat band in every built-in skin, Classic included.
    #[test]
    fn process_rows_are_readable() {
        let mut skins: Vec<Skin> = crate::skin::built_in().into_iter().filter(|s| s.id != "classic").collect();
        skins.extend(crate::palette::built_in().iter().map(|p| classic_skin(&p.palette)));
        for s in &skins {
            let p = paint(s, 1.0);
            for t in [0.0, 0.5, 1.0] {
                let band = skin_color(proc_band(&p, t));
                for (what, fg, min) in [("name", p.primary, 4.5), ("value", p.badge, 3.0)] {
                    let r = contrast(skin_color(fg), band);
                    assert!(r >= min, "{}: {what} on band t={t} is {r:.2}:1", s.id);
                }
            }
        }
    }

    #[test]
    fn classic_follows_the_app_palette_and_is_readable() {
        for pal in crate::palette::built_in().into_iter().map(|p| p.palette) {
            let s = classic_skin(&pal);
            let tile = s.surfaces.as_ref().unwrap().tile.as_ref().unwrap().top().unwrap();
            assert_eq!(tile, skin_color(pal.card));
            let ink = s.ink.as_ref().unwrap();
            for fg in [ink.primary, ink.secondary, ink.title] {
                let r = contrast(fg.unwrap(), tile);
                assert!(r >= 4.5, "dark={} ink {} is {r:.2}:1", pal.dark, fg.unwrap());
            }
            for q in s.resolved_quadrants() {
                let a = s.channel(q).accent.unwrap();
                let r = contrast(a, tile);
                assert!(r >= 4.5, "dark={} {} accent {a} is {r:.2}:1", pal.dark, q.key());
            }
        }
    }
}
