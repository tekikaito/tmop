//! Custom-painted widgets: scrolling history graphs with compressed time, glow and
//! smoothed curves; segmented LED meters; dot-matrix LED panels; cards; toggles; icons.

use super::theme::{self, dim, lighten, mix, with_alpha, MetricColors, Palette};
use crate::metrics::Series;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Mesh, Pos2, Rect, Response, Rounding, Sense, Shape, Stroke, Ui, Vec2};

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

/// Card: rounded panel with a 1 px border.
pub fn card(painter: &egui::Painter, rect: Rect, pal: &Palette) {
    painter.rect_filled(rect, 6.0, pal.card);
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, pal.card_stroke));
}

/// Card whose border is tinted with a metric colour (bottom tiles, per-core graphs).
pub fn accent_card(painter: &egui::Painter, rect: Rect, pal: &Palette, accent: Color32, bloom: f32) {
    painter.rect_filled(rect, 6.0, pal.card);
    if bloom > 0.0 {
        painter.rect_stroke(rect.expand(1.5), 7.0, Stroke::new(3.0_f32, with_alpha(accent, 0.10 * bloom)));
        painter.rect_stroke(rect.shrink(1.0), 5.0, Stroke::new(2.0_f32, with_alpha(accent, 0.08 * bloom)));
    }
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0_f32, with_alpha(accent, 0.85)));
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

/// Maps a sample's age (updates, fractional) to x. The newest `width / regions` (the live part)
/// advances `p` px per update. Older time is compressed continuously: the px-per-update rate
/// decays exponentially with distance, so spacing never jumps. The decay is chosen so the whole
/// width holds as much history as `regions` equal regions of 1x, 2x, 4x, 8x... would.
#[derive(Clone, Copy)]
struct TimeAxis {
    right: f32,
    p: f32,
    /// Width of the uncompressed live part.
    live: f32,
    /// Decay rate per px beyond the live part (0 = uniform time).
    k: f32,
}

impl TimeAxis {
    fn new(right: f32, width: f32, p: f32, regions: u32) -> Self {
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

    fn x_of(&self, age: f32) -> f32 {
        self.right - self.dist(age)
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
    let p = g.px_per_update.max(2.0);
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
    /// Green → yellow → red by position (CPU load meters): the CPU and clock colours around yellow.
    pub fn traffic(m: &MetricColors, bloom: f32) -> Self {
        Self { positional: Some([m.get("cpu"), m.get("yellow"), m.get("clock")]), ..Self::new(m.get("cpu"), bloom) }
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

/// Flat button in the application style.
pub fn button(ui: &mut Ui, label: &str, pal: &Palette, icon: Option<Icon>) -> Response {
    let font = theme::ui(13.0);
    let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), pal.text).size().x + 24.0 + if icon.is_some() { 20.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    let p = ui.painter();
    let enabled = ui.is_enabled();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let bg = if !enabled { pal.control } else if pressed { pal.selection } else if resp.hovered() { pal.hover } else { pal.control };
    let fg = if pressed { pal.on_selection() } else if enabled { pal.text } else { pal.text_faint };
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, pal.control_stroke));
    let mut x = rect.left() + 12.0;
    if let Some(i) = icon {
        draw_icon(p, Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0)), i, fg);
        x += 20.0;
    }
    p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Text field with a hint, in the application style.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32, pal: &Palette) -> Response {
    let frame = egui::Frame::none().fill(pal.control).stroke(Stroke::new(1.0_f32, pal.control_stroke)).rounding(4.0).inner_margin(egui::Margin::symmetric(8.0, 6.0));
    frame
        .show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(width - 16.0).frame(false).font(theme::ui(13.0)))
        })
        .inner
}

// ---------------------------------------------------------------------------
// Icons (simple vector glyphs, 1.5 px strokes)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Home,
    Chart,
    List,
    Gear,
    Popout,
    Refresh,
    Pulse,
    Bolt,
    Record,
    Globe,
    Apps,
    Driver,
    Disk,
    Gauge,
    Cpu,
    Memory,
    Gpu,
    Network,
    Thermal,
    Close,
    Chevron,
    Play,
    Stop,
    Folder,
    Info,
    History,
    User,
    Wrench,
}

pub fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.5_f32, color);
    let (l, t, w, h) = (r.left(), r.top(), r.width(), r.height());
    let pt = |x: f32, y: f32| pos2(l + w * x, t + h * y);
    let line = |a: (f32, f32), b: (f32, f32)| p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], s);
    let poly = |pts: &[(f32, f32)]| p.add(Shape::closed_line(pts.iter().map(|&(x, y)| pt(x, y)).collect(), s));
    match icon {
        Icon::Home => {
            poly(&[(0.5, 0.08), (0.95, 0.5), (0.82, 0.5), (0.82, 0.92), (0.18, 0.92), (0.18, 0.5), (0.05, 0.5)]);
            line((0.42, 0.92), (0.42, 0.62));
            line((0.42, 0.62), (0.58, 0.62));
            line((0.58, 0.62), (0.58, 0.92));
        }
        Icon::Chart => {
            poly(&[(0.08, 0.1), (0.92, 0.1), (0.92, 0.9), (0.08, 0.9)]);
            p.add(Shape::line(vec![pt(0.18, 0.7), pt(0.38, 0.4), pt(0.55, 0.6), pt(0.82, 0.25)], s));
        }
        Icon::List => {
            for y in [0.22, 0.5, 0.78] {
                line((0.12, y), (0.88, y));
            }
            for y in [0.22, 0.5, 0.78] {
                p.circle_filled(pt(0.14, y), 1.6, color);
            }
        }
        Icon::Gear => {
            p.circle_stroke(r.center(), w * 0.22, s);
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([r.center() + d * w * 0.3, r.center() + d * w * 0.45], Stroke::new(2.0_f32, color));
            }
        }
        Icon::Popout => {
            poly(&[(0.08, 0.3), (0.65, 0.3), (0.65, 0.92), (0.08, 0.92)]);
            p.add(Shape::line(vec![pt(0.35, 0.3), pt(0.35, 0.08), pt(0.92, 0.08), pt(0.92, 0.7), pt(0.65, 0.7)], s));
        }
        Icon::Refresh => {
            let c = r.center();
            let rad = w * 0.36;
            let pts: Vec<Pos2> = (0..20).map(|i| {
                let a = -0.4 * std::f32::consts::PI + 1.6 * std::f32::consts::PI * i as f32 / 19.0;
                c + vec2(a.cos(), a.sin()) * rad
            }).collect();
            p.add(Shape::line(pts, s));
            line((0.55, 0.08), (0.8, 0.2));
            line((0.8, 0.2), (0.62, 0.42));
        }
        Icon::Pulse => {
            p.add(Shape::line(vec![pt(0.0, 0.55), pt(0.25, 0.55), pt(0.38, 0.15), pt(0.52, 0.9), pt(0.64, 0.4), pt(0.72, 0.55), pt(1.0, 0.55)], Stroke::new(1.8_f32, color)));
        }
        Icon::Bolt => {
            poly(&[(0.6, 0.05), (0.2, 0.55), (0.48, 0.55), (0.4, 0.95), (0.82, 0.42), (0.54, 0.42)]);
        }
        Icon::Record => {
            p.circle_stroke(r.center(), w * 0.42, s);
            p.circle_filled(r.center(), w * 0.2, color);
        }
        Icon::Globe => {
            p.circle_stroke(r.center(), w * 0.42, s);
            line((0.08, 0.5), (0.92, 0.5));
            p.add(Shape::closed_line(ellipse_points(Rect::from_center_size(r.center(), vec2(w * 0.4, h * 0.84)), 24), s));
        }
        Icon::Apps => {
            for (x, y) in [(0.1, 0.1), (0.55, 0.1), (0.1, 0.55), (0.55, 0.55)] {
                poly(&[(x, y), (x + 0.35, y), (x + 0.35, y + 0.35), (x, y + 0.35)]);
            }
        }
        Icon::Driver => {
            poly(&[(0.1, 0.25), (0.9, 0.25), (0.9, 0.75), (0.1, 0.75)]);
            for x in [0.3, 0.5, 0.7] {
                line((x, 0.75), (x, 0.92));
                line((x, 0.08), (x, 0.25));
            }
        }
        Icon::Disk => {
            poly(&[(0.08, 0.35), (0.92, 0.35), (0.92, 0.65), (0.08, 0.65)]);
            p.circle_filled(pt(0.8, 0.5), 1.8, color);
        }
        Icon::Gauge => {
            let c = pt(0.5, 0.65);
            let rad = w * 0.42;
            let pts: Vec<Pos2> = (0..16).map(|i| {
                let a = std::f32::consts::PI + std::f32::consts::PI * i as f32 / 15.0;
                c + vec2(a.cos(), a.sin()) * rad
            }).collect();
            p.add(Shape::line(pts, s));
            p.line_segment([c, c + vec2(0.5, -0.75).normalized() * rad * 0.8], Stroke::new(2.0_f32, color));
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
            p.add(Shape::line(vec![pt(0.15, 0.35), pt(0.85, 0.35)], s));
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
        Icon::Chevron => {
            p.add(Shape::line(vec![pt(0.2, 0.35), pt(0.5, 0.65), pt(0.8, 0.35)], s));
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![pt(0.25, 0.1), pt(0.9, 0.5), pt(0.25, 0.9)], color, Stroke::NONE));
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_min_max(pt(0.2, 0.2), pt(0.8, 0.8)), 1.0, color);
        }
        Icon::Folder => {
            p.add(Shape::closed_line(vec![pt(0.08, 0.2), pt(0.4, 0.2), pt(0.48, 0.32), pt(0.92, 0.32), pt(0.92, 0.85), pt(0.08, 0.85)], s));
        }
        Icon::Info => {
            p.circle_stroke(r.center(), w * 0.42, s);
            line((0.5, 0.45), (0.5, 0.75));
            p.circle_filled(pt(0.5, 0.29), 1.3, color);
        }
        Icon::History => {
            let c = r.center();
            let rad = w * 0.4;
            let pts: Vec<Pos2> = (0..24).map(|i| {
                let a = -0.9 * std::f32::consts::PI + 1.75 * std::f32::consts::PI * i as f32 / 23.0;
                c + vec2(a.cos(), a.sin()) * rad
            }).collect();
            p.add(Shape::line(pts, s));
            line((0.5, 0.3), (0.5, 0.52));
            line((0.5, 0.52), (0.66, 0.62));
            line((0.08, 0.28), (0.14, 0.42));
            line((0.14, 0.42), (0.3, 0.38));
        }
        Icon::User => {
            p.circle_stroke(pt(0.5, 0.32), w * 0.18, s);
            p.add(Shape::line((0..=16).map(|i| {
                let a = std::f32::consts::PI + std::f32::consts::PI * i as f32 / 16.0;
                pt(0.5, 0.95) + vec2(a.cos() * w * 0.36, a.sin() * h * 0.36)
            }).collect(), s));
        }
        Icon::Wrench => {
            p.circle_stroke(pt(0.68, 0.32), w * 0.2, s);
            line((0.54, 0.46), (0.14, 0.86));
            line((0.2, 0.92), (0.08, 0.8));
        }
    }
}
