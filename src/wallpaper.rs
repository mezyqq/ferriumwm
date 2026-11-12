//! Native wallpaper: decoding (in a thread), palette generation, blurred copy
//! for window backdrop blur, crossfade transitions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use image::RgbaImage;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{ImportMem, element::Id, gles::{GlesRenderer, GlesTexture}},
    },
    reexports::calloop::channel::Sender,
};
use tracing::{info, warn};

use crate::{
    anim::{AnimParams, Progress},
    config::expand_path,
    state::Fe,
    theme::palette::Palette,
};

pub struct Loaded {
    pub path: String,
    /// the image actually shown: the path itself, or a Wallpaper Engine preview
    pub image_path: String,
    pub image: Option<Arc<RgbaImage>>,
    pub blurred: Option<Arc<RgbaImage>>,
    pub palette: Palette,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Wallpaper {
    pub path: String,
    pub image: Option<Arc<RgbaImage>>,
    pub blurred: Option<Arc<RgbaImage>>,
    pub tex: Option<GlesTexture>,
    pub blur_tex: Option<GlesTexture>,
    pub prev_tex: Option<GlesTexture>,
    pub fade: Option<Progress>,
    pub dirty: bool,
    pub id: Option<Id>,
    pub prev_id: Option<Id>,
    pub blur_id: Option<Id>,
    pub tx: Option<Sender<Loaded>>,
    pub loading: bool,
}

impl Wallpaper {
    pub fn animating(&mut self, now: Instant) -> bool {
        if let Some(f) = &self.fade {
            if f.done(now) {
                self.fade = None;
                self.prev_tex = None;
                return false;
            }
            return true;
        }
        false
    }

    /// Upload pending images to the GPU.
    pub fn upload(&mut self, r: &mut GlesRenderer) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let up = |r: &mut GlesRenderer, img: &RgbaImage| -> Option<GlesTexture> {
            r.import_memory(img.as_raw(), Fourcc::Abgr8888, (img.width() as i32, img.height() as i32).into(), false)
                .map_err(|e| warn!("обои: загрузка в GPU: {e}"))
                .ok()
        };
        let new = self.image.as_ref().and_then(|i| up(r, i));
        if self.tex.is_some() && self.fade.is_some() {
            self.prev_tex = self.tex.take();
            self.prev_id = self.id.take();
        }
        self.tex = new;
        self.id = Some(Id::new());
        self.blur_tex = self.blurred.as_ref().and_then(|i| up(r, i));
        self.blur_id = Some(Id::new());
    }
}

fn box_blur(img: &RgbaImage, radius: u32, passes: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut src: Vec<[f32; 4]> = img.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]).collect();
    let mut tmp = src.clone();
    let r = radius.max(1) as i64;
    for _ in 0..passes.max(1) {
        // horizontal
        for y in 0..h as i64 {
            let mut acc = [0f32; 4];
            let row = (y * w as i64) as usize;
            for x in -r..=r {
                let xx = x.clamp(0, w as i64 - 1) as usize;
                for c in 0..4 {
                    acc[c] += src[row + xx][c];
                }
            }
            let n = (2 * r + 1) as f32;
            for x in 0..w as i64 {
                for c in 0..4 {
                    tmp[row + x as usize][c] = acc[c] / n;
                }
                let out_x = (x - r).clamp(0, w as i64 - 1) as usize;
                let in_x = (x + r + 1).clamp(0, w as i64 - 1) as usize;
                for c in 0..4 {
                    acc[c] += src[row + in_x][c] - src[row + out_x][c];
                }
            }
        }
        // vertical
        for x in 0..w as i64 {
            let mut acc = [0f32; 4];
            for y in -r..=r {
                let yy = y.clamp(0, h as i64 - 1) as usize;
                for c in 0..4 {
                    acc[c] += tmp[yy * w as usize + x as usize][c];
                }
            }
            let n = (2 * r + 1) as f32;
            for y in 0..h as i64 {
                for c in 0..4 {
                    src[y as usize * w as usize + x as usize][c] = acc[c] / n;
                }
                let out_y = (y - r).clamp(0, h as i64 - 1) as usize;
                let in_y = (y + r + 1).clamp(0, h as i64 - 1) as usize;
                for c in 0..4 {
                    acc[c] += tmp[in_y * w as usize + x as usize][c] - tmp[out_y * w as usize + x as usize][c];
                }
            }
        }
    }
    let mut out = RgbaImage::new(w, h);
    for (i, p) in out.pixels_mut().enumerate() {
        let s = src[i];
        *p = image::Rgba([s[0] as u8, s[1] as u8, s[2] as u8, s[3] as u8]);
    }
    out
}

/// Decode + palette + blur off the main thread.
pub fn load_async(fe: &mut Fe, path: &str) {
    let Some(tx) = fe.wallpaper.tx.clone() else { return };
    let cfg = fe.cfg.clone();
    let path = path.to_string();
    fe.wallpaper.loading = true;
    std::thread::spawn(move || {
        // "we:<id>": FeWM shows (and takes colours from) the wallpaper's preview, the engine draws on top
        let image_path = crate::wallpaper_engine::image_for(&path);
        let p = expand_path(&image_path);
        let (image, error) = if image_path.is_empty() {
            (None, None)
        } else {
            match crate::theme::load_image(&p) {
                Ok(i) => (Some(i), None),
                Err(e) => (None, Some(format!("{e:#}"))),
            }
        };
        let palette = crate::theme::palette_for(&cfg, image.as_ref());
        let blurred = image.as_ref().map(|img| {
            // downscale ~8x then blur; the GPU upscales with linear filtering
            let (w, h) = img.dimensions();
            let small = image::imageops::resize(img, (w / 8).max(1), (h / 8).max(1), image::imageops::FilterType::Triangle);
            let radius = (cfg.theme.blur_radius / 8.0).round().max(1.0) as u32;
            Arc::new(box_blur(&small, radius, cfg.theme.blur_passes.max(1)))
        });
        let _ = tx.send(Loaded { path, image_path, image: image.map(Arc::new), blurred, palette, error });
    });
}

/// Called on the main thread when a wallpaper finished loading.
pub fn loaded(fe: &mut Fe, l: Loaded) {
    fe.wallpaper.loading = false;
    if let Some(e) = &l.error {
        warn!("обои: {e}");
        crate::notify(fe, "Обои", e);
    }
    let had = fe.wallpaper.tex.is_some();
    fe.wallpaper.path = l.path.clone();
    fe.wallpaper.image = l.image;
    fe.wallpaper.blurred = l.blurred;
    fe.wallpaper.dirty = true;
    if had {
        let p = AnimParams::from_cfg(&fe.cfg.animations.wallpaper, fe.cfg.animations.enabled, fe.cfg.animations.speed);
        if p.enabled {
            fe.wallpaper.fade = Some(Progress::new(p, Instant::now()));
        }
    }
    fe.palette = l.palette;
    crate::apply_style(fe);
    crate::theme::apply_templates(&fe.cfg, &fe.palette, &expand_path(&l.image_path).to_string_lossy());
    fe.we_sync();
    info!("обои: {} (основной цвет {})", l.path, fe.palette.source.hex());
    crate::ipc::event(fe, "theme-changed");
    fe.queue_redraw();
}

pub fn set_wallpaper(fe: &mut Fe, path: &str) {
    let path = path.trim().trim_matches('"').trim_matches('\'').to_string();
    if fe.cfg.theme.wallpaper != path {
        fe.cfg.theme.wallpaper = path.clone();
        // persist in all three config files
        if let Err(e) = crate::config::sync::write_all(&fe.cfg, None) {
            warn!("не сохранил обои в конфиг: {e:#}");
        }
    }
    load_async(fe, &path);
}

fn list_images(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn cycle_wallpaper(fe: &mut Fe, how: &str) {
    let dir = expand_path(&fe.cfg.theme.wallpaper_dir);
    let imgs = list_images(&dir);
    if imgs.is_empty() {
        crate::notify(fe, "Обои", &format!("нет картинок в {}", dir.display()));
        return;
    }
    let cur = expand_path(&fe.cfg.theme.wallpaper);
    let idx = imgs.iter().position(|p| *p == cur);
    let next = match how {
        "wallpaper-next" => idx.map(|i| (i + 1) % imgs.len()).unwrap_or(0),
        "wallpaper-prev" => idx.map(|i| (i + imgs.len() - 1) % imgs.len()).unwrap_or(0),
        _ => {
            let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let mut n = (seed % imgs.len() as u128) as usize;
            if Some(n) == idx && imgs.len() > 1 {
                n = (n + 1) % imgs.len();
            }
            n
        }
    };
    let p = imgs[next].to_string_lossy().into_owned();
    set_wallpaper(fe, &p);
}
