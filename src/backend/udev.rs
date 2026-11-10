//! Real session backend: libseat + DRM/KMS + GBM + libinput (single render GPU).

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, ImportEgl, ImportMemWl, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Scale as OutScale},
    reexports::{
        calloop::{EventLoop, RegistrationToken},
        drm::control::{ModeTypeFlags, connector, crtc},
        input::{AccelProfile, Device as LiDevice, DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface},
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
};
use smithay_drm_extras::{
    display_info,
    drm_scanner::{DrmScanEvent, DrmScanner},
};
use tracing::{error, info, warn};

use crate::{
    config::{Config, OutputConfig},
    render::elements::FeElement,
    shell::OutputState,
    state::{Backend, Fe},
};

type Manager = DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type Out = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

struct Surface {
    output: Output,
    drm: Out,
    _global: GlobalId,
    pending: bool,
    pending_since: Option<Instant>,
    scheduled: bool,
}

struct Gpu {
    manager: Manager,
    scanner: DrmScanner,
    surfaces: HashMap<crtc::Handle, Surface>,
    _token: RegistrationToken,
}

pub struct UdevData {
    pub session: LibSeatSession,
    primary: DrmNode,
    renderer: Option<GlesRenderer>,
    gpus: HashMap<DrmNode, Gpu>,
    libinput: Libinput,
    devices: Vec<LiDevice>,
    pub monitors_off: bool,
}

impl UdevData {
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        self.renderer.as_mut()
    }
    pub fn early_import(&mut self, _surface: &WlSurface) {}
}

const FORMATS: &[Fourcc] = &[Fourcc::Abgr8888, Fourcc::Argb8888];

pub fn init(event_loop: &mut EventLoop<'static, Fe>, fe: &mut Fe) -> anyhow::Result<()> {
    let (session, notifier) = LibSeatSession::new().map_err(|e| anyhow::anyhow!("libseat: {e}"))?;
    let seat = session.seat();
    let primary = match std::env::var("FEWM_DRM_DEVICE") {
        Ok(p) => DrmNode::from_path(p)?,
        Err(_) => primary_gpu(&seat)
            .ok()
            .flatten()
            .and_then(|p| DrmNode::from_path(p).ok()?.node_with_type(NodeType::Render)?.ok())
            .or_else(|| all_gpus(&seat).ok()?.into_iter().find_map(|p| DrmNode::from_path(p).ok()))
            .ok_or_else(|| anyhow::anyhow!("нет GPU"))?,
    };
    info!("GPU: {primary}");

    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat).map_err(|_| anyhow::anyhow!("libinput: seat"))?;
    let li_backend = LibinputInputBackend::new(libinput.clone());

    fe.backend = Backend::Udev(UdevData {
        session,
        primary,
        renderer: None,
        gpus: HashMap::new(),
        libinput,
        devices: Vec::new(),
        monitors_off: false,
    });

    let udev = UdevBackend::new(&seat)?;
    for (dev_id, path) in udev.device_list() {
        if let Ok(node) = DrmNode::from_dev_id(dev_id) {
            if let Err(e) = device_added(fe, node, path) {
                warn!("устройство {path:?}: {e:#}");
            }
        }
    }
    if fe.backend_udev().is_some_and(|u| u.renderer.is_none()) {
        anyhow::bail!("не удалось инициализировать рендер на {primary}");
    }

    event_loop
        .handle()
        .insert_source(udev, |ev, _, fe| match ev {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    if let Err(e) = device_added(fe, node, &path) {
                        warn!("{e:#}");
                    }
                }
            }
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_changed(fe, node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_removed(fe, node);
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    event_loop
        .handle()
        .insert_source(li_backend, |mut ev, _, fe| {
            if let InputEvent::DeviceAdded { device } = &mut ev {
                configure_libinput(&fe.cfg, device);
                if let Some(u) = fe.backend_udev() {
                    u.devices.push(device.clone());
                }
            } else if let InputEvent::DeviceRemoved { device } = &ev {
                if let Some(u) = fe.backend_udev() {
                    u.devices.retain(|d| d != device);
                }
            }
            // any input wakes monitors
            if let Some(u) = fe.backend_udev() {
                if u.monitors_off && !matches!(ev, InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }) {
                    u.monitors_off = false;
                    fe.queue_redraw();
                }
            }
            fe.process_input_event(ev);
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    event_loop
        .handle()
        .insert_source(notifier, |ev, _, fe| match ev {
            SessionEvent::PauseSession => {
                info!("сессия на паузе (VT)");
                if let Some(u) = fe.backend_udev() {
                    u.libinput.suspend();
                    for g in u.gpus.values_mut() {
                        g.manager.pause();
                    }
                }
            }
            SessionEvent::ActivateSession => {
                info!("сессия активна");
                if let Some(u) = fe.backend_udev() {
                    if let Err(e) = u.libinput.resume() {
                        error!("libinput resume: {e:?}");
                    }
                    for g in u.gpus.values_mut() {
                        if let Err(e) = g.manager.lock().activate(false) {
                            error!("drm activate: {e}");
                        }
                        for s in g.surfaces.values_mut() {
                            s.pending = false;
                        }
                    }
                }
                fe.queue_redraw();
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(())
}

impl Fe {
    pub fn backend_udev(&mut self) -> Option<&mut UdevData> {
        match &mut self.backend {
            Backend::Udev(u) => Some(u),
            _ => None,
        }
    }
}

fn device_added(fe: &mut Fe, node: DrmNode, path: &Path) -> anyhow::Result<()> {
    let Some(u) = fe.backend_udev() else { return Ok(()) };
    let primary = u.primary;
    // only drive the render GPU's display device(s) (single-GPU setup)
    let render = node.node_with_type(NodeType::Render).and_then(|n| n.ok());
    if render != Some(primary) && node != primary {
        info!("пропускаю второстепенный GPU {path:?}");
        return Ok(());
    }
    let fd = u.session.open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;

    let token = fe
        .lh
        .insert_source(notifier, move |ev, _meta, fe| match ev {
            DrmEvent::VBlank(crtc) => vblank(fe, node, crtc),
            DrmEvent::Error(e) => error!("drm: {e:?}"),
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_device = EGLDevice::device_for_display(&display)?;
    let context = EGLContext::new(&display)?;
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node).into());
    let manager = DrmOutputManager::new(drm, allocator, exporter, Some(gbm), FORMATS.iter().copied(), render_formats);

    // client buffer support
    let _ = renderer.bind_wl_display(&fe.dh);
    let formats = renderer.dmabuf_formats();
    let rnode = egl_device.try_get_render_node().ok().flatten().unwrap_or(node);
    let global = match DmabufFeedbackBuilder::new(rnode.dev_id(), formats.clone()).build() {
        Ok(fb) => fe.dmabuf_state.create_global_with_default_feedback::<Fe>(&fe.dh, &fb),
        Err(_) => fe.dmabuf_state.create_global::<Fe>(&fe.dh, formats),
    };
    fe.dmabuf_global = Some(global);
    fe.capture.render_node = Some(rnode);
    fe.shm_state.update_formats(renderer.shm_formats());

    let u = fe.backend_udev().unwrap();
    u.renderer = Some(renderer);
    u.gpus.insert(node, Gpu { manager, scanner: DrmScanner::new(), surfaces: HashMap::new(), _token: token });
    device_changed(fe, node);
    Ok(())
}

fn device_changed(fe: &mut Fe, node: DrmNode) {
    let Some(u) = fe.backend_udev() else { return };
    let Some(gpu) = u.gpus.get_mut(&node) else { return };
    let events: Vec<DrmScanEvent> = match gpu.scanner.scan_connectors(gpu.manager.device()) {
        Ok(r) => r.into_iter().collect(),
        Err(e) => {
            warn!("скан коннекторов: {e:?}");
            return;
        }
    };
    for ev in events {
        match ev {
            DrmScanEvent::Connected { connector, crtc: Some(crtc) } => connector_connected(fe, node, connector, crtc),
            DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => connector_disconnected(fe, node, crtc),
            _ => {}
        }
    }
}

fn device_removed(fe: &mut Fe, node: DrmNode) {
    let crtcs: Vec<crtc::Handle> = fe
        .backend_udev()
        .and_then(|u| u.gpus.get(&node))
        .map(|g| g.surfaces.keys().copied().collect())
        .unwrap_or_default();
    for c in crtcs {
        connector_disconnected(fe, node, c);
    }
    if let Some(u) = fe.backend_udev() {
        u.gpus.remove(&node);
    }
}

fn pick_mode(conn: &connector::Info, want: &str) -> smithay::reexports::drm::control::Mode {
    let modes = conn.modes();
    let preferred = modes
        .iter()
        .position(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(0);
    let want = want.trim();
    if want.is_empty() || want == "preferred" || want == "auto" || want == "highrr" {
        // preferred resolution, highest refresh rate (EDID overclocks like 95 Hz panels)
        let size = modes[preferred].size();
        return modes
            .iter()
            .filter(|m| m.size() == size)
            .max_by_key(|m| m.vrefresh())
            .copied()
            .unwrap_or(modes[preferred]);
    }
    let (res, rate) = want.split_once('@').map(|(a, b)| (a, Some(b))).unwrap_or((want, None));
    let Some((w, h)) = res.split_once('x').and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?))) else {
        return modes[preferred];
    };
    let rate: Option<f64> = rate.and_then(|r| r.trim_end_matches("Hz").parse().ok());
    modes
        .iter()
        .filter(|m| m.size() == (w, h))
        .min_by_key(|m| rate.map(|r| ((m.vrefresh() as f64 - r).abs() * 1000.0) as i64).unwrap_or(-(m.vrefresh() as i64)))
        .copied()
        .unwrap_or(modes[preferred])
}

fn parse_transform(s: &str) -> Transform {
    match s {
        "90" => Transform::_90,
        "180" => Transform::_180,
        "270" => Transform::_270,
        "flipped" => Transform::Flipped,
        "flipped-90" => Transform::Flipped90,
        "flipped-180" => Transform::Flipped180,
        "flipped-270" => Transform::Flipped270,
        _ => Transform::Normal,
    }
}

fn connector_connected(fe: &mut Fe, node: DrmNode, conn: connector::Info, crtc: crtc::Handle) {
    let name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());
    let ocfg: OutputConfig = fe.cfg.outputs.iter().find(|o| o.name == name).cloned().unwrap_or_default();
    if !ocfg.enabled {
        info!("{name} выключен в конфиге");
        return;
    }
    let dh = fe.dh.clone();
    let x_next: i32 = (0..fe.outputs.len()).filter_map(|i| fe.output_geo(i)).map(|g| g.loc.x + g.size.w).max().unwrap_or(0);
    let Some(u) = fe.backend_udev() else { return };
    let Some(gpu) = u.gpus.get_mut(&node) else { return };
    let Some(renderer) = u.renderer.as_mut() else { return };
    let info = display_info::for_connector(gpu.manager.device(), conn.handle());
    let make = info.as_ref().and_then(|i| i.make()).unwrap_or_else(|| "Unknown".into());
    let model = info.as_ref().and_then(|i| i.model()).unwrap_or_else(|| "Unknown".into());
    let serial = info.as_ref().and_then(|i| i.serial()).unwrap_or_else(|| "Unknown".into());
    let mode = pick_mode(&conn, &ocfg.mode);
    let wl_mode = WlMode::from(mode);
    let (pw, ph) = conn.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties { size: (pw as i32, ph as i32).into(), subpixel: conn.subpixel().into(), make, model, serial_number: serial },
    );
    let global = output.create_global::<Fe>(&dh);
    let pos = match ocfg.position.split_once('x') {
        Some((x, y)) if ocfg.position != "auto" => (x.parse().unwrap_or(x_next), y.parse().unwrap_or(0)),
        _ => (x_next, 0),
    };
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(parse_transform(&ocfg.transform)),
        Some(OutScale::Fractional(if ocfg.scale > 0.0 { ocfg.scale } else { 1.0 })),
        Some(pos.into()),
    );
    let planes = gpu.manager.device().planes(&crtc).ok();
    let drm = match gpu.manager.lock().initialize_output::<_, FeElement>(
        crtc,
        mode,
        &[conn.handle()],
        &output,
        planes,
        renderer,
        &DrmOutputRenderElements::default(),
    ) {
        Ok(d) => d,
        Err(e) => {
            warn!("{name}: не удалось настроить вывод: {e}");
            return;
        }
    };
    gpu.surfaces.insert(crtc, Surface { output: output.clone(), drm, _global: global, pending: false, pending_since: None, scheduled: false });
    info!("монитор {name}: {}x{}@{}", wl_mode.size.w, wl_mode.size.h, wl_mode.refresh / 1000);

    fe.space.map_output(&output, pos);
    let used: Vec<u32> = fe.outputs.iter().map(|o| o.ws).collect();
    let ws = (1..=99).find(|w| !used.contains(w)).unwrap_or(1);
    fe.outputs.push(OutputState { output, ws, switch: None });
    fe.ensure_workspaces();
    fe.relayout_all();
    fe.queue_redraw();
}

fn connector_disconnected(fe: &mut Fe, node: DrmNode, crtc: crtc::Handle) {
    let Some(u) = fe.backend_udev() else { return };
    let Some(gpu) = u.gpus.get_mut(&node) else { return };
    let Some(s) = gpu.surfaces.remove(&crtc) else { return };
    let output = s.output;
    info!("монитор {} отключён", output.name());
    fe.space.unmap_output(&output);
    fe.outputs.retain(|o| o.output != output);
    if fe.focused_output >= fe.outputs.len() {
        fe.focused_output = 0;
    }
    fe.relayout_all();
    fe.queue_redraw();
}

fn vblank(fe: &mut Fe, node: DrmNode, crtc: crtc::Handle) {
    let Some(u) = fe.backend_udev() else { return };
    let Some(s) = u.gpus.get_mut(&node).and_then(|g| g.surfaces.get_mut(&crtc)) else { return };
    if let Err(e) = s.drm.frame_submitted() {
        warn!("frame_submitted: {e:?}");
    }
    s.pending = false;
    s.pending_since = None;
    let busy = fe.tick_animations(Instant::now());
    if fe.redraw || busy {
        schedule(fe, node, crtc);
    }
}

fn schedule(fe: &mut Fe, node: DrmNode, crtc: crtc::Handle) {
    let Some(u) = fe.backend_udev() else { return };
    let Some(s) = u.gpus.get_mut(&node).and_then(|g| g.surfaces.get_mut(&crtc)) else { return };
    if s.scheduled || s.pending {
        return;
    }
    s.scheduled = true;
    fe.lh.insert_idle(move |fe| render(fe, node, crtc));
}

/// If a page flip never completes (driver hiccup), don't freeze forever.
pub fn watchdog(fe: &mut Fe) {
    let Some(u) = fe.backend_udev() else { return };
    let mut stuck = false;
    for g in u.gpus.values_mut() {
        for s in g.surfaces.values_mut() {
            if s.pending && s.pending_since.is_some_and(|t| t.elapsed().as_millis() > 500) {
                warn!("{}: vblank не пришёл за 500 мс, продолжаю рисовать", s.output.name());
                s.pending = false;
                s.pending_since = None;
                stuck = true;
            }
        }
    }
    if stuck {
        fe.redraw = true;
        schedule_all(fe);
    }
}

pub fn schedule_all(fe: &mut Fe) {
    let targets: Vec<(DrmNode, crtc::Handle)> = match &fe.backend {
        Backend::Udev(u) => u.gpus.iter().flat_map(|(n, g)| g.surfaces.keys().map(move |c| (*n, *c))).collect(),
        _ => return,
    };
    for (n, c) in targets {
        schedule(fe, n, c);
    }
}

fn render(fe: &mut Fe, node: DrmNode, crtc: crtc::Handle) {
    let mut backend = std::mem::replace(&mut fe.backend, Backend::None);
    let Backend::Udev(u) = &mut backend else {
        fe.backend = backend;
        return;
    };
    if !u.session.is_active() {
        if let Some(s) = u.gpus.get_mut(&node).and_then(|g| g.surfaces.get_mut(&crtc)) {
            s.scheduled = false;
        }
        fe.backend = backend;
        return;
    }
    let monitors_off = u.monitors_off;
    let Some(renderer) = u.renderer.as_mut() else {
        fe.backend = backend;
        return;
    };
    let Some(s) = u.gpus.get_mut(&node).and_then(|g| g.surfaces.get_mut(&crtc)) else {
        fe.backend = backend;
        return;
    };
    s.scheduled = false;
    let Some(oi) = fe.outputs.iter().position(|o| o.output == s.output) else {
        fe.backend = backend;
        return;
    };
    fe.tick_animations(Instant::now());
    fe.redraw = false;
    let elements = if monitors_off { Vec::new() } else { fe.render_elements(renderer, oi) };
    let flags = if fe.game.is_some() {
        FrameFlags::DEFAULT | FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT_ANY
    } else {
        FrameFlags::DEFAULT
    };
    match s.drm.render_frame(renderer, &elements, [0.0, 0.0, 0.0, 1.0], flags) {
        Ok(res) => {
            if !res.is_empty {
                match s.drm.queue_frame(()) {
                    Ok(()) => {
                        s.pending = true;
                        s.pending_since = Some(Instant::now());
                    }
                    Err(e) => warn!("queue_frame: {e:?}"),
                }
            }
        }
        Err(e) => warn!("render_frame: {e:?}"),
    }
    drop(elements);
    fe.backend = backend;
    fe.post_render(oi);
    fe.cleanup_render_cache();
    fe.housekeeping();
}

fn configure_libinput(cfg: &Config, d: &mut LiDevice) {
    let i = &cfg.input;
    if d.has_capability(DeviceCapability::Pointer) {
        let touchpad = d.config_tap_finger_count() > 0;
        if touchpad {
            let _ = d.config_tap_set_enabled(i.touchpad_tap_to_click);
            let _ = d.config_tap_set_drag_enabled(i.touchpad_drag);
            let _ = d.config_dwt_set_enabled(i.touchpad_disable_while_typing);
            let _ = d.config_scroll_set_natural_scroll_enabled(i.touchpad_natural_scroll);
            let _ = d.config_accel_set_speed(i.touchpad_accel_speed.clamp(-1.0, 1.0));
        } else {
            let _ = d.config_scroll_set_natural_scroll_enabled(i.natural_scroll);
            let _ = d.config_accel_set_speed(i.accel_speed.clamp(-1.0, 1.0));
            let _ = d.config_accel_set_profile(if i.accel_profile == "flat" { AccelProfile::Flat } else { AccelProfile::Adaptive });
        }
        let _ = d.config_left_handed_set(i.left_handed);
    }
}

pub fn reconfigure_inputs(fe: &mut Fe) {
    let cfg = fe.cfg.clone();
    if let Some(u) = fe.backend_udev() {
        for d in &mut u.devices {
            configure_libinput(&cfg, d);
        }
    }
}

pub fn power_off_monitors(fe: &mut Fe) {
    if let Some(u) = fe.backend_udev() {
        u.monitors_off = true;
    }
    fe.queue_redraw();
}

pub fn change_vt(fe: &mut Fe, vt: i32) {
    if let Some(u) = fe.backend_udev() {
        if let Err(e) = u.session.change_vt(vt) {
            warn!("смена VT: {e}");
        }
    }
}

pub fn configure_device<D>(_fe: &mut Fe, _device: D) {}
