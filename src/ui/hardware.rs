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
        Hardware::CpuExploded => exploded(p, top_left, s, Exploded::Cpu, color, color),
    }
}

/// A device taken apart into four stacked layers (200 x 240 view box, see [`exploded`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exploded {
    /// Heat spreader, die, substrate, socket.
    Cpu,
    /// Heat spreader, DRAM chips, PCB, slot.
    Memory,
    /// Label, NAND, controller, M.2 interface.
    Disk,
    /// Shroud, die, VRAM, PCB and bus.
    Gpu,
    /// Shield, controller, PHY, link.
    Network,
    /// Fins, heat pipes, sensor plate, base.
    Thermal,
    /// Containers, engine, runtime, host.
    Docker,
}

/// Top of each layer in the exploded view box; a layer's diamond is centred 30 units lower.
const LAYERS: [f32; 4] = [0.0, 52.0, 104.0, 156.0];

/// Drawing surface of the exploded view: view-box units at `tl`, `s` px per unit, 1.2 px strokes.
struct Fig<'a> {
    p: &'a Painter,
    tl: Pos2,
    s: f32,
    color: Color32,
    accent: Color32,
}

type Uv = (f32, f32, f32, f32);

impl Fig<'_> {
    fn at(&self, (x, y): (f32, f32)) -> Pos2 {
        self.tl + vec2(x, y) * self.s
    }
    fn stroke(&self, a: f32) -> Stroke {
        Stroke::new(1.2_f32, with_alpha(self.color, a))
    }
    fn line(&self, pts: &[(f32, f32)], a: f32) {
        self.p.add(Shape::line(pts.iter().map(|&q| self.at(q)).collect(), self.stroke(a)));
    }
    fn closed(&self, pts: &[(f32, f32)], a: f32) {
        self.p.add(Shape::closed_line(pts.iter().map(|&q| self.at(q)).collect(), self.stroke(a)));
    }
    fn dot(&self, q: (f32, f32), r: f32, a: f32) {
        self.p.circle_filled(self.at(q), r * self.s, with_alpha(self.color, a));
    }
    fn guide(&self, x: f32, y0: f32, y1: f32) {
        self.p.extend(Shape::dashed_line(&[self.at((x, y0)), self.at((x, y1))], self.stroke(0.35), 2.0, 3.0));
    }
    /// Guides dropping from the top layer to the bottom one through the (u, v) points.
    fn guides(&self, uv: &[(f32, f32)]) {
        for &(u, v) in uv {
            let ((x, a), (_, b)) = (Self::q(LAYERS[0], u, v), Self::q(LAYERS[3], u, v));
            self.guide(x, a, b);
        }
    }
    /// Point on the plane of the layer at `y`: (0, 0) is the top corner of its 120 x 60 diamond,
    /// `u` runs to the right corner, `v` to the left one.
    fn q(y: f32, u: f32, v: f32) -> (f32, f32) {
        (100.0 + 60.0 * (u - v), 6.0 + y + 30.0 * (u + v))
    }
    /// The (u0, v0)..(u1, v1) patch of layer `y`: top, right, bottom, left corner.
    fn quad(y: f32, (u0, v0, u1, v1): Uv) -> [(f32, f32); 4] {
        [Self::q(y, u0, v0), Self::q(y, u1, v0), Self::q(y, u1, v1), Self::q(y, u0, v1)]
    }
    fn patch(&self, y: f32, uv: Uv, a: f32) {
        self.closed(&Self::quad(y, uv), a);
    }
    fn patch_fill(&self, y: f32, uv: Uv, a: f32, fill: f32) {
        let pts: Vec<Pos2> = Self::quad(y, uv).iter().map(|&q| self.at(q)).collect();
        self.p.add(Shape::convex_polygon(pts, with_alpha(self.color, fill), self.stroke(a)));
    }
    /// The highlighted part: filled and stroked in the accent colour.
    fn accent(&self, y: f32, uv: Uv) {
        let pts: Vec<Pos2> = Self::quad(y, uv).iter().map(|&q| self.at(q)).collect();
        self.p.add(Shape::convex_polygon(pts, with_alpha(self.accent, 0.35), Stroke::new(1.2_f32, self.accent)));
    }
    /// A slab: the patch outlined at `a` with its two front faces `h` units deep.
    fn block(&self, y: f32, uv: Uv, h: f32, a: f32) {
        let [_, r, b, l] = Self::quad(y, uv);
        self.closed(&Self::quad(y, uv), a);
        self.line(&[l, (l.0, l.1 + h), (b.0, b.1 + h), (r.0, r.1 + h), r], 0.6);
        self.line(&[b, (b.0, b.1 + h)], 0.6);
    }
    /// Circle of radius `r` (uv units) on layer `y`, an ellipse on screen.
    fn ring(&self, y: f32, cu: f32, cv: f32, r: f32, a: f32) {
        let pts: Vec<(f32, f32)> = (0..32).map(|i| {
            let t = std::f32::consts::TAU * i as f32 / 32.0;
            Self::q(y, cu + r * t.cos(), cv + r * t.sin())
        }).collect();
        self.closed(&pts, a);
    }
}

/// The exploded `kind` (200 x 240 view box) at `top_left`, `s` px per unit. `accent` tints the
/// layer that does the work (the CPU die, the DRAM chips, the NAND, ...).
fn exploded(p: &Painter, top_left: Pos2, s: f32, kind: Exploded, color: Color32, accent: Color32) {
    let f = Fig { p, tl: top_left, s, color, accent };
    let [y0, y1, y2, y3] = LAYERS;
    let q = Fig::q;
    match kind {
        Exploded::Cpu => {
            for (x, a, b) in [(40.0, 36.0, 192.0), (160.0, 36.0, 192.0), (100.0, 6.0, 162.0), (100.0, 72.0, 226.0)] {
                f.guide(x, a, b);
            }
            // Heat spreader.
            f.block(y0, (0.0, 0.0, 1.0, 1.0), 6.0, 1.0);
            f.patch(y0, (0.1, 0.1, 0.9, 0.9), 0.6);
            f.line(&[(92.0, 20.0), (108.0, 20.0)], 0.4);
            // Die on its package.
            f.patch(y1, (0.2, 0.2, 0.8, 0.8), 0.6);
            let (d0, d1) = (11.0 / 30.0, 19.0 / 30.0);
            f.accent(y1, (d0, d0, d1, d1));
            f.line(&[q(y1, d0, d1), q(y1, d1, d1), q(y1, d1, d0)], 0.5);
            for (u, v) in [(0.2, 0.5), (0.8, 0.5), (0.5, 0.2), (0.5, 0.8)] {
                let (x, yy) = q(y1, u, v);
                f.dot((x, yy), 1.2, 1.0);
            }
            // Substrate with solder balls, socket with pins.
            let grid: Vec<(f32, f32)> = (0..5).flat_map(|i| (0..5).map(move |j| (100.0 + 8.0 * (i as f32 - j as f32), 20.0 + 4.0 * (i + j) as f32))).collect();
            f.block(y2, (0.0, 0.0, 1.0, 1.0), 4.0, 1.0);
            for &(x, yy) in &grid {
                f.dot((x, yy + y2), 1.6, 0.7);
            }
            f.block(y3, (0.0, 0.0, 1.0, 1.0), 8.0, 1.0);
            for &(x, yy) in &grid {
                f.line(&[(x, yy + y3), (x, yy + y3 + 6.0)], 0.7);
            }
        }
        Exploded::Memory => {
            // A module runs corner to corner, as wide as the CPU's diamond.
            let stick = (-0.5, 0.4, 1.5, 0.6);
            f.guides(&[(-0.5, 0.6), (1.5, 0.4)]);
            // Heat spreader with its groove.
            f.block(y0, stick, 7.0, 1.0);
            f.patch(y0, (-0.44, 0.46, 1.44, 0.54), 0.6);
            // DRAM chips.
            f.patch(y1, stick, 0.35);
            for i in 0..8 {
                let u = -0.44 + 0.24 * i as f32;
                f.accent(y1, (u, 0.43, u + 0.17, 0.57));
            }
            // PCB with its contacts and key notch.
            f.block(y2, (-0.5, 0.38, 1.5, 0.62), 3.0, 1.0);
            for i in 0..40 {
                let u = -0.46 + 0.05 * i as f32;
                if (u - 0.4).abs() > 0.04 {
                    f.line(&[q(y2, u, 0.57), q(y2, u, 0.62)], 0.7);
                }
            }
            // Slot with latches.
            f.block(y3, (-0.5, 0.34, 1.5, 0.66), 9.0, 1.0);
            f.patch(y3, (-0.46, 0.47, 1.46, 0.53), 0.6);
            f.patch(y3, (-0.62, 0.4, -0.54, 0.6), 0.7);
            f.patch(y3, (1.54, 0.4, 1.62, 0.6), 0.7);
        }
        Exploded::Disk => {
            let stick = (-0.5, 0.28, 1.5, 0.72);
            f.guides(&[(-0.5, 0.72), (1.5, 0.28)]);
            // Label.
            f.block(y0, stick, 2.0, 1.0);
            f.line(&[q(y0, -0.25, 0.4), q(y0, 0.6, 0.4)], 0.4);
            f.line(&[q(y0, -0.25, 0.5), q(y0, 0.3, 0.5)], 0.4);
            f.line(&[q(y0, -0.25, 0.6), q(y0, 0.1, 0.6)], 0.4);
            // NAND packages.
            f.patch(y1, stick, 0.35);
            f.accent(y1, (-0.1, 0.34, 0.45, 0.66));
            f.accent(y1, (0.55, 0.34, 1.1, 0.66));
            // Board with controller, DRAM cache and the screw notch.
            f.block(y2, stick, 3.0, 1.0);
            f.patch_fill(y2, (-0.38, 0.36, -0.08, 0.64), 1.0, 0.12);
            f.patch(y2, (0.02, 0.4, 0.2, 0.6), 0.6);
            for v in [0.36, 0.44, 0.56, 0.64] {
                f.line(&[q(y2, -0.08, v), q(y2, 0.3, v)], 0.3);
            }
            f.ring(y2, 1.5, 0.5, 0.07, 0.7);
            // M.2 socket, keyed, with the stick's outline where it plugs in.
            f.patch(y3, (-0.3, 0.28, 1.5, 0.72), 0.25);
            f.block(y3, (-0.6, 0.22, -0.3, 0.78), 9.0, 1.0);
            for k in 0..12 {
                let v = 0.27 + 0.042 * k as f32;
                if (v - 0.6).abs() > 0.02 {
                    f.line(&[q(y3, -0.54, v), q(y3, -0.36, v)], 0.7);
                }
            }
        }
        Exploded::Gpu => {
            let card = (-0.1, 0.08, 1.1, 0.92);
            f.guides(&[(-0.1, 0.92), (1.1, 0.08)]);
            // Shroud with two fans.
            f.block(y0, card, 7.0, 1.0);
            for cu in [0.24, 0.76] {
                f.ring(y0, cu, 0.5, 0.26, 1.0);
                f.ring(y0, cu, 0.5, 0.07, 0.7);
                for k in 0..6 {
                    let t = std::f32::consts::TAU * k as f32 / 6.0;
                    f.line(&[q(y0, cu + 0.07 * t.cos(), 0.5 + 0.07 * t.sin()), q(y0, cu + 0.24 * (t + 0.5).cos(), 0.5 + 0.24 * (t + 0.5).sin())], 0.45);
                }
            }
            // Die on its package.
            f.patch(y1, card, 0.35);
            f.patch(y1, (0.28, 0.28, 0.72, 0.72), 0.6);
            f.accent(y1, (0.38, 0.38, 0.62, 0.62));
            // VRAM around the die.
            f.patch(y2, card, 0.35);
            for (cu, cv) in [(0.1, 0.3), (0.1, 0.7), (0.5, 0.17), (0.5, 0.83), (0.9, 0.3), (0.9, 0.7)] {
                f.patch_fill(y2, (cu - 0.08, cv - 0.07, cu + 0.08, cv + 0.07), 1.0, 0.12);
            }
            // PCB with the PCIe fingers and the bracket.
            f.block(y3, card, 3.0, 1.0);
            for i in 0..20 {
                let u = 0.1 + 0.03 * i as f32;
                f.line(&[q(y3, u, 0.85), q(y3, u, 0.92)], 0.7);
            }
            f.patch(y3, (-0.18, 0.02, -0.12, 0.98), 0.7);
        }
        Exploded::Network => {
            let board = (0.0, 0.05, 1.0, 0.95);
            f.guides(&[(0.0, 0.95), (1.0, 0.05)]);
            // Shield can with vents.
            f.block(y0, (0.15, 0.15, 0.85, 0.85), 5.0, 1.0);
            for i in 0..4 {
                for j in 0..4 {
                    f.dot(q(y0, 0.29 + 0.14 * i as f32, 0.29 + 0.14 * j as f32), 1.1, 0.5);
                }
            }
            // Controller.
            f.patch(y1, board, 0.35);
            f.accent(y1, (0.36, 0.36, 0.64, 0.64));
            for (a, b) in [((0.08, 0.5), (0.36, 0.5)), ((0.64, 0.5), (0.92, 0.5)), ((0.5, 0.12), (0.5, 0.36)), ((0.5, 0.64), (0.5, 0.88))] {
                f.line(&[q(y1, a.0, a.1), q(y1, b.0, b.1)], 0.5);
            }
            // PHY and magnetics on the board.
            f.block(y2, board, 3.0, 1.0);
            f.patch_fill(y2, (0.12, 0.18, 0.38, 0.44), 1.0, 0.12);
            f.patch(y2, (0.55, 0.6, 0.72, 0.82), 0.7);
            f.patch(y2, (0.76, 0.6, 0.93, 0.82), 0.7);
            f.line(&[q(y2, 0.38, 0.31), q(y2, 0.64, 0.31), q(y2, 0.64, 0.6)], 0.4);
            // RJ45 jack with its contacts.
            f.block(y3, (0.22, 0.15, 0.78, 0.85), 12.0, 1.0);
            f.patch(y3, (0.3, 0.26, 0.7, 0.74), 0.6);
            for k in 0..8 {
                let v = 0.32 + 0.05 * k as f32;
                f.line(&[q(y3, 0.36, v), q(y3, 0.5, v)], 0.7);
            }
        }
        Exploded::Thermal => {
            f.guides(&[(0.0, 1.0), (1.0, 0.0)]);
            // Fins.
            for i in 0..8 {
                let u = 0.06 + 0.125 * i as f32;
                let (a, b) = (q(y0, u, 0.0), q(y0, u, 1.0));
                f.closed(&[a, b, (b.0, b.1 + 9.0), (a.0, a.1 + 9.0)], if i == 7 { 1.0 } else { 0.55 });
            }
            // Heat pipes.
            for v in [0.3, 0.5, 0.7] {
                f.line(&[q(y1, 0.0, v - 0.025), q(y1, 1.0, v - 0.025)], 1.0);
                f.line(&[q(y1, 0.0, v + 0.025), q(y1, 1.0, v + 0.025)], 1.0);
                f.ring(y1, 1.0, v, 0.025, 0.7);
            }
            // Cold plate with the sensor.
            f.block(y2, (0.2, 0.2, 0.8, 0.8), 5.0, 1.0);
            f.accent(y2, (0.38, 0.38, 0.62, 0.62));
            // Base with mounting holes.
            f.block(y3, (0.0, 0.0, 1.0, 1.0), 3.0, 1.0);
            f.patch(y3, (0.25, 0.25, 0.75, 0.75), 0.5);
            for (cu, cv) in [(0.1, 0.1), (0.9, 0.1), (0.1, 0.9), (0.9, 0.9)] {
                f.ring(y3, cu, cv, 0.04, 0.8);
            }
        }
        Exploded::Docker => {
            f.guides(&[(0.0, 1.0), (1.0, 0.0)]);
            // Containers.
            for u in [0.0, 0.35, 0.7] {
                f.block(y0, (u, 0.25, u + 0.3, 0.75), 9.0, 1.0);
                f.line(&[q(y0, u + 0.05, 0.5), q(y0, u + 0.25, 0.5)], 0.4);
            }
            // Engine.
            f.patch(y1, (0.05, 0.1, 0.95, 0.9), 0.35);
            f.accent(y1, (0.34, 0.34, 0.66, 0.66));
            // Runtime.
            f.block(y2, (0.05, 0.1, 0.95, 0.9), 3.0, 1.0);
            for v in [0.3, 0.5, 0.7] {
                f.line(&[q(y2, 0.15, v), q(y2, 0.85, v)], 0.4);
            }
            // Host.
            f.block(y3, (0.0, 0.0, 1.0, 1.0), 4.0, 1.0);
            for i in 0..5 {
                f.dot(q(y3, 0.1 + 0.2 * i as f32, 0.5), 1.4, 0.7);
            }
        }
    }
}

/// The exploded `kind` in `rect` (aspect 5:6, e.g. 150 x 180) with its four layer labels to the
/// right, top to bottom, each a title and a mono subline clipped to `label_w`.
#[allow(clippy::too_many_arguments)]
pub fn exploded_view(p: &Painter, rect: Rect, kind: Exploded, color: Color32, accent: Color32, labels: &[(&str, &str); 4], label_w: f32, pal: &Palette) {
    let (vw, vh) = Hardware::CpuExploded.view_box();
    let s = (rect.width() / vw).min(rect.height() / vh);
    let top_left = rect.center() - vec2(vw, vh) * s / 2.0;
    exploded(p, top_left, s, kind, color, accent);
    // Labels sit at the layers' centres: 36, 88, 140, 192 (view units).
    let x = rect.right() + 18.0;
    for ((title, sub), ly) in labels.iter().zip(LAYERS) {
        let y = top_left.y + (36.0 + ly) * s;
        let r = widgets::text_fit(p, pos2(x, y - 2.0), Align2::LEFT_BOTTOM, title, super::theme::semibold(12.0), pal.text, label_w);
        widgets::text_fit(p, pos2(x, r.bottom() + 1.0), Align2::LEFT_TOP, sub, super::theme::mono(11.0), pal.text_faint, label_w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;
    use eframe::egui;

    /// Every drawing and the exploded CPU with its labels draw without trouble.
    #[test]
    fn every_drawing_draws() {
        let ctx = egui::Context::default();
        theme::DARK.apply(&ctx);
        theme::install_fonts(&ctx, None);
        let kinds = [Hardware::Cpu, Hardware::Memory, Hardware::Disk, Hardware::Hdd, Hardware::Gpu, Hardware::Network, Hardware::Battery, Hardware::Npu, Hardware::Mainboard, Hardware::CpuExploded];
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = ui.painter().clone();
                    for kind in kinds {
                        let (vw, vh) = kind.view_box();
                        let rect = Rect::from_min_size(pos2(40.0, 40.0), vec2(vw, vh));
                        draw(&p, rect, kind, theme::DARK.text);
                    }
                    for kind in [Exploded::Cpu, Exploded::Memory, Exploded::Disk, Exploded::Gpu, Exploded::Network, Exploded::Thermal, Exploded::Docker] {
                        exploded_view(&p, Rect::from_min_size(pos2(40.0, 40.0), vec2(150.0, 180.0)), kind, theme::DARK.text, theme::DARK.accent, &[("a", "b"), ("c", "d"), ("e", "f"), ("g", "h")], 200.0, &theme::DARK);
                    }
                });
            });
        }
    }

    /// The iso transform: local (0, 0) lands at the origin, x moves right and down, y left and down.
    #[test]
    fn iso_axes() {
        let ctx = egui::Context::default();
        let p = Painter::new(ctx, egui::LayerId::background(), Rect::EVERYTHING);
        let g = Iso { p: &p, origin: pos2(72.0, 10.0), kx: 1.04, ky: 0.6, scale: 1.0, width: 1.2, color: Color32::WHITE };
        assert_eq!(g.pt(0.0, 0.0), pos2(72.0, 10.0));
        let (x, y) = (g.pt(10.0, 0.0), g.pt(0.0, 10.0));
        assert!(x.x > 72.0 && x.y > 10.0 && y.x < 72.0 && y.y > 10.0);
        assert!((x.x - 72.0 + (y.x - 72.0)).abs() < 1e-4, "x and y mirror each other");
    }
}
