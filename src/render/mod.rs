//! Frame composition: wallpaper, layers, windows with borders/shadows/blur, cursor.

pub mod elements;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::{
        Renderer,
        element::{
            Id, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            texture::TextureRenderElement,
            AsRenderElements,
        },
        gles::{GlesRenderer, element::PixelShaderElement},
        utils::CommitCounter,
    },
    desktop::{PopupManager, layer_map_for_output, utils::send_frames_surface_tree},
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::{compositor::with_states, shell::wlr_layer::Layer},
};

use crate::{
    anim::RectF,
    layout::WinId,
    state::Fe,
    style::{Paint, Shadow},
};
use elements::{Clipped, FeElement, border_uniforms, shadow_area, shadow_uniforms};

#[derive(Default)]
pub struct RenderCache {
    borders: HashMap<WinId, (PixelShaderElement, Vec<u32>)>,
    shadows: HashMap<WinId, (PixelShaderElement, Vec<u32>)>,
    blur_ids: HashMap<WinId, Id>,
    bg_id: Option<Id>,
}

fn key_of(area: Rectangle<i32, Logical>, vals: &[f32]) -> Vec<u32> {
    let mut k = vec![area.loc.x as u32, area.loc.y as u32, area.size.w as u32, area.size.h as u32];
    k.extend(vals.iter().map(|v| v.to_bits()));
    k
}

fn round_rect(r: RectF) -> Rectangle<i32, Logical> {
    let x0 = r.x.round() as i32;
    let y0 = r.y.round() as i32;
    let x1 = (r.x + r.w).round() as i32;
    let y1 = (r.y + r.h).round() as i32;
    Rectangle::new((x0, y0).into(), ((x1 - x0).max(1), (y1 - y0).max(1)).into())
}

fn phys(r: RectF, scale: f64) -> Rectangle<f64, Physical> {
    Rectangle::new((r.x * scale, r.y * scale).into(), (r.w * scale, r.h * scale).into())
}

/// screen = offset + image * k  (per axis)
fn wall_map(mode: &str, ow: f64, oh: f64, iw: f64, ih: f64) -> (f64, f64, f64, f64) {
    match mode {
        "fit" => {
            let k = (ow / iw).min(oh / ih);
            (k, k, (ow - iw * k) / 2.0, (oh - ih * k) / 2.0)
        }
        "stretch" => (ow / iw, oh / ih, 0.0, 0.0),
        "center" => (1.0, 1.0, (ow - iw) / 2.0, (oh - ih) / 2.0),
        _ => {
            let k = (ow / iw).max(oh / ih);
            (k, k, (ow - iw * k) / 2.0, (oh - ih * k) / 2.0)
        }
    }
}

/// (dst rect output-local logical, src rect in image px) for the visible wallpaper part covering `r`.
fn wall_piece(mode: &str, out: (f64, f64), img: (f64, f64), r: RectF) -> Option<(RectF, RectF)> {
    let (kx, ky, ox, oy) = wall_map(mode, out.0, out.1, img.0, img.1);
    let full = RectF::new(ox, oy, img.0 * kx, img.1 * ky);
    let x0 = r.x.max(full.x);
    let y0 = r.y.max(full.y);
    let x1 = (r.x + r.w).min(full.x + full.w);
    let y1 = (r.y + r.h).min(full.y + full.h);
    if x1 - x0 < 0.5 || y1 - y0 < 0.5 {
        return None;
    }
    let dst = RectF::new(x0, y0, x1 - x0, y1 - y0);
    let src = RectF::new((x0 - ox) / kx, (y0 - oy) / ky, (x1 - x0) / kx, (y1 - y0) / ky);
    Some((dst, src))
}

impl Fe {
    pub fn render_elements(&mut self, renderer: &mut GlesRenderer, oi: usize) -> Vec<FeElement> {
        let now = Instant::now();
        let mut out: Vec<FeElement> = Vec::new();
        let Some(ostate) = self.outputs.get(oi) else { return out };
        let output = ostate.output.clone();
        let cur_ws = ostate.ws;
        let switch = ostate.switch.as_ref().map(|s| (s.from, s.prog.value(now), s.dir, s.style.clone()));
        let scale = output.current_scale().fractional_scale();
        let og = self.output_geo(oi).unwrap_or_default();
        let ow = og.size.w as f64;
        let oh = og.size.h as f64;

        if self.shaders.is_none() {
            match elements::Shaders::compile(renderer) {
                Ok(s) => self.shaders = Some(s),
                Err(e) => tracing::error!("шейдеры: {e:?}"),
            }
        }
        self.wallpaper.upload(renderer);

        // ---- cursor
        self.push_cursor(renderer, oi, &mut out, scale);

        // ---- locked: only the lock surface, black otherwise
        if self.lock.locked {
            if let Some(ls) = self.lock.surface_for(&output) {
                let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, ls.wl_surface(), (0, 0), scale, 1.0, Kind::Unspecified);
                out.extend(elems.into_iter().map(FeElement::Surface));
            }
            let id = self.render_cache.bg_id.get_or_insert_with(Id::new).clone();
            let geo = Rectangle::new((0, 0).into(), Size::<f64, Logical>::from((ow, oh)).to_physical_precise_round(scale));
            out.push(FeElement::Solid(SolidColorRenderElement::new(id, geo, CommitCounter::default(), [0.0, 0.0, 0.0, 1.0], Kind::Unspecified)));
            return out;
        }

        // ---- game mode: nothing but the game, so its buffer can be scanned out directly
        if let Some(gid) = self.game_window() {
            let game = self.windows.get(&gid).filter(|w| w.fullscreen && w.mapped && w.ws == cur_ws);
            if let (Some(w), None) = (game, switch.as_ref()) {
                push_layers(renderer, &output, Layer::Overlay, scale, &mut out);
                if let Some(t) = w.window.toplevel() {
                    let surface = t.wl_surface().clone();
                    let geo = w.window.geometry();
                    let loc: Point<i32, Physical> = geo.loc.to_f64().to_physical(scale).to_i32_round().upscale(-1);
                    for (popup, poff) in PopupManager::popups_for_surface(&surface) {
                        let off = (geo.loc + poff - popup.geometry().loc).to_f64().to_physical(scale).to_i32_round();
                        let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                            render_elements_from_surface_tree(renderer, popup.wl_surface(), loc + off, scale, 1.0, Kind::Unspecified);
                        out.extend(elems.into_iter().map(FeElement::Surface));
                    }
                    let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                        render_elements_from_surface_tree(renderer, &surface, loc, scale, 1.0, Kind::Unspecified);
                    out.extend(elems.into_iter().map(FeElement::Surface));
                }
                return out;
            }
        }

        // ---- overlay layers
        push_layers(renderer, &output, Layer::Overlay, scale, &mut out);

        let fullscreen = self.windows.values().any(|w| w.ws == cur_ws && w.fullscreen && w.mapped);
        if !fullscreen {
            push_layers(renderer, &output, Layer::Top, scale, &mut out);
        }

        // ---- workspaces (with switch animation)
        match switch {
            Some((from, t, dir, style)) => {
                let (nx, ny, fx, fy, na, fa) = match style.as_str() {
                    "fade" => (0.0, 0.0, 0.0, 0.0, t.clamp(0.0, 1.0), (1.0 - t).clamp(0.0, 1.0)),
                    "slide-vertical" | "slidevert" => (0.0, (1.0 - t) * dir * oh, 0.0, -t * dir * oh, 1.0, 1.0),
                    _ => ((1.0 - t) * dir * ow, 0.0, -t * dir * ow, 0.0, 1.0, 1.0),
                };
                self.push_workspace(renderer, oi, cur_ws, nx, ny, na as f32, scale, now, &mut out);
                self.push_workspace(renderer, oi, from, fx, fy, fa as f32, scale, now, &mut out);
            }
            None => self.push_workspace(renderer, oi, cur_ws, 0.0, 0.0, 1.0, scale, now, &mut out),
        }

        // ---- bottom/background layers
        push_layers(renderer, &output, Layer::Bottom, scale, &mut out);
        push_layers(renderer, &output, Layer::Background, scale, &mut out);

        // ---- wallpaper
        self.push_wallpaper(renderer, (ow, oh), scale, &mut out);

        // ---- solid background
        let bg = self.style.background().unwrap_or(crate::style::Color::rgba(0.07, 0.07, 0.08, 1.0));
        let id = self.render_cache.bg_id.get_or_insert_with(Id::new).clone();
        let geo = Rectangle::new((0, 0).into(), Size::<f64, Logical>::from((ow, oh)).to_physical_precise_round(scale));
        let [r, g, b, _] = bg.0;
        out.push(FeElement::Solid(SolidColorRenderElement::new(id, geo, CommitCounter::default(), [r, g, b, 1.0], Kind::Unspecified)));
        out
    }

    fn push_wallpaper(&mut self, renderer: &mut GlesRenderer, out_size: (f64, f64), scale: f64, out: &mut Vec<FeElement>) {
        let mode = self.cfg.theme.wallpaper_mode.clone();
        let fade = self.wallpaper.fade.as_ref().map(|f| f.value(Instant::now()).clamp(0.0, 1.0) as f32);
        let ctx = renderer.context_id();
        let full = RectF::new(0.0, 0.0, out_size.0, out_size.1);
        let mut push = |tex: &smithay::backend::renderer::gles::GlesTexture, id: &Id, alpha: f32| {
            use smithay::backend::renderer::Texture;
            let sz = tex.size();
            let Some((dst, src)) = wall_piece(&mode, out_size, (sz.w as f64, sz.h as f64), full) else { return };
            out.push(FeElement::Texture(TextureRenderElement::from_static_texture(
                id.clone(),
                ctx.clone(),
                Point::<f64, Physical>::from((dst.x * scale, dst.y * scale)),
                tex.clone(),
                1,
                Transform::Normal,
                Some(alpha),
                Some(Rectangle::new((src.x, src.y).into(), (src.w, src.h).into())),
                Some(Size::from((dst.w.round() as i32, dst.h.round() as i32))),
                None,
                Kind::Unspecified,
            )));
        };
        if let (Some(tex), Some(id)) = (&self.wallpaper.tex, &self.wallpaper.id) {
            push(tex, id, fade.unwrap_or(1.0));
        }
        if let (Some(f), Some(tex), Some(id)) = (fade, &self.wallpaper.prev_tex, &self.wallpaper.prev_id) {
            let _ = f;
            push(tex, id, 1.0);
        }
    }

    fn push_cursor(&mut self, renderer: &mut GlesRenderer, oi: usize, out: &mut Vec<FeElement>, scale: f64) {
        if self.capture_no_cursor {
            return;
        }
        let Some(ptr) = self.seat.get_pointer() else { return };
        let pos = ptr.current_location();
        let Some(og) = self.output_geo(oi) else { return };
        if !og.to_f64().contains(pos) {
            return;
        }
        if self.cfg.cursor.hide_after_ms > 0
            && self.cursor.last_move.elapsed() > Duration::from_millis(self.cfg.cursor.hide_after_ms as u64)
        {
            return;
        }
        let local = pos - og.loc.to_f64();
        match self.cursor.status.clone() {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(&surface, |s| {
                    s.data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let loc = (local - hotspot.to_f64()).to_physical_precise_round(scale);
                let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, &surface, loc, scale, 1.0, Kind::Cursor);
                out.extend(elems.into_iter().map(FeElement::Surface));
            }
            CursorImageStatus::Named(icon) => {
                let int_scale = scale.ceil() as u32;
                let t = self.start.elapsed();
                if let Some((buf, hot)) = self.cursor.named_buffer(icon, int_scale, t) {
                    let loc = local.to_physical(scale) - hot.to_f64().upscale(scale / int_scale as f64);
                    if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(renderer, loc, &buf, None, None, None, Kind::Cursor) {
                        out.push(FeElement::Memory(e));
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_workspace(
        &mut self,
        renderer: &mut GlesRenderer,
        oi: usize,
        ws: u32,
        dx: f64,
        dy: f64,
        ws_alpha: f32,
        scale: f64,
        now: Instant,
        out: &mut Vec<FeElement>,
    ) {
        let Some(wsp) = self.workspaces.get(ws as usize - 1) else { return };
        let vx = wsp.view_x.value(now);
        let vy = wsp.view_y.value(now);
        let og = self.output_geo(oi).unwrap_or_default();
        let out_size = (og.size.w as f64, og.size.h as f64);
        // closing windows (snapshots) above everything else on the workspace
        let close_style = self.cfg.animations.windows_close.style.clone();
        let close_from = self.cfg.animations.windows_close.from_scale.clamp(0.05, 1.0);
        let ctx_id = renderer.context_id();
        for c in self.closing.iter().filter(|c| c.ws == ws) {
            let t = c.prog.value(now).clamp(0.0, 1.0);
            let mut r = c.rect;
            if !c.fixed {
                r.x -= vx;
                r.y -= vy;
            }
            r.x += dx;
            r.y += dy;
            let mut s = 1.0;
            match close_style.as_str() {
                "fade" => {}
                "slide" => r.y += t * 60.0,
                _ => s = 1.0 - (1.0 - close_from) * t,
            }
            let (cx, cy) = r.center();
            let vr = RectF::new(cx - r.w * s / 2.0, cy - r.h * s / 2.0, r.w * s, r.h * s);
            out.push(FeElement::Texture(TextureRenderElement::from_static_texture(
                Id::new(),
                ctx_id.clone(),
                Point::<f64, Physical>::from((vr.x * scale, vr.y * scale)),
                c.tex.clone(),
                1,
                Transform::Normal,
                Some((1.0 - t) as f32 * ws_alpha),
                None,
                Some(Size::from((vr.w.round().max(1.0) as i32, vr.h.round().max(1.0) as i32))),
                None,
                Kind::Unspecified,
            )));
        }

        let order = self.stacking(ws);
        let mut remember = Vec::new();
        for id in order.into_iter().rev() {
            let style = self.win_style(id);
            let Some(w) = self.windows.get(&id) else { continue };
            let Some(toplevel) = w.window.toplevel() else { continue };
            let surface = toplevel.wl_surface().clone();
            let mut r = w.rect.value(now);
            if let Some(dr) = self.drag_rect(id, oi, (r.w, r.h)) {
                // held by the mouse: stays under the cursor, even mid workspace switch
                r = dr;
            } else {
                if !w.floating && !w.fullscreen {
                    r.x -= vx;
                    r.y -= vy;
                }
                if self.dragged_window() != Some(id) {
                    r.x += dx;
                    r.y += dy;
                }
            }
            // skip windows far outside the output (scroller strip)
            if r.x > out_size.0 + 200.0 || r.x + r.w < -200.0 || r.y > out_size.1 + 200.0 || r.y + r.h < -200.0 {
                continue;
            }
            // open animation
            let (mut s, mut a) = (1.0f64, 1.0f64);
            if let Some(p) = &w.open {
                let t = p.value(now);
                match self.cfg.animations.windows_open.style.as_str() {
                    "fade" => a = t.clamp(0.0, 1.0),
                    "slide" => {
                        r.y += (1.0 - t) * 60.0;
                        a = t.clamp(0.0, 1.0);
                    }
                    _ => {
                        let from = self.cfg.animations.windows_open.from_scale.clamp(0.05, 1.0);
                        s = from + (1.0 - from) * t;
                        a = (t * 1.5).clamp(0.0, 1.0);
                    }
                }
            }
            let (cx, cy) = r.center();
            let vr = RectF::new(cx - r.w * s / 2.0, cy - r.h * s / 2.0, r.w * s, r.h * s);
            let alpha = (a as f32) * style.opacity * ws_alpha;
            let geo = w.window.geometry();
            let surf_origin = (vr.x - geo.loc.x as f64 * s, vr.y - geo.loc.y as f64 * s);
            let loc: Point<i32, Physical> = Point::<f64, Logical>::from(surf_origin).to_physical_precise_round(scale);
            let es = scale * s;

            // popups on top, unclipped
            for (popup, poff) in PopupManager::popups_for_surface(&surface) {
                let off = (geo.loc + poff - popup.geometry().loc).to_f64().to_physical(es);
                let ploc = loc + off.to_i32_round();
                let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, popup.wl_surface(), ploc, es, alpha, Kind::Unspecified);
                out.extend(elems.into_iter().map(FeElement::Surface));
            }

            // the window itself, clipped to rounded geometry
            let radius = style.border_radius as f64;
            let clip_geo = phys(vr, scale);
            let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(renderer, &surface, loc, es, alpha, Kind::Unspecified);
            if w.open.is_none() && dx == 0.0 && dy == 0.0 {
                let origin = Point::<f64, Physical>::from((vr.x * scale, vr.y * scale)).to_i32_round();
                let size = Size::<f64, Physical>::from((vr.w * scale, vr.h * scale)).to_i32_round();
                remember.push((id, crate::closing::LastFrame::capture(&elems, origin, size, scale)));
            }
            let plain = w.fullscreen && radius <= 0.0 && style.dim <= 0.0 && w.open.is_none();
            if plain {
                // fullscreen games/videos: no clip shader, so the buffer can go straight to the display plane
                out.extend(elems.into_iter().map(FeElement::Surface));
            } else if let Some(sh) = &self.shaders {
                for e in elems {
                    out.push(FeElement::Clipped(Clipped::new(e, sh.clip.clone(), clip_geo, (radius * es) as f32, style.dim)));
                }
            } else {
                out.extend(elems.into_iter().map(FeElement::Surface));
            }

            // border
            let bw = style.border_width as f64;
            let border_paint = match &w.border {
                Some(b) => match &b.prog {
                    Some(p) => b.from.lerp(&b.to, p.value(now).clamp(0.0, 1.0) as f32),
                    None => style.border_color.clone(),
                },
                None => style.border_color.clone(),
            };
            let brect = RectF::new(vr.x - bw * s, vr.y - bw * s, vr.w + 2.0 * bw * s, vr.h + 2.0 * bw * s);
            if bw > 0.0 {
                if let Some(e) = self.border_element(id, brect, (bw * es) as f32, ((radius + bw) * es) as f32, &border_paint, alpha) {
                    out.push(FeElement::Pixel(e));
                }
            }

            // backdrop blur (wallpaper-based)
            if style.blur && (alpha < 1.0 || self.window_translucent(&surface)) {
                if let (Some(tex), Some(sh)) = (self.wallpaper.blur_tex.clone(), &self.shaders) {
                    use smithay::backend::renderer::Texture;
                    let iw = self.wallpaper.image.as_ref().map(|i| (i.width() as f64, i.height() as f64));
                    if let Some(img) = iw {
                        let tsz = tex.size();
                        let local_vr = RectF::new(vr.x - dx, vr.y - dy, vr.w, vr.h);
                        if let Some((dst, src)) = wall_piece(&self.cfg.theme.wallpaper_mode, out_size, img, local_vr) {
                            let kx = tsz.w as f64 / img.0;
                            let ky = tsz.h as f64 / img.1;
                            let id = self.render_cache.blur_ids.entry(id).or_insert_with(Id::new).clone();
                            let te = TextureRenderElement::from_static_texture(
                                id,
                                renderer.context_id(),
                                Point::<f64, Physical>::from(((dst.x + dx) * scale, (dst.y + dy) * scale)),
                                tex.clone(),
                                1,
                                Transform::Normal,
                                Some(ws_alpha * a as f32),
                                Some(Rectangle::new((src.x * kx, src.y * ky).into(), (src.w * kx, src.h * ky).into())),
                                Some(Size::from((dst.w.round().max(1.0) as i32, dst.h.round().max(1.0) as i32))),
                                None,
                                Kind::Unspecified,
                            );
                            out.push(FeElement::ClippedTex(Clipped::new(te, sh.clip.clone(), clip_geo, (radius * es) as f32, 0.0)));
                        }
                    }
                }
            }

            // shadow
            if let Some(shadow) = style.shadow {
                let mut sh = shadow;
                let sf = s as f32;
                sh.x *= sf;
                sh.y *= sf;
                sh.blur *= sf;
                sh.spread *= sf;
                if let Some(e) = self.shadow_element(id, brect, &sh, (radius + bw) * s, scale, alpha) {
                    out.push(FeElement::Pixel(e));
                }
            }
        }
        for (id, lf) in remember {
            if let Some(w) = self.windows.get_mut(&id) {
                w.last_frame = Some(lf);
            }
        }
    }

    fn window_translucent(&self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> bool {
        use smithay::wayland::compositor::SurfaceAttributes;
        // no opaque region declared → treat as translucent (e.g. kitty with background_opacity)
        with_states(surface, |s| s.cached_state.get::<SurfaceAttributes>().current().opaque_region.is_none())
    }

    fn border_element(&mut self, id: WinId, r: RectF, width: f32, radius: f32, paint: &Paint, alpha: f32) -> Option<PixelShaderElement> {
        let shader = self.shaders.as_ref()?.border.clone();
        let area = round_rect(r);
        let (c1, c2, ang) = paint.stops();
        let mut vals = vec![width, radius, ang, alpha];
        vals.extend(c1.0);
        vals.extend(c2.0);
        let key = key_of(area, &vals);
        let entry = self.render_cache.borders.entry(id).or_insert_with(|| {
            (PixelShaderElement::new(shader.clone(), area, None, 1.0, border_uniforms(width, radius, paint, alpha), Kind::Unspecified), Vec::new())
        });
        if entry.1 != key {
            entry.0.resize(area, None);
            entry.0.update_uniforms(border_uniforms(width, radius, paint, alpha));
            entry.1 = key;
        }
        Some(entry.0.clone())
    }

    fn shadow_element(&mut self, id: WinId, win: RectF, s: &Shadow, radius: f64, scale: f64, alpha: f32) -> Option<PixelShaderElement> {
        let shader = self.shaders.as_ref()?.shadow.clone();
        let win_l: Rectangle<f64, Logical> = Rectangle::new((win.x, win.y).into(), (win.w, win.h).into());
        let area_f = shadow_area(win_l, s);
        let area = round_rect(RectF::new(area_f.loc.x, area_f.loc.y, area_f.size.w, area_f.size.h));
        let area_l: Rectangle<f64, Logical> = area.to_f64();
        let uni = shadow_uniforms(area_l, win_l, s, radius as f32, scale, alpha);
        let mut vals = vec![s.x, s.y, s.blur, s.spread, radius as f32, scale as f32, alpha, win.x as f32, win.y as f32, win.w as f32, win.h as f32];
        vals.extend(s.color.0);
        let key = key_of(area, &vals);
        let entry = self
            .render_cache
            .shadows
            .entry(id)
            .or_insert_with(|| (PixelShaderElement::new(shader.clone(), area, None, 1.0, uni.clone(), Kind::Unspecified), Vec::new()));
        if entry.1 != key {
            entry.0.resize(area, None);
            entry.0.update_uniforms(uni);
            entry.1 = key;
        }
        Some(entry.0.clone())
    }

    pub fn cleanup_render_cache(&mut self) {
        let ids: Vec<WinId> = self.windows.keys().copied().collect();
        self.render_cache.borders.retain(|k, _| ids.contains(k));
        self.render_cache.shadows.retain(|k, _| ids.contains(k));
        self.render_cache.blur_ids.retain(|k, _| ids.contains(k));
    }

    /// Frame callbacks + housekeeping after an output was rendered.
    pub fn post_render(&mut self, oi: usize) {
        let Some(o) = self.outputs.get(oi) else { return };
        let output = o.output.clone();
        let ws = o.ws;
        let from = o.switch.as_ref().map(|s| s.from);
        let time = self.clock.now();
        let throttle = Some(Duration::from_secs(1));
        for w in self.windows.values() {
            if w.ws == ws || Some(w.ws) == from {
                w.window.send_frame(&output, time, throttle, |_, _| Some(output.clone()));
            }
        }
        {
            let map = layer_map_for_output(&output);
            for l in map.layers() {
                l.send_frame(&output, time, throttle, |_, _| Some(output.clone()));
            }
        }
        if let CursorImageStatus::Surface(s) = &self.cursor.status {
            send_frames_surface_tree(s, &output, time, throttle, |_, _| Some(output.clone()));
        }
        if let Some(ls) = self.lock.surface_for(&output) {
            send_frames_surface_tree(ls.wl_surface(), &output, time, throttle, |_, _| Some(output.clone()));
        }
    }

    pub fn housekeeping(&mut self) {
        self.space.refresh();
        self.popups.cleanup();
        for o in self.outputs.iter().map(|o| o.output.clone()).collect::<Vec<_>>() {
            layer_map_for_output(&o).cleanup();
        }
        let _ = self.dh.flush_clients();
    }
}

fn push_layers(renderer: &mut GlesRenderer, output: &Output, layer: Layer, scale: f64, out: &mut Vec<FeElement>) {
    let map = layer_map_for_output(output);
    let layers: Vec<_> = map.layers_on(layer).rev().cloned().collect();
    for l in layers {
        let Some(geo) = map.layer_geometry(&l) else { continue };
        let loc = geo.loc.to_physical_precise_round(scale);
        let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            AsRenderElements::<GlesRenderer>::render_elements(&l, renderer, loc, Scale::from(scale), 1.0);
        out.extend(elems.into_iter().map(FeElement::Surface));
    }
}

#[allow(dead_code)]
fn unused(_: Shadow) {}
