//! Cursor themes (xcursor) and named cursor shapes.

use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::{CursorIcon, CursorImageStatus},
    utils::{Point, Transform},
};
use xcursor::{CursorTheme, parser::{Image, parse_xcursor}};

use crate::config::Config;

pub struct CursorState {
    pub status: CursorImageStatus,
    theme_name: String,
    size: u32,
    cache: HashMap<(String, u32), Vec<Image>>,
    buffers: HashMap<(String, u32, u32), (MemoryRenderBuffer, Point<i32, smithay::utils::Physical>)>,
    pub last_move: Instant,
}

impl CursorState {
    pub fn new(cfg: &Config) -> Self {
        let mut s = Self {
            status: CursorImageStatus::default_named(),
            theme_name: String::new(),
            size: 24,
            cache: HashMap::new(),
            buffers: HashMap::new(),
            last_move: Instant::now(),
        };
        s.configure(cfg);
        s
    }

    pub fn configure(&mut self, cfg: &Config) {
        let theme = if cfg.cursor.theme.is_empty() {
            std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into())
        } else {
            cfg.cursor.theme.clone()
        };
        let size = if cfg.cursor.size > 0 {
            cfg.cursor.size
        } else {
            std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24)
        };
        if theme != self.theme_name || size != self.size {
            self.cache.clear();
            self.buffers.clear();
        }
        self.theme_name = theme;
        self.size = size;
        // SAFETY: single-threaded at this point; children inherit these
        unsafe {
            std::env::set_var("XCURSOR_THEME", &self.theme_name);
            std::env::set_var("XCURSOR_SIZE", self.size.to_string());
        }
    }

    fn load(&mut self, name: &str, size: u32) -> Option<&Vec<Image>> {
        let key = (name.to_string(), size);
        if !self.cache.contains_key(&key) {
            let theme = CursorTheme::load(&self.theme_name);
            let mut images = None;
            for n in [name, "default", "left_ptr"] {
                if let Some(p) = theme.load_icon(n) {
                    let mut data = Vec::new();
                    if std::fs::File::open(p).and_then(|mut f| f.read_to_end(&mut data)).is_ok() {
                        if let Some(imgs) = parse_xcursor(&data) {
                            images = Some(imgs);
                            break;
                        }
                    }
                }
            }
            let images = images.unwrap_or_else(|| vec![fallback()]);
            self.cache.insert(key.clone(), images);
        }
        self.cache.get(&key)
    }

    /// Buffer + hotspot (physical) for a named cursor at an integer scale.
    pub fn named_buffer(&mut self, icon: CursorIcon, scale: u32, time: Duration) -> Option<(MemoryRenderBuffer, Point<i32, smithay::utils::Physical>)> {
        let size = self.size * scale.max(1);
        let name = icon.name().to_string();
        let images = self.load(&name, size)?.clone();
        let nearest = images.iter().min_by_key(|i| (size as i32 - i.size as i32).abs())?;
        let frames: Vec<&Image> = images.iter().filter(|i| i.width == nearest.width && i.height == nearest.height).collect();
        let total: u32 = frames.iter().map(|i| i.delay).sum();
        let mut idx = 0;
        if total > 0 {
            let mut ms = time.as_millis() as u32 % total;
            for (i, f) in frames.iter().enumerate() {
                if ms < f.delay {
                    idx = i;
                    break;
                }
                ms -= f.delay;
            }
        }
        let img = frames[idx];
        let key = (name, size, idx as u32);
        if !self.buffers.contains_key(&key) {
            let buf = MemoryRenderBuffer::from_slice(
                &img.pixels_rgba,
                Fourcc::Abgr8888,
                (img.width as i32, img.height as i32),
                1,
                Transform::Normal,
                None,
            );
            self.buffers.insert(key.clone(), (buf, (img.xhot as i32, img.yhot as i32).into()));
        }
        self.buffers.get(&key).cloned()
    }
}

fn fallback() -> Image {
    // 16x16 white arrow with dark outline
    let w = 16u32;
    let h = 16u32;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let inside = x <= y && x + y / 3 < 12;
            let edge = inside && (x == 0 || x == y || x + y / 3 == 11);
            let i = ((y * w + x) * 4) as usize;
            if inside {
                let c = if edge { 20 } else { 245 };
                px[i] = c;
                px[i + 1] = c;
                px[i + 2] = c;
                px[i + 3] = 255;
            }
        }
    }
    Image { size: 16, width: w, height: h, xhot: 0, yhot: 0, delay: 0, pixels_rgba: px, pixels_argb: vec![] }
}
