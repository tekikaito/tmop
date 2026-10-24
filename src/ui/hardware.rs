//! Hardware line set: hairline isometric drawings of the machine's parts (CPU, memory module,
//! drive, graphics card, network module, battery, NPU, mainboard), drawn from the design's SVGs.
//! The same isometric transform draws the navigation rail icons (see [`super::widgets::draw_icon`]).

use super::theme::{with_alpha, Palette};
use super::widgets;
use eframe::egui::{pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Shape, Stroke};

/// A part of the machine to draw with [`draw`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hardware {
    Cpu,
    Memory,
    /// NVMe M.2 solid state drive.
    Disk,
    /// Spinning disk, for volumes that are not solid state.
    Hdd,
    Gpu,
    Network,
    Battery,
    Npu,
    Mainboard,
    /// The CPU package taken apart: heat spreader, die, substrate, socket pins.
    CpuExploded,
}

impl Hardware {
    /// Size of the drawing's view box: (144, 96) for the small parts, (72, 48) for [`Hardware::Hdd`]
    /// and (200, 240) for the exploded CPU.
    pub fn view_box(self) -> (f32, f32) {
        match self {
            Hardware::Hdd => (72.0, 48.0),
            Hardware::CpuExploded => (200.0, 240.0),
            _ => (144.0, 96.0),
        }
    }
}

/// Isometric drawing surface: local coordinates (x, y) land at
/// `origin + ((x - y) * kx, (x + y) * ky) * scale`, the SVG matrix `(kx ky -kx ky)`.
pub(super) struct Iso<'a> {
    pub p: &'a Painter,
    pub origin: Pos2,
    pub kx: f32,
    pub ky: f32,
    pub scale: f32,
    pub width: f32,
    pub color: Color32,
}

impl Iso<'_> {
    pub fn pt(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.origin.x + (x - y) * self.kx * self.scale, self.origin.y + (x + y) * self.ky * self.scale)
    }

    fn stroke(&self, alpha: f32) -> Stroke {
        Stroke::new(self.width, with_alpha(self.color, alpha))
    }

    fn fill(&self, pts: Vec<Pos2>, fill: f32) {
        if fill > 0.0 {
            self.p.add(Shape::convex_polygon(pts, with_alpha(self.color, fill), Stroke::NONE));
        }
    }

    /// Open or closed polyline through local points, stroked at `alpha`, closed shapes also filled at `fill`.
    pub fn poly(&self, pts: &[(f32, f32)], closed: bool, alpha: f32, fill: f32) {
        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| self.pt(x, y)).collect();
        if closed {
            self.fill(pts.clone(), fill);
            self.p.add(Shape::closed_line(pts, self.stroke(alpha)));
        } else {
            self.p.add(Shape::line(pts, self.stroke(alpha)));
        }
    }

    pub fn rect(&self, x: f32, y: f32, w: f32, h: f32, alpha: f32, fill: f32) {
        self.poly(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true, alpha, fill);
    }

    /// Rectangle with rounded corners of radius `r`.
    #[allow(clippy::too_many_arguments)]
    pub fn rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, alpha: f32, fill: f32) {
        let mut pts = Vec::new();
        for (cx, cy, a0) in [(x + w - r, y + r, -90.0_f32), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 90.0), (x + r, y + r, 180.0)] {
            for k in 0..=3 {
                let a = (a0 + 30.0 * k as f32).to_radians();
                pts.push((cx + r * a.cos(), cy + r * a.sin()));
            }
        }
        self.poly(&pts, true, alpha, fill);
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, alpha: f32, fill: f32) {
        self.ellipse(cx, cy, r, r, alpha, fill);
    }

    /// Ellipse with radii (`rx`, `ry`) in local coordinates.
    pub fn ellipse(&self, cx: f32, cy: f32, rx: f32, ry: f32, alpha: f32, fill: f32) {
        let pts: Vec<(f32, f32)> = (0..32).map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 32.0;
            (cx + rx * a.cos(), cy + ry * a.sin())
        }).collect();
        self.poly(&pts, true, alpha, fill);
    }

    /// A filled dot of radius `r` (SVG `fill="currentColor"` circles).
    pub fn dot(&self, cx: f32, cy: f32, r: f32, alpha: f32) {
        let pts: Vec<Pos2> = (0..12).map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 12.0;
            self.pt(cx + r * a.cos(), cy + r * a.sin())
        }).collect();
        self.p.add(Shape::convex_polygon(pts, with_alpha(self.color, alpha), Stroke::NONE));
    }

    pub fn seg(&self, x1: f32, y1: f32, x2: f32, y2: f32, alpha: f32) {
        self.p.line_segment([self.pt(x1, y1), self.pt(x2, y2)], self.stroke(alpha));
    }

    /// Elliptical arc of the circle (cx, cy, r) from `a0` to `a1` degrees.
    pub fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, alpha: f32) {
        let n = 24;
        let pts: Vec<Pos2> = (0..=n).map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            self.pt(cx + r * a.cos(), cy + r * a.sin())
        }).collect();
        self.p.add(Shape::line(pts, self.stroke(alpha)));
    }
}

/// Draws `kind` into `rect` (aspect 3:2, the exploded CPU 5:6, a disk 3:2) with `color`, 1.2 px
/// strokes and fills of at most 15 %. The drawing is fitted into `rect` and centred.
pub fn draw(p: &Painter, rect: Rect, kind: Hardware, color: Color32) {
    let (vw, vh) = kind.view_box();
    let s = (rect.width() / vw).min(rect.height() / vh);
    let top_left = rect.center() - vec2(vw, vh) * s / 2.0;
    let at = |x: f32, y: f32| top_left + vec2(x, y) * s;
    let iso = |ox: f32, oy: f32| Iso { p, origin: at(ox, oy), kx: 1.04, ky: 0.6, scale: s, width: 1.2, color };
    match kind {
        Hardware::Cpu => {
            let g = iso(72.0, 10.0);
            g.rect(14.0, 14.0, 36.0, 36.0, 1.0, 0.0);
            g.rect(22.0, 22.0, 20.0, 20.0, 1.0, 0.12);
            for v in [22.0, 28.0, 34.0, 40.0] {
                g.seg(v, 10.0, v, 14.0, 0.7);
                g.seg(v, 50.0, v, 54.0, 0.7);
                g.seg(10.0, v, 14.0, v, 0.7);
                g.seg(50.0, v, 54.0, v, 0.7);
            }
            g.dot(18.0, 18.0, 1.2, 1.0);
        }
        Hardware::Memory => {
            let g = iso(72.0, 14.0);
            g.rect(4.0, 22.0, 56.0, 20.0, 1.0, 0.0);
            for x in [8.0, 20.0, 32.0, 44.0] {
                g.rect(x, 25.0, 9.0, 8.0, 1.0, 0.12);
            }
            for x in [8.0, 12.0, 16.0, 20.0, 24.0, 28.0, 36.0, 40.0, 44.0, 48.0, 52.0, 56.0] {
                g.seg(x, 42.0, x, 45.0, 0.7);
            }
            g.poly(&[(31.0, 42.0), (31.0, 46.0), (33.0, 46.0), (33.0, 42.0)], false, 0.7, 0.0);
        }
        Hardware::Disk => {
            let g = iso(72.0, 14.0);
            g.rect(6.0, 26.0, 52.0, 12.0, 1.0, 0.0);
            g.rect(12.0, 28.0, 9.0, 8.0, 1.0, 0.12);
            g.rect(24.0, 28.0, 10.0, 8.0, 1.0, 0.12);
            g.rect(36.0, 28.0, 10.0, 8.0, 1.0, 0.12);
            g.circle(53.0, 32.0, 2.0, 1.0, 0.0);
            for y in [28.0, 31.0, 34.0, 37.0] {
                g.seg(6.0, y, 3.0, y, 0.7);
            }
        }
        Hardware::Hdd => {
            let g = Iso { p, origin: at(36.0, 2.0), kx: 0.52, ky: 0.3, scale: s, width: 1.2, color };
            g.rrect(6.0, 6.0, 52.0, 52.0, 2.0, 1.0, 0.0);
            g.circle(32.0, 32.0, 18.0, 1.0, 0.0);
            g.circle(32.0, 32.0, 4.0, 1.0, 0.0);
            g.seg(50.0, 50.0, 40.0, 36.0, 1.0);
        }
        Hardware::Gpu => {
            let g = iso(72.0, 10.0);
            g.rect(6.0, 18.0, 52.0, 28.0, 1.0, 0.0);
            g.rect(2.0, 16.0, 4.0, 32.0, 0.7, 0.0);
            for cx in [22.0, 42.0] {
                g.circle(cx, 32.0, 9.0, 1.0, 0.0);
                g.circle(cx, 32.0, 3.0, 1.0, 0.15);
                g.seg(cx, 23.0, cx, 41.0, 0.35);
                g.seg(cx - 9.0, 32.0, cx + 9.0, 32.0, 0.35);
            }
            for x in [12.0, 16.0, 20.0, 24.0, 28.0, 32.0] {
                g.seg(x, 46.0, x, 49.0, 0.7);
            }
        }
        Hardware::Network => {
            let g = iso(72.0, 16.0);
            g.rect(18.0, 30.0, 28.0, 20.0, 1.0, 0.0);
            g.rect(24.0, 34.0, 10.0, 10.0, 1.0, 0.12);
            g.circle(42.0, 46.0, 1.5, 1.0, 0.0);
            for y in [34.0, 38.0, 42.0, 46.0] {
                g.seg(18.0, y, 15.0, y, 0.7);
            }
            // Three arcs fanning out of the emitter, drawn flat (the design's `a r r 0 0 1` paths).
            let flat = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32, alpha: f32| {
                let pts: Vec<Pos2> = (0..=16).map(|i| {
                    let a = (a0 + (a1 - a0) * i as f32 / 16.0).to_radians();
                    at(cx + r * a.cos(), cy + r * a.sin())
                }).collect();
                p.add(Shape::line(pts, Stroke::new(1.2_f32, with_alpha(color, alpha))));
            };
            flat(110.0, 45.8, 14.0, -135.6, -44.4, 0.9);
            flat(110.0, 45.1, 22.0, -136.7, -43.3, 0.6);
            flat(110.0, 44.4, 30.0, -137.2, -42.8, 0.35);
            p.circle_filled(at(110.0, 42.0), 1.6 * s, color);
        }
        Hardware::Battery => {
            let g = iso(72.0, 14.0);
            g.rrect(8.0, 22.0, 46.0, 22.0, 2.0, 1.0, 0.0);
            g.rect(54.0, 29.0, 4.0, 8.0, 0.7, 0.0);
            g.rect(12.0, 26.0, 30.0, 14.0, 1.0, 0.12);
            g.seg(27.0, 30.0, 27.0, 36.0, 0.7);
            g.seg(24.0, 33.0, 30.0, 33.0, 0.7);
        }
        Hardware::Npu => {
            let g = iso(72.0, 10.0);
            g.rect(14.0, 14.0, 36.0, 36.0, 1.0, 0.0);
            g.poly(&[(20.0, 20.0), (44.0, 20.0), (44.0, 44.0), (20.0, 44.0)], true, 0.5, 0.0);
            g.poly(&[(26.0, 26.0), (38.0, 26.0), (38.0, 38.0), (26.0, 38.0)], true, 0.5, 0.0);
            for (a, b, c, d) in [(32.0, 14.0, 32.0, 10.0), (32.0, 50.0, 32.0, 54.0), (14.0, 32.0, 10.0, 32.0), (50.0, 32.0, 54.0, 32.0)] {
                g.seg(a, b, c, d, 0.7);
            }
            for v in [22.0, 42.0] {
                g.seg(v, 14.0, v, 10.0, 0.7);
                g.seg(v, 50.0, v, 54.0, 0.7);
                g.seg(14.0, v, 10.0, v, 0.7);
                g.seg(50.0, v, 54.0, v, 0.7);
            }
        }
        Hardware::Mainboard => {
            let g = iso(72.0, 6.0);
            g.rect(4.0, 4.0, 56.0, 56.0, 1.0, 0.0);
            g.rect(18.0, 12.0, 20.0, 20.0, 0.8, 0.0);
            g.rect(42.0, 10.0, 4.0, 26.0, 0.6, 0.0);
            g.rect(48.0, 10.0, 4.0, 26.0, 0.6, 0.0);
            g.rect(10.0, 40.0, 36.0, 5.0, 0.6, 0.0);
            g.rect(10.0, 49.0, 36.0, 5.0, 0.6, 0.0);
            for (cx, cy) in [(8.0, 8.0), (56.0, 8.0), (8.0, 56.0), (56.0, 56.0)] {
                g.circle(cx, cy, 1.2, 1.0, 0.0);
            }
        }
        Hardware::CpuExploded => exploded(p, top_left, s, color, color),
    }
}

/// The exploded CPU (200 x 240 view box) at `top_left`, `s` px per unit. `accent` tints the die.
fn exploded(p: &Painter, top_left: Pos2, s: f32, color: Color32, accent: Color32) {
    let at = |x: f32, y: f32| top_left + vec2(x, y) * s;
    let stroke = |a: f32| Stroke::new(1.2_f32, with_alpha(color, a));
    let line = |pts: &[(f32, f32)], a: f32| {
        p.add(Shape::line(pts.iter().map(|&(x, y)| at(x, y)).collect(), stroke(a)));
    };
    let closed = |pts: &[(f32, f32)], a: f32| {
        p.add(Shape::closed_line(pts.iter().map(|&(x, y)| at(x, y)).collect(), stroke(a)));
    };
    // Guides.
    for (x1, y1, x2, y2) in [(40.0, 36.0, 40.0, 192.0), (160.0, 36.0, 160.0, 192.0), (100.0, 6.0, 100.0, 162.0), (100.0, 72.0, 100.0, 226.0)] {
        p.extend(Shape::dashed_line(&[at(x1, y1), at(x2, y2)], stroke(0.35), 2.0, 3.0));
    }
    // Heat spreader.
    closed(&[(40.0, 36.0), (100.0, 6.0), (160.0, 36.0), (100.0, 66.0)], 1.0);
    closed(&[(52.0, 36.0), (100.0, 12.0), (148.0, 36.0), (100.0, 60.0)], 0.6);
    line(&[(40.0, 36.0), (40.0, 42.0), (100.0, 72.0), (160.0, 42.0), (160.0, 36.0)], 0.6);
    line(&[(100.0, 66.0), (100.0, 72.0)], 0.6);
    line(&[(92.0, 20.0), (108.0, 20.0)], 0.4);
    // Die layer.
    let y = 52.0;
    closed(&[(64.0, 36.0 + y), (100.0, 18.0 + y), (136.0, 36.0 + y), (100.0, 54.0 + y)], 0.6);
    let die: Vec<Pos2> = [(84.0, 36.0 + y), (100.0, 28.0 + y), (116.0, 36.0 + y), (100.0, 44.0 + y)].iter().map(|&(x, y)| at(x, y)).collect();
    p.add(Shape::convex_polygon(die.clone(), with_alpha(accent, 0.35), Stroke::new(1.2_f32, accent)));
    line(&[(84.0, 36.0 + y), (100.0, 44.0 + y), (116.0, 36.0 + y)], 0.5);
    for (x, yy) in [(76.0, 36.0), (124.0, 36.0), (100.0, 22.0), (100.0, 50.0)] {
        p.circle_filled(at(x, yy + y), 1.2 * s, color);
    }
    // Substrate with solder balls, socket with pins.
    let grid: Vec<(f32, f32)> = (0..5).flat_map(|i| (0..5).map(move |j| (100.0 + 8.0 * (i as f32 - j as f32), 20.0 + 4.0 * (i + j) as f32))).collect();
    let y = 104.0;
    closed(&[(40.0, 36.0 + y), (100.0, 6.0 + y), (160.0, 36.0 + y), (100.0, 66.0 + y)], 1.0);
    line(&[(40.0, 36.0 + y), (40.0, 40.0 + y), (100.0, 70.0 + y), (160.0, 40.0 + y), (160.0, 36.0 + y)], 0.6);
    line(&[(100.0, 66.0 + y), (100.0, 70.0 + y)], 0.6);
    for &(x, yy) in &grid {
        p.circle_filled(at(x, yy + y), 1.6 * s, with_alpha(color, 0.7));
    }
    let y = 156.0;
    closed(&[(40.0, 36.0 + y), (100.0, 6.0 + y), (160.0, 36.0 + y), (100.0, 66.0 + y)], 1.0);
    line(&[(40.0, 36.0 + y), (40.0, 44.0 + y), (100.0, 74.0 + y), (160.0, 44.0 + y), (160.0, 36.0 + y)], 0.6);
    line(&[(100.0, 66.0 + y), (100.0, 74.0 + y)], 0.6);
    for &(x, yy) in &grid {
        line(&[(x, yy + y), (x, yy + y + 6.0)], 0.7);
    }
}

/// The exploded CPU in `rect` (aspect 5:6, e.g. 150 x 180) with the four layer labels to its
/// right, top to bottom: heat spreader, die, substrate, socket. Each label is a title and a mono
/// subline; the die is tinted with `accent`.
pub fn cpu_exploded(p: &Painter, rect: Rect, color: Color32, accent: Color32, labels: &[(&str, &str); 4], pal: &Palette) {
    let (vw, vh) = Hardware::CpuExploded.view_box();
    let s = (rect.width() / vw).min(rect.height() / vh);
    let top_left = rect.center() - vec2(vw, vh) * s / 2.0;
    exploded(p, top_left, s, color, accent);
    // Labels sit at the layers' heights: spreader 36, die 88, substrate 140, socket 192 (view units).
    let x = rect.right() + 18.0;
    for (i, (title, sub)) in labels.iter().enumerate() {
        let y = top_left.y + (36.0 + 52.0 * i as f32) * s;
        let r = widgets::text(p, pos2(x, y - 2.0), Align2::LEFT_BOTTOM, title, super::theme::semibold(12.0), pal.text);
        widgets::text(p, pos2(x, r.bottom() + 1.0), Align2::LEFT_TOP, sub, super::theme::mono(11.0), pal.text_faint);
    }
}
