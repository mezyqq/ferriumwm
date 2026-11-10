//! Screen capture: screenshots (grim/flameshot) and screencasts (OBS, browsers, Discord…)
//! through ext-image-copy-capture-v1 + xdg-desktop-portal-wlr.
//!
//! Sources: whole outputs and single windows (ext-foreign-toplevel-list-v1).
//! Buffers: SHM (CPU readback) and DMA-BUF (GPU render straight into the client's buffer).
//! Frames are paced at a steady CAPTURE_FPS on a fixed time grid (static screens
//! included), so OBS & co. record a stable frame rate.

use std::cell::Cell;
use std::time::{Duration, Instant};

use smithay::{
    backend::{
        allocator::{Fourcc, Modifier},
        drm::DrmNode,
        renderer::{
            Bind, ExportMem, Offscreen, TextureMapping,
            damage::OutputDamageTracker,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    desktop::PopupManager,
    output::{Output, WeakOutput},
    reexports::wayland_server::protocol::{wl_buffer::WlBuffer, wl_shm},
    utils::{IsAlive, Physical, Point, Rectangle, Size, Transform},
    wayland::{
        dmabuf::get_dmabuf,
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState},
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState,
            ToplevelCaptureSourceHandler, ToplevelCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, CaptureFailureReason, DmabufConstraints, Frame, FrameRef, ImageCopyCaptureHandler,
            ImageCopyCaptureState, Session, SessionRef,
        },
        shm::with_buffer_contents_mut,
    },
};
use tracing::{debug, warn};

use crate::{
    layout::WinId,
    render::elements::FeElement,
    state::{Backend, Fe},
};

/// What a capture source points at (stored in the source's user data).
#[derive(Clone)]
enum Target {
    Output(WeakOutput),
    Window(WinId),
}

/// Session user data: time slot of the last delivered frame.
struct Served(Cell<Option<Instant>>);

const CAPTURE_FPS: u64 = 60;
const INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / CAPTURE_FPS);
/// serve a frame this early rather than waiting another tick
const SLACK: Duration = Duration::from_micros(1500);

const SHM_FORMATS: [wl_shm::Format; 4] =
    [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888, wl_shm::Format::Abgr8888, wl_shm::Format::Xbgr8888];
const DMA_FORMATS: [Fourcc; 4] = [Fourcc::Argb8888, Fourcc::Xrgb8888, Fourcc::Abgr8888, Fourcc::Xbgr8888];

/// Capture-related compositor state.
pub struct CaptureState {
    pub image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState,
    pub output_capture_state: OutputCaptureSourceState,
    pub toplevel_capture_state: ToplevelCaptureSourceState,
    pub image_copy_state: ImageCopyCaptureState,
    pub foreign_toplevels: ForeignToplevelListState,
    /// foreign-toplevel handle of every window, for window picking in the portal
    pub handles: std::collections::HashMap<WinId, ForeignToplevelHandle>,
    pub sessions: Vec<Session>,
    /// frames waiting for their time slot
    pending: Vec<(SessionRef, Frame, Instant)>,
    /// render node for DMA-BUF capture (None = SHM only)
    pub render_node: Option<DrmNode>,
    dma_formats: Option<Vec<(Fourcc, Vec<Modifier>)>>,
}

impl CaptureState {
    pub fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Self {
        Self {
            image_capture_source_state: smithay::wayland::image_capture_source::ImageCaptureSourceState::new(),
            output_capture_state: OutputCaptureSourceState::new::<Fe>(dh),
            toplevel_capture_state: ToplevelCaptureSourceState::new::<Fe>(dh),
            image_copy_state: ImageCopyCaptureState::new::<Fe>(dh),
            foreign_toplevels: ForeignToplevelListState::new::<Fe>(dh),
            handles: Default::default(),
            sessions: Vec::new(),
            pending: Vec::new(),
            render_node: None,
            dma_formats: None,
        }
    }

    /// Windows somebody is recording right now (they keep getting frame callbacks).
    pub fn captured_windows(&self) -> Vec<WinId> {
        self.sessions
            .iter()
            .filter(|s| s.alive())
            .filter_map(|s| match target_of(&s.as_ref().source()) {
                Some(Target::Window(id)) => Some(id),
                _ => None,
            })
            .collect()
    }
}

fn target_of(source: &ImageCaptureSource) -> Option<Target> {
    source.user_data().get::<Target>().cloned()
}

fn output_size(o: &Output) -> Option<Size<i32, Physical>> {
    let mode = o.current_mode()?;
    let s = o.current_transform().transform_size(mode.size);
    Some(s)
}

impl ImageCaptureSourceHandler for Fe {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

impl OutputCaptureSourceHandler for Fe {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.capture.output_capture_state
    }
    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| Target::Output(output.downgrade()));
    }
}

impl ToplevelCaptureSourceHandler for Fe {
    fn toplevel_capture_source_state(&mut self) -> &mut ToplevelCaptureSourceState {
        &mut self.capture.toplevel_capture_state
    }
    fn toplevel_source_created(&mut self, source: ImageCaptureSource, toplevel: ForeignToplevelHandle) {
        let id = self.capture.handles.iter().find(|(_, h)| h.matches(&toplevel)).map(|(id, _)| *id);
        if let Some(id) = id {
            source.user_data().insert_if_missing(|| Target::Window(id));
        }
    }
}

impl ForeignToplevelListHandler for Fe {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.capture.foreign_toplevels
    }
}

impl ImageCopyCaptureHandler for Fe {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.capture.image_copy_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let target = target_of(source)?;
        self.constraints(&target)
    }

    fn new_session(&mut self, session: Session) {
        session.as_ref().user_data().insert_if_missing(|| Served(Cell::new(None)));
        self.capture.sessions.retain(|s| s.alive());
        self.capture.sessions.push(session);
        // a recorded window on a hidden workspace still has to draw
        self.queue_redraw();
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let now = Instant::now();
        let last = session.user_data().get::<Served>().and_then(|s| s.0.get());
        let due = match last {
            Some(t) => t + INTERVAL,
            None => now,
        };
        if due <= now + SLACK {
            // keep the grid when the client keeps up, restart it after a pause
            let slot = if now.saturating_duration_since(due) < INTERVAL { due } else { now };
            self.serve(session, frame, slot);
            return;
        }
        self.capture.pending.push((session.clone(), frame, due));
        let timer = smithay::reexports::calloop::timer::Timer::from_deadline(due);
        if let Err(e) = self.lh.insert_source(timer, |_, _, fe| {
            fe.serve_due();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        }) {
            warn!("захват: таймер: {e}");
        }
    }

    fn frame_aborted(&mut self, frame: FrameRef) {
        self.capture.pending.retain(|(_, f, _)| *f != frame);
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.capture.pending.retain(|(s, _, _)| *s != session);
        self.capture.sessions.retain(|s| s.alive() && *s != session);
    }
}

impl Fe {
    // ------------------------------------------------------------ toplevel list

    /// Publish a window in ext-foreign-toplevel-list (portal window picker).
    pub fn toplevel_list_add(&mut self, id: WinId) {
        if self.capture.handles.contains_key(&id) {
            return;
        }
        let Some(w) = self.windows.get(&id) else { return };
        let h = self.capture.foreign_toplevels.new_toplevel::<Fe>(w.title.clone(), w.app_id.clone());
        h.send_done();
        self.capture.handles.insert(id, h);
    }

    pub fn toplevel_list_update(&mut self, id: WinId) {
        let (Some(w), Some(h)) = (self.windows.get(&id), self.capture.handles.get(&id)) else { return };
        if h.title() != w.title || h.app_id() != w.app_id {
            h.send_title(&w.title);
            h.send_app_id(&w.app_id);
            h.send_done();
        }
    }

    pub fn toplevel_list_remove(&mut self, id: WinId) {
        if let Some(h) = self.capture.handles.remove(&id) {
            self.capture.foreign_toplevels.remove_toplevel(&h);
        }
        // sessions recording this window are over
        let gone: Vec<usize> = self
            .capture
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(target_of(&s.as_ref().source()), Some(Target::Window(w)) if w == id))
            .map(|(i, _)| i)
            .collect();
        for i in gone.into_iter().rev() {
            let s = self.capture.sessions.remove(i);
            self.capture.pending.retain(|(p, _, _)| *p != s.as_ref());
            s.stop();
        }
    }

    // ------------------------------------------------------------ pacing

    /// Serve every waiting frame whose time slot has come.
    fn serve_due(&mut self) {
        let now = Instant::now();
        let (ready, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.capture.pending).into_iter().partition(|(_, _, due)| *due <= now + SLACK);
        self.capture.pending = rest;
        for (s, f, due) in ready {
            self.serve(&s, f, due);
        }
        let _ = self.dh.flush_clients();
    }

    // ------------------------------------------------------------ constraints

    fn constraints(&mut self, target: &Target) -> Option<BufferConstraints> {
        let size = match target {
            Target::Output(o) => output_size(&o.upgrade()?)?,
            Target::Window(id) => self.window_capture_geo(*id)?.0,
        };
        Some(BufferConstraints {
            size: (size.w, size.h).into(),
            shm: SHM_FORMATS.to_vec(),
            dma: self.dma_constraints(),
        })
    }

    fn dma_constraints(&mut self) -> Option<DmabufConstraints> {
        let node = self.capture.render_node?;
        if self.capture.dma_formats.is_none() {
            let renderer = self.backend.renderer()?;
            let set = renderer.egl_context().dmabuf_render_formats();
            let formats: Vec<(Fourcc, Vec<Modifier>)> = DMA_FORMATS
                .iter()
                .map(|code| (*code, set.iter().filter(|f| f.code == *code).map(|f| f.modifier).collect::<Vec<_>>()))
                .filter(|(_, m)| !m.is_empty())
                .collect();
            self.capture.dma_formats = Some(formats);
        }
        let formats = self.capture.dma_formats.clone()?;
        (!formats.is_empty()).then_some(DmabufConstraints { node, formats })
    }

    /// Size (physical) and scale for capturing a window.
    fn window_capture_geo(&self, id: WinId) -> Option<(Size<i32, Physical>, f64)> {
        let w = self.windows.get(&id)?;
        let oi = self.output_of_ws(w.ws).unwrap_or(self.focused_output);
        let scale = self
            .outputs
            .get(oi)
            .or(self.outputs.first())
            .map(|o| o.output.current_scale().fractional_scale())
            .unwrap_or(1.0);
        let geo = w.window.geometry();
        let s: Size<i32, Physical> = geo.size.to_f64().to_physical_precise_round(scale);
        Some((Size::from((s.w.max(1), s.h.max(1))), scale))
    }

    // ------------------------------------------------------------ rendering

    fn serve(&mut self, session: &SessionRef, frame: Frame, slot: Instant) {
        let Some(target) = target_of(&session.source()) else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        };
        // size changed (window resized, mode switch): tell the client to reallocate
        if let Some(c) = self.constraints(&target) {
            if session.current_constraints().is_some_and(|cur| cur.size != c.size) {
                debug!("захват: новый размер {:?}", c.size);
                session.update_constraints(c);
                frame.fail(CaptureFailureReason::BufferConstraints);
                return;
            }
        } else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        }
        let res = match &target {
            Target::Output(o) => match o.upgrade() {
                Some(o) => self.capture_output(&o, &frame.buffer(), session.draw_cursor()),
                None => Err("монитор отключён".into()),
            },
            Target::Window(id) => self.capture_window(*id, &frame.buffer()),
        };
        match res {
            Ok(size) => {
                if let Some(s) = session.user_data().get::<Served>() {
                    s.0.set(Some(slot));
                }
                let t = self.clock.now();
                frame.success(Transform::Normal, Some(vec![Rectangle::from_size((size.w, size.h).into())]), t);
                if let Target::Window(id) = target {
                    // keep a recorded window drawing even on a hidden workspace
                    self.send_window_frame(id);
                }
            }
            Err(e) => {
                warn!("захват экрана: {e}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }

    fn send_window_frame(&self, id: WinId) {
        let Some(w) = self.windows.get(&id) else { return };
        let Some(output) = self.output_of_ws(w.ws).or(Some(self.focused_output)).and_then(|i| self.outputs.get(i)) else {
            return;
        };
        let o = output.output.clone();
        w.window.send_frame(&o, self.clock.now(), None, |_, _| Some(o.clone()));
    }

    fn capture_output(&mut self, output: &Output, buffer: &WlBuffer, cursor: bool) -> Result<Size<i32, Physical>, String> {
        let oi = self.outputs.iter().position(|o| &o.output == output).ok_or("нет такого монитора")?;
        let size = output_size(output).ok_or("нет режима")?;
        let scale = output.current_scale().fractional_scale();
        self.render_to_buffer(buffer, size, scale, |fe, r| {
            fe.capture_no_cursor = !cursor;
            let e = fe.render_elements(r, oi);
            fe.capture_no_cursor = false;
            e
        })?;
        Ok(size)
    }

    fn capture_window(&mut self, id: WinId, buffer: &WlBuffer) -> Result<Size<i32, Physical>, String> {
        let (size, scale) = self.window_capture_geo(id).ok_or("окно закрыто")?;
        let w = self.windows.get(&id).ok_or("окно закрыто")?;
        let window = w.window.clone();
        let surface = window.toplevel().ok_or("окно закрыто")?.wl_surface().clone();
        let geo = window.geometry();
        self.render_to_buffer(buffer, size, scale, move |_, r| {
            let loc: Point<i32, Physical> = Point::<f64, smithay::utils::Logical>::from((-geo.loc.x as f64, -geo.loc.y as f64)).to_physical_precise_round(scale);
            let mut out = Vec::new();
            for (popup, poff) in PopupManager::popups_for_surface(&surface) {
                let off = (geo.loc + poff - popup.geometry().loc).to_f64().to_physical_precise_round(scale);
                let e: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(r, popup.wl_surface(), loc + off, scale, 1.0, Kind::Unspecified);
                out.extend(e.into_iter().map(FeElement::Surface));
            }
            let e: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(r, &surface, loc, scale, 1.0, Kind::Unspecified);
            out.extend(e.into_iter().map(FeElement::Surface));
            out
        })?;
        Ok(size)
    }

    /// Render `elements` (size × scale, top-down) into a client SHM or DMA-BUF buffer.
    fn render_to_buffer(
        &mut self,
        buffer: &WlBuffer,
        size: Size<i32, Physical>,
        scale: f64,
        elements: impl FnOnce(&mut Fe, &mut GlesRenderer) -> Vec<FeElement>,
    ) -> Result<(), String> {
        let mut backend = std::mem::replace(&mut self.backend, Backend::None);
        let result = (|| -> Result<Option<Vec<u8>>, String> {
            let renderer: &mut GlesRenderer = backend.renderer().ok_or("нет рендера")?;
            let elements = elements(self, renderer);
            let mut dt = OutputDamageTracker::new(size, scale, Transform::Normal);

            if let Ok(dmabuf) = get_dmabuf(buffer) {
                // GPU path: draw straight into the client's buffer
                let mut dmabuf = dmabuf.clone();
                let mut fb = renderer.bind(&mut dmabuf).map_err(|e| format!("bind dmabuf: {e:?}"))?;
                let res = dt
                    .render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 1.0])
                    .map_err(|e| format!("{e:?}"))?;
                // ext-image-copy-capture has no fences: the buffer must be done when we say ready
                let _ = res.sync.wait();
                return Ok(None);
            }

            let mut tex: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, (size.w, size.h).into())
                .map_err(|e| format!("{e:?}"))?;
            let mut fb = renderer.bind(&mut tex).map_err(|e| format!("{e:?}"))?;
            dt.render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 1.0]).map_err(|e| format!("{e:?}"))?;
            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size((size.w, size.h).into()), Fourcc::Abgr8888)
                .map_err(|e| format!("{e:?}"))?;
            drop(fb);
            // offscreen GL textures come out bottom-up
            let flipped = !mapping.flipped();
            let bytes = renderer.map_texture(&mapping).map_err(|e| format!("{e:?}"))?;
            let row = size.w as usize * 4;
            let mut out = vec![0u8; row * size.h as usize];
            for y in 0..size.h as usize {
                let sy = if flipped { size.h as usize - 1 - y } else { y };
                out[y * row..(y + 1) * row].copy_from_slice(&bytes[sy * row..(sy + 1) * row]);
            }
            Ok(Some(out))
        })();
        self.backend = backend;
        let Some(rgba) = result? else { return Ok(()) };
        copy_to_shm(buffer, size, &rgba)
    }
}

fn copy_to_shm(buffer: &WlBuffer, size: Size<i32, Physical>, rgba: &[u8]) -> Result<(), String> {
    with_buffer_contents_mut(buffer, |ptr, len, data| -> Result<(), String> {
        let w = data.width.min(size.w) as usize;
        let h = data.height.min(size.h) as usize;
        let stride = data.stride as usize;
        let base = data.offset as usize;
        if base + stride * h > len {
            return Err("буфер слишком мал".into());
        }
        let dst = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        let swap = matches!(data.format, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let src_stride = size.w as usize * 4;
        // Split rows across all available cores: screenshots get faster and
        // no longer pin a single core while copying a large framebuffer.
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, h);
        let rows_per = h.div_ceil(workers);
        let dst = &mut dst[base..base + stride * h];
        std::thread::scope(|scope| {
            for (chunk_idx, chunk) in dst.chunks_mut(rows_per * stride).enumerate() {
                let y0 = chunk_idx * rows_per;
                scope.spawn(move || {
                    for (j, d) in chunk.chunks_mut(stride).enumerate() {
                        let y = y0 + j;
                        if y >= h {
                            break;
                        }
                        let s = &rgba[y * src_stride..y * src_stride + w * 4];
                        if swap {
                            for x in 0..w {
                                let o = x * 4;
                                d[o] = s[o + 2];
                                d[o + 1] = s[o + 1];
                                d[o + 2] = s[o];
                                d[o + 3] = s[o + 3];
                            }
                        } else {
                            d[..w * 4].copy_from_slice(&s[..w * 4]);
                        }
                    }
                });
            }
        });
        Ok(())
    })
    .map_err(|e| format!("{e:?}"))?
}
