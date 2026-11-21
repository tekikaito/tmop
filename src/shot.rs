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
}

impl Run {
    pub fn new(plan: Plan) -> Self {
        let now = Instant::now();
        Self { next_at: now + plan.wait, plan, started: now, taken: 0, waiting: false, inbox: Default::default() }
    }

    /// Called once per frame: collects an arrived screenshot, asks for the next one when due.
    pub fn step(&mut self, ctx: &eframe::egui::Context) {
        use eframe::egui::ViewportCommand;
        let arrived = self.inbox.lock().ok().and_then(|mut g| g.take());
        if let Some(image) = arrived {
            self.waiting = false;
            let path = self.plan.frame_path(self.taken);
            match write_png(&path, &image) {
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

/// Writes `image` as an RGBA PNG.
pub fn write_png(path: &Path, image: &eframe::egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    let mut data = Vec::with_capacity(w * h * 4);
    for p in &image.pixels {
        data.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
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
        write_png(&path, &img).unwrap();
        let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let reader = dec.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (3, 2));
        let _ = std::fs::remove_dir_all(dir);
    }
}
