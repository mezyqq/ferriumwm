//! Screen capture for grim/flameshot (ext-image-copy-capture-v1, output sources, SHM buffers).

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen, TextureMapping,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    output::{Output, WeakOutput},
    reexports::wayland_server::protocol::wl_shm,
    utils::{IsAlive, Rectangle, Size, Transform},
    wayland::{
        image_capture_source::{
            ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState,
        },
        image_copy_capture::{
            BufferConstraints, CaptureFailureReason, Frame, ImageCopyCaptureHandler, ImageCopyCaptureState, Session,
            SessionRef,
        },
        shm::with_buffer_contents_mut,
    },
};
use tracing::warn;

use crate::state::{Backend, Fe};

fn output_of(source: &ImageCaptureSource) -> Option<Output> {
    source.user_data().get::<WeakOutput>()?.upgrade()
}

fn buffer_size(o: &Output) -> Option<Size<i32, smithay::utils::Buffer>> {
    let mode = o.current_mode()?;
    let s = o.current_transform().transform_size(mode.size);
    Some((s.w, s.h).into())
}

impl ImageCaptureSourceHandler for Fe {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

impl OutputCaptureSourceHandler for Fe {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_state
    }
    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

impl ImageCopyCaptureHandler for Fe {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let o = output_of(source)?;
        Some(BufferConstraints {
            size: buffer_size(&o)?,
            shm: vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888, wl_shm::Format::Abgr8888, wl_shm::Format::Xbgr8888],
            dma: None,
        })
    }

    fn new_session(&mut self, session: Session) {
        self.capture_sessions.retain(|s| s.alive());
        self.capture_sessions.push(session);
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let Some(output) = output_of(&session.source()) else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        };
        match self.capture_output(&output, &frame) {
            Ok(()) => {
                let size = buffer_size(&output).unwrap_or_default();
                let t = self.clock.now();
                frame.success(Transform::Normal, Some(vec![Rectangle::from_size(size)]), t);
            }
            Err(e) => {
                warn!("захват экрана: {e}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }
}

impl Fe {
    fn capture_output(&mut self, output: &Output, frame: &Frame) -> Result<(), String> {
        let oi = self.outputs.iter().position(|o| &o.output == output).ok_or("нет такого монитора")?;
        let mode = output.current_mode().ok_or("нет режима")?;
        let scale = output.current_scale().fractional_scale();
        let size = mode.size;
        let buffer = frame.buffer();

        let mut backend = std::mem::replace(&mut self.backend, Backend::None);
        let result = (|| -> Result<Vec<u8>, String> {
            let renderer: &mut GlesRenderer = backend.renderer().ok_or("нет рендера")?;
            self.capture_no_cursor = true;
            let elements = self.render_elements(renderer, oi);
            self.capture_no_cursor = false;
            let mut tex: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, (size.w, size.h).into())
                .map_err(|e| format!("{e:?}"))?;
            let mut fb = renderer.bind(&mut tex).map_err(|e| format!("{e:?}"))?;
            let mut dt = OutputDamageTracker::new(size, scale, Transform::Normal);
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
            Ok(out)
        })();
        self.backend = backend;
        let rgba = result?;

        with_buffer_contents_mut(&buffer, |ptr, len, data| -> Result<(), String> {
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
            let rgba = rgba.as_slice();
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
}
