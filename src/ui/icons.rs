//! Texture cache of process / file icons, extracted on a worker thread.

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

type Done = Arc<Mutex<Vec<(String, Option<crate::pro::icons::IconImage>)>>>;

pub struct IconCache {
    textures: HashMap<String, Option<TextureHandle>>,
    pending: HashSet<String>,
    tx: Option<Sender<String>>,
    done: Done,
}

impl IconCache {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = channel::<String>();
        let done: Done = Arc::default();
        let out = done.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("tmop-icons".into()).spawn(move || {
            #[cfg(windows)]
            // SAFETY: COM must be initialised on the thread that calls SHGetFileInfo.
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            while let Ok(path) = rx.recv() {
                let img = crate::pro::icons::file_icon(&path);
                if let Ok(mut g) = out.lock() {
                    g.push((path, img));
                }
                ctx.request_repaint();
            }
        });
        Self { textures: HashMap::new(), pending: HashSet::new(), tx: spawned.ok().map(|_| tx), done }
    }

    /// Uploads finished icons. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let items: Vec<_> = self.done.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
        for (path, img) in items {
            self.pending.remove(&path);
            let tex = img.map(|i| ctx.load_texture(format!("icon:{path}"), ColorImage::from_rgba_unmultiplied([i.size, i.size], &i.rgba), TextureOptions::LINEAR));
            self.textures.insert(path, tex);
        }
    }

    /// Texture for a file's icon; requests it on first use and returns None until ready.
    pub fn get(&mut self, path: Option<&str>) -> Option<TextureId> {
        let path = path?;
        if path.is_empty() {
            return None;
        }
        let key = path.to_lowercase();
        if let Some(t) = self.textures.get(&key) {
            return t.as_ref().map(|t| t.id());
        }
        if self.pending.len() < 512 && self.pending.insert(key.clone()) {
            if let Some(tx) = &self.tx {
                let _ = tx.send(key);
            }
        }
        None
    }

    /// Icon for a process: its image, or the kernel's for the built-in system processes.
    pub fn for_process(&mut self, pid: u32, path: Option<&str>) -> Option<TextureId> {
        match path {
            Some(p) => self.get(Some(p)),
            None if pid == 4 => {
                let k = format!(r"{}\System32\ntoskrnl.exe", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
                self.get(Some(&k))
            }
            None => None,
        }
    }
}
