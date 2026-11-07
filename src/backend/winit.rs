//! Nested backend: FeWM runs as a window inside another compositor.

use std::time::Instant;

use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, ImportEgl, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
    wayland::dmabuf::DmabufFeedbackBuilder,
};
use tracing::{info, warn};

use crate::{
    shell::OutputState,
    state::{Backend, Fe},
};

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage: OutputDamageTracker,
    pub output: Output,
}

pub fn init(event_loop: &mut EventLoop<'static, Fe>, fe: &mut Fe) -> anyhow::Result<()> {
    let (mut backend, winit) = winit::init::<GlesRenderer>().map_err(|e| anyhow::anyhow!("winit: {e}"))?;
    let size = backend.window_size();
    let mode = Mode { size, refresh: 60_000 };
    let output = Output::new(
        "winit".into(),
        PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "FerriumWM".into(), model: "nested".into(), serial_number: "0".into() },
    );
    let _global = output.create_global::<Fe>(&fe.dh);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    fe.space.map_output(&output, (0, 0));

    // GPU buffers from clients
    let formats = backend.renderer().dmabuf_formats();
    let node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .ok()
        .and_then(|d| d.try_get_render_node().ok().flatten());
    let global = match node.and_then(|n| DmabufFeedbackBuilder::new(n.dev_id(), formats.clone()).build().ok()) {
        Some(fb) => fe.dmabuf_state.create_global_with_default_feedback::<Fe>(&fe.dh, &fb),
        None => fe.dmabuf_state.create_global::<Fe>(&fe.dh, formats),
    };
    fe.dmabuf_global = Some(global);
    if backend.renderer().bind_wl_display(&fe.dh).is_ok() {
        info!("EGL hardware acceleration включён");
    }

    let damage = OutputDamageTracker::from_output(&output);
    fe.outputs.push(OutputState { output: output.clone(), ws: 1, switch: None });
    fe.ensure_workspaces();
    fe.backend = Backend::Winit(WinitData { backend, damage, output });

    event_loop.handle().insert_source(winit, move |event, _, fe| match event {
        WinitEvent::Resized { size, .. } => {
            if let Backend::Winit(d) = &fe.backend {
                d.output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
            }
            for o in fe.outputs.iter().map(|o| o.output.clone()).collect::<Vec<_>>() {
                smithay::desktop::layer_map_for_output(&o).arrange();
            }
            fe.relayout_all();
        }
        WinitEvent::Input(ev) => fe.process_input_event(ev),
        WinitEvent::Redraw => render(fe),
        WinitEvent::CloseRequested => fe.signal.stop(),
        _ => {}
    })
    .map_err(|e| anyhow::anyhow!("winit source: {e}"))?;
    Ok(())
}

pub fn render(fe: &mut Fe) {
    let mut backend = std::mem::replace(&mut fe.backend, Backend::None);
    if let Backend::Winit(d) = &mut backend {
        let now = Instant::now();
        let busy = fe.tick_animations(now);
        fe.redraw = false;
        let age = d.backend.buffer_age().unwrap_or(0);
        let damage = match d.backend.bind() {
            Ok((renderer, mut fb)) => {
                let elements = fe.render_elements(renderer, 0);
                match d.damage.render_output(renderer, &mut fb, age, &elements, [0.0, 0.0, 0.0, 1.0]) {
                    Ok(r) => r.damage.cloned(),
                    Err(e) => {
                        warn!("render: {e:?}");
                        None
                    }
                }
            }
            Err(e) => {
                warn!("bind: {e:?}");
                None
            }
        };
        if let Some(damage) = damage {
            if let Err(e) = d.backend.submit(Some(&damage)) {
                warn!("submit: {e:?}");
            }
        }
        fe.backend = backend;
        fe.post_render(0);
        fe.cleanup_render_cache();
        fe.housekeeping();
        if busy || fe.redraw {
            if let Backend::Winit(d) = &fe.backend {
                d.backend.window().request_redraw();
            }
        }
        return;
    }
    fe.backend = backend;
}
