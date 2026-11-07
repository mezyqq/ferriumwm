//! Close animations. Every frame FeWM remembers which textures a window was
//! drawn with (cheap: textures are shared handles). When the window goes away,
//! those textures are drawn into a snapshot which is then animated out.

use std::time::Instant;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Frame, ImportMem, Offscreen, Renderer, TextureMapping,
            element::{Element, surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture}},
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
};
use tracing::debug;

use crate::{
    anim::{Progress, RectF},
    layout::WinId,
    state::{Backend, Fe},
};

pub struct Piece {
    tex: GlesTexture,
    dst: Rectangle<i32, Physical>,
    src: Rectangle<f64, BufferCoords>,
    transform: Transform,
}

/// What a window looked like the last time it was drawn normally.
pub struct LastFrame {
    /// window geometry origin and size, output physical coords
    pub origin: Point<i32, Physical>,
    pub size: Size<i32, Physical>,
    pieces: Vec<Piece>,
}

impl LastFrame {
    pub fn capture(
        elems: &[WaylandSurfaceRenderElement<GlesRenderer>],
        origin: Point<i32, Physical>,
        size: Size<i32, Physical>,
        scale: f64,
    ) -> Self {
        let pieces = elems
            .iter()
            .filter_map(|e| match e.texture() {
                WaylandSurfaceTexture::Texture(t) => Some(Piece {
                    tex: t.clone(),
                    dst: e.geometry(Scale::from(scale)),
                    src: e.src(),
                    transform: e.transform(),
                }),
                WaylandSurfaceTexture::SolidColor(_) => None,
            })
            .collect();
        LastFrame { origin, size, pieces }
    }
}

pub struct Closing {
    pub ws: u32,
    /// workspace-local visual rect when it closed
    pub rect: RectF,
    /// floating/fullscreen windows don't follow the scroller view
    pub fixed: bool,
    pub tex: GlesTexture,
    pub prog: Progress,
}

fn draw_snapshot(r: &mut GlesRenderer, lf: &LastFrame) -> Option<GlesTexture> {
    let size = lf.size;
    if size.w <= 0 || size.h <= 0 || lf.pieces.is_empty() {
        return None;
    }
    let mut target: GlesTexture = Offscreen::<GlesTexture>::create_buffer(r, Fourcc::Abgr8888, (size.w, size.h).into()).ok()?;
    let mapping = {
        let mut fb = r.bind(&mut target).ok()?;
        {
            let mut frame = r.render(&mut fb, size, Transform::Normal).ok()?;
            frame.clear([0.0, 0.0, 0.0, 0.0].into(), &[Rectangle::from_size(size)]).ok()?;
            for p in &lf.pieces {
                let dst = Rectangle::new(p.dst.loc - lf.origin, p.dst.size);
                let _ = Frame::render_texture_from_to(&mut frame, &p.tex, p.src, dst, &[Rectangle::from_size(dst.size)], &[], p.transform, 1.0);
            }
            let _ = frame.finish().ok()?;
        }
        r.copy_framebuffer(&fb, Rectangle::from_size((size.w, size.h).into()), Fourcc::Abgr8888).ok()?
    };
    // GL render targets come out bottom-up (same as in capture.rs)
    let flip = !mapping.flipped();
    let bytes = r.map_texture(&mapping).ok()?.to_vec();
    let row = size.w as usize * 4;
    let mut px = vec![0u8; row * size.h as usize];
    for y in 0..size.h as usize {
        let sy = if flip { size.h as usize - 1 - y } else { y };
        px[y * row..(y + 1) * row].copy_from_slice(&bytes[sy * row..(sy + 1) * row]);
    }
    r.import_memory(&px, Fourcc::Abgr8888, (size.w, size.h).into(), false).ok()
}

impl Fe {
    /// Start the close animation for a window that is disappearing.
    pub fn snapshot_closing(&mut self, id: WinId) {
        let p = self.anim(&self.cfg.animations.windows_close);
        let style = self.cfg.animations.windows_close.style.clone();
        let Some(w) = self.windows.get_mut(&id) else { return };
        let last = w.last_frame.take();
        if !p.enabled || style == "none" || w.rule.no_anim || !w.mapped {
            return;
        }
        let Some(lf) = last else {
            debug!("закрытие: окно ещё ни разу не было нарисовано");
            return;
        };
        let (ws, rect, fixed) = (w.ws, w.rect.value(Instant::now()), w.floating || w.fullscreen);

        let mut backend = std::mem::replace(&mut self.backend, Backend::None);
        let tex = backend.renderer().and_then(|r| draw_snapshot(r, &lf));
        self.backend = backend;
        let Some(tex) = tex else {
            debug!("снимок для анимации закрытия не получился");
            return;
        };
        self.closing.push(Closing { ws, rect, fixed, tex, prog: Progress::new(p, Instant::now()) });
        self.queue_redraw();
    }

    pub fn tick_closing(&mut self, now: Instant) -> bool {
        self.closing.retain(|c| !c.prog.done(now));
        !self.closing.is_empty()
    }
}
