//! Global compositor state and protocol handlers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{ImportDma, utils::on_commit_buffer_handler},
    },
    delegate_dispatch2,
    desktop::{
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        keyboard::XkbConfig,
        pointer::{CursorImageStatus, CursorImageSurfaceData, Focus},
    },
    output::Output,
    reexports::{
        calloop::{Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecoMode,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_output, wl_surface::WlSurface},
        },
    },
    utils::{Clock, Monotonic, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, get_parent,
            is_sync_subsurface, with_states,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::{PointerConstraintsHandler, PointerConstraintsState},
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
            wlr_data_control::{DataControlHandler, DataControlState},
        },
        shell::{
            wlr_layer::{Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{
                ToplevelSurface, XdgShellState, XdgToplevelSurfaceData,
                decoration::{XdgDecorationHandler, XdgDecorationState},
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
    },
};
use tracing::{info, warn};

use crate::{
    config::Config,
    cursor::CursorState,
    layout::WinId,
    render::elements::Shaders,
    shell::{FeWindow, OutputState, Workspace},
    style::Stylesheet,
    theme::palette::Palette,
    wallpaper::Wallpaper,
};

pub enum Backend {
    None,
    Winit(crate::backend::winit::WinitData),
    Udev(crate::backend::udev::UdevData),
}

impl Backend {
    pub fn renderer(&mut self) -> Option<&mut smithay::backend::renderer::gles::GlesRenderer> {
        match self {
            Backend::Winit(w) => Some(w.backend.renderer()),
            Backend::Udev(u) => u.renderer(),
            Backend::None => None,
        }
    }
    pub fn is_nested(&self) -> bool {
        matches!(self, Backend::Winit(_))
    }
}

pub struct Fe {
    pub dh: DisplayHandle,
    pub lh: LoopHandle<'static, Fe>,
    pub signal: LoopSignal,
    pub socket_name: String,
    pub start: Instant,
    pub clock: Clock<Monotonic>,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Fe>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub data_control_state: DataControlState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub xdg_activation_state: XdgActivationState,
    pub kbd_inhibit_state: KeyboardShortcutsInhibitState,
    pub presentation_state: PresentationState,

    pub seat: Seat<Fe>,
    pub popups: PopupManager,
    pub space: Space<Window>,

    pub cfg: Config,
    pub style: Stylesheet,
    pub css_src: String,
    pub palette: Palette,
    pub wallpaper: Wallpaper,
    pub we: crate::wallpaper_engine::Engine,

    pub windows: HashMap<WinId, FeWindow>,
    pub next_id: WinId,
    pub workspaces: Vec<Workspace>,
    pub outputs: Vec<OutputState>,
    pub focus: Option<WinId>,
    pub focused_output: usize,
    pub layer_focus: Option<WlSurface>,
    pub closing: Vec<crate::closing::Closing>,
    /// window being dragged with the mouse (Super+LMB)
    pub drag: Option<crate::drag::Drag>,
    /// sandbox mode: windows as physical bodies (Super+B)
    pub physics: crate::physics::Physics,
    pub game: Option<crate::game::GameMode>,
    pub layer_states: HashMap<smithay::reexports::wayland_server::backend::ObjectId, (bool, smithay::wayland::shell::wlr_layer::KeyboardInteractivity)>,

    pub cursor: CursorState,
    pub backend: Backend,
    pub shaders: Option<Shaders>,
    pub redraw: bool,
    pub suppressed_keys: Vec<u32>,
    pub inhibit_shortcuts: bool,
    pub binds: Vec<crate::input::Bind>,
    pub wheel_cooldown: HashMap<String, Instant>,
    pub scroll_accum: (f64, f64),
    pub swipe: Option<crate::input::Swipe>,
    pub last_config_error: Option<String>,
    pub xwayland_display: Option<String>,
    pub render_cache: crate::render::RenderCache,
    pub ipc: crate::ipc::Ipc,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub lock: crate::lock::LockState,
    pub capture: crate::capture::CaptureState,
    pub capture_no_cursor: bool,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl Fe {
    pub fn new(display: Display<Fe>, lh: LoopHandle<'static, Fe>, signal: LoopSignal, cfg: Config) -> Fe {
        let dh = display.handle();

        let source = ListeningSocketSource::new_auto().expect("wayland socket");
        let socket_name = source.socket_name().to_string_lossy().into_owned();
        lh.insert_source(source, |stream, _, fe| {
            if let Err(e) = fe.dh.insert_client(stream, Arc::new(ClientState::default())) {
                warn!("клиент не добавлен: {e}");
            }
        })
        .expect("socket source");
        lh.insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, fe| {
            unsafe {
                display.get_mut().dispatch_clients(fe).unwrap();
            }
            Ok(PostAction::Continue)
        })
        .expect("display source");
        info!("слушаю WAYLAND_DISPLAY={socket_name}");

        let clock = Clock::new();
        let compositor_state = CompositorState::new::<Fe>(&dh);
        let xdg_shell_state = XdgShellState::new::<Fe>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Fe>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Fe>(&dh);
        let shm_state = ShmState::new::<Fe>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Fe>(&dh);
        let data_device_state = DataDeviceState::new::<Fe>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Fe>(&dh);
        let data_control_state = DataControlState::new::<Fe, _>(&dh, Some(&primary_selection_state), |_| true);
        let fractional_scale_state = FractionalScaleManagerState::new::<Fe>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Fe>(&dh);
        let kbd_inhibit_state = KeyboardShortcutsInhibitState::new::<Fe>(&dh);
        let presentation_state = PresentationState::new::<Fe>(&dh, clock.id() as u32);
        ViewporterState::new::<Fe>(&dh);
        SinglePixelBufferState::new::<Fe>(&dh);
        RelativePointerManagerState::new::<Fe>(&dh);
        PointerConstraintsState::new::<Fe>(&dh);
        PointerGesturesState::new::<Fe>(&dh);
        smithay::wayland::tablet_manager::TabletManagerState::new::<Fe>(&dh);
        CursorShapeManagerState::new::<Fe>(&dh);

        let dh_c = dh.clone();
        let session_lock_state = smithay::wayland::session_lock::SessionLockManagerState::new::<Fe, _>(&dh, |_| true);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_pointer();
        let xkb = crate::input::xkb_config(&cfg);
        if let Err(e) = seat.add_keyboard(xkb, cfg.input.repeat_delay, cfg.input.repeat_rate) {
            warn!("раскладка {e:?}, беру по умолчанию");
            seat.add_keyboard(XkbConfig::default(), 300, 30).expect("keyboard");
        }

        if cfg.input.numlock {
            if let Some(kb) = seat.get_keyboard() {
                let mut m = kb.modifier_state();
                m.num_lock = true;
                kb.set_modifier_state(m);
            }
        }
        let cursor = CursorState::new(&cfg);
        let binds = crate::input::parse_binds(&cfg);
        let palette = Palette::fallback();
        let css_src = std::fs::read_to_string(crate::config::sync::style_path())
            .unwrap_or_else(|_| crate::style::DEFAULT_CSS.to_string());
        let style = Stylesheet::parse(&css_src, &palette.css_vars());

        Fe {
            dh,
            lh,
            signal,
            socket_name,
            start: Instant::now(),
            clock,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            layer_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            fractional_scale_state,
            xdg_activation_state,
            kbd_inhibit_state,
            presentation_state,
            seat,
            popups: PopupManager::default(),
            space: Space::default(),
            workspaces: Vec::new(),
            cfg,
            style,
            css_src,
            palette,
            wallpaper: Wallpaper::default(),
            we: Default::default(),
            windows: HashMap::new(),
            next_id: 1,
            outputs: Vec::new(),
            focus: None,
            focused_output: 0,
            layer_focus: None,
            closing: Vec::new(),
            drag: None,
            physics: Default::default(),
            game: None,
            layer_states: HashMap::new(),
            cursor,
            backend: Backend::None,
            shaders: None,
            redraw: true,
            suppressed_keys: Vec::new(),
            inhibit_shortcuts: false,
            binds,
            wheel_cooldown: HashMap::new(),
            scroll_accum: (0.0, 0.0),
            swipe: None,
            last_config_error: None,
            xwayland_display: None,
            render_cache: Default::default(),
            ipc: Default::default(),
            session_lock_state,
            lock: Default::default(),
            capture: crate::capture::CaptureState::new(&dh_c),
            capture_no_cursor: false,
        }
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<WinId> {
        self.windows
            .iter()
            .find(|(_, w)| w.window.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .map(|(id, _)| *id)
    }

    pub fn output_for_layer(&self, surface: &WlSurface) -> Option<Output> {
        self.outputs.iter().map(|o| &o.output).find(|o| {
            layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).is_some()
        }).cloned()
    }

    pub fn queue_redraw(&mut self) {
        self.redraw = true;
        if let Backend::Winit(w) = &self.backend {
            w.backend.window().request_redraw();
        }
        if matches!(self.backend, Backend::Udev(_)) {
            crate::backend::udev::schedule_all(self);
        }
    }
}

// ---------------------------------------------------------------- compositor

impl CompositorHandler for Fe {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // a mapped window attaching a null buffer is going away (close or hide):
        // snapshot it for the close animation while its old texture still exists
        if let Some(id) = self.window_for_surface(surface) {
            use smithay::wayland::compositor::BufferAssignment;
            let removed = with_states(surface, |s| {
                matches!(s.cached_state.get::<SurfaceAttributes>().current().buffer, Some(BufferAssignment::Removed))
            });
            if removed && self.windows.get(&id).is_some_and(|w| w.mapped) {
                tracing::debug!("окно {id}: пустой буфер → снимок");
                self.snapshot_closing(id);
            }
        }
        on_commit_buffer_handler::<Self>(surface);
        if let Backend::Udev(u) = &mut self.backend {
            u.early_import(surface);
        }

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(id) = self.window_for_surface(&root) {
                self.windows[&id].window.on_commit();
                if &root == surface {
                    self.toplevel_commit(id);
                }
            }
        }

        // popups
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                let _ = popup.send_configure();
            }
        }

        // layer surfaces
        if let Some(output) = self.output_for_layer(surface) {
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            let mut map = layer_map_for_output(&output);
            let zone_before = map.non_exclusive_zone();
            map.arrange();
            let zone_changed = map.non_exclusive_zone() != zone_before;
            if !initial_sent {
                if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                    layer.layer_surface().send_configure();
                }
            }
            drop(map);
            self.layer_committed(surface);
            if zone_changed || !initial_sent {
                self.relayout_all();
            }
        }

        // cursor surface hotspot
        if matches!(&self.cursor.status, CursorImageStatus::Surface(s) if s == surface) {
            with_states(surface, |states| {
                if let Some(mut attrs) = states.data_map.get::<CursorImageSurfaceData>().map(|a| a.lock().unwrap()) {
                    if let Some(delta) = states.cached_state.get::<SurfaceAttributes>().current().buffer_delta.take() {
                        attrs.hotspot -= delta;
                    }
                }
            });
        }

        self.queue_redraw();
    }
}

impl BufferHandler for Fe {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Fe {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for Fe {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let ok = self.backend.renderer().is_some_and(|r| r.import_dmabuf(&dmabuf, None).is_ok());
        if ok {
            let _ = notifier.successful::<Fe>();
        } else {
            notifier.failed();
        }
    }
}

// ---------------------------------------------------------------- seat & selection

impl SeatHandler for Fe {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Fe> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, seat, client.clone());
        set_primary_focus(&self.dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.status = image;
        self.queue_redraw();
    }
}

impl smithay::input::tablet::TabletSeatHandler for Fe {
    type ToolFocus = WlSurface;
    fn tablet_tool_image(&mut self, _tool: &smithay::backend::input::TabletToolDescriptor, image: CursorImageStatus) {
        self.cursor.status = image;
    }
}

impl SelectionHandler for Fe {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Fe {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for Fe {}

impl WaylandDndGrabHandler for Fe {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let ptr = seat.get_pointer().unwrap();
                if let Some(start) = ptr.grab_start_data() {
                    let grab = DnDGrab::new_pointer(&self.dh, start, source, seat);
                    ptr.set_grab(self, grab, serial, Focus::Keep);
                }
            }
            GrabType::Touch => source.cancel(),
        }
    }
}

impl PrimarySelectionHandler for Fe {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl DataControlHandler for Fe {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

impl OutputHandler for Fe {}

impl PointerConstraintsHandler for Fe {
    fn new_constraint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) {
    }
    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &smithay::input::pointer::PointerHandle<Self>,
        _location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
    }
}

impl KeyboardShortcutsInhibitHandler for Fe {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.kbd_inhibit_state
    }
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}

impl FractionalScaleHandler for Fe {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .outputs
            .first()
            .map(|o| o.output.current_scale().fractional_scale())
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
        });
    }
}

impl XdgActivationHandler for Fe {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, _data: XdgActivationTokenData) -> bool {
        true
    }

    fn request_activation(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        if data.timestamp.elapsed().as_secs() < 10 {
            if let Some(id) = self.window_for_surface(&surface) {
                self.focus_window(Some(id), true);
            }
        } else if let Some(id) = self.window_for_surface(&surface) {
            if let Some(w) = self.windows.get_mut(&id) {
                w.urgent = true;
            }
            self.queue_redraw();
        }
    }
}

impl XdgDecorationHandler for Fe {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecoMode) {
        // FeWM always draws its own borders.
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

// ---------------------------------------------------------------- layer shell

impl WlrLayerShellHandler for Fe {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.outputs.get(self.focused_output).map(|o| o.output.clone()))
            .or_else(|| self.outputs.first().map(|o| o.output.clone()));
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(e) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            warn!("layer: {e}");
        }
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: smithay::wayland::shell::xdg::PopupSurface) {
        let _ = self.popups.track_popup(PopupKind::Xdg(popup));
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for o in self.outputs.iter().map(|o| o.output.clone()).collect::<Vec<_>>() {
            let mut map = layer_map_for_output(&o);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        self.layer_states.remove(&surface.wl_surface().id());
        if self.layer_focus.as_ref() == Some(surface.wl_surface()) {
            self.layer_focus = None;
            let f = self.focus;
            self.focus_window(f, false);
        }
        self.relayout_all();
    }
}

impl Fe {
    /// Give keyboard focus to layer surfaces that ask for exclusive focus.
    fn layer_committed(&mut self, surface: &WlSurface) {
        use smithay::backend::renderer::utils::with_renderer_surface_state;
        use smithay::wayland::shell::wlr_layer::KeyboardInteractivity as KI;
        let Some(output) = self.output_for_layer(surface) else { return };
        let map = layer_map_for_output(&output);
        let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() else { return };
        drop(map);
        let data = layer.cached_state();
        let ki = data.keyboard_interactivity;
        let mapped = with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false);
        let (was_mapped, was_ki) = self.layer_states.get(&surface.id()).copied().unwrap_or((false, KI::None));
        self.layer_states.insert(surface.id(), (mapped, ki));

        let top = matches!(data.layer, Layer::Top | Layer::Overlay);
        let focused = self.layer_focus.as_ref() == Some(surface);
        // Exclusive: always while shown. OnDemand (launchers, quickshell `focusable`):
        // when it appears or starts asking for focus, like niri/Hyprland do.
        let grab = mapped
            && top
            && match ki {
                KI::Exclusive => true,
                KI::OnDemand => !was_mapped || was_ki == KI::None,
                KI::None => false,
            };
        let release = focused && (!mapped || ki == KI::None);
        if grab && !focused {
            self.layer_focus = Some(surface.clone());
            let kb = self.seat.get_keyboard().unwrap();
            kb.set_focus(self, Some(surface.clone()), smithay::utils::SERIAL_COUNTER.next_serial());
        } else if release {
            self.layer_focus = None;
            let f = self.focus;
            self.focus = None;
            self.focus_window(f, false);
        }
    }
}

pub fn toplevel_app_id_title(t: &ToplevelSurface) -> (String, String) {
    with_states(t.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|d| {
                let d = d.lock().unwrap();
                (d.app_id.clone().unwrap_or_default(), d.title.clone().unwrap_or_default())
            })
            .unwrap_or_default()
    })
}

delegate_dispatch2!(Fe);
