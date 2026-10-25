//! Custom-painted widgets: scrolling history graphs with compressed time, glow and
//! smoothed curves; sparklines, rings and bars; segmented LED meters; dot-matrix LED panels;
//! cards; buttons, tabs and chips; toggles; icons.

use super::hardware::Iso;
use super::theme::{self, dim, lighten, mix, with_alpha, Palette};
use crate::metrics::Series;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Mesh, Pos2, Rect, Response, Rounding, Sense, Shape, Stroke, TextureId, Ui, Vec2};
use std::cell::Cell;

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Polygon outline of a rounded rectangle (convex), clockwise from the top-left.
pub fn rounded_rect_points(rect: Rect, r: Rounding) -> Vec<Pos2> {
    let max = rect.width().min(rect.height()) / 2.0;
    let mut pts = Vec::with_capacity(40);
    let mut corner = |c: Pos2, rad: f32, start_deg: f32| {
        let rad = rad.clamp(0.0, max);
        if rad < 0.5 {
            pts.push(c);
            return;
        }
        let steps = 8;
        let center = pos2(if c.x < rect.center().x { c.x + rad } else { c.x - rad }, if c.y < rect.center().y { c.y + rad } else { c.y - rad });
        for i in 0..=steps {
            let a = (start_deg + 90.0 * i as f32 / steps as f32).to_radians();
            pts.push(center + vec2(a.cos(), a.sin()) * rad);
        }
    };
    corner(rect.left_top(), r.nw, 180.0);
    corner(rect.right_top(), r.ne, 270.0);
    corner(rect.right_bottom(), r.se, 0.0);
    corner(rect.left_bottom(), r.sw, 90.0);
    pts
}

/// Points of an ellipse inscribed in `rect`.
pub fn ellipse_points(rect: Rect, steps: usize) -> Vec<Pos2> {
    let c = rect.center();
    let (rx, ry) = (rect.width() / 2.0, rect.height() / 2.0);
    (0..steps).map(|i| {
        let a = std::f32::consts::TAU * i as f32 / steps as f32;
        pos2(c.x + rx * a.cos(), c.y + ry * a.sin())
    }).collect()
}

/// Regular polygon inscribed in `rect`.
pub fn polygon_points(rect: Rect, sides: usize, rotation_deg: f32) -> Vec<Pos2> {
    let c = rect.center();
    let (rx, ry) = (rect.width() / 2.0, rect.height() / 2.0);
    (0..sides.max(3)).map(|i| {
        let a = (rotation_deg - 90.0).to_radians() + std::f32::consts::TAU * i as f32 / sides.max(3) as f32;
        pos2(c.x + rx * a.cos(), c.y + ry * a.sin())
    }).collect()
}

/// Fills a convex polygon with a vertical gradient from `top` to `bottom`.
pub fn fill_vertical_gradient(painter: &egui::Painter, pts: &[Pos2], top: Color32, bottom: Color32) {
    if pts.len() < 3 {
        return;
    }
    let (min_y, max_y) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.y), b.max(p.y)));
    let h = (max_y - min_y).max(1.0);
    let lerp = |y: f32| mix(top, bottom, (y - min_y) / h);
    let mut mesh = Mesh::default();
    let cx = pts.iter().map(|p| p.x).sum::<f32>() / pts.len() as f32;
    let cy = pts.iter().map(|p| p.y).sum::<f32>() / pts.len() as f32;
    mesh.colored_vertex(pos2(cx, cy), lerp(cy));
    for p in pts {
        mesh.colored_vertex(*p, lerp(p.y));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}

/// Fills `rect` with a left-to-right gradient.
pub fn fill_horizontal_gradient(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// Glass card: rounded panel (radius 10) with a 1 px border.
pub fn card(painter: &egui::Painter, rect: Rect, pal: &Palette) {
    painter.rect_filled(rect, 10.0, pal.card);
    painter.rect_stroke(rect, 10.0, Stroke::new(1.0_f32, pal.card_stroke));
}

/// Card washed with `tint` (pressure and anomaly cards, warning banners, the recording in
/// progress): a faint fill and a stronger border of the same colour.
pub fn tinted_card(painter: &egui::Painter, rect: Rect, pal: &Palette, tint: Color32) {
    painter.rect_filled(rect, 10.0, pal.card);
    painter.rect_filled(rect, 10.0, with_alpha(tint, 0.08));
    painter.rect_stroke(rect, 10.0, Stroke::new(1.0_f32, with_alpha(tint, 0.3)));
}

/// Card whose border is tinted with a metric colour (the running test, the active device).
/// With bloom the border also glows.
pub fn accent_card(painter: &egui::Painter, rect: Rect, pal: &Palette, accent: Color32, bloom: f32) {
    painter.rect_filled(rect, 10.0, pal.card);
    painter.rect_filled(rect, 10.0, with_alpha(accent, 0.10));
    if bloom > 0.0 {
        painter.rect_stroke(rect.expand(1.5), 11.0, Stroke::new(3.0_f32, with_alpha(accent, 0.10 * bloom)));
        painter.rect_stroke(rect.shrink(1.0), 9.0, Stroke::new(2.0_f32, with_alpha(accent, 0.08 * bloom)));
    }
    painter.rect_stroke(rect, 10.0, Stroke::new(1.0_f32, with_alpha(accent, 0.5)));
}

pub fn text(painter: &egui::Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32) -> Rect {
    painter.text(pos, anchor, s, font, color)
}

/// Single-line text truncated with an ellipsis to `max_w` points.
pub fn text_fit(painter: &egui::Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_w: f32) -> Rect {
    let mut job = egui::text::LayoutJob::single_section(s.to_string(), egui::TextFormat::simple(font, color));
    job.wrap = egui::text::TextWrapping { max_width: max_w.max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

pub fn text_width(painter: &egui::Painter, s: &str, font: FontId) -> f32 {
    painter.layout_no_wrap(s.to_string(), font, Color32::WHITE).size().x
}

// ---------------------------------------------------------------------------
// Glowing polylines and smoothing
// ---------------------------------------------------------------------------

/// Draws a polyline with a soft bloom (`bloom` 0 = none, 1 = strongest). `additive` blends the
/// glow as light (dark backgrounds), so crossing traces brighten instead of muddying each other.
pub fn glow_polyline(painter: &egui::Painter, pts: &[Pos2], color: Color32, width: f32, bloom: f32, dashed: bool, additive: bool) {
    let pts = dedup_points(pts);
    if pts.len() < 2 {
        return;
    }
    if bloom > 0.0 {
        let radius = width * 0.5 + 3.0 + 10.0 * bloom;
        let peak = if additive { 0.35 + 0.45 * bloom } else { 0.18 + 0.22 * bloom };
        painter.add(Shape::mesh(glow_splats(painter, &pts, color, radius, peak, additive)));
    }
    let stroke = Stroke::new(width, color);
    if dashed {
        painter.add(Shape::Vec(Shape::dashed_line(&pts, stroke, 6.0, 4.0)));
    } else {
        painter.add(Shape::mesh(line_ribbon(&pts, color, width)));
        if additive && bloom > 0.0 {
            // Hot core: a thin brighter centre makes the trace read as emitted light.
            painter.add(Shape::mesh(line_ribbon(&pts, with_alpha(lighten(color, 0.45), 0.35 + 0.4 * bloom), width * 0.45)));
        }
    }
}

/// Drops points closer than a quarter pixel to their predecessor (degenerate normals otherwise)
/// and splits segments longer than 2 px. The ribbons take their normals per point, so coarse
/// polylines with sharp corners need short segments to keep the corner effects local.
fn dedup_points(pts: &[Pos2]) -> Vec<Pos2> {
    const MAX_SEG: f32 = 2.0;
    let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
    for &p in pts {
        let Some(&q) = out.last() else {
            out.push(p);
            continue;
        };
        let d = q.distance(p);
        if d <= 0.25 {
            continue;
        }
        let pieces = (d / MAX_SEG).ceil().min(512.0) as usize;
        for k in 1..=pieces {
            out.push(q + (p - q) * (k as f32 / pieces as f32));
        }
    }
    out
}

/// A soft glow around a polyline: Gaussian splats placed at even arc-length steps and summed.
/// This convolves the trace with a Gaussian (a real blur), so tight wiggles narrower than the glow
/// can't fold the geometry, and ends and spike tips get round glow for free. Each splat is one
/// quad sampling a Gaussian texture, which keeps the vertex count (and CPU cost) low.
fn glow_splats(painter: &egui::Painter, pts: &[Pos2], color: Color32, radius: f32, peak: f32, additive: bool) -> Mesh {
    // Splat profile (see `splat_texture`): exp(-(r / sigma)^2) with sigma = radius / 2.
    let sigma = radius * 0.5;
    let step = (0.36 * radius).max(1.5);
    // Summed along a straight line, splats of amplitude A every `step` peak at A * sigma * sqrt(pi) / step.
    let amp = peak * step / (sigma * std::f32::consts::PI.sqrt());
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    let tint = if additive {
        // Premultiplied colour with zero alpha is pure additive blending.
        let s = |v: u8| (v as f32 * amp).round() as u8;
        Color32::from_rgba_premultiplied(s(r), s(g), s(b), 0)
    } else {
        Color32::from_rgba_unmultiplied(r, g, b, (amp * 255.0).round() as u8)
    };
    // Splat centres at even arc-length spacing.
    let mut centres = vec![pts[0]];
    let mut carry = 0.0;
    for w in pts.windows(2) {
        let len = w[0].distance(w[1]);
        let mut t = step - carry;
        while t <= len {
            centres.push(w[0] + (w[1] - w[0]) * (t / len));
            t += step;
        }
        carry = len - (t - step);
    }
    let mut mesh = Mesh::with_texture(splat_texture(painter.ctx()));
    mesh.reserve_vertices(centres.len() * 4);
    mesh.reserve_triangles(centres.len() * 2);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    for c in centres {
        mesh.add_rect_with_uv(Rect::from_center_size(c, Vec2::splat(2.0 * radius)), uv, tint);
    }
    mesh
}

/// The shared Gaussian splat texture: exp(-(r / 0.5)^2) over the unit disc (r = 1 at the quad's
/// edge), eased to exactly zero at the rim so splats have no visible boundary.
fn splat_texture(ctx: &egui::Context) -> egui::TextureId {
    const N: usize = 64;
    let id = egui::Id::new("tmop-glow-splat");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t.id();
    }
    let pixels: Vec<Color32> = (0..N * N)
        .map(|i| {
            let (x, y) = ((i % N) as f32 + 0.5, (i / N) as f32 + 0.5);
            let r = ((x / N as f32 - 0.5).hypot(y / N as f32 - 0.5) * 2.0).min(1.0);
            let rim = 1.0 - r * r;
            let v = ((-(r / 0.5).powi(2)).exp() * rim * rim * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(v, v, v, v)
        })
        .collect();
    let image = egui::ColorImage { size: [N, N], pixels };
    let handle = ctx.load_texture("glow-splat", image, egui::TextureOptions::LINEAR);
    let tid = handle.id();
    ctx.data_mut(|d| d.insert_temp(id, handle));
    tid
}

/// An anti-aliased solid line as a mesh: opaque core with a one-pixel feathered edge. It shares the
/// fold-free geometry of the glow, so tight kinks in dense history don't sprout miter spikes.
fn line_ribbon(pts: &[Pos2], color: Color32, width: f32) -> Mesh {
    let hw = (width * 0.5).max(0.5);
    let radius = hw + 0.5;
    let inner = ((hw - 0.5) / radius).max(0.0);
    // Lines thinner than the feather fade their core instead of shrinking below a pixel.
    let core = if hw < 1.0 { with_alpha(color, color.a() as f32 / 255.0 * hw) } else { color };
    let clear = Color32::TRANSPARENT;
    ribbon(pts, radius, 1.0, &[(0.0, core), (inner.max(0.001), core), (1.0, clear)])
}

/// Shared geometry: `rings` are (offset as a fraction of `radius`, colour) from the centre out.
/// `window` is the arc length over which normals are averaged.
fn ribbon(pts: &[Pos2], radius: f32, window: f32, rings: &[(f32, Color32)]) -> Mesh {
    let m = rings.len() - 1;
    let ring_color = |k: usize| rings[k].1;
    let n = pts.len();
    // Per-point tangents (central differences)...
    let tangents: Vec<Vec2> = (0..n)
        .map(|i| {
            let t = pts[(i + 1).min(n - 1)] - pts[i.saturating_sub(1)];
            if t.length_sq() < 1e-9 { vec2(1.0, 0.0) } else { t.normalized() }
        })
        .collect();
    // ...then normals averaged over `window` of arc length. The offset rings
    // then turn gradually around sharp corners instead of folding over on the inside.
    let arc: Vec<f32> = std::iter::once(0.0)
        .chain(pts.windows(2).scan(0.0, |s, w| {
            *s += w[0].distance(w[1]);
            Some(*s)
        }))
        .collect();
    let normals: Vec<Vec2> = (0..n)
        .map(|i| {
            let mut acc = Vec2::ZERO;
            let mut j = i;
            while j > 0 && arc[i] - arc[j - 1] <= window {
                j -= 1;
            }
            while j < n && arc[j] - arc[i] <= window {
                let w = 1.0 - (arc[j] - arc[i]).abs() / (window + 1e-3);
                acc += tangents[j] * w.max(0.05);
                j += 1;
            }
            let t = if acc.length_sq() < 1e-9 { tangents[i] } else { acc.normalized() };
            t.rot90()
        })
        .collect();

    let per = (2 * m + 1) as u32;
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(n * per as usize + 2 * CAP_STEPS * m + 2);
    mesh.reserve_triangles((n - 1) * 4 * m + 2 * CAP_STEPS * (2 * m - 1));
    // Offset positions per (side, ring); a vertex that would move backwards along the curve is held
    // at its predecessor, which collapses would-be inverted triangles instead of drawing them.
    let mut prev: [[Option<Pos2>; 8]; 2] = [[None; 8]; 2];
    for i in 0..n {
        mesh.colored_vertex(pts[i], ring_color(0));
        for (s, side) in [1.0_f32, -1.0].into_iter().enumerate() {
            for (k, x) in rings.iter().map(|r| r.0).enumerate().skip(1) {
                let mut v = pts[i] + normals[i] * (side * x * radius);
                if let Some(p) = prev[s][k] {
                    if (v - p).dot(tangents[i]) < 0.0 {
                        v = p;
                    }
                }
                prev[s][k] = Some(v);
                mesh.colored_vertex(v, ring_color(k));
            }
        }
    }
    let idx = |i: usize, side: usize, k: usize| -> u32 {
        let base = i as u32 * per;
        if k == 0 { base } else { base + (side * m + k) as u32 }
    };
    for i in 0..n - 1 {
        for side in 0..2 {
            for k in 0..m {
                let (a0, a1, b0, b1) = (idx(i, side, k), idx(i, side, k + 1), idx(i + 1, side, k), idx(i + 1, side, k + 1));
                mesh.add_triangle(a0, a1, b0);
                mesh.add_triangle(a1, b1, b0);
            }
        }
    }
    // Round caps so the glow fades out around the ends instead of stopping on a straight edge.
    for (i, outward) in [(0, -tangents[0]), (n - 1, tangents[n - 1])] {
        let normal = outward.rot90();
        let center = mesh.vertices.len() as u32;
        mesh.colored_vertex(pts[i], ring_color(0));
        for j in 0..=CAP_STEPS {
            let a = std::f32::consts::PI * (j as f32 / CAP_STEPS as f32 - 0.5);
            let dir = outward * a.cos() + normal * a.sin();
            for (k, x) in rings.iter().map(|r| r.0).enumerate().skip(1) {
                mesh.colored_vertex(pts[i] + dir * (x * radius), ring_color(k));
            }
        }
        let at = |j: usize, k: usize| -> u32 { if k == 0 { center } else { center + 1 + (j * m + k - 1) as u32 } };
        for j in 0..CAP_STEPS {
            mesh.add_triangle(center, at(j, 1), at(j + 1, 1));
            for k in 1..m {
                mesh.add_triangle(at(j, k), at(j, k + 1), at(j + 1, k));
                mesh.add_triangle(at(j, k + 1), at(j + 1, k + 1), at(j + 1, k));
            }
        }
    }
    mesh
}

const CAP_STEPS: usize = 8;

/// Cubic Hermite smoothing of a polyline ordered by x. The curve passes through every sample.
/// Tangents are Catmull-Rom tangents with the Fritsch-Carlson limit on rising/falling runs, so
/// there is no overshoot there. Peaks keep a soft, rounded tangent, and flat runs (for example
/// idle at 0%) stay flat, so the curve never dips below the floor. `y_clamp` guards the bounds.
pub fn smooth_points(pts: &[Pos2], y_clamp: (f32, f32)) -> Vec<Pos2> {
    let pts: Vec<Pos2> = {
        let mut v: Vec<Pos2> = Vec::with_capacity(pts.len());
        for &p in pts {
            match v.last() {
                Some(q) if p.x - q.x < 0.05 => *v.last_mut().expect("non-empty") = p,
                _ => v.push(p),
            }
        }
        v
    };
    let n = pts.len();
    if n < 3 {
        return pts;
    }
    let h: Vec<f32> = (0..n - 1).map(|i| pts[i + 1].x - pts[i].x).collect();
    let d: Vec<f32> = (0..n - 1).map(|i| (pts[i + 1].y - pts[i].y) / h[i]).collect();
    let mut m = vec![0.0_f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        let (d0, d1) = (d[i - 1], d[i]);
        if d0.abs() < 1e-4 || d1.abs() < 1e-4 {
            continue; // next to a flat run: stay flat
        }
        let catmull = (pts[i + 1].y - pts[i - 1].y) / (pts[i + 1].x - pts[i - 1].x);
        m[i] = if d0 * d1 > 0.0 {
            catmull.signum() * catmull.abs().min(3.0 * d0.abs().min(d1.abs()))
        } else {
            0.35 * catmull // local peak/dip: rounded, with only a slight lean
        };
    }
    let mut out = Vec::with_capacity(n * 6);
    out.push(pts[0]);
    for i in 0..n - 1 {
        let (p1, p2, dx) = (pts[i], pts[i + 1], h[i]);
        let steps = ((dx / 2.0).ceil() as usize).clamp(1, 16);
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p1.y + (t3 - 2.0 * t2 + t) * dx * m[i] + (-2.0 * t3 + 3.0 * t2) * p2.y + (t3 - t2) * dx * m[i + 1];
            out.push(pos2(p1.x + dx * t, y.clamp(y_clamp.0, y_clamp.1)));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// History graph
// ---------------------------------------------------------------------------

pub struct GraphLine<'a> {
    pub series: &'a Series,
    pub color: Color32,
    /// Value range mapped to the graph height.
    pub range: (f32, f32),
}

pub struct Graph<'a> {
    pub lines: Vec<GraphLine<'a>>,
    /// Pixels the graph advances per data update (`graphPixelsPerUpdate`).
    pub px_per_update: f32,
    /// 0..1 progress since the newest sample arrived.
    pub phase: f32,
    /// Let the newest sample scroll in over the update interval (needs a running `phase`).
    pub glide: bool,
    /// History compression level: the graph holds as much as this many equal regions of
    /// 1x, 2x, 4x, 8x history would, compressed continuously (1 = uniform time).
    pub regions: u32,
    pub smooth: bool,
    pub dashed: bool,
    /// 0 = no glow, 1 = strongest.
    pub bloom: f32,
    pub grid: Option<Color32>,
    pub border: Option<Color32>,
    pub background: Option<Color32>,
    pub markers: bool,
    /// Alpha of the area fill under each line (0 = none).
    pub fill: f32,
    pub line_width: f32,
}

impl<'a> Graph<'a> {
    pub fn new(lines: Vec<GraphLine<'a>>) -> Self {
        Self {
            lines,
            px_per_update: 16.0,
            phase: 0.0,
            glide: false,
            regions: 1,
            smooth: true,
            dashed: false,
            bloom: 0.7,
            grid: None,
            border: None,
            background: None,
            markers: true,
            fill: 0.0,
            line_width: 2.0,
        }
    }
}

/// Eases an auto-scaled graph bound towards `target` over half a second, so a new sample that
/// moves the scale doesn't make the whole trace jump. Keyed by `id`; the first call returns
/// `target` unchanged.
pub fn eased(ctx: &egui::Context, id: impl std::hash::Hash, target: f32) -> f32 {
    ctx.animate_value_with_time(egui::Id::new(("tmop-eased-scale", id)), target, 0.5)
}

/// [`eased`] for a (low, high) range.
pub fn eased_range(ctx: &egui::Context, id: impl std::hash::Hash + Copy, (lo, hi): (f32, f32)) -> (f32, f32) {
    (eased(ctx, (id, 0u8), lo), eased(ctx, (id, 1u8), hi))
}

/// Number of compressed regions for a history multiplier (1, 3, 7, 15 -> 1..4).
pub fn regions_for(multiplier: u32, compressed: bool) -> u32 {
    if !compressed {
        1
    } else if multiplier >= 15 {
        4
    } else if multiplier >= 7 {
        3
    } else if multiplier >= 3 {
        2
    } else {
        1
    }
}

/// Smallest px per update a graph is drawn at: a long history squeezed into a small sparkline
/// stays complete (the point vector is still only a few thousand entries).
pub const MIN_PX_PER_UPDATE: f32 = 0.02;

/// Maps a sample's age (updates, fractional) to x. The newest `width / regions` (the live part)
/// advances `p` px per update. Older time is compressed continuously: the px-per-update rate
/// decays exponentially with distance, so spacing never jumps. The decay is chosen so the whole
/// width holds as much history as `regions` equal regions of 1x, 2x, 4x, 8x... would.
#[derive(Clone, Copy)]
pub struct TimeAxis {
    right: f32,
    p: f32,
    /// Width of the uncompressed live part.
    live: f32,
    /// Decay rate per px beyond the live part (0 = uniform time).
    k: f32,
}

impl TimeAxis {
    pub fn new(right: f32, width: f32, p: f32, regions: u32) -> Self {
        let r = regions.max(1);
        let live = width / r as f32;
        if r == 1 {
            return Self { right, p, live: width, k: 0.0 };
        }
        // Updates held, in units of `live / p`: 1 + (b^(r-1) - 1) / ln b for decay base b over
        // each further `live` px. Solve for b so it equals 2^r - 1 (the stepped layout's total).
        let target = (1u32 << r) as f32 - 1.0;
        let held = |b: f32| 1.0 + (b.powi(r as i32 - 1) - 1.0) / b.ln();
        let (mut lo, mut hi) = (1.0001_f32, 64.0_f32);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if held(mid) < target { lo = mid } else { hi = mid }
        }
        Self { right, p, live, k: (0.5 * (lo + hi)).ln() / live }
    }

    /// Distance from the right edge of a sample `age` updates old.
    fn dist(&self, age: f32) -> f32 {
        let u = age * self.p;
        if self.k == 0.0 || u <= self.live {
            return u;
        }
        // Beyond the live part: age = live/p + (e^(k*(d - live)) - 1) / (k*p), inverted.
        self.live + (1.0 + self.k * (u - self.live)).ln() / self.k
    }

    pub fn x_of(&self, age: f32) -> f32 {
        self.right - self.dist(age)
    }

    /// Age (updates, fractional) of the sample drawn `d` px from the right edge; inverse of [`Self::x_of`].
    pub fn age_at(&self, d: f32) -> f32 {
        let d = d.max(0.0);
        if self.k == 0.0 || d <= self.live {
            return d / self.p;
        }
        (self.live + ((self.k * (d - self.live)).exp() - 1.0) / self.k) / self.p
    }

    /// Pixels per update at distance `d` from the right edge.
    fn px_per_update(&self, d: f32) -> f32 {
        if self.k == 0.0 || d <= self.live { self.p } else { self.p * (-self.k * (d - self.live)).exp() }
    }
}

/// Draws a scrolling history graph into `rect`.
pub fn draw_graph(painter: &egui::Painter, rect: Rect, g: &Graph<'_>) {
    let painter = painter.with_clip_rect(rect.expand(1.0));
    if let Some(bg) = g.background {
        painter.rect_filled(rect, 0.0, bg);
    }
    let p = g.px_per_update.max(MIN_PX_PER_UPDATE);
    let axis = TimeAxis::new(rect.right(), rect.width(), p, g.regions);
    // Sub-pixel motion between updates: anti-aliased traces glide instead of stepping.
    let shift = g.phase.clamp(0.0, 1.0);
    // Additive glow only reads well on dark graph backgrounds.
    let additive = g.background.is_none_or(|bg| {
        let [r, gg, b, _] = bg.to_srgba_unmultiplied();
        (r as u32 + gg as u32 + b as u32) < 3 * 96
    });
    if let Some(grid) = g.grid {
        let s = Stroke::new(1.0_f32, grid);
        let rows = 10;
        for i in 1..rows {
            let y = (rect.top() + rect.height() * i as f32 / rows as f32).round() + 0.5;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], s);
        }
        // Vertical lines ride on the samples and move sub-pixel with the traces. Each line is tied
        // to a sample's absolute index (not its age), so the same lines persist as samples arrive.
        // A line whose index is a multiple of 2^n stands for a 2^n-sample cell; its opacity follows
        // the width that cell has at the line's position, so as time compresses towards the left
        // the finer lines fade out gradually and the visible spacing stays between about
        // MIN_GAP and 2 * MIN_GAP everywhere.
        const MIN_GAP: f32 = 8.0;
        let grid_offset = if g.glide { shift - 1.0 } else { shift };
        let newest = g.lines.first().map_or(0, |l| l.series.pushed as i64) - 1;
        let mut age = 0u32;
        loop {
            let x = axis.x_of(age as f32 + grid_offset);
            if x < rect.left() - 1.0 || age > 100_000 {
                break;
            }
            age += 1;
            if x > rect.right() {
                continue;
            }
            let idx = newest - (age as i64 - 1);
            // Largest power of two (up to 64) dividing the sample index.
            let step = if idx == 0 { 64 } else { 1i64 << idx.unsigned_abs().trailing_zeros().min(6) } as f32;
            let cell = step * axis.px_per_update(rect.right() - x);
            let alpha = ((cell - 0.5 * MIN_GAP) / (0.5 * MIN_GAP)).clamp(0.0, 1.0);
            if alpha > 0.0 {
                let x = x + 0.5;
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, grid.gamma_multiply(alpha)));
            }
        }
    }
    let y_clamp = (rect.top(), rect.bottom());
    for line in &g.lines {
        let (lo, hi) = line.range;
        let span = if hi > lo { hi - lo } else { 1.0 };
        let y_of = |v: f32| rect.bottom() - ((v - lo) / span).clamp(0.0, 1.0) * rect.height();
        let n = line.series.values.len();
        if n == 0 {
            continue;
        }
        // Gliding: the newest sample starts one step beyond the right edge and scrolls in over the
        // update interval, so the visible end moves continuously from the previous value to the
        // new one instead of jumping when a sample arrives.
        let offset = if g.glide && n >= 2 { shift - 1.0 } else { shift };
        let mut pts: Vec<Pos2> = Vec::with_capacity(n + 2);
        for (age, v) in line.series.values.iter().rev().enumerate() {
            let x = axis.x_of(age as f32 + offset);
            pts.push(pos2(x, y_of(*v)));
            if x < rect.left() - p {
                break;
            }
        }
        pts.reverse();
        let newest = *pts.last().expect("non-empty");
        if newest.x < rect.right() - 0.5 {
            // Hold the newest value to the right edge so the trace always meets the marker.
            pts.push(pos2(rect.right(), newest.y));
        }
        let mut drawn = if g.smooth { smooth_points(&pts, y_clamp) } else { pts.clone() };
        cut_at_x(&mut drawn, rect.right());
        let head = *drawn.last().unwrap_or(&newest);
        if g.fill > 0.0 && drawn.len() >= 2 {
            let mut mesh = Mesh::default();
            let c = with_alpha(line.color, g.fill);
            let c0 = with_alpha(line.color, 0.0);
            for (i, pt) in drawn.iter().enumerate() {
                mesh.colored_vertex(*pt, c);
                mesh.colored_vertex(pos2(pt.x, rect.bottom()), c0);
                if i > 0 {
                    let k = (i * 2) as u32;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k + 1, k);
                }
            }
            painter.add(Shape::mesh(mesh));
        }
        glow_polyline(&painter, &drawn, line.color, g.line_width, g.bloom, g.dashed, additive);
        if g.markers {
            let y = head.y;
            let m = Color32::from_rgba_unmultiplied(0xF0, 0xF0, 0xF0, 230);
            painter.add(Shape::convex_polygon(vec![pos2(rect.right() - 6.0, y), pos2(rect.right(), y - 4.0), pos2(rect.right(), y + 4.0)], m, Stroke::NONE));
        }
    }
    if let Some(b) = g.border {
        if g.bloom > 0.0 {
            painter.rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0_f32, with_alpha(b, 0.10 * g.bloom)));
        }
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, b));
    }
}

/// Truncates a polyline ordered by x at `x_max`, ending exactly on the edge (interpolated).
fn cut_at_x(pts: &mut Vec<Pos2>, x_max: f32) {
    if let Some(i) = pts.iter().position(|q| q.x > x_max) {
        if i > 0 {
            let (a, b) = (pts[i - 1], pts[i]);
            let t = (x_max - a.x) / (b.x - a.x).max(1e-6);
            pts.truncate(i);
            pts.push(a + (b - a) * t);
        } else {
            pts.truncate(1);
        }
    }
}

// ---------------------------------------------------------------------------
// LED meters
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct LedStyle {
    pub lit: Color32,
    /// Brightness of unlit segments relative to `lit` (0..1).
    pub unlit_factor: f32,
    /// Colour by position (bottom/left, middle, top/right) instead of `lit`.
    pub positional: Option<[Color32; 3]>,
    pub bloom: f32,
    pub rounding: f32,
    pub gap: f32,
    /// Draw the bright rim on lit segments.
    pub rim: bool,
}

impl LedStyle {
    pub fn new(lit: Color32, bloom: f32) -> Self {
        Self { lit, unlit_factor: 0.22, positional: None, bloom, rounding: 1.5, gap: 2.0, rim: true }
    }
    /// Green → yellow → red by position (CPU load meters). A severity ramp, so it keeps fixed
    /// colours whatever the metric colours are.
    pub fn traffic(bloom: f32) -> Self {
        Self { positional: Some([theme::GREEN, theme::YELLOW, theme::RED]), ..Self::new(theme::GREEN, bloom) }
    }
    fn color_at(&self, t: f32) -> Color32 {
        match self.positional {
            Some([a, b, c]) => {
                if t < 0.5 {
                    mix(a, b, t * 2.0)
                } else {
                    mix(b, c, (t - 0.5) * 2.0)
                }
            }
            None => self.lit,
        }
    }
}

/// Background unlit LED segments blend into: 0 = black (dark theme), otherwise an
/// RGBA-packed colour (light theme). Set once per frame by the app.
static LED_GROUND: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_led_ground(ground: Option<Color32>) {
    let v = ground.map(|c| u32::from_be_bytes([c.r(), c.g(), c.b(), 255])).unwrap_or(0);
    LED_GROUND.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Colour of an unlit segment of colour `c`.
pub fn unlit(c: Color32, factor: f32) -> Color32 {
    match LED_GROUND.load(std::sync::atomic::Ordering::Relaxed) {
        0 => dim(c, factor),
        v => {
            let [r, g, b, _] = v.to_be_bytes();
            mix(Color32::from_rgb(r, g, b), c, (factor * 1.4).min(1.0))
        }
    }
}

fn led_segment(painter: &egui::Painter, r: Rect, c: Color32, lit: bool, style: &LedStyle) {
    if lit {
        painter.rect_filled(r, style.rounding, c);
        if style.rim && r.height() >= 4.0 && r.width() >= 4.0 {
            painter.rect_stroke(r.shrink(0.5), style.rounding, Stroke::new(1.0_f32, lighten(c, 0.38)));
        }
    } else {
        painter.rect_filled(r, style.rounding, unlit(c, style.unlit_factor));
    }
}

/// Vertical LED column filling from the bottom. Returns the number of lit segments.
pub fn led_column(painter: &egui::Painter, rect: Rect, segments: usize, fraction: f32, style: &LedStyle) -> usize {
    let n = segments.max(1);
    let seg_h = (rect.height() - style.gap * (n as f32 - 1.0)) / n as f32;
    let lit_n = ((fraction.clamp(0.0, 1.0) * n as f32).round() as usize).min(n);
    if lit_n > 0 && style.bloom > 0.0 {
        let top = rect.bottom() - lit_n as f32 * (seg_h + style.gap) + style.gap;
        let span = Rect::from_min_max(pos2(rect.left(), top), rect.right_bottom());
        let c = style.color_at((lit_n as f32 / n as f32) * 0.5);
        painter.rect_filled(span.expand(7.0 * style.bloom), 6.0, with_alpha(c, 0.07 * style.bloom));
        painter.rect_filled(span.expand(3.0 * style.bloom), 4.0, with_alpha(c, 0.14 * style.bloom));
    }
    for i in 0..n {
        let y1 = rect.bottom() - i as f32 * (seg_h + style.gap);
        let r = Rect::from_min_max(pos2(rect.left(), y1 - seg_h), pos2(rect.right(), y1));
        let t = (i as f32 + 0.5) / n as f32;
        led_segment(painter, r, style.color_at(t), i < lit_n, style);
    }
    lit_n
}

/// Horizontal LED row filling from the left. Returns the number of lit segments.
pub fn led_row(painter: &egui::Painter, rect: Rect, segments: usize, fraction: f32, style: &LedStyle) -> usize {
    let n = segments.max(1);
    let seg_w = (rect.width() - style.gap * (n as f32 - 1.0)) / n as f32;
    let lit_n = ((fraction.clamp(0.0, 1.0) * n as f32).round() as usize).min(n);
    if lit_n > 0 && style.bloom > 0.0 {
        let right = rect.left() + lit_n as f32 * (seg_w + style.gap) - style.gap;
        let span = Rect::from_min_max(rect.left_top(), pos2(right, rect.bottom()));
        let c = style.color_at((lit_n as f32 / n as f32) * 0.5);
        painter.rect_filled(span.expand(6.0 * style.bloom), 5.0, with_alpha(c, 0.07 * style.bloom));
        painter.rect_filled(span.expand(2.5 * style.bloom), 3.0, with_alpha(c, 0.14 * style.bloom));
    }
    for i in 0..n {
        let x0 = rect.left() + i as f32 * (seg_w + style.gap);
        let r = Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + seg_w, rect.bottom()));
        let t = (i as f32 + 0.5) / n as f32;
        led_segment(painter, r, style.color_at(t), i < lit_n, style);
    }
    lit_n
}

/// Horizontal bar whose segments shade from `left` to `right` colour along the bar
/// (the Performance page utilization bar).
pub fn led_gradient_row(painter: &egui::Painter, rect: Rect, segments: usize, fraction: f32, colors: [Color32; 3], bloom: f32) -> usize {
    let style = LedStyle { positional: Some(colors), unlit_factor: 0.2, gap: 2.0, rounding: 1.0, rim: false, ..LedStyle::new(colors[0], bloom) };
    led_row(painter, rect, segments, fraction, &style)
}

// ---------------------------------------------------------------------------
// Dot-matrix LED panel
// ---------------------------------------------------------------------------

/// 5x7 glyphs, one byte per row (bit 4 = leftmost column).
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        '\u{b0}' => [0x0C, 0x12, 0x12, 0x0C, 0x00, 0x00, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        _ => [0; 7],
    }
}

pub const LED_PITCH: f32 = 2.5;
pub const LED_PANEL_H: f32 = 7.0 * LED_PITCH + 6.0;

/// Width in points of a dot-matrix string.
pub fn led_text_width(s: &str) -> f32 {
    s.chars().count() as f32 * 6.0 * LED_PITCH
}

/// Black dot-matrix panel: `left` text left-aligned, `right` text right-aligned, dim dots
/// filling the rest ("CPU OVERVIEW ........ 9.8%").
pub fn led_panel(painter: &egui::Painter, rect: Rect, left: &str, right: &str, pal: &Palette, bloom: f32) {
    painter.rect_filled(rect, 2.0, pal.led_bg);
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0_f32, with_alpha(pal.card_stroke, 0.6)));
    let inner = rect.shrink2(vec2(5.0, 3.0));
    let cols = (inner.width() / LED_PITCH).floor() as usize;
    let rows = 7usize;
    let dot = LED_PITCH - 0.7;
    let origin = pos2(inner.left(), inner.center().y - rows as f32 * LED_PITCH / 2.0);
    let mut lit = vec![false; cols * rows];
    let mut stamp = |s: &str, col0: usize| {
        for (ci, ch) in s.chars().enumerate() {
            let g = glyph(ch);
            for (r, bits) in g.iter().enumerate() {
                for b in 0..5 {
                    if bits & (0x10 >> b) != 0 {
                        let c = col0 + ci * 6 + b;
                        if c < cols {
                            lit[r * cols + c] = true;
                        }
                    }
                }
            }
        }
    };
    stamp(left, 1);
    if !right.is_empty() {
        let w = right.chars().count() * 6;
        if cols > w + 1 {
            stamp(right, cols - w);
        }
    }
    // Dim lattice + lit dots.
    let mut mesh = Mesh::default();
    let mut glow = Mesh::default();
    for r in 0..rows {
        for c in 0..cols {
            let p = pos2(origin.x + c as f32 * LED_PITCH, origin.y + r as f32 * LED_PITCH);
            let on = lit[r * cols + c];
            let color = if on { pal.led_lit } else { pal.led_dim };
            add_rect(&mut mesh, Rect::from_min_size(p, vec2(dot, dot)), color);
            if on && bloom > 0.0 {
                add_rect(&mut glow, Rect::from_min_size(p, vec2(dot, dot)).expand(1.6), with_alpha(pal.led_lit, 0.18 * bloom));
            }
        }
    }
    painter.add(Shape::mesh(glow));
    painter.add(Shape::mesh(mesh));
}

/// Small dot-matrix readout without the lattice (status bar clock).
pub fn led_digits(painter: &egui::Painter, pos: Pos2, s: &str, color: Color32, pitch: f32) {
    let dot = pitch - 0.6;
    let mut mesh = Mesh::default();
    for (ci, ch) in s.chars().enumerate() {
        let g = glyph(ch);
        for (r, bits) in g.iter().enumerate() {
            for b in 0..5 {
                let on = bits & (0x10 >> b) != 0;
                let p = pos2(pos.x + (ci * 6 + b) as f32 * pitch, pos.y + r as f32 * pitch);
                add_rect(&mut mesh, Rect::from_min_size(p, vec2(dot, dot)), if on { color } else { with_alpha(color, 0.12) });
            }
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Seven-segment readout (digits, `:` and `.`) with dim unlit segments, like a stopwatch
/// LCD. `pos` is the top-left corner, `h` the digit height. Returns the drawn width.
pub fn seven_segment(painter: &egui::Painter, pos: Pos2, s: &str, h: f32, lit: Color32, unlit: Color32, bloom: f32) -> f32 {
    // Segment bits: a b c d e f g
    const DIGITS: [u8; 10] = [0x7E, 0x30, 0x6D, 0x79, 0x33, 0x5B, 0x5F, 0x70, 0x7F, 0x7B];
    let w = h * 0.52;
    let t = (h * 0.13).max(1.5);
    let gap = h * 0.22;
    let mut x = pos.x;
    let seg = |mesh: &mut Mesh, a: Pos2, b: Pos2, c: Color32| {
        // Hexagonal segment from a to b.
        let d = (b - a).normalized();
        let n = vec2(-d.y, d.x) * (t / 2.0);
        let a2 = a + d * (t * 0.55);
        let b2 = b - d * (t * 0.55);
        let pts = [a2 - d * (t * 0.5), a2 + n, b2 + n, b2 + d * (t * 0.5), b2 - n, a2 - n];
        let i = mesh.vertices.len() as u32;
        for p in pts {
            mesh.colored_vertex(p, c);
        }
        for k in 1..5 {
            mesh.add_triangle(i, i + k, i + k + 1);
        }
    };
    let mut mesh = Mesh::default();
    let mut glow = Mesh::default();
    for ch in s.chars() {
        match ch {
            '0'..='9' => {
                let bits = DIGITS[(ch as u8 - b'0') as usize];
                let (l, r, tp, m, b) = (x + t / 2.0, x + w - t / 2.0, pos.y + t / 2.0, pos.y + h / 2.0, pos.y + h - t / 2.0);
                let segs = [
                    (pos2(l, tp), pos2(r, tp)),
                    (pos2(r, tp), pos2(r, m)),
                    (pos2(r, m), pos2(r, b)),
                    (pos2(l, b), pos2(r, b)),
                    (pos2(l, m), pos2(l, b)),
                    (pos2(l, tp), pos2(l, m)),
                    (pos2(l, m), pos2(r, m)),
                ];
                for (k, (a, bb)) in segs.iter().enumerate() {
                    let on = bits & (0x40 >> k) != 0;
                    seg(&mut mesh, *a, *bb, if on { lit } else { unlit });
                    if on && bloom > 0.0 {
                        let c = with_alpha(lit, 0.16 * bloom);
                        let d = (*bb - *a).normalized();
                        let n = vec2(-d.y, d.x) * (t * 1.4);
                        let i = glow.vertices.len() as u32;
                        for p in [*a - n, *a + n, *bb + n, *bb - n] {
                            glow.colored_vertex(p, c);
                        }
                        glow.add_triangle(i, i + 1, i + 2);
                        glow.add_triangle(i, i + 2, i + 3);
                    }
                }
                x += w + gap;
            }
            ':' => {
                let cx = x + t * 0.6;
                for fy in [0.3, 0.72] {
                    add_rect(&mut mesh, Rect::from_center_size(pos2(cx, pos.y + h * fy), vec2(t, t)), lit);
                }
                x += t * 1.2 + gap;
            }
            '.' => {
                add_rect(&mut mesh, Rect::from_center_size(pos2(x + t * 0.6, pos.y + h - t / 2.0), vec2(t, t)), lit);
                x += t * 1.2 + gap;
            }
            _ => x += w + gap,
        }
    }
    painter.add(Shape::mesh(glow));
    painter.add(Shape::mesh(mesh));
    x - pos.x - gap
}

/// Width `seven_segment` will use for `s`.
pub fn seven_segment_width(s: &str, h: f32) -> f32 {
    let w = h * 0.52;
    let t = (h * 0.13).max(1.5);
    let gap = h * 0.22;
    s.chars().map(|c| if c == ':' || c == '.' { t * 1.2 + gap } else { w + gap }).sum::<f32>() - gap
}

fn add_rect(mesh: &mut Mesh, r: Rect, c: Color32) {
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(r.left_top(), c);
    mesh.colored_vertex(r.right_top(), c);
    mesh.colored_vertex(r.right_bottom(), c);
    mesh.colored_vertex(r.left_bottom(), c);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// Pill toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool, pal: &Palette) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(34.0, 18.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let bg = mix(pal.control_stroke, pal.accent, t);
    let p = ui.painter();
    p.rect_filled(rect, 9.0, bg);
    if resp.hovered() {
        p.rect_stroke(rect, 9.0, Stroke::new(1.0_f32, lighten(bg, 0.25)));
    }
    let knob_x = rect.left() + 9.0 + t * (rect.width() - 18.0);
    // The ring keeps the knob visible on a light track (High Contrast's white control border).
    p.circle_filled(pos2(knob_x, rect.center().y), 6.0, Color32::from_rgb(0xE4, 0xE4, 0xE4));
    p.circle_stroke(pos2(knob_x, rect.center().y), 6.0, Stroke::new(1.0_f32, with_alpha(pal.control, 0.6)));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}


// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

/// How a [`Button`] looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Faint wash with a hairline border (the default).
    Secondary,
    /// `text` fill with `body` text: the one main action.
    Primary,
    /// No fill until hovered.
    Ghost,
    /// Red outline for destructive actions.
    Danger,
    /// Solid red for the destructive action of a dialog.
    DangerFill,
}

/// Button heights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSize {
    /// 28 px, inside rows.
    Compact,
    /// 32 px.
    Small,
    /// 34 px.
    Medium,
    /// 36 px, the toolbar size (and the default).
    Toolbar,
    /// 44 px, hero and dialog buttons.
    Hero,
}

impl ButtonSize {
    pub fn height(self) -> f32 {
        match self {
            ButtonSize::Compact => 28.0,
            ButtonSize::Small => 32.0,
            ButtonSize::Medium => 34.0,
            ButtonSize::Toolbar => 36.0,
            ButtonSize::Hero => 44.0,
        }
    }
}

/// Button builder: `Button::primary("Scan").size(ButtonSize::Hero).icon(Icon::Play).show(ui, &pal)`.
pub struct Button<'a> {
    label: &'a str,
    kind: ButtonKind,
    size: ButtonSize,
    icon: Option<Icon>,
    min_width: f32,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str) -> Self {
        Self { label, kind: ButtonKind::Secondary, size: ButtonSize::Toolbar, icon: None, min_width: 0.0 }
    }
    pub fn primary(label: &'a str) -> Self {
        Self::new(label).kind(ButtonKind::Primary)
    }
    pub fn ghost(label: &'a str) -> Self {
        Self::new(label).kind(ButtonKind::Ghost)
    }
    pub fn danger(label: &'a str) -> Self {
        Self::new(label).kind(ButtonKind::Danger)
    }
    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }
    pub fn icon(mut self, icon: impl Into<Option<Icon>>) -> Self {
        self.icon = icon.into();
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }

    pub fn show(self, ui: &mut Ui, pal: &Palette) -> Response {
        let st = theme::status(pal);
        let font = theme::semibold(13.0);
        let pad = if self.size == ButtonSize::Hero { 20.0 } else { 14.0 };
        let tw = ui.painter().layout_no_wrap(self.label.to_string(), font.clone(), pal.text).size().x;
        let icon_w = if self.icon.is_some() { 22.0 } else { 0.0 };
        let w = (tw + 2.0 * pad + icon_w).max(self.min_width);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, self.size.height()), Sense::click());
        let enabled = ui.is_enabled();
        let hot = enabled && resp.hovered();
        let down = enabled && resp.is_pointer_button_down_on();
        let step = |idle: f32, hover: f32, press: f32| {
            if down {
                press
            } else if hot {
                hover
            } else {
                idle
            }
        };
        let (fill, border, fg) = match self.kind {
            ButtonKind::Secondary => (pal.raise(step(0.06, 0.10, 0.14)), pal.raise(step(0.10, 0.14, 0.18)), pal.text),
            ButtonKind::Primary => (mix(pal.text, pal.body, step(0.0, 0.08, 0.2)), Color32::TRANSPARENT, pal.body),
            ButtonKind::Ghost => (pal.raise(step(0.0, 0.06, 0.10)), Color32::TRANSPARENT, if hot { pal.text } else { pal.text_dim }),
            ButtonKind::Danger => (with_alpha(st.danger, step(0.08, 0.16, 0.22)), with_alpha(st.danger, 0.4), st.danger_text),
            ButtonKind::DangerFill => (lighten(theme::DANGER_FILL, step(0.0, 0.1, 0.0)), Color32::TRANSPARENT, theme::ON_DANGER),
        };
        let (fill, border, fg) = if enabled {
            (fill, border, fg)
        } else {
            (if self.kind == ButtonKind::Ghost { Color32::TRANSPARENT } else { pal.raise(0.04) }, pal.raise(0.06), pal.text_faint)
        };
        let p = ui.painter();
        p.rect_filled(rect, 6.0, fill);
        if border != Color32::TRANSPARENT {
            p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, border));
        }
        let mut x = rect.center().x - (tw + icon_w) / 2.0;
        if let Some(i) = self.icon {
            draw_icon(p, Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0)), i, fg);
            x += 22.0;
        }
        p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, self.label, font, fg);
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

/// Secondary toolbar button (36 px) in the application style.
pub fn button(ui: &mut Ui, label: &str, pal: &Palette, icon: Option<Icon>) -> Response {
    Button::new(label).icon(icon).show(ui, pal)
}

/// The one main action: `text` fill, `body` text.
pub fn primary_button(ui: &mut Ui, label: &str, pal: &Palette) -> Response {
    Button::primary(label).show(ui, pal)
}

/// Transparent button for the least important action.
pub fn ghost_button(ui: &mut Ui, label: &str, pal: &Palette) -> Response {
    Button::ghost(label).show(ui, pal)
}

/// Red outline button for destructive actions.
pub fn danger_button(ui: &mut Ui, label: &str, pal: &Palette) -> Response {
    Button::danger(label).show(ui, pal)
}

/// Text field with a magnifier and a hint, in the application style (36 px high).
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32, pal: &Palette) -> Response {
    let frame = egui::Frame::none().fill(pal.control).stroke(Stroke::new(1.0_f32, pal.control_stroke)).rounding(6.0).inner_margin(egui::Margin::symmetric(10.0, 8.0));
    frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (icon, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                draw_icon(ui.painter(), Rect::from_center_size(icon.center(), vec2(15.0, 15.0)), Icon::Search, pal.text_faint);
                ui.add(egui::TextEdit::singleline(text).hint_text(egui::RichText::new(hint).color(pal.text_faint)).desired_width(width - 46.0).frame(false).font(theme::ui(13.0)))
            })
            .inner
        })
        .inner
}

/// Segmented control drawn in one rectangle (layout direction does not matter): 34 px high with
/// 30 px segments, or 32 / 28 px with `mono` labels. Returns the index clicked.
pub fn segmented(ui: &mut Ui, labels: &[&str], selected: usize, pal: &Palette, mono: bool) -> Option<usize> {
    let (outer_h, font_off, font_on) = if mono { (32.0, theme::mono(12.0), theme::mono_medium(12.0)) } else { (34.0, theme::ui(13.0), theme::semibold(13.0)) };
    let widths: Vec<f32> = labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), font_on.clone(), pal.text).size().x + if mono { 20.0 } else { 24.0 }).collect();
    let (rect, _) = ui.allocate_exact_size(vec2(widths.iter().sum::<f32>() + 4.0, outer_h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, 6.0, pal.control);
    p.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, pal.control_stroke));
    let mut x = rect.left() + 2.0;
    let mut clicked = None;
    for (i, (l, w)) in labels.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(*w, outer_h - 4.0));
        let resp = ui.interact(r, ui.id().with(("segment", rect.min.x as i32, rect.min.y as i32, i)), Sense::click());
        let on = i == selected;
        if on {
            p.rect_filled(r, 4.0, pal.raise(0.12));
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, pal.raise(0.05));
        }
        let (font, color) = if on { (font_on.clone(), pal.text) } else { (font_off.clone(), if resp.hovered() { pal.text_dim } else { pal.text_faint }) };
        p.text(r.center(), Align2::CENTER_CENTER, l, font, color);
        if resp.clicked() && !on {
            clicked = Some(i);
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        x += w;
    }
    clicked
}

/// Underline tabs in the 36 px band at the top of `rect`: Medium when active with a 2 px accent
/// underline, an optional count (mono, faint) after each label, `trailing` text at the right end
/// and a hairline along the bottom. Returns the index clicked.
pub fn tab_bar(ui: &mut Ui, rect: Rect, pal: &Palette, tabs: &[(&str, Option<usize>)], selected: usize, trailing: Option<&str>) -> Option<usize> {
    let band = Rect::from_min_size(rect.min, vec2(rect.width(), 36.0));
    let p = ui.painter().clone();
    p.line_segment([pos2(band.left(), band.bottom() - 0.5), pos2(band.right(), band.bottom() - 0.5)], Stroke::new(1.0_f32, pal.sidebar_rule));
    let mut x = band.left();
    let mut clicked = None;
    for (i, (label, count)) in tabs.iter().enumerate() {
        let on = i == selected;
        let font = if on { theme::semibold(13.0) } else { theme::ui(13.0) };
        let lw = text_width(&p, label, font.clone());
        let cw = count.map(|c| text_width(&p, &c.to_string(), theme::mono(11.0)) + 6.0).unwrap_or(0.0);
        let r = Rect::from_min_size(pos2(x, band.top()), vec2(lw + cw + 28.0, band.height()));
        let resp = ui.interact(r, ui.id().with(("tab", band.min.x as i32, band.min.y as i32, i)), Sense::click());
        let color = if on || resp.hovered() { pal.text } else { pal.text_dim };
        p.text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, label, font, color);
        if let Some(c) = count {
            p.text(pos2(r.left() + 14.0 + lw + 6.0, r.center().y + 1.0), Align2::LEFT_CENTER, c, theme::mono(11.0), pal.text_faint);
        }
        if on {
            p.rect_filled(Rect::from_min_max(pos2(r.left() + 4.0, band.bottom() - 2.0), pos2(r.right() - 4.0, band.bottom())), 1.0, pal.accent);
        }
        if resp.clicked() && !on {
            clicked = Some(i);
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        x += r.width();
    }
    if let Some(t) = trailing {
        p.text(pos2(band.right() - 4.0, band.center().y), Align2::RIGHT_CENTER, t, theme::mono(12.0), pal.text_faint);
    }
    clicked
}

// ---------------------------------------------------------------------------
// Rings, bars, sparklines
// ---------------------------------------------------------------------------

thread_local! {
    /// (track, label) colours of the palette in use, for the bar and legend helpers that take no
    /// palette; set once per frame by the app (see [`set_ink`]).
    static INK: Cell<(Color32, Color32)> = const { Cell::new((Color32::TRANSPARENT, Color32::GRAY)) };
}

/// Sets the colours [`bar`] tracks and [`legend`] labels use. Called by the app every frame.
pub fn set_ink(pal: &Palette) {
    INK.with(|i| i.set((pal.raise(0.08), pal.text_dim)));
}

/// Whether sparklines smooth their curves (`graphs.smoothCurves`); set once per frame.
static SPARK_SMOOTH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_spark_smooth(on: bool) {
    SPARK_SMOOTH.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// How far the newest sample has scrolled in (0..1 of an update interval, as `f32` bits) and whether
/// live sparklines glide it in; set once per frame from the update clock.
static SPARK_PHASE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SPARK_GLIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_spark_glide(phase: f32, glide: bool) {
    SPARK_PHASE.store(phase.to_bits(), std::sync::atomic::Ordering::Relaxed);
    SPARK_GLIDE.store(glide, std::sync::atomic::Ordering::Relaxed);
}

/// The frame's glide for a graph of live samples: (phase, glide).
pub fn spark_glide() -> (f32, bool) {
    (f32::from_bits(SPARK_PHASE.load(std::sync::atomic::Ordering::Relaxed)), SPARK_GLIDE.load(std::sync::atomic::Ordering::Relaxed))
}

/// A progress ring: `track` full circle, `color` arc from 12 o'clock over `frac` (0..1) of it,
/// round-capped.
pub fn ring(p: &egui::Painter, center: Pos2, radius: f32, width: f32, frac: f32, color: Color32, track: Color32) {
    p.circle_stroke(center, radius, Stroke::new(width, track));
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    let n = ((frac * 72.0).ceil() as usize).max(2);
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * i as f32 / n as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::line(pts.clone(), Stroke::new(width, color)));
    p.circle_filled(pts[0], width / 2.0, color);
    p.circle_filled(pts[n], width / 2.0, color);
}

/// Concentric rings (the Summary hero): ring `i` has radius `radii[i]` and shows `fracs[i]`.
pub fn rings(p: &egui::Painter, center: Pos2, radii: &[f32], width: f32, fracs: &[(f32, Color32)], track: Color32) {
    for (r, (frac, color)) in radii.iter().zip(fracs) {
        ring(p, center, *r, width, *frac, *color, track);
    }
}

/// Thin progress bar: the height of `rect` is the thickness (2, 3 or 4 px), filled from the left
/// over `frac` (0..1) on a faint track.
pub fn bar(p: &egui::Painter, rect: Rect, frac: f32, color: Color32) {
    let r = rect.height() / 2.0;
    p.rect_filled(rect, r, INK.with(|i| i.get().0));
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.5 {
        p.rect_filled(Rect::from_min_size(rect.min, vec2(w.max(rect.height()), rect.height())), r, color);
    }
}

/// A bar of segments sized by their weights, `gap` px apart, each fully rounded.
pub fn share_bar(p: &egui::Painter, rect: Rect, parts: &[(f32, Color32)], gap: f32) {
    let parts: Vec<&(f32, Color32)> = parts.iter().filter(|(w, _)| *w > 0.0).collect();
    let total: f32 = parts.iter().map(|(w, _)| *w).sum();
    if parts.is_empty() || total <= 0.0 {
        return;
    }
    let avail = (rect.width() - gap * (parts.len() - 1) as f32).max(0.0);
    let mut x = rect.left();
    for (w, c) in parts {
        let sw = (avail * w / total).max(2.0);
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(sw, rect.height()));
        p.rect_filled(r, (rect.height() / 2.0).min(sw / 2.0), *c);
        x += sw + gap;
    }
}

/// Legend: 8 px dots with labels in a row from `pos` (left, vertically centred). Returns the width used.
pub fn legend(p: &egui::Painter, pos: Pos2, items: &[(&str, Color32)], font: FontId) -> f32 {
    let label = INK.with(|i| i.get().1);
    let mut x = pos.x;
    for (i, (text, color)) in items.iter().enumerate() {
        if i > 0 {
            x += 16.0;
        }
        p.circle_filled(pos2(x + 4.0, pos.y), 4.0, *color);
        let r = p.text(pos2(x + 14.0, pos.y), Align2::LEFT_CENTER, text, font.clone(), label);
        x = r.right();
    }
    x - pos.x
}

/// Whether sparklines use one neutral blue instead of their metric colours (`graphs.colorKeyed`
/// off); set once per frame.
static SPARK_NEUTRAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_spark_neutral(on: bool) {
    SPARK_NEUTRAL.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The colour of the `i`-th trace of a graph without colour keying: one neutral blue, later
/// traces dimmer.
pub fn neutral_graph_color(i: usize) -> Color32 {
    theme::dim(Color32::from_rgb(0x4F, 0xB4, 0xFF), 1.0 - 0.3 * i.min(2) as f32)
}

/// `color` for the `i`-th trace of a sparkline: itself, or the neutral blue without colour keying.
pub fn spark_color(i: usize, color: Color32) -> Color32 {
    if SPARK_NEUTRAL.load(std::sync::atomic::Ordering::Relaxed) { neutral_graph_color(i) } else { color }
}

/// A line chart without chrome: every series fitted across the whole width (oldest left, newest
/// right) at 1.4 px with an area fill of alpha `fill` (0 for none) and no markers, bloom or grid.
/// Each line is `(series, colour, (low, high))`. With `dashed_second` the second series is dashed
/// and unfilled (network down solid, up dashed).
pub fn sparkline(p: &egui::Painter, rect: Rect, lines: &[(&Series, Color32, (f32, f32))], fill: f32, dashed_second: bool) {
    spark_lines(p, rect, lines, fill, true, |i| dashed_second && i == 1);
}

/// [`sparkline`] for a series that is not sampled live (e.g. past benchmark results): no glide.
pub fn sparkline_static(p: &egui::Painter, rect: Rect, lines: &[(&Series, Color32, (f32, f32))], fill: f32) {
    spark_lines(p, rect, lines, fill, false, |_| false);
}

/// [`sparkline`] with every line dashed and unfilled (`graphs.dashedSummary`).
pub fn sparkline_dashed(p: &egui::Painter, rect: Rect, lines: &[(&Series, Color32, (f32, f32))]) {
    spark_lines(p, rect, lines, 0.0, true, |_| true);
}

/// Narrowest spacing of the samples of a live sparkline: a long history shows only its newest part.
pub const SPARK_MIN_PX: f32 = 2.0;

/// How many of `len` samples (history `cap`) a live sparkline `width` px wide shows.
pub fn spark_visible(width: f32, len: usize, cap: usize) -> usize {
    let px = (width / (cap.max(len).max(2) - 1) as f32).max(SPARK_MIN_PX);
    len.min((width / px).floor() as usize + 1)
}

fn spark_lines(p: &egui::Painter, rect: Rect, lines: &[(&Series, Color32, (f32, f32))], fill: f32, live: bool, dashed_at: impl Fn(usize) -> bool) {
    if lines.iter().map(|(s, _, _)| s.values.len()).max().unwrap_or(0) < 2 {
        return;
    }
    // A live series keeps a fixed time scale (its full history, at least SPARK_MIN_PX per sample), so
    // it grows in from the right instead of stretching (every point moving) while the history fills.
    let n = lines.iter().map(|(s, _, _)| if live { s.capacity.max(s.values.len()) } else { s.values.len() }).max().unwrap_or(2).max(2);
    let px = rect.width() / (n - 1) as f32;
    let px = if live { px.max(SPARK_MIN_PX) } else { px };
    let smooth = SPARK_SMOOTH.load(std::sync::atomic::Ordering::Relaxed);
    let (phase, glide) = if live { spark_glide() } else { (0.0, false) };
    for (i, (series, color, range)) in lines.iter().enumerate() {
        let dashed = dashed_at(i);
        let color = spark_color(i, *color);
        let mut g = Graph::new(vec![GraphLine { series, color, range: *range }]);
        g.regions = 1;
        g.px_per_update = px;
        g.bloom = 0.0;
        g.markers = false;
        g.smooth = smooth;
        g.line_width = 1.4;
        g.dashed = dashed;
        g.phase = phase;
        g.glide = glide;
        g.fill = if dashed { 0.0 } else { fill };
        draw_graph(p, rect, &g);
    }
}

/// A dashed horizontal line across `rect` at `y_frac` of its height measured from the bottom
/// (0 = bottom edge, 1 = top edge), e.g. the base clock in a clock history.
pub fn baseline(p: &egui::Painter, rect: Rect, y_frac: f32, color: Color32) {
    let y = rect.bottom() - rect.height() * y_frac.clamp(0.0, 1.0);
    p.extend(Shape::dashed_line(&[pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0_f32, color), 3.0, 3.0));
}

// ---------------------------------------------------------------------------
// Chips, badges, text with tracking
// ---------------------------------------------------------------------------

/// Text laid out glyph by glyph with `em` of letter-spacing (in multiples of the font size;
/// negative values are ignored, egui cannot tighten). Returns the rect used.
pub fn tracked_text(p: &egui::Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32, em: f32) -> Rect {
    let space = em.max(0.0) * font.size;
    if space < 0.05 {
        return p.text(pos, anchor, text, font, color);
    }
    let glyphs: Vec<(String, f32)> = text.chars().map(|c| (c.to_string(), text_width(p, &c.to_string(), font.clone()))).collect();
    let total = glyphs.iter().map(|(_, w)| *w).sum::<f32>() + space * glyphs.len().saturating_sub(1) as f32;
    let height = p.layout_no_wrap("Ag".into(), font.clone(), color).size().y;
    let rect = anchor.anchor_size(pos, vec2(total, height));
    let mut x = rect.left();
    for (g, w) in glyphs {
        if g != " " {
            p.text(pos2(x, rect.top()), Align2::LEFT_TOP, g, font.clone(), color);
        }
        x += w + space;
    }
    rect
}

/// Width of a [`chip`] with `text` (and an icon).
pub fn chip_width(p: &egui::Painter, text: &str, icon: bool) -> f32 {
    text_width(p, text, theme::semibold(12.0)) + 16.0 + if icon { 16.0 } else { 0.0 }
}

/// Status chip: a 22 px box washed with `color` holding `text` in Medium and an optional icon.
/// `pos` is the left edge, vertically centred.
pub fn chip(p: &egui::Painter, pos: Pos2, text: &str, color: Color32, icon: Option<Icon>) -> Rect {
    let rect = Rect::from_min_size(pos2(pos.x, pos.y - 11.0), vec2(chip_width(p, text, icon.is_some()), 22.0));
    p.rect_filled(rect, 4.0, with_alpha(color, 0.16));
    let mut x = rect.left() + 8.0;
    if let Some(i) = icon {
        draw_icon(p, Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)), i, color);
        x += 16.0;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, text, theme::semibold(12.0), color);
    rect
}

/// Width of a [`badge`] with `text`.
pub fn badge_width(p: &egui::Painter, text: &str) -> f32 {
    text_width(p, text, theme::ui(10.0)) + 0.06 * 10.0 * text.chars().count().saturating_sub(1) as f32 + 12.0
}

/// Outline badge ("WINDOWS"): 10 px text tracked 0.06 em, faint, hairline border. `pos` is the
/// left edge, vertically centred.
pub fn badge(p: &egui::Painter, pos: Pos2, text: &str, pal: &Palette) -> Rect {
    let rect = Rect::from_min_size(pos2(pos.x, pos.y - 8.0), vec2(badge_width(p, text), 16.0));
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, pal.raise(0.14)));
    tracked_text(p, rect.center(), Align2::CENTER_CENTER, text, theme::ui(10.0), pal.text_faint, 0.06);
    rect
}

/// Keyboard hint ("Ctrl K"): mono 11 in a hairline box. `pos` is the left edge, vertically centred.
pub fn key_hint(p: &egui::Painter, pos: Pos2, text: &str, pal: &Palette) -> Rect {
    let rect = Rect::from_min_size(pos2(pos.x, pos.y - 9.0), vec2(text_width(p, text, theme::mono(11.0)) + 12.0, 18.0));
    p.rect_stroke(rect, 3.0, Stroke::new(1.0_f32, pal.raise(0.12)));
    p.text(rect.center(), Align2::CENTER_CENTER, text, theme::mono(11.0), pal.text_faint);
    rect
}

/// How [`status_text`] marks a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    /// Running, established.
    Full,
    /// Stopped.
    Hollow,
    /// Paused.
    Half,
}

/// A drawn dot and `text` in `color`: running, stopped, paused. `pos` is the left edge,
/// vertically centred.
pub fn status_text(p: &egui::Painter, pos: Pos2, text: &str, color: Color32, kind: Dot) -> Rect {
    let c = pos2(pos.x + 4.5, pos.y);
    match kind {
        Dot::Full => {
            p.circle_filled(c, 4.0, color);
        }
        Dot::Hollow => {
            p.circle_stroke(c, 3.6, Stroke::new(1.2_f32, color));
        }
        Dot::Half => {
            p.circle_stroke(c, 3.6, Stroke::new(1.2_f32, color));
            let pts: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 12.0;
                    c + vec2(a.cos(), a.sin()) * 3.6
                })
                .collect();
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
    }
    let r = p.text(pos2(pos.x + 15.0, pos.y), Align2::LEFT_CENTER, text, theme::ui(13.0), color);
    Rect::from_min_max(pos2(pos.x, r.top().min(pos.y - 6.0)), pos2(r.right(), r.bottom().max(pos.y + 6.0)))
}

/// Rounded square standing in for an app icon: the shell icon `icon` when there is a texture,
/// else a gradient with the app's first letter.
pub fn icon_box(p: &egui::Painter, rect: Rect, pal: &Palette, icon: Option<TextureId>, letter: char) {
    let radius = (rect.height() * 0.22).clamp(3.0, 10.0);
    if let Some(tex) = icon {
        p.rect_filled(rect, radius, pal.raise(0.05));
        p.image(tex, rect.shrink(rect.height() * 0.14), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        return;
    }
    let (a, b, ink) = if pal.dark {
        (Color32::from_rgb(0x3B, 0x4B, 0x78), Color32::from_rgb(0x1F, 0x2A, 0x4A), pal.text)
    } else {
        (Color32::from_rgb(0xD6, 0xDD, 0xF0), Color32::from_rgb(0xB8, 0xC3, 0xE0), pal.text_dim)
    };
    let pts = rounded_rect_points(rect, Rounding::same(radius));
    let span = rect.width() + rect.height();
    let color_at = |q: Pos2| mix(a, b, ((q.x - rect.left()) + (q.y - rect.top())) / span.max(1.0));
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), color_at(rect.center()));
    for q in &pts {
        mesh.colored_vertex(*q, color_at(*q));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(Shape::mesh(mesh));
    let letter = letter.to_uppercase().next().unwrap_or('?').to_string();
    p.text(rect.center() + vec2(0.0, 0.5), Align2::CENTER_CENTER, letter, theme::semibold((rect.height() * 0.46).max(9.0)), ink);
}

// ---------------------------------------------------------------------------
// Logo and window backdrop
// ---------------------------------------------------------------------------

/// The TMOP mark: an orbit ring (three quarters of a circle) around a dot.
pub fn logo(p: &egui::Painter, center: Pos2, size: f32, accent: Color32) {
    let s = size / 28.0;
    let start = (-60.0_f32).to_radians();
    let pts: Vec<Pos2> = (0..=48)
        .map(|i| {
            let a = start + std::f32::consts::TAU * 0.75 * i as f32 / 48.0;
            center + vec2(a.cos(), a.sin()) * 11.0 * s
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.5 * s, accent)));
    p.circle_filled(center, 4.0 * s, accent);
}

/// Paints the window backdrop over `rect`: an elliptical gradient centred above the top left
/// (18 % across, 10 % above), `hi` in the middle, `mid` at 45 % of the radius, `edge` from there
/// out. The ellipse is 1200 x 700 on a 1440 x 900 window and scales with `rect`.
pub fn fill_radial_backdrop(p: &egui::Painter, rect: Rect, hi: Color32, mid: Color32, edge: Color32) {
    p.rect_filled(rect, 0.0, edge);
    let center = pos2(rect.left() + 0.18 * rect.width(), rect.top() - 0.10 * rect.height());
    let (rx, ry) = (1200.0 * rect.width() / 1440.0, 700.0 * rect.height() / 900.0);
    const SPOKES: u32 = 48;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, hi);
    for (t, c) in [(0.45_f32, mid), (1.0, edge)] {
        for i in 0..SPOKES {
            let a = std::f32::consts::TAU * i as f32 / SPOKES as f32;
            mesh.colored_vertex(center + vec2(a.cos() * rx * t, a.sin() * ry * t), c);
        }
    }
    for i in 0..SPOKES {
        let j = (i + 1) % SPOKES;
        mesh.add_triangle(0, 1 + i, 1 + j);
        mesh.add_triangle(1 + i, 1 + SPOKES + i, 1 + j);
        mesh.add_triangle(1 + j, 1 + SPOKES + i, 1 + SPOKES + j);
    }
    p.with_clip_rect(rect).add(Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// Icons (1.2 px isometric line set for navigation, flat glyphs for controls)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    // Navigation (isometric).
    Home,
    Chart,
    List,
    Container,
    Info,
    History,
    Power,
    User,
    Wrench,
    Bolt,
    Record,
    Globe,
    Apps,
    Driver,
    Disk,
    Gauge,
    Gear,
    Popout,
    // Devices and controls (flat).
    Refresh,
    Pulse,
    Cpu,
    Memory,
    Gpu,
    Network,
    Thermal,
    Close,
    Chevron,
    ChevronDown,
    ChevronRight,
    Play,
    Pause,
    Stop,
    Folder,
    Warning,
    Alert,
    Octagon,
    Shield,
    Search,
    Pin,
    Moon,
    Sun,
    Dots,
    Copy,
    Check,
    ArrowRight,
    Link,
}

/// Stroke width for an icon drawn `w` px wide: 1.2 px at rail size (22 px and up), heavier as
/// icons get smaller so they stay legible (1.8 px at 16 px).
fn icon_stroke(w: f32) -> f32 {
    if w >= 20.0 {
        1.2
    } else {
        (1.2 + (20.0 - w) * 0.15).min(1.8)
    }
}

/// Points of a cubic Bezier curve.
fn cubic(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) -> Vec<(f32, f32)> {
    (0..=8)
        .map(|i| {
            let t = i as f32 / 8.0;
            let u = 1.0 - t;
            let f = |a: f32, b: f32, c: f32, d: f32| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
            (f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1))
        })
        .collect()
}

/// Points of the circle (cx, cy, r) from `a0` to `a1` degrees.
fn arc_points(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    (0..=20)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / 20.0).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Draws `icon` into `r` in `color`. Navigation icons are the design's isometric 22 px glyphs
/// (`r` should be square, 22 px is the rail size); the rest are flat glyphs in a unit box.
pub fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let w0 = r.width().min(r.height());
    let sw = icon_stroke(w0);
    let s = Stroke::new(sw, color);
    // The 22 px box of the rail icons, and its isometric surface.
    let k = w0 / 22.0;
    let tl = r.center() - vec2(11.0, 11.0) * k;
    let iso = Iso { p, origin: tl + vec2(11.0, 6.0) * k, kx: 0.34, ky: 0.2, scale: k, width: sw, color };
    let at = |x: f32, y: f32| tl + vec2(x, y) * k;
    let (l, t, w, h) = (r.left(), r.top(), r.width(), r.height());
    let pt = |x: f32, y: f32| pos2(l + w * x, t + h * y);
    let line = |a: (f32, f32), b: (f32, f32)| p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], s);
    let poly = |pts: &[(f32, f32)]| p.add(Shape::closed_line(pts.iter().map(|&(x, y)| pt(x, y)).collect(), s));
    let open = |pts: &[(f32, f32)]| p.add(Shape::line(pts.iter().map(|&(x, y)| pt(x, y)).collect(), s));
    match icon {
        Icon::Home => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.rect(10.0, 10.0, 12.0, 12.0, 1.0, 0.0);
            iso.dot(16.0, 16.0, 2.0, 1.0);
        }
        Icon::Chart => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.poly(&[(6.0, 24.0), (12.0, 14.0), (18.0, 19.0), (26.0, 7.0)], false, 1.0, 0.0);
            iso.dot(26.0, 7.0, 1.6, 1.0);
        }
        Icon::List => {
            for y in [4.0, 13.0, 22.0] {
                iso.rect(2.0, y, 28.0, 6.0, 1.0, 0.0);
                iso.dot(6.0, y + 3.0, 1.0, 1.0);
            }
        }
        Icon::Container => {
            let line = |pts: &[(f32, f32)], a: f32| {
                p.add(Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), Stroke::new(sw, with_alpha(color, a))));
            };
            line(&[(11.0, 3.0), (18.0, 7.0), (11.0, 11.0), (4.0, 7.0), (11.0, 3.0)], 1.0);
            line(&[(4.0, 7.0), (4.0, 14.0), (11.0, 18.0), (11.0, 11.0)], 1.0);
            line(&[(18.0, 7.0), (18.0, 14.0), (11.0, 18.0)], 1.0);
            for (x, y0, y1) in [(6.0, 9.5, 13.0), (8.0, 10.5, 14.0), (14.0, 10.5, 14.0), (16.0, 9.5, 13.0)] {
                line(&[(x, y0), (x, y1)], 0.6);
            }
        }
        Icon::Info => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.rect(7.0, 7.0, 10.0, 10.0, 1.0, 0.0);
            iso.rect(21.0, 5.0, 3.0, 14.0, 1.0, 0.0);
            iso.rect(26.0, 5.0, 3.0, 14.0, 1.0, 0.0);
            iso.rect(7.0, 22.0, 18.0, 3.0, 1.0, 0.0);
        }
        Icon::History => {
            iso.circle(16.0, 16.0, 13.0, 1.0, 0.0);
            iso.poly(&[(16.0, 7.0), (16.0, 16.0), (23.0, 16.0)], false, 1.0, 0.0);
            iso.dot(16.0, 16.0, 1.4, 1.0);
        }
        Icon::Power => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.poly(&arc_points(16.0, 14.9, 7.0, -135.6, -404.4), false, 1.0, 0.0);
            iso.seg(16.0, 7.0, 16.0, 16.0, 1.0);
        }
        Icon::User => {
            iso.rect(2.0, 6.0, 28.0, 20.0, 1.0, 0.0);
            iso.circle(10.0, 14.0, 3.0, 1.0, 0.0);
            iso.poly(&cubic((6.0, 21.0), (7.0, 18.0), (13.0, 18.0), (14.0, 21.0)), false, 1.0, 0.0);
            iso.seg(17.0, 12.0, 26.0, 12.0, 1.0);
            iso.seg(17.0, 17.0, 26.0, 17.0, 1.0);
        }
        Icon::Wrench => {
            iso.circle(16.0, 16.0, 9.0, 1.0, 0.0);
            iso.circle(16.0, 16.0, 3.0, 1.0, 0.0);
            for (a, b, c, d) in [
                (16.0, 4.0, 16.0, 7.0),
                (16.0, 25.0, 16.0, 28.0),
                (4.0, 16.0, 7.0, 16.0),
                (25.0, 16.0, 28.0, 16.0),
                (7.5, 7.5, 9.5, 9.5),
                (22.5, 22.5, 24.5, 24.5),
                (7.5, 24.5, 9.5, 22.5),
                (22.5, 9.5, 24.5, 7.5),
            ] {
                iso.seg(a, b, c, d, 1.0);
            }
        }
        Icon::Bolt => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.poly(&[(19.0, 4.0), (10.0, 17.0), (16.0, 17.0), (13.0, 28.0), (23.0, 14.0), (17.0, 14.0)], true, 1.0, 0.15);
        }
        Icon::Record => {
            iso.circle(16.0, 16.0, 13.0, 1.0, 0.0);
            iso.circle(16.0, 16.0, 5.0, 1.0, 0.5);
            iso.seg(29.0, 16.0, 34.0, 16.0, 0.6);
        }
        Icon::Globe => {
            iso.circle(16.0, 16.0, 13.0, 1.0, 0.0);
            iso.ellipse(16.0, 16.0, 5.0, 13.0, 1.0, 0.0);
            iso.seg(3.0, 16.0, 29.0, 16.0, 0.6);
            iso.seg(5.0, 9.0, 27.0, 9.0, 0.6);
            iso.seg(5.0, 23.0, 27.0, 23.0, 0.6);
        }
        Icon::Apps => {
            for (x, y, f) in [(3.0, 3.0, 0.0), (18.0, 3.0, 0.0), (3.0, 18.0, 0.0), (18.0, 18.0, 0.15)] {
                iso.rect(x, y, 11.0, 11.0, 1.0, f);
            }
        }
        Icon::Driver => {
            iso.rect(9.0, 9.0, 14.0, 14.0, 1.0, 0.0);
            for (a, b, c, d) in [
                (13.0, 9.0, 13.0, 5.0),
                (19.0, 9.0, 19.0, 5.0),
                (13.0, 23.0, 13.0, 27.0),
                (19.0, 23.0, 19.0, 27.0),
                (9.0, 13.0, 5.0, 13.0),
                (9.0, 19.0, 5.0, 19.0),
                (23.0, 13.0, 27.0, 13.0),
                (23.0, 19.0, 27.0, 19.0),
            ] {
                iso.seg(a, b, c, d, 1.0);
            }
        }
        Icon::Disk => {
            iso.circle(16.0, 16.0, 13.0, 1.0, 0.0);
            iso.circle(16.0, 16.0, 4.0, 1.0, 0.0);
            iso.seg(27.0, 27.0, 20.0, 19.0, 1.0);
        }
        Icon::Gauge => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            iso.arc(16.0, 23.0, 10.0, 180.0, 360.0, 1.0);
            iso.seg(16.0, 23.0, 22.0, 13.0, 1.0);
            iso.dot(16.0, 23.0, 1.6, 1.0);
        }
        Icon::Gear => {
            iso.rect(2.0, 2.0, 28.0, 28.0, 1.0, 0.0);
            for (y, x) in [(10.0, 12.0), (16.0, 20.0), (22.0, 10.0)] {
                iso.seg(6.0, y, 26.0, y, 1.0);
                iso.dot(x, y, 2.0, 1.0);
            }
        }
        Icon::Popout => {
            iso.rect(2.0, 8.0, 20.0, 20.0, 1.0, 0.0);
            iso.poly(&[(14.0, 4.0), (28.0, 4.0), (28.0, 18.0)], false, 1.0, 0.0);
            iso.seg(28.0, 4.0, 16.0, 16.0, 1.0);
        }
        Icon::Refresh => {
            let c = r.center();
            let rad = w * 0.36;
            let pts: Vec<Pos2> = (0..20)
                .map(|i| {
                    let a = -0.4 * std::f32::consts::PI + 1.6 * std::f32::consts::PI * i as f32 / 19.0;
                    c + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            p.add(Shape::line(pts, s));
            line((0.55, 0.08), (0.8, 0.2));
            line((0.8, 0.2), (0.62, 0.42));
        }
        Icon::Pulse => {
            p.add(Shape::line(vec![pt(0.0, 0.55), pt(0.25, 0.55), pt(0.38, 0.15), pt(0.52, 0.9), pt(0.64, 0.4), pt(0.72, 0.55), pt(1.0, 0.55)], Stroke::new(sw.max(1.5), color)));
        }
        Icon::Cpu => {
            poly(&[(0.25, 0.25), (0.75, 0.25), (0.75, 0.75), (0.25, 0.75)]);
            for v in [0.38, 0.5, 0.62] {
                line((v, 0.08), (v, 0.25));
                line((v, 0.75), (v, 0.92));
                line((0.08, v), (0.25, v));
                line((0.75, v), (0.92, v));
            }
        }
        Icon::Memory => {
            poly(&[(0.08, 0.3), (0.92, 0.3), (0.92, 0.7), (0.08, 0.7)]);
            for x in [0.25, 0.42, 0.58, 0.75] {
                line((x, 0.42), (x, 0.58));
            }
        }
        Icon::Gpu => {
            poly(&[(0.08, 0.2), (0.92, 0.2), (0.92, 0.7), (0.08, 0.7)]);
            p.circle_stroke(pt(0.5, 0.45), w * 0.14, s);
            line((0.15, 0.7), (0.15, 0.88));
            line((0.35, 0.7), (0.35, 0.88));
        }
        Icon::Network => {
            poly(&[(0.3, 0.6), (0.7, 0.6), (0.7, 0.9), (0.3, 0.9)]);
            line((0.5, 0.6), (0.5, 0.35));
            line((0.15, 0.35), (0.85, 0.35));
            line((0.15, 0.35), (0.15, 0.18));
            line((0.85, 0.35), (0.85, 0.18));
        }
        Icon::Thermal => {
            poly(&[(0.4, 0.08), (0.6, 0.08), (0.6, 0.55), (0.7, 0.7), (0.5, 0.92), (0.3, 0.7), (0.4, 0.55)]);
            p.circle_filled(pt(0.5, 0.72), 2.0, color);
        }
        Icon::Close => {
            line((0.2, 0.2), (0.8, 0.8));
            line((0.8, 0.2), (0.2, 0.8));
        }
        Icon::Chevron | Icon::ChevronDown => {
            open(&[(0.2, 0.35), (0.5, 0.65), (0.8, 0.35)]);
        }
        Icon::ChevronRight => {
            open(&[(0.35, 0.2), (0.65, 0.5), (0.35, 0.8)]);
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![pt(0.25, 0.1), pt(0.9, 0.5), pt(0.25, 0.9)], color, Stroke::NONE));
        }
        Icon::Pause => {
            p.rect_filled(Rect::from_min_max(pt(0.26, 0.15), pt(0.42, 0.85)), 1.0, color);
            p.rect_filled(Rect::from_min_max(pt(0.58, 0.15), pt(0.74, 0.85)), 1.0, color);
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_min_max(pt(0.2, 0.2), pt(0.8, 0.8)), 1.0, color);
        }
        Icon::Folder => {
            p.add(Shape::closed_line(vec![pt(0.08, 0.2), pt(0.4, 0.2), pt(0.48, 0.32), pt(0.92, 0.32), pt(0.92, 0.85), pt(0.08, 0.85)], s));
        }
        Icon::Warning | Icon::Alert => {
            poly(&[(0.5, 0.08), (0.95, 0.9), (0.05, 0.9)]);
            line((0.5, 0.38), (0.5, 0.62));
            p.circle_filled(pt(0.5, 0.76), sw.max(1.3) * 0.9, color);
        }
        Icon::Octagon => {
            // A stop sign: solid, with the bar and dot cut out in the dark danger ink.
            let pts: Vec<Pos2> = [(8.0, 2.0), (16.0, 2.0), (22.0, 8.0), (22.0, 16.0), (16.0, 22.0), (8.0, 22.0), (2.0, 16.0), (2.0, 8.0)].iter().map(|&(x, y)| pt(x / 24.0, y / 24.0)).collect();
            p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
            p.rect_filled(Rect::from_min_max(pt(11.0 / 24.0, 7.0 / 24.0), pt(13.0 / 24.0, 14.0 / 24.0)), 0.5, theme::ON_DANGER);
            p.rect_filled(Rect::from_min_max(pt(11.0 / 24.0, 15.5 / 24.0), pt(13.0 / 24.0, 17.5 / 24.0)), 0.5, theme::ON_DANGER);
        }
        Icon::Shield => {
            poly(&[(0.5, 0.06), (0.9, 0.2), (0.9, 0.5), (0.5, 0.94), (0.1, 0.5), (0.1, 0.2)]);
        }
        Icon::Search => {
            p.circle_stroke(pt(11.0 / 24.0, 11.0 / 24.0), w * 7.0 / 24.0, s);
            line((16.5 / 24.0, 16.5 / 24.0), (21.0 / 24.0, 21.0 / 24.0));
        }
        Icon::Pin => {
            poly(&[(9.0 / 24.0, 4.0 / 24.0), (15.0 / 24.0, 4.0 / 24.0), (14.0 / 24.0, 10.0 / 24.0), (17.0 / 24.0, 13.0 / 24.0), (7.0 / 24.0, 13.0 / 24.0), (10.0 / 24.0, 10.0 / 24.0)]);
            line((0.5, 13.0 / 24.0), (0.5, 20.0 / 24.0));
        }
        Icon::Moon => {
            // Two arcs of radius 8: the inner edge about (16.85, 7.15), the outer about (12.65, 11.35).
            let mut pts: Vec<(f32, f32)> = arc_points(16.85, 7.15, 8.0, 66.8, 203.2);
            pts.extend(arc_points(12.65, 11.35, 8.0, 246.8, 23.2));
            open(&pts.iter().map(|&(x, y)| (x / 24.0, y / 24.0)).collect::<Vec<_>>());
        }
        Icon::Sun => {
            p.circle_stroke(r.center(), w * 4.0 / 24.0, s);
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([r.center() + d * w * 7.0 / 24.0, r.center() + d * w * 9.5 / 24.0], s);
            }
        }
        Icon::Dots => {
            for x in [0.2, 0.5, 0.8] {
                p.circle_filled(pt(x, 0.5), (w * 0.075).max(1.3), color);
            }
        }
        Icon::Copy => {
            poly(&[(0.36, 0.36), (0.86, 0.36), (0.86, 0.86), (0.36, 0.86)]);
            open(&[(0.14, 0.64), (0.14, 0.14), (0.64, 0.14)]);
        }
        Icon::Check => {
            open(&[(0.2, 0.55), (0.42, 0.75), (0.8, 0.28)]);
        }
        Icon::ArrowRight => {
            line((0.15, 0.5), (0.85, 0.5));
            open(&[(0.55, 0.2), (0.85, 0.5), (0.55, 0.8)]);
        }
        Icon::Link => {
            // Two links of a chain, side by side along the diagonal.
            let capsule = |c: (f32, f32)| {
                let (hl, rad) = (0.17_f32, 0.13_f32);
                let d = vec2(1.0, -1.0).normalized();
                let n = vec2(d.y, -d.x);
                let centre = pt(c.0, c.1);
                let mut pts = Vec::new();
                for (end, a0) in [(1.0_f32, -90.0_f32), (-1.0, 90.0)] {
                    let cap = centre + d * (end * hl * w);
                    for i in 0..=8 {
                        let a = (a0 + 180.0 * i as f32 / 8.0).to_radians();
                        pts.push(cap + (d * a.cos() + n * a.sin()) * rad * w);
                    }
                }
                p.add(Shape::closed_line(pts, s));
            };
            capsule((0.40, 0.60));
            capsule((0.60, 0.40));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Series;

    #[test]
    fn live_sparklines_keep_a_fixed_scale() {
        // 960-sample history in 200 px: 2 px per sample, so at most 101 samples show.
        assert_eq!(spark_visible(200.0, 10, 960), 10);
        assert_eq!(spark_visible(200.0, 960, 960), 101);
        // A short history spans the whole width.
        assert_eq!(spark_visible(200.0, 60, 60), 60);
    }

    fn run(add: impl Fn(&egui::Painter, &mut Ui, &Palette)) {
        let ctx = egui::Context::default();
        let pal = theme::DARK;
        pal.apply(&ctx);
        theme::install_fonts(&ctx, None);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = ui.painter().clone();
                    add(&p, ui, &pal);
                });
            });
        }
    }

    #[test]
    fn the_time_axis_maps_back_to_ages() {
        for regions in [1, 2, 4] {
            let axis = TimeAxis::new(700.0, 700.0, 16.0, regions);
            for age in [0.0_f32, 1.0, 10.0, 43.0, 120.0, 300.0] {
                let back = axis.age_at(700.0 - axis.x_of(age));
                assert!((back - age).abs() < 0.05 * age.max(1.0), "{regions} regions: {age} -> {back}");
            }
        }
    }

    #[test]
    fn every_icon_draws() {
        let icons = [
            Icon::Home, Icon::Chart, Icon::List, Icon::Container, Icon::Info, Icon::History, Icon::Power, Icon::User, Icon::Wrench, Icon::Bolt, Icon::Record, Icon::Globe, Icon::Apps, Icon::Driver, Icon::Disk, Icon::Gauge, Icon::Gear,
            Icon::Popout, Icon::Refresh, Icon::Pulse, Icon::Cpu, Icon::Memory, Icon::Gpu, Icon::Network, Icon::Thermal, Icon::Close, Icon::Chevron, Icon::ChevronDown, Icon::ChevronRight, Icon::Play, Icon::Pause, Icon::Stop,
            Icon::Folder, Icon::Warning, Icon::Alert, Icon::Octagon, Icon::Shield, Icon::Search, Icon::Pin, Icon::Moon, Icon::Sun, Icon::Dots, Icon::Copy, Icon::Check, Icon::ArrowRight, Icon::Link,
        ];
        run(|p, _, pal| {
            for (i, icon) in icons.iter().enumerate() {
                draw_icon(p, Rect::from_min_size(pos2(10.0 + 30.0 * i as f32, 10.0), vec2(22.0, 22.0)), *icon, pal.text);
                draw_icon(p, Rect::from_min_size(pos2(10.0 + 30.0 * i as f32, 50.0), vec2(14.0, 14.0)), *icon, pal.text);
            }
        });
    }

    #[test]
    fn components_draw() {
        let mut series = Series::new(60);
        let mut second = Series::new(60);
        for i in 0..60 {
            series.push((i % 17) as f32 * 5.0);
            second.push((i % 9) as f32 * 3.0);
        }
        run(|p, ui, pal| {
            let r = Rect::from_min_size(pos2(20.0, 20.0), vec2(200.0, 40.0));
            sparkline(p, r, &[(&series, pal.accent, (0.0, 100.0)), (&second, pal.text, (0.0, 100.0))], 0.15, true);
            sparkline(p, r, &[(&series, pal.accent, (0.0, 100.0))], 0.0, false);
            sparkline(p, r, &[], 0.0, false);
            baseline(p, r, 0.5, pal.text_faint);
            ring(p, pos2(100.0, 100.0), 26.0, 4.0, 0.7, pal.accent, pal.raise(0.08));
            ring(p, pos2(100.0, 100.0), 26.0, 4.0, 0.0, pal.accent, pal.raise(0.08));
            rings(p, pos2(200.0, 100.0), &[52.0, 44.0], 5.0, &[(0.5, pal.accent), (1.0, pal.ok)], pal.raise(0.08));
            bar(p, Rect::from_min_size(pos2(20.0, 150.0), vec2(100.0, 3.0)), 0.4, pal.accent);
            share_bar(p, Rect::from_min_size(pos2(20.0, 170.0), vec2(200.0, 12.0)), &[(1.0, pal.accent), (0.0, pal.ok), (2.0, pal.warn)], 3.0);
            share_bar(p, Rect::from_min_size(pos2(20.0, 170.0), vec2(200.0, 12.0)), &[], 3.0);
            assert!(legend(p, pos2(20.0, 200.0), &[("CPU", pal.accent), ("RAM", pal.ok)], theme::ui(12.0)) > 40.0);
            let c = chip(p, pos2(20.0, 220.0), "Running", pal.ok, Some(Icon::Check));
            assert_eq!(c.width(), chip_width(p, "Running", true));
            let b = badge(p, pos2(20.0, 250.0), "WINDOWS", pal);
            assert_eq!(b.width(), badge_width(p, "WINDOWS"));
            key_hint(p, pos2(20.0, 270.0), "Ctrl K", pal);
            for kind in [Dot::Full, Dot::Hollow, Dot::Half] {
                status_text(p, pos2(20.0, 290.0), "Paused", pal.warn, kind);
            }
            icon_box(p, Rect::from_min_size(pos2(20.0, 300.0), vec2(28.0, 28.0)), pal, None, 'c');
            let t = tracked_text(p, pos2(20.0, 340.0), Align2::LEFT_TOP, "TMOP", theme::semibold(15.0), pal.text, 0.18);
            let plain = tracked_text(p, pos2(20.0, 340.0), Align2::LEFT_TOP, "TMOP", theme::semibold(15.0), pal.text, 0.0);
            assert!(t.width() > plain.width(), "tracking widens the text");
            logo(p, pos2(300.0, 300.0), 28.0, pal.accent);
            fill_radial_backdrop(p, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)), pal.accent, pal.body, pal.card);
            // Controls.
            for kind in [ButtonKind::Secondary, ButtonKind::Primary, ButtonKind::Ghost, ButtonKind::Danger, ButtonKind::DangerFill] {
                let r = Button::new("Go").kind(kind).size(ButtonSize::Hero).icon(Icon::Play).show(ui, pal);
                assert_eq!(r.rect.height(), 44.0);
            }
            assert_eq!(segmented(ui, &["a", "b"], 0, pal, false), None);
            assert_eq!(segmented(ui, &["a", "b"], 1, pal, true), None);
            assert_eq!(tab_bar(ui, Rect::from_min_size(pos2(20.0, 400.0), vec2(300.0, 36.0)), pal, &[("All", Some(3)), ("Apps", None)], 0, Some("x")), None);
        });
    }

    #[test]
    fn button_sizes() {
        assert_eq!([ButtonSize::Compact, ButtonSize::Small, ButtonSize::Medium, ButtonSize::Toolbar, ButtonSize::Hero].map(ButtonSize::height), [28.0, 32.0, 34.0, 36.0, 44.0]);
    }
}
