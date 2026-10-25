//! The program icon: the line-drawn CPU chip of [`crate::ui::hardware`] in the accent colour on
//! a dark rounded square. `assets/icon/` holds it as PNGs and a multi-size `.ico` (the exe's
//! resource, see `build.rs`); regenerate them with
//! `cargo test --locked --lib icon::tests::write_assets -- --ignored`.

/// The window icon (title bar, taskbar, Alt+Tab), 64 px.
const PNG_64: &[u8] = include_bytes!("../assets/icon/tmop-64.png");

/// RGBA pixels of a square PNG, or `None` when it cannot be decoded.
fn decode(bytes: &[u8]) -> Option<(Vec<u8>, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba && info.width == info.height).then(|| (buf[..info.buffer_size()].to_vec(), info.width))
}

/// The window icon for a viewport builder, decoded once.
pub fn window_icon() -> Option<std::sync::Arc<eframe::egui::IconData>> {
    static ICON: std::sync::OnceLock<Option<std::sync::Arc<eframe::egui::IconData>>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let (rgba, size) = decode(PNG_64)?;
        Some(std::sync::Arc::new(eframe::egui::IconData { rgba, width: size, height: size }))
    })
    .clone()
}

#[cfg(test)]
mod art {
    //! Software rasteriser for the icon: 4 x 4 supersampled coverage of filled polygons and
    //! round-ended strokes, composited in order.

    type P = (f32, f32);
    const BODY: [u8; 3] = [0x0B, 0x0F, 0x1C];
    const RIM: [u8; 3] = [0x26, 0x2E, 0x44];
    const ACCENT: [u8; 3] = [0x5E, 0xEA, 0xD4];

    enum Mark {
        /// Convex polygon.
        Fill(Vec<P>),
        /// Segments `width` px wide with round ends.
        Stroke(Vec<(P, P)>, f32),
        /// Rounded square inset by `inset`, corner radius `r` (all in px).
        Rounded(f32, f32),
    }

    fn inside(m: &Mark, size: f32, x: f32, y: f32) -> bool {
        match m {
            Mark::Fill(pts) => {
                let mut sign = 0.0_f32;
                for i in 0..pts.len() {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    let c = (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
                    if c != 0.0 {
                        if sign != 0.0 && c.signum() != sign {
                            return false;
                        }
                        sign = c.signum();
                    }
                }
                true
            }
            Mark::Stroke(segs, w) => segs.iter().any(|&(a, b)| {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
                let (px, py) = (a.0 + t * dx - x, a.1 + t * dy - y);
                px * px + py * py <= w * w / 4.0
            }),
            Mark::Rounded(inset, r) => {
                let (lo, hi) = (inset + r, size - inset - r);
                let (cx, cy) = (x.clamp(lo, hi), y.clamp(lo, hi));
                (x - cx).powi(2) + (y - cy).powi(2) <= r * r
            }
        }
    }

    /// The icon at `size` px as RGBA.
    pub fn rgba(size: u32) -> Vec<u8> {
        let s = size as f32;
        // The chip of `Hardware::Cpu`: its isometric surface (x - y, x + y) * (1.04, 0.6), fitted
        // to the icon. Local units span 10..54 with the pins; small sizes drop the pins and let
        // the package (14..50) fill the width.
        let small = size < 32;
        let (lo, hi) = if small { (14.0, 50.0) } else { (10.0, 54.0) };
        let fit = if small { 0.9 } else { 0.86 } * s / ((hi - lo) * 2.0 * 1.04);
        let (ox, oy) = (s / 2.0, s / 2.0 - (lo + hi) * 0.6 * fit);
        let pt = |x: f32, y: f32| (ox + (x - y) * 1.04 * fit, oy + (x + y) * 0.6 * fit);
        let quad = |x0: f32, y0: f32, x1: f32, y1: f32| vec![pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)];
        let outline = |q: &[P]| (0..q.len()).map(|i| (q[i], q[(i + 1) % q.len()])).collect::<Vec<_>>();
        let w = (s / 36.0).max(1.0);
        let pins: &[f32] = if small { &[] } else if size >= 40 { &[22.0, 28.0, 34.0, 40.0] } else { &[24.0, 32.0, 40.0] };
        let mut pin_segs = Vec::new();
        for &v in pins {
            pin_segs.extend([(pt(v, 10.0), pt(v, 14.0)), (pt(v, 50.0), pt(v, 54.0)), (pt(10.0, v), pt(14.0, v)), (pt(50.0, v), pt(54.0, v))]);
        }
        let marks: Vec<(Mark, [u8; 3], f32)> = vec![
            (Mark::Rounded(0.0, s * 0.22), RIM, 1.0),
            (Mark::Rounded((s / 64.0).max(1.0), s * 0.22 - (s / 64.0).max(1.0)), BODY, 1.0),
            (Mark::Fill(quad(14.0, 14.0, 50.0, 50.0)), ACCENT, 0.10),
            (Mark::Fill(quad(22.0, 22.0, 42.0, 42.0)), ACCENT, if small { 1.0 } else if size >= 40 { 0.35 } else { 0.55 }),
            (Mark::Stroke(pin_segs, w), ACCENT, 0.75),
            (Mark::Stroke(outline(&quad(14.0, 14.0, 50.0, 50.0)), w), ACCENT, 1.0),
            (Mark::Stroke(outline(&quad(22.0, 22.0, 42.0, 42.0)), w), ACCENT, 1.0),
        ];
        let mut out = vec![0u8; (size * size * 4) as usize];
        for py in 0..size {
            for px in 0..size {
                // Premultiplied accumulation over the 16 samples.
                let mut acc = [0.0_f32; 4];
                for sy in 0..4 {
                    for sx in 0..4 {
                        let (x, y) = (px as f32 + (sx as f32 + 0.5) / 4.0, py as f32 + (sy as f32 + 0.5) / 4.0);
                        let mut c = [0.0_f32; 4];
                        for (m, rgb, a) in &marks {
                            if inside(m, s, x, y) {
                                for k in 0..3 {
                                    c[k] = rgb[k] as f32 / 255.0 * a + c[k] * (1.0 - a);
                                }
                                c[3] = a + c[3] * (1.0 - a);
                            }
                        }
                        for k in 0..4 {
                            acc[k] += c[k] / 16.0;
                        }
                    }
                }
                let i = ((py * size + px) * 4) as usize;
                let a = acc[3];
                for k in 0..3 {
                    out[i + k] = if a > 0.0 { (acc[k] / a * 255.0).round() as u8 } else { 0 };
                }
                out[i + 3] = (a * 255.0).round() as u8;
            }
        }
        out
    }

    pub fn png(size: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut enc = png::Encoder::new(&mut bytes, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&rgba(size)).unwrap();
        w.finish().unwrap();
        bytes
    }

    /// A Windows .ico with one PNG-compressed image per size.
    pub fn ico(sizes: &[u32]) -> Vec<u8> {
        let images: Vec<Vec<u8>> = sizes.iter().map(|&s| png(s)).collect();
        let mut out = vec![0, 0, 1, 0];
        out.extend((sizes.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * sizes.len() as u32;
        for (&s, img) in sizes.iter().zip(&images) {
            let b = if s >= 256 { 0 } else { s as u8 };
            out.extend([b, b, 0, 0]);
            out.extend(1u16.to_le_bytes());
            out.extend(32u16.to_le_bytes());
            out.extend((img.len() as u32).to_le_bytes());
            out.extend(offset.to_le_bytes());
            offset += img.len() as u32;
        }
        for img in images {
            out.extend(img);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

    /// The committed window icon decodes and matches the drawing.
    #[test]
    fn window_icon_matches_art() {
        let icon = window_icon().expect("icon decodes");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba, art::rgba(64), "assets/icon is stale: run the write_assets test");
    }

    /// The committed .ico is the drawing at every size.
    #[test]
    fn ico_matches_art() {
        // Compared by size and decoded pixels, so a different PNG encoder output still passes.
        let ico = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon/tmop.ico")).unwrap();
        assert_eq!(u16::from_le_bytes([ico[4], ico[5]]) as usize, ICO_SIZES.len(), "assets/icon is stale: run the write_assets test");
        for (i, &s) in ICO_SIZES.iter().enumerate() {
            let e = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            let (len, at) = (u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize, u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize);
            let (rgba, size) = decode(&ico[at..at + len]).expect("ico image decodes");
            assert_eq!((size, rgba), (s, art::rgba(s)), "assets/icon is stale at {s} px: run the write_assets test");
        }
    }

    /// Writes `assets/icon/` from the drawing.
    #[test]
    #[ignore]
    fn write_assets() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tmop-64.png"), art::png(64)).unwrap();
        std::fs::write(dir.join("tmop-256.png"), art::png(256)).unwrap();
        std::fs::write(dir.join("tmop.ico"), art::ico(&ICO_SIZES)).unwrap();
    }
}
