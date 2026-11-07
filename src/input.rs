//! Input: keybindings, pointer, scrolling, gestures, interactive move/resize.

use std::time::{Duration, Instant};

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, InputTime, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent,
        GestureSwipeUpdateEvent, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent,
        PointerButtonEvent, PointerMotionEvent,
    },
    desktop::{WindowSurfaceType, layer_map_for_output},
    input::{
        keyboard::{FilterResult, ModifiersState, XkbConfig, keysyms},
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent as PtrSwipeUpdate, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
            RelativeMotionEvent,
        },
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer,
};
use tracing::warn;
use xkbcommon::xkb;

use crate::{
    anim::RectF,
    config::Config,
    layout::{Ctx, WinId},
    state::Fe,
};

pub fn xkb_config(cfg: &Config) -> XkbConfig<'_> {
    XkbConfig {
        rules: "",
        model: &cfg.input.kb_model,
        layout: &cfg.input.kb_layout,
        variant: &cfg.input.kb_variant,
        options: if cfg.input.kb_options.is_empty() { None } else { Some(cfg.input.kb_options.clone()) },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    fn from_state(m: &ModifiersState) -> Self {
        Mods { logo: m.logo, ctrl: m.ctrl, alt: m.alt, shift: m.shift }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Key(u32),
    Button(u32),
    Wheel(&'static str),
}

#[derive(Debug, Clone)]
pub struct Bind {
    pub text: String,
    pub mods: Mods,
    pub trigger: Trigger,
    pub action: String,
}

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

fn parse_combo(s: &str) -> Option<(Mods, String)> {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let (key, mods) = parts.split_last()?;
    let mut m = Mods::default();
    for p in mods {
        match p.to_ascii_uppercase().as_str() {
            "SUPER" | "MOD" | "WIN" | "LOGO" | "MOD4" | "META" => m.logo = true,
            "CTRL" | "CONTROL" => m.ctrl = true,
            "ALT" | "MOD1" => m.alt = true,
            "SHIFT" => m.shift = true,
            _ => return None,
        }
    }
    Some((m, key.to_string()))
}

fn keysym_from(name: &str) -> Option<u32> {
    let k = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
    let k = if k.raw() == keysyms::KEY_NoSymbol {
        xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE)
    } else {
        k
    };
    if k.raw() == keysyms::KEY_NoSymbol {
        return None;
    }
    Some(lower(k.raw()))
}

/// Lowercase a keysym (latin and cyrillic ranges are what matter for binds).
fn lower(k: u32) -> u32 {
    match k {
        0x41..=0x5a => k + 0x20,
        0xc0..=0xde if k != 0xd7 => k + 0x20,
        0x6e0..=0x6ff => k - 0x20,
        0x6b1..=0x6bf => k - 0x10,
        _ => k,
    }
}

pub fn parse_binds(cfg: &Config) -> Vec<Bind> {
    let mut out = Vec::new();
    for (combo, action) in &cfg.binds {
        let Some((mods, key)) = parse_combo(combo) else {
            warn!("бинд {combo:?}: не понял модификаторы");
            continue;
        };
        let Some(sym) = keysym_from(&key) else {
            warn!("бинд {combo:?}: нет клавиши {key:?}");
            continue;
        };
        out.push(Bind { text: combo.clone(), mods, trigger: Trigger::Key(sym), action: action.clone() });
    }
    for (combo, action) in &cfg.mouse_binds {
        let Some((mods, key)) = parse_combo(combo) else { continue };
        let trigger = match key.to_ascii_lowercase().as_str() {
            "mouse_left" | "mouse:272" | "left" => Trigger::Button(BTN_LEFT),
            "mouse_right" | "mouse:273" | "right" => Trigger::Button(BTN_RIGHT),
            "mouse_middle" | "mouse:274" | "middle" => Trigger::Button(BTN_MIDDLE),
            "wheel_up" | "mouse_up" => Trigger::Wheel("up"),
            "wheel_down" | "mouse_down" => Trigger::Wheel("down"),
            "wheel_left" => Trigger::Wheel("left"),
            "wheel_right" => Trigger::Wheel("right"),
            _ => {
                warn!("мышь {combo:?}: неизвестная кнопка");
                continue;
            }
        };
        out.push(Bind { text: combo.clone(), mods, trigger, action: action.clone() });
    }
    out
}

pub fn switch_kb_layout(fe: &mut Fe, arg: &str) {
    let kb = fe.seat.get_keyboard().unwrap();
    kb.with_xkb_state(fe, |mut ctx| {
        let n = ctx.xkb().lock().unwrap().layouts().count() as u32;
        if n == 0 {
            return;
        }
        let cur = ctx.xkb().lock().unwrap().active_layout().0;
        let next = match arg {
            "prev" => (cur + n - 1) % n,
            "" | "next" => (cur + 1) % n,
            s => s.parse::<u32>().unwrap_or(cur) % n,
        };
        ctx.set_layout(smithay::input::keyboard::Layout(next));
    });
}

pub struct Swipe {
    pub fingers: u32,
    pub dx: f64,
    pub dy: f64,
    pub axis: Option<bool>, // Some(true) horizontal
    pub start_ws: u32,
}

impl Fe {
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let oi = self.output_at(pos)?;
        if self.lock.locked {
            let og = self.output_geo(oi)?;
            let ls = self.lock.surface_for(&self.outputs[oi].output)?;
            return Some((ls.wl_surface().clone(), og.loc.to_f64()));
        }
        let output = &self.outputs[oi].output;
        let og = self.output_geo(oi)?;
        let local = pos - og.loc.to_f64();
        let map = layer_map_for_output(output);
        let layer_hit = |layer: Layer| {
            map.layer_under(layer, local).and_then(|l| {
                let lg = map.layer_geometry(l)?;
                l.surface_under(local - lg.loc.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + lg.loc + og.loc).to_f64()))
            })
        };
        if let Some(h) = layer_hit(Layer::Overlay) {
            return Some(h);
        }
        let fullscreen = self.windows.values().any(|w| w.ws == self.outputs[oi].ws && w.fullscreen);
        if !fullscreen {
            if let Some(h) = layer_hit(Layer::Top) {
                return Some(h);
            }
        }
        for id in self.stacking(self.outputs[oi].ws).into_iter().rev() {
            let Some((_, r)) = self.window_visual_rect(id) else { continue };
            let w = &self.windows[&id];
            let geo = w.window.geometry();
            let origin = Point::<f64, Logical>::from((og.loc.x as f64 + r.x - geo.loc.x as f64, og.loc.y as f64 + r.y - geo.loc.y as f64));
            if let Some((s, p)) = w.window.surface_under(pos - origin, WindowSurfaceType::ALL) {
                return Some((s, p.to_f64() + origin));
            }
        }
        if let Some(h) = layer_hit(Layer::Bottom) {
            return Some(h);
        }
        layer_hit(Layer::Background)
    }

    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<WinId> {
        let oi = self.output_at(pos)?;
        let og = self.output_geo(oi)?;
        let (x, y) = (pos.x - og.loc.x as f64, pos.y - og.loc.y as f64);
        self.stacking(self.outputs[oi].ws).into_iter().rev().find(|id| {
            self.window_visual_rect(*id).is_some_and(|(_, r)| r.contains(x, y))
        })
    }

    /// Bottom → top render/hit order for a workspace.
    pub fn stacking(&self, ws: u32) -> Vec<WinId> {
        let Some(wsp) = self.workspaces.get(ws as usize - 1) else { return Vec::new() };
        let mut out: Vec<WinId> = wsp
            .layout
            .windows()
            .into_iter()
            .filter(|id| self.windows.get(id).is_some_and(|w| w.mapped && !w.fullscreen))
            .collect();
        // focused tiled window above other tiled (for animations overlapping)
        if let Some(f) = self.focus {
            if let Some(i) = out.iter().position(|x| *x == f) {
                let x = out.remove(i);
                out.push(x);
            }
        }
        out.extend(wsp.float_stack.iter().copied().filter(|id| self.windows.get(id).is_some_and(|w| w.mapped && !w.fullscreen)));
        out.extend(self.windows.values().filter(|w| w.ws == ws && w.mapped && w.fullscreen).map(|w| w.id));
        // the window held by the mouse is above everything
        if let Some(d) = self.dragged_window() {
            if let Some(i) = out.iter().position(|x| *x == d) {
                let x = out.remove(i);
                out.push(x);
            }
        }
        out
    }

    pub fn output_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        (0..self.outputs.len()).find(|i| self.output_geo(*i).is_some_and(|g| g.to_f64().contains(pos))).or(
            if self.outputs.is_empty() { None } else { Some(self.focused_output.min(self.outputs.len() - 1)) },
        )
    }

    fn clamp_pointer(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        if (0..self.outputs.len()).any(|i| self.output_geo(i).is_some_and(|g| g.to_f64().contains(pos))) {
            return pos;
        }
        let Some(g) = self.output_geo(self.output_at(pos).unwrap_or(0)) else { return pos };
        (
            pos.x.clamp(g.loc.x as f64, (g.loc.x + g.size.w) as f64 - 1.0),
            pos.y.clamp(g.loc.y as f64, (g.loc.y + g.size.h) as f64 - 1.0),
        )
            .into()
    }

    fn find_bind(&self, mods: Mods, trigger: &Trigger) -> Option<String> {
        let exact = self.binds.iter().find(|b| b.mods == mods && &b.trigger == trigger).map(|b| b.action.clone());
        if exact.is_some() {
            return exact;
        }
        // Nested in another compositor the host eats Super combos: Alt acts as Super (like niri).
        if self.backend.is_nested() && mods.alt && !mods.logo {
            let swapped = Mods { logo: true, alt: false, ..mods };
            return self.binds.iter().find(|b| b.mods == swapped && &b.trigger == trigger).map(|b| b.action.clone());
        }
        None
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => self.on_key(event.key_code().raw(), event.state(), event.time()),
            InputEvent::PointerMotion { event } => {
                let ptr = self.seat.get_pointer().unwrap();
                let pos = self.clamp_pointer(ptr.current_location() + event.delta());
                self.pointer_moved(pos, event.time());
                let under = self.surface_under(pos);
                ptr.relative_motion(
                    self,
                    under,
                    &RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        time: event.time(),
                    },
                );
                ptr.frame(self);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(g) = self.output_geo(0) else { return };
                let pos = event.position_transformed(g.size) + g.loc.to_f64();
                self.pointer_moved(pos, event.time());
                self.seat.get_pointer().unwrap().frame(self);
            }
            InputEvent::PointerButton { event } => self.on_button(event.button_code(), event.state(), event.time()),
            InputEvent::PointerAxis { event } => self.on_axis::<I>(&event),
            InputEvent::GestureSwipeBegin { event } => self.on_swipe_begin(event.fingers(), event.time()),
            InputEvent::GestureSwipeUpdate { event } => self.on_swipe_update(event.delta_x(), event.delta_y(), event.time()),
            InputEvent::GestureSwipeEnd { event } => self.on_swipe_end(event.cancelled(), event.time()),
            InputEvent::GesturePinchBegin { event } => {
                let ptr = self.seat.get_pointer().unwrap();
                ptr.gesture_pinch_begin(
                    self,
                    &GesturePinchBeginEvent { serial: SERIAL_COUNTER.next_serial(), time: event.time(), fingers: event.fingers() },
                );
            }
            InputEvent::GesturePinchUpdate { event } => {
                use smithay::backend::input::GesturePinchUpdateEvent as _;
                let ptr = self.seat.get_pointer().unwrap();
                ptr.gesture_pinch_update(
                    self,
                    &GesturePinchUpdateEvent {
                        time: event.time(),
                        delta: event.delta(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    },
                );
            }
            InputEvent::GesturePinchEnd { event } => {
                let ptr = self.seat.get_pointer().unwrap();
                ptr.gesture_pinch_end(
                    self,
                    &GesturePinchEndEvent { serial: SERIAL_COUNTER.next_serial(), time: event.time(), cancelled: event.cancelled() },
                );
            }
            InputEvent::GestureHoldBegin { event } => {
                let ptr = self.seat.get_pointer().unwrap();
                ptr.gesture_hold_begin(
                    self,
                    &GestureHoldBeginEvent { serial: SERIAL_COUNTER.next_serial(), time: event.time(), fingers: event.fingers() },
                );
            }
            InputEvent::GestureHoldEnd { event } => {
                let ptr = self.seat.get_pointer().unwrap();
                ptr.gesture_hold_end(
                    self,
                    &GestureHoldEndEvent { serial: SERIAL_COUNTER.next_serial(), time: event.time(), cancelled: event.cancelled() },
                );
            }
            InputEvent::DeviceAdded { device } => crate::backend::udev::configure_device(self, device),
            _ => {}
        }
    }

    fn on_key(&mut self, code: u32, state: KeyState, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let kb = self.seat.get_keyboard().unwrap();
        let inhibited = self.inhibit_shortcuts;
        let action = kb.input::<Option<String>, _>(self, code.into(), state, serial, time, |fe, mods, handle| {
            if state == KeyState::Released {
                if let Some(i) = fe.suppressed_keys.iter().position(|k| *k == code) {
                    fe.suppressed_keys.remove(i);
                    return FilterResult::Intercept(None);
                }
                return FilterResult::Forward;
            }
            // VT switching on a real session
            let sym = handle.modified_sym().raw();
            if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
                fe.suppressed_keys.push(code);
                let vt = (sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32;
                return FilterResult::Intercept(Some(format!("__vt {vt}")));
            }
            if inhibited || fe.lock.locked {
                return FilterResult::Forward;
            }
            let m = Mods::from_state(mods);
            // layout-independent: try the raw latin keysym first, then the current one
            let raw = handle.raw_latin_sym_or_raw_current_sym().map(|k| lower(k.raw()));
            let mut cands: Vec<u32> = raw.into_iter().collect();
            cands.extend(handle.raw_syms().iter().map(|k| lower(k.raw())));
            for sym in cands {
                if let Some(a) = fe.find_bind(m, &Trigger::Key(sym)) {
                    fe.suppressed_keys.push(code);
                    return FilterResult::Intercept(Some(a));
                }
            }
            FilterResult::Forward
        });
        if let Some(Some(a)) = action {
            if let Some(vt) = a.strip_prefix("__vt ") {
                crate::backend::udev::change_vt(self, vt.parse().unwrap_or(1));
            } else {
                self.run_action(&a);
            }
        }
    }

    fn mods(&self) -> Mods {
        self.seat.get_keyboard().map(|k| Mods::from_state(&k.modifier_state())).unwrap_or_default()
    }

    fn pointer_moved(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let ptr = self.seat.get_pointer().unwrap();
        let under = self.surface_under(pos);
        if under.is_none() && !matches!(self.cursor.status, smithay::input::pointer::CursorImageStatus::Named(_)) {
            self.cursor.status = smithay::input::pointer::CursorImageStatus::default_named();
        }
        if let Some(oi) = self.output_at(pos) {
            self.focused_output = oi;
        }
        if self.cfg.general.focus_follows_mouse && !ptr.is_grabbed() && self.layer_focus.is_none() && !self.lock.locked {
            if let Some(id) = self.window_under(pos) {
                if Some(id) != self.focus && self.windows.get(&id).is_some_and(|w| w.ws == self.active_ws()) {
                    // don't scroll the scroller view when focusing by mouse
                    self.focus_by_mouse(id);
                }
            }
        }
        ptr.motion(self, under, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        self.cursor.last_move = Instant::now();
        self.queue_redraw();
    }

    fn focus_by_mouse(&mut self, id: WinId) {
        let old = self.focus;
        self.focus = Some(id);
        for w in self.windows.values() {
            if w.window.set_activated(Some(w.id) == self.focus) {
                if let Some(t) = w.window.toplevel() {
                    if t.is_initial_configure_sent() {
                        t.send_pending_configure();
                    }
                }
            }
        }
        let ws = self.windows[&id].ws;
        let wsp = &mut self.workspaces[ws as usize - 1];
        wsp.focus_hist.retain(|x| *x != id);
        wsp.focus_hist.push(id);
        if self.windows[&id].floating {
            wsp.float_stack.retain(|x| *x != id);
            wsp.float_stack.push(id);
        }
        let s = self.windows[&id].window.toplevel().map(|t| t.wl_surface().clone());
        let kb = self.seat.get_keyboard().unwrap();
        kb.set_focus(self, s, SERIAL_COUNTER.next_serial());
        // animate border colors
        let bp = self.anim(&self.cfg.animations.border_color);
        let now = Instant::now();
        for wid in [old, Some(id)].into_iter().flatten() {
            let after = self.win_style(wid).border_color;
            let prev = self.focus;
            self.focus = if Some(wid) == Some(id) { old } else { Some(id) };
            let before = self.win_style(wid).border_color;
            self.focus = prev;
            if let Some(w) = self.windows.get_mut(&wid) {
                w.border = Some(crate::shell::BorderAnim { from: before, to: after, prog: Some(crate::anim::Progress::new(bp, now)) });
            }
        }
        crate::ipc::event(self, "focus-changed");
    }

    fn on_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let ptr = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        let pos = ptr.current_location();
        if state == ButtonState::Pressed && self.lock.locked {
            if let Some((s, _)) = self.surface_under(pos) {
                let kb = self.seat.get_keyboard().unwrap();
                kb.set_focus(self, Some(s), serial);
            }
        } else if state == ButtonState::Pressed && !ptr.is_grabbed() {
            let mods = self.mods();
            if mods != Mods::default() {
                if let Some(action) = self.find_bind(mods, &Trigger::Button(button)) {
                    if let Some(id) = self.window_under(pos) {
                        self.focus_window(Some(id), false);
                        let kind = match action.as_str() {
                            "move" | "movewindow" => Some(GrabKind::Move),
                            "resize" | "resizewindow" => Some(GrabKind::Resize),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            let start = GrabStartData { focus: None, button, location: pos };
                            let grab = WinGrab::new(self, id, kind, start);
                            ptr.set_grab(self, grab, serial, smithay::input::pointer::Focus::Clear);
                            return;
                        }
                    }
                    self.run_action(&action);
                    return;
                }
            }
            // click focus
            if let Some(id) = self.window_under(pos) {
                if self.focus != Some(id) {
                    self.focus_window(Some(id), false);
                }
                if self.layer_focus.is_some() {
                    // clicking a window closes on-demand layer focus
                    self.layer_focus = None;
                    self.focus_window(Some(id), false);
                }
            } else if let Some((surface, _)) = self.surface_under(pos) {
                // layer surfaces with on-demand keyboard interactivity
                if let Some(output) = self.output_for_layer(&surface) {
                    let map = layer_map_for_output(&output);
                    if let Some(l) = map.layer_for_surface(&surface, WindowSurfaceType::TOPLEVEL) {
                        if l.can_receive_keyboard_focus() {
                            let s = l.wl_surface().clone();
                            drop(map);
                            self.layer_focus = Some(s.clone());
                            let kb = self.seat.get_keyboard().unwrap();
                            kb.set_focus(self, Some(s), serial);
                        }
                    }
                }
            }
        }
        ptr.button(self, &ButtonEvent { button, state, serial, time });
        ptr.frame(self);
    }

    fn on_axis<I: InputBackend>(&mut self, event: &I::PointerAxisEvent) {
        let source = event.source();
        let factor = if source == AxisSource::Finger { self.cfg.input.touchpad_scroll_factor } else { self.cfg.input.scroll_factor };
        let h = event.amount(Axis::Horizontal).unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0) * factor;
        let v = event.amount(Axis::Vertical).unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0) * factor;
        let mods = self.mods();

        if mods != Mods::default() && !self.lock.locked {
            // wheel binds (with cooldown, like niri's cooldown-ms=150)
            let dir = if v > 0.0 {
                Some("down")
            } else if v < 0.0 {
                Some("up")
            } else if h > 0.0 {
                Some("right")
            } else if h < 0.0 {
                Some("left")
            } else {
                None
            };
            if let Some(d) = dir {
                if let Some(action) = self.find_bind(mods, &Trigger::Wheel(d)) {
                    let key = format!("{mods:?}{d}");
                    let now = Instant::now();
                    let continuous = source == AxisSource::Finger;
                    let (ax, ay) = &mut self.scroll_accum;
                    if continuous {
                        *ax += h;
                        *ay += v;
                        if ax.abs() < 30.0 && ay.abs() < 30.0 {
                            return;
                        }
                        *ax = 0.0;
                        *ay = 0.0;
                    }
                    let is_ws = action.contains("workspace");
                    let cool = if is_ws { Duration::from_millis(150) } else { Duration::from_millis(0) };
                    if self.wheel_cooldown.get(&key).is_some_and(|t| now.duration_since(*t) < cool) {
                        return;
                    }
                    self.wheel_cooldown.insert(key, now);
                    // scrolling actions in the scroller follow the scroll amount
                    if action.starts_with("scroll") && continuous {
                        let px = if h != 0.0 { h } else { v };
                        self.scroll_view(px * self.cfg.scroller.scroll_speed, false);
                    } else {
                        self.run_action(&action);
                    }
                    return;
                }
            }
        }

        let ptr = self.seat.get_pointer().unwrap();
        let mut frame = AxisFrame::new(event.time()).source(source);
        if h != 0.0 {
            frame = frame.value(Axis::Horizontal, h);
            if let Some(d) = event.amount_v120(Axis::Horizontal) {
                frame = frame.v120(Axis::Horizontal, d as i32);
            }
        }
        if v != 0.0 {
            frame = frame.value(Axis::Vertical, v);
            if let Some(d) = event.amount_v120(Axis::Vertical) {
                frame = frame.v120(Axis::Vertical, d as i32);
            }
        }
        if source == AxisSource::Finger {
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
        }
        ptr.axis(self, frame);
        ptr.frame(self);
    }

    fn gesture_is_ours(&self, fingers: u32) -> bool {
        let g = &self.cfg.gestures;
        (g.workspace_swipe && fingers == g.workspace_swipe_fingers) || fingers == g.scroller_swipe_fingers
    }

    fn on_swipe_begin(&mut self, fingers: u32, time: InputTime) {
        if self.gesture_is_ours(fingers) {
            self.swipe = Some(Swipe { fingers, dx: 0.0, dy: 0.0, axis: None, start_ws: self.active_ws() });
            return;
        }
        let ptr = self.seat.get_pointer().unwrap();
        ptr.gesture_swipe_begin(self, &GestureSwipeBeginEvent { serial: SERIAL_COUNTER.next_serial(), time, fingers });
    }

    fn on_swipe_update(&mut self, dx: f64, dy: f64, time: InputTime) {
        if let Some(mut s) = self.swipe.take() {
            let inv = if self.cfg.gestures.swipe_invert { -1.0 } else { 1.0 };
            s.dx += dx;
            s.dy += dy;
            if s.axis.is_none() && (s.dx.abs() > 8.0 || s.dy.abs() > 8.0) {
                s.axis = Some(s.dx.abs() >= s.dy.abs());
            }
            let scroller = self.workspaces.get(self.active_ws() as usize - 1).is_some_and(|w| w.layout.name() == "scroller");
            match s.axis {
                Some(horizontal) if scroller && s.fingers == self.cfg.gestures.scroller_swipe_fingers
                    && horizontal == (self.cfg.scroller.direction != "vertical") && self.cfg.scroller.touchpad_scrolls_view =>
                {
                    let d = if horizontal { dx } else { dy };
                    self.scroll_view(-d * inv * 1.5 * self.cfg.scroller.scroll_speed, false);
                }
                _ => {}
            }
            self.swipe = Some(s);
            return;
        }
        let ptr = self.seat.get_pointer().unwrap();
        ptr.gesture_swipe_update(self, &PtrSwipeUpdate { time, delta: (dx, dy).into() });
    }

    fn on_swipe_end(&mut self, cancelled: bool, time: InputTime) {
        if let Some(s) = self.swipe.take() {
            let scroller = self.workspaces.get(self.active_ws() as usize - 1).is_some_and(|w| w.layout.name() == "scroller");
            let horizontal_scroll = self.cfg.scroller.direction != "vertical";
            if scroller && s.axis == Some(horizontal_scroll) && s.fingers == self.cfg.gestures.scroller_swipe_fingers && self.cfg.scroller.touchpad_scrolls_view {
                self.scroll_view(0.0, true);
                return;
            }
            if !cancelled && self.cfg.gestures.workspace_swipe && s.fingers == self.cfg.gestures.workspace_swipe_fingers {
                let vertical = self.cfg.gestures.workspace_swipe_vertical;
                let d = if vertical { s.dy } else { s.dx };
                let inv = if self.cfg.gestures.swipe_invert { -1.0 } else { 1.0 };
                if d.abs() > self.cfg.gestures.workspace_swipe_distance * 0.25 {
                    let step = if d * inv < 0.0 { 1 } else { -1 };
                    let n = self.cfg.general.workspaces.max(1) as i32;
                    let t = (s.start_ws as i32 + step).clamp(1, n) as u32;
                    self.switch_workspace(t);
                }
            }
            return;
        }
        let ptr = self.seat.get_pointer().unwrap();
        ptr.gesture_swipe_end(self, &GestureSwipeEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled });
    }
}

// ---------------------------------------------------------------- interactive grabs

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GrabKind {
    Move,
    Resize,
}

pub struct WinGrab {
    start: GrabStartData<Fe>,
    id: WinId,
    kind: GrabKind,
    start_rect: RectF,
    last: Point<f64, Logical>,
    edge_left: bool,
    edge_top: bool,
}

impl WinGrab {
    pub fn new(fe: &mut Fe, id: WinId, kind: GrabKind, start: GrabStartData<Fe>) -> Self {
        let w = &fe.windows[&id];
        let start_rect = if w.floating { w.float_rect } else { fe.window_visual_rect(id).map(|(_, r)| r).unwrap_or_default() };
        let og = fe.output_of_ws(w.ws).and_then(|i| fe.output_geo(i)).map(|g| g.loc.to_f64()).unwrap_or_default();
        let lx = start.location.x - og.x;
        let ly = start.location.y - og.y;
        let (cx, cy) = start_rect.center();
        let last = start.location;
        if kind == GrabKind::Move {
            fe.drag = Some(crate::drag::Drag::new(id, start_rect, og.into(), start.location, !w.floating));
        }
        Self { start, id, kind, start_rect, last, edge_left: lx < cx, edge_top: ly < cy }
    }
}

impl PointerGrab<Fe> for WinGrab {
    fn motion(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, _focus: Option<(WlSurface, Point<f64, Logical>)>, event: &MotionEvent) {
        handle.motion(fe, None, event);
        let Some(w) = fe.windows.get(&self.id) else { return };
        let ws = w.ws;
        let dx = event.location.x - self.start.location.x;
        let dy = event.location.y - self.start.location.y;
        if w.floating {
            let w = fe.windows.get_mut(&self.id).unwrap();
            let s = self.start_rect;
            match self.kind {
                GrabKind::Move => {
                    w.float_rect.x = s.x + dx;
                    w.float_rect.y = s.y + dy;
                }
                GrabKind::Resize => {
                    let (nw, nx) = if self.edge_left { ((s.w - dx).max(80.0), s.x + s.w - (s.w - dx).max(80.0)) } else { ((s.w + dx).max(80.0), s.x) };
                    let (nh, ny) = if self.edge_top { ((s.h - dy).max(60.0), s.y + s.h - (s.h - dy).max(60.0)) } else { ((s.h + dy).max(60.0), s.y) };
                    w.float_rect = RectF::new(nx, ny, nw, nh);
                }
            }
            let r = w.float_rect;
            w.rect.jump(r);
            fe.relayout(ws);
            if self.kind == GrabKind::Move {
                fe.drag_motion(event.location);
            }
        } else if self.kind == GrabKind::Move {
            // tiled: the window follows the cursor; dropped onto another window = swap
            fe.drag_motion(event.location);
        } else if self.kind == GrabKind::Resize {
            let (area, gap) = fe.ws_area(ws);
            let ddx = (event.location.x - self.last.x) / area.w.max(1.0);
            let ddy = (event.location.y - self.last.y) / area.h.max(1.0);
            let ddx = if self.edge_left { -ddx } else { ddx };
            let ddy = if self.edge_top { -ddy } else { ddy };
            let ctx = Ctx { cfg: &fe.cfg, area, gap };
            fe.workspaces[ws as usize - 1].layout.resize(&ctx, self.id, ddx, ddy);
            fe.relayout(ws);
        }
        self.last = event.location;
        fe.queue_redraw();
    }

    fn relative_motion(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, _focus: Option<(WlSurface, Point<f64, Logical>)>, event: &RelativeMotionEvent) {
        handle.relative_motion(fe, None, event);
    }

    fn button(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &ButtonEvent) {
        handle.button(fe, event);
        if handle.current_pressed().is_empty() {
            // tiled move: drop onto another window = swap, then glide into place
            if self.kind == GrabKind::Move && fe.dragged_window() == Some(self.id) {
                fe.drag_drop();
            }
            handle.unset_grab(self, fe, event.serial, event.time, true);
        }
    }

    fn axis(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, details: AxisFrame) {
        handle.axis(fe, details)
    }
    fn frame(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>) {
        handle.frame(fe);
    }
    fn gesture_swipe_begin(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GestureSwipeBeginEvent) {
        handle.gesture_swipe_begin(fe, event)
    }
    fn gesture_swipe_update(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &PtrSwipeUpdate) {
        handle.gesture_swipe_update(fe, event)
    }
    fn gesture_swipe_end(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GestureSwipeEndEvent) {
        handle.gesture_swipe_end(fe, event)
    }
    fn gesture_pinch_begin(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GesturePinchBeginEvent) {
        handle.gesture_pinch_begin(fe, event)
    }
    fn gesture_pinch_update(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GesturePinchUpdateEvent) {
        handle.gesture_pinch_update(fe, event)
    }
    fn gesture_pinch_end(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GesturePinchEndEvent) {
        handle.gesture_pinch_end(fe, event)
    }
    fn gesture_hold_begin(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GestureHoldBeginEvent) {
        handle.gesture_hold_begin(fe, event)
    }
    fn gesture_hold_end(&mut self, fe: &mut Fe, handle: &mut PointerInnerHandle<'_, Fe>, event: &GestureHoldEndEvent) {
        handle.gesture_hold_end(fe, event)
    }
    fn start_data(&self) -> &GrabStartData<Fe> {
        &self.start
    }
    fn unset(&mut self, fe: &mut Fe) {
        if fe.dragged_window() == Some(self.id) {
            fe.drag = None;
            fe.queue_redraw();
        }
    }
}
