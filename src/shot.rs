//! `--screenshot=<file.png>`: the window renders its own frames to PNG files (maintainers use it for
//! the README images). Nothing is captured from the desktop, the window may sit behind others.
//!
//! The run uses a private settings folder ([`private_home`]) and neutral host and user names in the
//! header, waits `--wait=<seconds>` (default 25) so the graphs have history, then writes
//! `--frames=<n>` PNGs `--every=<ms>` apart and exits. One frame goes to the file as given; several
//! to `name-0001.png`, `name-0002.png`, ...

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::platform::transparency::Corners;

/// What `--screenshot=` asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub path: PathBuf,
    pub frames: u32,
    pub every: Duration,
    pub wait: Duration,
    /// `dark` or `light`.
    pub theme: String,
    /// Folder for settings, `--shot-home=` or a temporary one.
    pub home: Option<PathBuf>,
}

impl Plan {
    /// `None` without `--screenshot=`; an error for a value that does not parse.
    pub fn parse(args: &[String]) -> Result<Option<Plan>, String> {
        let value = |name: &str| args.iter().find_map(|a| a.strip_prefix(name).map(str::to_string));
        let Some(path) = value("--screenshot=") else { return Ok(None) };
        if path.trim().is_empty() {
            return Err("--screenshot= needs a file name".into());
        }
        let number = |name: &str, default: u64| match value(name) {
            None => Ok(default),
            Some(v) => v.trim().parse::<u64>().map_err(|_| format!("{name} needs a whole number, not {v:?}")),
        };
        let theme = value("--theme=").unwrap_or_else(|| "dark".into());
        if theme != "dark" && theme != "light" {
            return Err(format!("--theme= is dark or light, not {theme:?}"));
        }
        let frames = number("--frames=", 1)?.clamp(1, 10_000) as u32;
        Ok(Some(Plan {
            path: PathBuf::from(path),
            frames,
            every: Duration::from_millis(number("--every=", 1000)?.max(100)),
            wait: Duration::from_secs(number("--wait=", 25)?),
            theme,
            home: value("--shot-home=").map(PathBuf::from),
        }))
    }

    /// The file of frame `n` (0-based).
    pub fn frame_path(&self, n: u32) -> PathBuf {
        if self.frames == 1 {
            return self.path.clone();
        }
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.path.with_file_name(format!("{stem}-{:04}.png", n + 1))
    }
}

/// Points settings, data and cache at a folder of their own so the run neither reads nor changes the
/// user's settings, and the Settings page shows no profile path. Call before anything reads them.
pub fn private_home(plan: &Plan) -> PathBuf {
    let dir = plan.home.clone().unwrap_or_else(|| std::env::temp_dir().join("tmop-screenshot"));
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    std::env::set_var("LOCALAPPDATA", &dir);
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.join("data"));
    }
    #[cfg(target_os = "macos")]
    std::env::set_var("HOME", &dir);
    dir
}

/// Names shown in the header in place of the machine's own.
pub const MACHINE: &str = "DESKTOP \u{b7} user";

/// A run in progress: asks for a frame when it is due, writes it, closes the window after the last.
pub struct Run {
    plan: Plan,
    started: Instant,
    next_at: Instant,
    taken: u32,
    waiting: bool,
    inbox: Arc<Mutex<Option<Arc<eframe::egui::ColorImage>>>>,
    /// A `--popout` run: the corners the popout drew with. The PNGs keep the framebuffer's alpha
    /// and each frame's corners are checked against them ([`check_popout`]).
    popout: Option<Corners>,
}

impl Run {
    pub fn new(plan: Plan) -> Self {
        let now = Instant::now();
        Self { next_at: now + plan.wait, plan, started: now, taken: 0, waiting: false, inbox: Default::default(), popout: None }
    }

    /// Makes this a run of the `--popout` window drawn with `corners`.
    pub fn popout(mut self, corners: Corners) -> Self {
        self.popout = Some(corners);
        self
    }

    /// Called once per frame: collects an arrived screenshot, asks for the next one when due.
    pub fn step(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui::ViewportCommand;
        let arrived = self.inbox.lock().ok().and_then(|mut g| g.take());
        if let Some(image) = arrived {
            self.waiting = false;
            let path = self.plan.frame_path(self.taken);
            if let Some(corners) = self.popout {
                match check_popout(&image, corners) {
                    Ok(what) => eprintln!("TMOP: popout check ok: {what}"),
                    Err(what) => eprintln!("TMOP: popout check FAILED: {what}"),
                }
            }
            match write_png(&path, &image, self.popout.is_some()) {
                Ok(()) => eprintln!("TMOP: wrote {} ({}x{})", path.display(), image.size[0], image.size[1]),
                Err(e) => {
                    eprintln!("TMOP: cannot write {}: {e}", path.display());
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return;
                }
            }
            self.taken += 1;
            // Frames are spaced from the previous request, not from when the file was written.
            self.next_at += self.plan.every;
            if self.taken >= self.plan.frames {
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
        }
        if !self.waiting && Instant::now() >= self.next_at {
            self.waiting = true;
            let inbox = Arc::clone(&self.inbox);
            let repaint = ctx.clone();
            ctx.request_screenshot(move |image| {
                if let Ok(mut g) = inbox.lock() {
                    *g = Some(image);
                }
                repaint.request_repaint();
            });
        }
        // Frames are due on a clock, not on input.
        ctx.request_repaint_after(Duration::from_millis(50));
        // A frame that never arrives must not leave a window open for good.
        if self.started.elapsed() > self.plan.wait + self.plan.every * (self.plan.frames + 1) + Duration::from_secs(60) {
            eprintln!("TMOP: no screenshot arrived, giving up");
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }
}

/// Checks a frame of the `--popout` window against the `corners` it was drawn with (#166): the
/// four outermost corner pixels are transparent (alpha 0) for [`Corners::Rounded`], opaque and
/// not black for [`Corners::Square`]; the centre pixel is opaque either way. Returns the pixel
/// values found (as `r,g,b,a`, alpha premultiplied as read from the framebuffer).
pub fn check_popout(image: &eframe::egui::ColorImage, corners: Corners) -> Result<String, String> {
    let [w, h] = image.size;
    if w < 2 || h < 2 {
        return Err(format!("{w}x{h} image"));
    }
    let px = |x: usize, y: usize| image.pixels[y * w + x];
    let rgba = |c: eframe::egui::Color32| format!("{},{},{},{}", c.r(), c.g(), c.b(), c.a());
    let edges = [px(0, 0), px(w - 1, 0), px(0, h - 1), px(w - 1, h - 1)];
    let centre = px(w / 2, h / 2);
    let what = format!("{corners:?}, {w}x{h}, corners {}, centre {}", edges.iter().map(|c| rgba(*c)).collect::<Vec<_>>().join(" "), rgba(centre));
    let corners_ok = match corners {
        Corners::Rounded => edges.iter().all(|c| c.a() == 0),
        Corners::Square => edges.iter().all(|c| c.a() == 255 && (c.r(), c.g(), c.b()) != (0, 0, 0)),
    };
    if corners_ok && centre.a() == 255 {
        Ok(what)
    } else {
        Err(what)
    }
}

/// Writes `image` as an RGBA PNG: opaque, or with the framebuffer's alpha (`keep_alpha`,
/// converted from premultiplied to the straight alpha PNG stores).
pub fn write_png(path: &Path, image: &eframe::egui::ColorImage, keep_alpha: bool) -> Result<(), String> {
    let [w, h] = image.size;
    let mut data = Vec::with_capacity(w * h * 4);
    for p in &image.pixels {
        if keep_alpha {
            data.extend_from_slice(&p.to_srgba_unmultiplied());
        } else {
            data.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
        }
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    enc.write_header().and_then(|mut w| w.write_image_data(&data)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_flag_no_plan() {
        assert_eq!(Plan::parse(&args(&["--page=summary"])), Ok(None));
    }

    #[test]
    fn defaults() {
        let p = Plan::parse(&args(&["--screenshot=a.png"])).unwrap().unwrap();
        assert_eq!((p.frames, p.wait, p.theme.as_str()), (1, Duration::from_secs(25), "dark"));
        assert_eq!(p.frame_path(0), PathBuf::from("a.png"));
    }

    #[test]
    fn frames_are_numbered() {
        let p = Plan::parse(&args(&["--screenshot=out/perf.png", "--frames=3", "--every=500", "--theme=light"])).unwrap().unwrap();
        assert_eq!((p.every, p.theme.as_str()), (Duration::from_millis(500), "light"));
        assert_eq!(p.frame_path(1), PathBuf::from("out/perf-0002.png"));
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(Plan::parse(&args(&["--screenshot=a.png", "--frames=x"])).is_err());
        assert!(Plan::parse(&args(&["--screenshot=a.png", "--theme=blue"])).is_err());
        assert!(Plan::parse(&args(&["--screenshot="])).is_err());
    }

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("tmop-shot-test-{}", std::process::id()));
        let path = dir.join("x.png");
        let img = eframe::egui::ColorImage::filled([3, 2], eframe::egui::Color32::from_rgb(1, 2, 3));
        write_png(&path, &img, false).unwrap();
        let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let reader = dec.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (3, 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn first_pixel(img: &eframe::egui::ColorImage, keep_alpha: bool) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("tmop-shot-alpha-{}-{keep_alpha}", std::process::id()));
        let path = dir.join("x.png");
        write_png(&path, img, keep_alpha).unwrap();
        let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        buf[..4].to_vec()
    }

    #[test]
    fn png_keeps_alpha_when_asked() {
        use eframe::egui::Color32;
        let clear = eframe::egui::ColorImage::filled([2, 2], Color32::TRANSPARENT);
        assert_eq!(first_pixel(&clear, true), [0, 0, 0, 0]);
        assert_eq!(first_pixel(&clear, false), [0, 0, 0, 255], "the README images stay opaque");
        let half = eframe::egui::ColorImage::filled([2, 2], Color32::from_rgba_premultiplied(100, 50, 0, 128));
        let p = first_pixel(&half, true);
        assert_eq!(p[3], 128);
        assert!((198..=200).contains(&p[0]), "unpremultiplied: {p:?}");
    }

    /// A 40x30 frame: `inside` everywhere but the four corner pixels, which are `corner`.
    fn frame(inside: eframe::egui::Color32, corner: eframe::egui::Color32) -> eframe::egui::ColorImage {
        let mut img = eframe::egui::ColorImage::filled([40, 30], inside);
        for (x, y) in [(0, 0), (39, 0), (0, 29), (39, 29)] {
            img.pixels[y * 40 + x] = corner;
        }
        img
    }

    #[test]
    fn popout_corners_are_checked() {
        use eframe::egui::Color32;
        let body = Color32::from_rgb(14, 17, 30);
        assert!(check_popout(&frame(body, Color32::TRANSPARENT), Corners::Rounded).is_ok());
        let black = check_popout(&frame(body, Color32::BLACK), Corners::Rounded).unwrap_err();
        assert!(black.contains("0,0,0,255"), "{black}");
        assert!(check_popout(&frame(body, body), Corners::Square).is_ok());
        assert!(check_popout(&frame(body, Color32::BLACK), Corners::Square).is_err(), "opaque black is the fault itself");
        assert!(check_popout(&frame(body, Color32::TRANSPARENT), Corners::Square).is_err());
        assert!(check_popout(&frame(Color32::TRANSPARENT, Color32::TRANSPARENT), Corners::Rounded).is_err(), "nothing drawn");
        assert!(check_popout(&eframe::egui::ColorImage::filled([1, 1], body), Corners::Square).is_err());
    }
}
