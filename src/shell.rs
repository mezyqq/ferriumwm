//! Windows, workspaces, focus and actions.

use std::time::Instant;

use regex::Regex;
use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::{PopupKind, Window, find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output},
    input::{Seat, pointer::Focus},
    output::Output,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::{wl_output, wl_seat},
    },
    utils::{Rectangle, SERIAL_COUNTER, Serial},
    wayland::{
        compositor::with_states,
        shell::xdg::{PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler, XdgShellState},
    },
};
use tracing::{info, warn};

use crate::{
    anim::{AnimF64, AnimParams, AnimRect, Progress, RectF},
    config::WindowRule,
    layout::{self, CmdResult, Ctx, Dir, Layout, WinId},
    state::{Fe, toplevel_app_id_title},
    style::{Paint, WinQuery, WinStyle},
};

#[derive(Debug, Default, Clone)]
pub struct RuleFx {
    pub opacity: Option<f64>,
    pub no_border: bool,
    pub no_shadow: bool,
    pub no_anim: bool,
    pub blur: Option<bool>,
}

pub struct BorderAnim {
    pub from: Paint,
    pub to: Paint,
    pub prog: Option<Progress>,
}

pub struct FeWindow {
    pub id: WinId,
    pub window: Window,
    pub ws: u32,
    pub mapped: bool,
    pub floating: bool,
    pub float_rect: RectF,
    pub fullscreen: bool,
    pub maximized: bool,
    pub urgent: bool,
    pub rect: AnimRect,
    pub open: Option<Progress>,
    pub rule: RuleFx,
    pub app_id: String,
    pub title: String,
    pub border: Option<BorderAnim>,
    pub pending_size: Option<(i32, i32)>,
    /// elements of the last shown frame (keep textures alive for the close animation)
    pub last_frame: Option<crate::closing::LastFrame>,
}

pub struct WsSwitch {
    pub from: u32,
    pub prog: Progress,
    pub dir: f64,
    pub style: String,
}

pub struct OutputState {
    pub output: Output,
    pub ws: u32,
    pub switch: Option<WsSwitch>,
}

pub struct Workspace {
    pub id: u32,
    pub layout: Box<dyn Layout>,
    pub focus_hist: Vec<WinId>,
    pub float_stack: Vec<WinId>,
    pub view_x: AnimF64,
    pub view_y: AnimF64,
}

impl Workspace {
    pub fn new(id: u32, layout_name: &str, cfg: &crate::config::Config) -> Self {
        Self {
            id,
            layout: layout::make(layout_name, cfg),
            focus_hist: Vec::new(),
            float_stack: Vec::new(),
            view_x: AnimF64::new(0.0),
            view_y: AnimF64::new(0.0),
        }
    }
}

fn regex_match(pat: &str, v: &str) -> bool {
    pat.is_empty() || Regex::new(pat).map(|r| r.is_match(v)).unwrap_or(false)
}

fn rule_matches(r: &WindowRule, app_id: &str, title: &str) -> bool {
    (!r.app_id.is_empty() || !r.title.is_empty()) && regex_match(&r.app_id, app_id) && regex_match(&r.title, title)
}

fn parse_size(s: &str, area: RectF) -> Option<(f64, f64)> {
    let (w, h) = s.split_once(['x', 'X', ' '])?;
    let p = |v: &str, total: f64| -> Option<f64> {
        let v = v.trim();
        if let Some(pc) = v.strip_suffix('%') { pc.parse::<f64>().ok().map(|x| x / 100.0 * total) } else { v.parse().ok() }
    };
    Some((p(w, area.w)?, p(h, area.h)?))
}

impl Fe {
    // ------------------------------------------------------------ params

    pub fn anim(&self, a: &crate::config::Anim) -> AnimParams {
        AnimParams::from_cfg(a, self.cfg.animations.enabled, self.cfg.animations.speed)
    }

    pub fn dynamic_ws(&self) -> bool {
        self.cfg.general.dynamic_workspaces
    }

    /// Number of workspaces you can navigate to.
    pub fn ws_count(&self) -> u32 {
        if self.dynamic_ws() { (self.workspaces.len() as u32).max(1) } else { self.cfg.general.workspaces.max(1) }
    }

    fn ws_has_content(&self, id: u32) -> bool {
        self.windows.values().any(|w| w.ws == id) || self.closing.iter().any(|c| c.ws == id)
    }

    fn ws_in_use(&self, id: u32) -> bool {
        self.outputs.iter().any(|o| o.ws == id || o.switch.as_ref().is_some_and(|s| s.from == id))
    }

    /// niri-style workspaces: drop empty ones nobody looks at, keep exactly one
    /// empty workspace at the end. Later workspaces move up to fill the gap.
    pub fn cleanup_workspaces(&mut self) {
        if !self.dynamic_ws() {
            return;
        }
        let mut changed = false;
        loop {
            let n = self.workspaces.len();
            let victim = (0..n.saturating_sub(1)).find(|&i| {
                let id = i as u32 + 1;
                !self.ws_has_content(id) && !self.ws_in_use(id)
            });
            let Some(i) = victim else { break };
            self.remove_ws_at(i);
            changed = true;
        }
        // trailing empty workspace(s): exactly one, unless the one before it is being shown
        while self.workspaces.len() >= 2 {
            let n = self.workspaces.len() as u32;
            if !self.ws_has_content(n) && !self.ws_has_content(n - 1) && !self.ws_in_use(n) {
                self.remove_ws_at(n as usize - 1);
                changed = true;
            } else {
                break;
            }
        }
        let n = self.workspaces.len() as u32;
        if n == 0 || self.ws_has_content(n) {
            let id = n + 1;
            let name = self.cfg.workspace_layout(id);
            self.workspaces.push(Workspace::new(id, &name, &self.cfg));
            changed = true;
        }
        if changed {
            crate::ipc::event(self, "workspace-changed");
            self.queue_redraw();
        }
    }

    fn remove_ws_at(&mut self, i: usize) {
        let gone = i as u32 + 1;
        self.workspaces.remove(i);
        for (k, w) in self.workspaces.iter_mut().enumerate() {
            w.id = k as u32 + 1;
        }
        let shift = |v: &mut u32| {
            if *v > gone {
                *v -= 1;
            }
        };
        for w in self.windows.values_mut() {
            shift(&mut w.ws);
        }
        for o in &mut self.outputs {
            shift(&mut o.ws);
            if let Some(s) = &mut o.switch {
                shift(&mut s.from);
            }
        }
        for c in &mut self.closing {
            shift(&mut c.ws);
        }
    }

    pub fn ensure_workspaces(&mut self) {
        if self.dynamic_ws() {
            let need = self
                .windows
                .values()
                .map(|w| w.ws)
                .chain(self.outputs.iter().map(|o| o.ws))
                .max()
                .unwrap_or(1)
                .max(1);
            while (self.workspaces.len() as u32) < need {
                let id = self.workspaces.len() as u32 + 1;
                let name = self.cfg.workspace_layout(id);
                self.workspaces.push(Workspace::new(id, &name, &self.cfg));
            }
            let n = self.workspaces.len() as u32;
            if self.ws_has_content(n) {
                let name = self.cfg.workspace_layout(n + 1);
                self.workspaces.push(Workspace::new(n + 1, &name, &self.cfg));
            }
            return;
        }
        let want = self.cfg.general.workspaces.max(1);
        let max_used = self.windows.values().map(|w| w.ws).max().unwrap_or(1);
        let want = want.max(max_used).max(self.outputs.iter().map(|o| o.ws).max().unwrap_or(1));
        while (self.workspaces.len() as u32) < want {
            let id = self.workspaces.len() as u32 + 1;
            let name = self.cfg.workspace_layout(id);
            self.workspaces.push(Workspace::new(id, &name, &self.cfg));
        }
    }

    pub fn active_ws(&self) -> u32 {
        self.outputs.get(self.focused_output).map(|o| o.ws).unwrap_or(1)
    }

    pub fn output_of_ws(&self, ws: u32) -> Option<usize> {
        self.outputs.iter().position(|o| o.ws == ws)
    }

    pub fn ws_visible(&self, ws: u32) -> bool {
        self.outputs.iter().any(|o| o.ws == ws || o.switch.as_ref().is_some_and(|s| s.from == ws))
    }

    /// Output geometry (global logical) for output index.
    pub fn output_geo(&self, idx: usize) -> Option<Rectangle<i32, smithay::utils::Logical>> {
        self.outputs.get(idx).and_then(|o| self.space.output_geometry(&o.output))
    }

    pub fn output_rect_local(&self, idx: usize) -> RectF {
        self.output_geo(idx)
            .map(|g| RectF::new(0.0, 0.0, g.size.w as f64, g.size.h as f64))
            .unwrap_or(RectF::new(0.0, 0.0, 1280.0, 720.0))
    }

    /// Usable tiling area of the output showing `ws` (output-local, minus layer zones and outer gaps).
    pub fn ws_area(&self, ws: u32) -> (RectF, f64) {
        let idx = self.output_of_ws(ws).unwrap_or(self.focused_output);
        let layout_name = self.workspaces.get(ws as usize - 1).map(|w| w.layout.name()).unwrap_or("dwindle");
        let wst = self.style.workspace(layout_name);
        let zone = self
            .outputs
            .get(idx)
            .map(|o| {
                let z = layer_map_for_output(&o.output).non_exclusive_zone();
                RectF::new(z.loc.x as f64, z.loc.y as f64, z.size.w as f64, z.size.h as f64)
            })
            .unwrap_or_else(|| self.output_rect_local(idx));
        let g = wst.gaps_out as f64;
        (RectF::new(zone.x + g, zone.y + g, (zone.w - 2.0 * g).max(50.0), (zone.h - 2.0 * g).max(50.0)), wst.gaps_in as f64)
    }

    pub fn win_style(&self, id: WinId) -> WinStyle {
        let Some(w) = self.windows.get(&id) else { return WinStyle::default() };
        let layout = self.workspaces.get(w.ws as usize - 1).map(|w| w.layout.name()).unwrap_or("");
        let mut st = self.style.window(&WinQuery {
            app_id: &w.app_id,
            title: &w.title,
            layout,
            focus: self.focus == Some(id),
            urgent: w.urgent,
            floating: w.floating,
            fullscreen: w.fullscreen,
            maximized: w.maximized,
            pinned: false,
        });
        if let Some(o) = w.rule.opacity {
            st.opacity = o as f32;
        }
        if w.rule.no_border {
            st.border_width = 0.0;
        }
        if w.rule.no_shadow {
            st.shadow = None;
        }
        if let Some(b) = w.rule.blur {
            st.blur = b;
        }
        if self.game.is_some() {
            st.shadow = None;
            st.blur = false;
        }
        st
    }

    // ------------------------------------------------------------ layout

    pub fn relayout_all(&mut self) {
        for ws in 1..=self.workspaces.len() as u32 {
            self.relayout(ws);
        }
    }

    pub fn relayout(&mut self, ws: u32) {
        let Some(wsi) = (ws as usize).checked_sub(1).filter(|i| *i < self.workspaces.len()) else { return };
        let now = Instant::now();
        let (area, gap) = self.ws_area(ws);
        let full = self.output_of_ws(ws).map(|i| self.output_rect_local(i)).unwrap_or(area);
        let rects = {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            let w = &mut self.workspaces[wsi];
            let rects = w.layout.arrange(&ctx);
            let (vx, vy) = w.layout.view_offset(&ctx);
            let p = AnimParams::from_cfg(&self.cfg.animations.scroll, self.cfg.animations.enabled, self.cfg.animations.speed);
            w.view_x.set(vx, p, now);
            w.view_y.set(vy, p, now);
            rects
        };
        let move_p = self.anim(&self.cfg.animations.windows_move);
        let mut targets: Vec<(WinId, RectF)> = Vec::new();
        for (id, r) in rects {
            targets.push((id, r));
        }
        let floating: Vec<WinId> = self.windows.values().filter(|w| w.ws == ws && w.floating).map(|w| w.id).collect();
        for id in floating {
            targets.push((id, self.windows[&id].float_rect));
        }
        for (id, r) in targets {
            let st = self.win_style(id);
            let Some(w) = self.windows.get_mut(&id) else { continue };
            let target = if w.fullscreen {
                full
            } else if w.maximized {
                area
            } else if w.floating {
                r
            } else {
                let b = st.border_width as f64;
                RectF::new(r.x + b, r.y + b, (r.w - 2.0 * b).max(1.0), (r.h - 2.0 * b).max(1.0))
            };
            let params = if w.rule.no_anim || !w.mapped { AnimParams::instant() } else { move_p };
            if !w.mapped {
                w.rect.jump(target);
            } else {
                w.rect.set(target, params, now);
            }
            let size = (target.w.round().max(1.0) as i32, target.h.round().max(1.0) as i32);
            if let Some(t) = w.window.toplevel() {
                let tiled = !w.floating;
                let fs = w.fullscreen;
                let mx = w.maximized;
                t.with_pending_state(|s| {
                    s.size = Some(size.into());
                    for st in [
                        xdg_toplevel::State::TiledLeft,
                        xdg_toplevel::State::TiledRight,
                        xdg_toplevel::State::TiledTop,
                        xdg_toplevel::State::TiledBottom,
                    ] {
                        if tiled { s.states.set(st) } else { s.states.unset(st) };
                    }
                    if fs { s.states.set(xdg_toplevel::State::Fullscreen) } else { s.states.unset(xdg_toplevel::State::Fullscreen) };
                    if mx { s.states.set(xdg_toplevel::State::Maximized) } else { s.states.unset(xdg_toplevel::State::Maximized) };
                });
                w.pending_size = Some(size);
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
        }
        self.map_ws_in_space(ws);
        self.queue_redraw();
    }

    /// Keep smithay's Space in sync (for output enter/leave and scale).
    pub fn map_ws_in_space(&mut self, ws: u32) {
        let out = self.output_of_ws(ws);
        let (ox, oy) = out
            .and_then(|i| self.output_geo(i))
            .map(|g| (g.loc.x as f64, g.loc.y as f64))
            .unwrap_or((0.0, 0.0));
        let (vx, vy) = self
            .workspaces
            .get(ws as usize - 1)
            .map(|w| (w.view_x.target(), w.view_y.target()))
            .unwrap_or((0.0, 0.0));
        let ids: Vec<WinId> = self.windows.values().filter(|w| w.ws == ws).map(|w| w.id).collect();
        for id in ids {
            let w = &self.windows[&id];
            if out.is_some() && w.mapped {
                let r = w.rect.target();
                let (dx, dy) = if w.floating || w.fullscreen { (0.0, 0.0) } else { (vx, vy) };
                let geo = w.window.geometry();
                let loc = ((ox + r.x - dx) as i32 - geo.loc.x, (oy + r.y - dy) as i32 - geo.loc.y);
                self.space.map_element(w.window.clone(), loc, false);
            } else {
                self.space.unmap_elem(&w.window);
            }
        }
    }

    // ------------------------------------------------------------ window lifecycle

    pub fn toplevel_commit(&mut self, id: WinId) {
        let Some(w) = self.windows.get(&id) else { return };
        let Some(t) = w.window.toplevel().cloned() else { return };
        if !t.is_initial_configure_sent() {
            self.setup_new_window(id);
            self.toplevel_list_add(id);
            t.send_configure();
            return;
        }
        let has_buffer = with_renderer_surface_state(t.wl_surface(), |s| s.buffer().is_some()).unwrap_or(false);
        // a window hidden earlier (null buffer) comes back: put it back into the layout
        let (ws0, floating0, mapped0) = {
            let w = &self.windows[&id];
            (w.ws, w.floating, w.mapped)
        };
        if has_buffer && !mapped0 && !floating0 {
            let wsi = ws0 as usize - 1;
            if self.workspaces.get(wsi).is_some_and(|w| !w.layout.windows().contains(&id)) {
                let (area, gap) = self.ws_area(ws0);
                let focus = self.workspaces[wsi].focus_hist.last().copied();
                let ctx = Ctx { cfg: &self.cfg, area, gap };
                self.workspaces[wsi].layout.add(&ctx, id, focus, None);
            }
        }
        let w = self.windows.get_mut(&id).unwrap();
        // pick up title/app_id changes
        let (app_id, title) = toplevel_app_id_title(&t);
        let changed = app_id != w.app_id || title != w.title;
        w.app_id = app_id;
        w.title = title;
        if changed {
            self.toplevel_list_update(id);
        }
        let w = self.windows.get_mut(&id).unwrap();
        if !has_buffer && w.mapped {
            // the client hid the window (null buffer, e.g. minimize to tray):
            // take it out of the layout until it maps again
            w.mapped = false;
            w.open = None;
            let ws = w.ws;
            let win = w.window.clone();
            self.space.unmap_elem(&win);
            let (area, gap) = self.ws_area(ws);
            if let Some(wsp) = self.workspaces.get_mut(ws as usize - 1) {
                let ctx = Ctx { cfg: &self.cfg, area, gap };
                wsp.layout.remove(&ctx, id);
                wsp.focus_hist.retain(|x| *x != id);
            }
            if self.focus == Some(id) {
                self.focus = None;
                let next = self.workspaces.get(ws as usize - 1).and_then(|w| w.focus_hist.last().copied());
                self.focus_window(next, false);
            }
            self.relayout(ws);
            return;
        }
        if has_buffer && !w.mapped {
            w.mapped = true;
            let geo = w.window.geometry();
            if w.floating && (w.float_rect.w < 2.0 || w.float_rect.h < 2.0) {
                let (area, _) = self.ws_area(self.windows[&id].ws);
                let ww = geo.size.w.max(200) as f64;
                let hh = geo.size.h.max(120) as f64;
                let w = self.windows.get_mut(&id).unwrap();
                w.float_rect = RectF::new(area.x + (area.w - ww) / 2.0, area.y + (area.h - hh) / 2.0, ww, hh);
            }
            let p = self.anim(&self.cfg.animations.windows_open);
            let style = self.cfg.animations.windows_open.style.clone();
            let w = self.windows.get_mut(&id).unwrap();
            if p.enabled && style != "none" && !w.rule.no_anim {
                w.open = Some(Progress::new(p, Instant::now()));
            }
            let ws = w.ws;
            self.relayout(ws);
            if let Some(w) = self.windows.get_mut(&id) {
                let r = w.rect.target();
                w.rect.jump(r);
            }
            let visible = self.output_of_ws(ws).is_some();
            if self.cfg.general.focus_new_windows && visible {
                self.focus_window(Some(id), false);
            } else if !visible {
                self.windows.get_mut(&id).unwrap().urgent = true;
            }
            crate::ipc::event(self, "window-opened");
        } else if changed {
            crate::ipc::event(self, "window-changed");
        }
        if let Some(w) = self.windows.get(&id) {
            if w.floating && w.mapped {
                // follow client-driven size changes of floating windows
                let geo = w.window.geometry();
                let fr = w.float_rect;
                if geo.size.w > 0 && ((geo.size.w as f64 - fr.w).abs() > 1.0 || (geo.size.h as f64 - fr.h).abs() > 1.0) {
                    let w = self.windows.get_mut(&id).unwrap();
                    if w.pending_size.is_none_or(|(pw, ph)| pw == geo.size.w && ph == geo.size.h) {
                        w.float_rect.w = geo.size.w as f64;
                        w.float_rect.h = geo.size.h as f64;
                        let r = w.float_rect;
                        w.rect.jump(r);
                    }
                }
            }
        }
    }

    fn setup_new_window(&mut self, id: WinId) {
        let Some(t) = self.windows.get(&id).and_then(|w| w.window.toplevel().cloned()) else { return };
        let (app_id, title) = toplevel_app_id_title(&t);
        let mut ws = self.active_ws();
        let mut floating = self.cfg.general.float_new_windows;
        let mut fullscreen = false;
        let mut maximize = false;
        let mut size: Option<String> = None;
        let mut center = false;
        let mut col_width = None;
        let mut fx = RuleFx::default();

        let (min, max) = with_states(t.wl_surface(), |s| {
            let mut c = s.cached_state.get::<SurfaceCachedState>();
            let c = c.current();
            (c.min_size, c.max_size)
        });
        let is_dialog = t.parent().is_some() || (min.w > 0 && min.h > 0 && min == max);
        if self.cfg.general.auto_float_dialogs && is_dialog {
            floating = true;
            center = true;
        }
        for r in &self.cfg.window_rules {
            if !rule_matches(r, &app_id, &title) {
                continue;
            }
            if let Some(v) = r.float {
                floating = v;
            }
            if let Some(v) = r.fullscreen {
                fullscreen = v;
            }
            if let Some(v) = r.maximize {
                maximize = v;
            }
            if !r.size.is_empty() {
                size = Some(r.size.clone());
            }
            if let Some(v) = r.center {
                center = v;
            }
            if let Some(v) = r.workspace {
                ws = v.max(1);
                if self.cfg.general.dynamic_workspaces {
                    ws = ws.min((self.workspaces.len() as u32).max(1));
                }
            }
            if r.opacity.is_some() {
                fx.opacity = r.opacity;
            }
            if let Some(v) = r.no_border {
                fx.no_border = v;
            }
            if let Some(v) = r.no_shadow {
                fx.no_shadow = v;
            }
            if let Some(v) = r.no_anim {
                fx.no_anim = v;
            }
            if r.blur.is_some() {
                fx.blur = r.blur;
            }
            if r.column_width.is_some() {
                col_width = r.column_width;
            }
        }
        self.ensure_workspaces();
        let wsi = ws as usize - 1;
        if self.workspaces[wsi].layout.name() == "floating" {
            floating = true;
        }
        let (area, gap) = self.ws_area(ws);
        let mut float_rect = RectF::default();
        if floating {
            if let Some((w, h)) = size.as_deref().and_then(|s| parse_size(s, area)) {
                float_rect = RectF::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h);
            } else if center {
                float_rect = RectF::default(); // centered at map time with the client's size
            }
        }
        {
            let w = self.windows.get_mut(&id).unwrap();
            w.app_id = app_id;
            w.title = title;
            w.ws = ws;
            w.floating = floating;
            w.fullscreen = fullscreen;
            w.maximized = maximize;
            w.float_rect = float_rect;
            w.rule = fx;
        }
        if floating {
            self.workspaces[wsi].float_stack.push(id);
        } else {
            let focus = self.workspaces[wsi].focus_hist.last().copied().filter(|f| {
                self.windows.get(f).is_some_and(|w| !w.floating)
            });
            let cursor = self.cursor_ws_local(ws);
            if let Some(cw) = col_width {
                self.workspaces[wsi].layout.set_next_width(cw);
            }
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            self.workspaces[wsi].layout.add(&ctx, id, focus, cursor);
        }
        self.relayout(ws);
        if floating && float_rect.w < 2.0 && !fullscreen && !maximize {
            // let the client choose its size; fullscreen/maximized windows must
            // keep the size from relayout (a 0x0 configure with the fullscreen
            // state set breaks Qt clients such as flameshot's overlay)
            if let Some(t) = self.windows[&id].window.toplevel() {
                t.with_pending_state(|s| s.size = None);
            }
        }
    }

    fn cursor_ws_local(&self, ws: u32) -> Option<(f64, f64)> {
        let idx = self.output_of_ws(ws)?;
        let g = self.output_geo(idx)?;
        let p = self.seat.get_pointer()?.current_location();
        Some((p.x - g.loc.x as f64, p.y - g.loc.y as f64))
    }

    pub fn remove_window(&mut self, id: WinId) {
        let Some(w) = self.windows.remove(&id) else { return };
        self.toplevel_list_remove(id);
        self.space.unmap_elem(&w.window);
        let ws = w.ws;
        let wsi = ws as usize - 1;
        let (area, gap) = self.ws_area(ws);
        if let Some(wsp) = self.workspaces.get_mut(wsi) {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            wsp.layout.remove(&ctx, id);
            wsp.float_stack.retain(|x| *x != id);
            wsp.focus_hist.retain(|x| *x != id);
        }
        if self.focus == Some(id) {
            self.focus = None;
            let next = self.workspaces.get(wsi).and_then(|w| w.focus_hist.last().copied());
            if self.active_ws() == ws {
                self.focus_window(next, false);
            }
        }
        self.relayout(ws);
        crate::ipc::event(self, "window-closed");
        self.check_game_mode();
        self.cleanup_workspaces();
    }

    // ------------------------------------------------------------ focus

    pub fn focus_window(&mut self, id: Option<WinId>, warp: bool) {
        let id = id.filter(|i| self.windows.contains_key(i));
        if let Some(i) = id {
            let ws = self.windows[&i].ws;
            if self.output_of_ws(ws).is_none() {
                self.switch_workspace(ws);
            } else if let Some(oi) = self.output_of_ws(ws) {
                self.focused_output = oi;
            }
        }
        let old = self.focus;
        self.focus = id;
        let now = Instant::now();
        let bp = self.anim(&self.cfg.animations.border_color);
        for wid in [old, id].into_iter().flatten() {
            let from = self.windows.get(&wid).map(|_| ());
            if from.is_none() {
                continue;
            }
            // border color crossfade: from previous style to the new one
            let prev_focus = self.focus;
            self.focus = if Some(wid) == id { old } else { Some(wid) };
            let before = self.win_style(wid).border_color;
            self.focus = prev_focus;
            let after = self.win_style(wid).border_color;
            if let Some(w) = self.windows.get_mut(&wid) {
                w.border = Some(BorderAnim { from: before, to: after, prog: Some(Progress::new(bp, now)) });
            }
        }
        for w in self.windows.values() {
            let active = Some(w.id) == id;
            if w.window.set_activated(active) {
                if let Some(t) = w.window.toplevel() {
                    if t.is_initial_configure_sent() {
                        t.send_pending_configure();
                    }
                }
            }
        }
        if let Some(i) = id {
            let ws = self.windows[&i].ws;
            let wsi = ws as usize - 1;
            if let Some(w) = self.windows.get_mut(&i) {
                w.urgent = false;
            }
            let (area, gap) = self.ws_area(ws);
            let floating = self.windows[&i].floating;
            {
                let wsp = &mut self.workspaces[wsi];
                wsp.focus_hist.retain(|x| *x != i);
                wsp.focus_hist.push(i);
                if floating {
                    wsp.float_stack.retain(|x| *x != i);
                    wsp.float_stack.push(i);
                } else {
                    let ctx = Ctx { cfg: &self.cfg, area, gap };
                    wsp.layout.focus_changed(&ctx, i);
                }
            }
            self.relayout(ws);
        }
        if self.layer_focus.is_none() && !self.lock.locked {
            let surface = id.and_then(|i| self.windows[&i].window.toplevel().map(|t| t.wl_surface().clone()));
            let kb = self.seat.get_keyboard().unwrap();
            kb.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
        if warp && self.cfg.general.warp_mouse_to_focus {
            if let Some(i) = id {
                self.warp_to_window(i);
            }
        }
        if old != id {
            crate::ipc::event(self, "focus-changed");
        }
        self.queue_redraw();
    }

    fn warp_to_window(&mut self, id: WinId) {
        let Some((oi, r)) = self.window_visual_rect(id) else { return };
        let Some(g) = self.output_geo(oi) else { return };
        let (cx, cy) = r.center();
        let pos = (g.loc.x as f64 + cx, g.loc.y as f64 + cy);
        let ptr = self.seat.get_pointer().unwrap();
        let under = self.surface_under(pos.into());
        ptr.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location: pos.into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: smithay::backend::input::InputTime::now(),
            },
        );
        ptr.frame(self);
    }

    /// Target visual rect of a window, output-local.
    pub fn window_visual_rect(&self, id: WinId) -> Option<(usize, RectF)> {
        let w = self.windows.get(&id)?;
        let oi = self.output_of_ws(w.ws)?;
        let wsp = self.workspaces.get(w.ws as usize - 1)?;
        let mut r = w.rect.target();
        if !w.floating && !w.fullscreen {
            r.x -= wsp.view_x.target();
            r.y -= wsp.view_y.target();
        }
        Some((oi, r))
    }

    // ------------------------------------------------------------ workspaces

    pub fn switch_workspace(&mut self, target: u32) {
        self.ensure_workspaces();
        let target = if self.dynamic_ws() { target.clamp(1, self.ws_count()) } else { target.clamp(1, 99) };
        // a window held with the mouse comes along
        let target = self.drag_carry(target);
        while self.workspaces.len() < target as usize {
            let id = self.workspaces.len() as u32 + 1;
            let name = self.cfg.workspace_layout(id);
            self.workspaces.push(Workspace::new(id, &name, &self.cfg));
        }
        if let Some(other) = self.output_of_ws(target) {
            // already shown somewhere: focus that output
            self.focused_output = other;
        } else {
            let Some(o) = self.outputs.get_mut(self.focused_output) else { return };
            let from = o.ws;
            if from == target {
                return;
            }
            let p = AnimParams::from_cfg(
                &self.cfg.animations.workspaces,
                self.cfg.animations.enabled,
                self.cfg.animations.speed,
            );
            o.ws = target;
            o.switch = if p.enabled && self.cfg.animations.workspaces.style != "none" {
                Some(WsSwitch {
                    from,
                    prog: Progress::new(p, Instant::now()),
                    dir: if target > from { 1.0 } else { -1.0 },
                    style: self.cfg.animations.workspaces.style.clone(),
                })
            } else {
                None
            };
            self.relayout(target);
            self.map_ws_in_space(from);
        }
        let next = self.workspaces[target as usize - 1].focus_hist.last().copied();
        self.focus = None;
        self.focus_window(next, true);
        if next.is_none() {
            let kb = self.seat.get_keyboard().unwrap();
            if self.layer_focus.is_none() {
                kb.set_focus(self, None, SERIAL_COUNTER.next_serial());
            }
        }
        crate::ipc::event(self, "workspace-changed");
        self.cleanup_workspaces();
        self.queue_redraw();
    }

    pub fn move_to_workspace(&mut self, id: WinId, target: u32, follow: bool) {
        self.ensure_workspaces();
        let target = if self.dynamic_ws() { target.min(self.ws_count()) } else { target };
        let Some(w) = self.windows.get(&id) else { return };
        let from = w.ws;
        if from == target || target == 0 {
            return;
        }
        let floating = w.floating;
        while self.workspaces.len() < target as usize {
            let i = self.workspaces.len() as u32 + 1;
            let name = self.cfg.workspace_layout(i);
            self.workspaces.push(Workspace::new(i, &name, &self.cfg));
        }
        let (area, gap) = self.ws_area(from);
        {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            let f = &mut self.workspaces[from as usize - 1];
            f.layout.remove(&ctx, id);
            f.float_stack.retain(|x| *x != id);
            f.focus_hist.retain(|x| *x != id);
        }
        let (area2, gap2) = self.ws_area(target);
        {
            let focus = self.workspaces[target as usize - 1].focus_hist.last().copied();
            let ctx = Ctx { cfg: &self.cfg, area: area2, gap: gap2 };
            let t = &mut self.workspaces[target as usize - 1];
            if floating {
                t.float_stack.push(id);
            } else {
                t.layout.add(&ctx, id, focus, None);
            }
            t.focus_hist.push(id);
        }
        self.windows.get_mut(&id).unwrap().ws = target;
        if self.focus == Some(id) {
            self.focus = None;
        }
        self.relayout(from);
        self.relayout(target);
        if follow {
            self.switch_workspace(target);
            self.focus_window(Some(id), true);
        } else if self.active_ws() == from {
            let next = self.workspaces[from as usize - 1].focus_hist.last().copied();
            self.focus_window(next, false);
        }
        self.cleanup_workspaces();
    }

    /// Swap the contents of two workspaces (niri move-workspace-up/down).
    pub fn swap_workspaces(&mut self, a: u32, b: u32) {
        if a == b || a == 0 || b == 0 {
            return;
        }
        let n = a.max(b) as usize;
        while self.workspaces.len() < n {
            let i = self.workspaces.len() as u32 + 1;
            let name = self.cfg.workspace_layout(i);
            self.workspaces.push(Workspace::new(i, &name, &self.cfg));
        }
        self.workspaces.swap(a as usize - 1, b as usize - 1);
        self.workspaces[a as usize - 1].id = a;
        self.workspaces[b as usize - 1].id = b;
        for w in self.windows.values_mut() {
            if w.ws == a {
                w.ws = b;
            } else if w.ws == b {
                w.ws = a;
            }
        }
        self.relayout(a);
        self.relayout(b);
        self.switch_workspace(b);
        self.cleanup_workspaces();
    }

    pub fn set_layout(&mut self, ws: u32, name: &str) {
        let wsi = ws as usize - 1;
        let Some(wsp) = self.workspaces.get(wsi) else { return };
        let order = wsp.layout.windows();
        let focus = wsp.focus_hist.last().copied();
        let mut new = layout::make(name, &self.cfg);
        let (area, gap) = self.ws_area(ws);
        let ctx = Ctx { cfg: &self.cfg, area, gap };
        let mut prev = None;
        for id in &order {
            new.add(&ctx, *id, prev, None);
            prev = Some(*id);
        }
        if let Some(f) = focus.filter(|f| order.contains(f)) {
            new.focus_changed(&ctx, f);
        }
        let to_float = new.name() == "floating";
        self.workspaces[wsi].layout = new;
        if to_float {
            for id in order {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.floating = true;
                    w.float_rect = w.rect.target();
                }
                self.workspaces[wsi].float_stack.push(id);
            }
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            self.workspaces[wsi].layout = layout::make("floating", &self.cfg);
            let _ = ctx;
        }
        info!("стол {ws}: раскладка {name}");
        self.relayout(ws);
        crate::ipc::event(self, "layout-changed");
    }

    // ------------------------------------------------------------ window ops

    pub fn toggle_float(&mut self, id: WinId) {
        let Some(w) = self.windows.get(&id) else { return };
        let ws = w.ws;
        let wsi = ws as usize - 1;
        let (area, gap) = self.ws_area(ws);
        let now_floating = !w.floating;
        let cur = w.rect.target();
        let ctx = Ctx { cfg: &self.cfg, area, gap };
        if now_floating {
            self.workspaces[wsi].layout.remove(&ctx, id);
            self.workspaces[wsi].float_stack.push(id);
            let w = self.windows.get_mut(&id).unwrap();
            w.floating = true;
            if w.float_rect.w < 2.0 {
                let ww = (cur.w * 0.8).max(300.0).min(area.w);
                let hh = (cur.h * 0.8).max(200.0).min(area.h);
                w.float_rect = RectF::new(area.x + (area.w - ww) / 2.0, area.y + (area.h - hh) / 2.0, ww, hh);
            }
        } else {
            let focus = self.workspaces[wsi].focus_hist.iter().rev().copied().find(|f| {
                *f != id && self.windows.get(f).is_some_and(|w| !w.floating)
            });
            self.workspaces[wsi].float_stack.retain(|x| *x != id);
            self.workspaces[wsi].layout.add(&ctx, id, focus, None);
            let w = self.windows.get_mut(&id).unwrap();
            w.float_rect = cur;
            w.floating = false;
        }
        self.relayout(ws);
    }

    pub fn set_fullscreen(&mut self, id: WinId, on: bool) {
        let Some(w) = self.windows.get_mut(&id) else { return };
        w.fullscreen = on;
        let ws = w.ws;
        self.relayout(ws);
        if !on {
            self.check_game_mode();
        }
    }

    pub fn close(&mut self, id: WinId) {
        if let Some(t) = self.windows.get(&id).and_then(|w| w.window.toplevel()) {
            t.send_close();
        }
    }

    pub fn spawn(&self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        info!("запуск: {cmd}");
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c.env("WAYLAND_DISPLAY", &self.socket_name);
        c.env("XDG_CURRENT_DESKTOP", "FerriumWM");
        c.env("XDG_SESSION_DESKTOP", "FerriumWM");
        c.env("XDG_SESSION_TYPE", "wayland");
        c.env("FEWM_SOCKET", crate::ipc::socket_path());
        match &self.xwayland_display {
            Some(d) => {
                c.env("DISPLAY", d);
            }
            None => {
                c.env_remove("DISPLAY");
            }
        }
        for (k, v) in &self.cfg.env {
            c.env(k, v);
        }
        c.stdin(std::process::Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        match c.spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(e) => warn!("не запустилось {cmd}: {e}"),
        }
    }

    fn ws_step(&self, cur: u32, step: i32) -> u32 {
        let n = self.ws_count() as i32;
        let next = cur as i32 + step;
        if next < 1 {
            if self.cfg.general.cycle_focus { n as u32 } else { 1 }
        } else if next > n {
            if self.cfg.general.cycle_focus { 1 } else { n as u32 }
        } else {
            next as u32
        }
    }

    fn parse_ws(&self, arg: &str) -> Option<u32> {
        let cur = self.active_ws();
        match arg {
            "next" | "down" | "e+1" | "+1" | "r+1" => Some(self.ws_step(cur, 1)),
            "prev" | "up" | "e-1" | "-1" | "r-1" => Some(self.ws_step(cur, -1)),
            "last" => Some(self.ws_count()),
            "first" => Some(1),
            s => {
                let n = s.parse::<u32>().ok().filter(|n| *n >= 1)?;
                // dynamic: "workspace 7" with 3 workspaces goes to the last (empty) one
                Some(if self.dynamic_ws() { n.min(self.ws_count()) } else { n })
            }
        }
    }

    /// Run a layout command; returns true if it was handled.
    fn layout_cmd(&mut self, name: &str, args: &[&str]) -> bool {
        let ws = self.active_ws();
        let wsi = ws as usize - 1;
        let focus = self.focus.filter(|f| self.windows.get(f).is_some_and(|w| w.ws == ws && !w.floating));
        let (area, gap) = self.ws_area(ws);
        let res = {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            self.workspaces[wsi].layout.cmd(&ctx, focus, name, args)
        };
        match res {
            CmdResult::Unhandled => false,
            CmdResult::Done => {
                self.relayout(ws);
                true
            }
            CmdResult::Focus(id) => {
                self.focus_window(Some(id), true);
                true
            }
        }
    }

    pub fn focus_dir(&mut self, dir: Dir) {
        let ws = self.active_ws();
        let wsi = ws as usize - 1;
        let Some(f) = self.focus.filter(|f| self.windows.contains_key(f)) else {
            let last = self.workspaces[wsi].focus_hist.last().copied();
            self.focus_window(last, true);
            return;
        };
        let (area, gap) = self.ws_area(ws);
        let floating = self.windows[&f].floating;
        let next = if floating {
            // geometric among floating + tiled visuals
            let rects: Vec<(WinId, RectF)> = self
                .windows
                .values()
                .filter(|w| w.ws == ws && w.mapped)
                .filter_map(|w| self.window_visual_rect(w.id).map(|(_, r)| (w.id, r)))
                .collect();
            layout::geometric_neighbor(&rects, f, dir)
        } else {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            self.workspaces[wsi].layout.neighbor(&ctx, f, dir)
        };
        match next {
            Some(n) => self.focus_window(Some(n), true),
            None => {
                // at the edge: hop to the neighbouring output
                if dir.horizontal() && self.outputs.len() > 1 {
                    self.focus_output_dir(dir);
                }
            }
        }
    }

    pub fn move_dir(&mut self, dir: Dir) {
        let ws = self.active_ws();
        let wsi = ws as usize - 1;
        let Some(f) = self.focus.filter(|f| self.windows.contains_key(f)) else { return };
        if self.windows[&f].floating {
            let step = self.cfg.general.move_floating_step as f64;
            let w = self.windows.get_mut(&f).unwrap();
            match dir {
                Dir::Left => w.float_rect.x -= step,
                Dir::Right => w.float_rect.x += step,
                Dir::Up => w.float_rect.y -= step,
                Dir::Down => w.float_rect.y += step,
            }
            self.relayout(ws);
            return;
        }
        let (area, gap) = self.ws_area(ws);
        let moved = {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            self.workspaces[wsi].layout.move_dir(&ctx, f, dir)
        };
        if !moved && dir.horizontal() && self.outputs.len() > 1 {
            self.move_to_output_dir(f, dir);
            return;
        }
        let (area, gap) = self.ws_area(ws);
        let ctx = Ctx { cfg: &self.cfg, area, gap };
        self.workspaces[wsi].layout.focus_changed(&ctx, f);
        self.relayout(ws);
    }

    fn output_in_dir(&self, dir: Dir) -> Option<usize> {
        let cur = self.output_geo(self.focused_output)?;
        let (cx, cy) = (cur.loc.x + cur.size.w / 2, cur.loc.y + cur.size.h / 2);
        (0..self.outputs.len())
            .filter(|i| *i != self.focused_output)
            .filter_map(|i| self.output_geo(i).map(|g| (i, g)))
            .filter(|(_, g)| {
                let (x, y) = (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2);
                match dir {
                    Dir::Left => x < cx,
                    Dir::Right => x > cx,
                    Dir::Up => y < cy,
                    Dir::Down => y > cy,
                }
            })
            .min_by_key(|(_, g)| {
                let (x, y) = (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2);
                (x - cx).abs() + (y - cy).abs()
            })
            .map(|(i, _)| i)
    }

    pub fn focus_output_dir(&mut self, dir: Dir) {
        if let Some(i) = self.output_in_dir(dir) {
            self.focused_output = i;
            let ws = self.outputs[i].ws;
            let next = self.workspaces.get(ws as usize - 1).and_then(|w| w.focus_hist.last().copied());
            self.focus_window(next, true);
        }
    }

    fn move_to_output_dir(&mut self, id: WinId, dir: Dir) {
        if let Some(i) = self.output_in_dir(dir) {
            let ws = self.outputs[i].ws;
            self.move_to_workspace(id, ws, false);
            self.focused_output = i;
            self.focus_window(Some(id), true);
        }
    }

    // ------------------------------------------------------------ actions

    pub fn run_action(&mut self, action: &str) {
        let action = action.trim();
        let (name, rest) = action.split_once(char::is_whitespace).unwrap_or((action, ""));
        let rest = rest.trim();
        let args: Vec<&str> = rest.split_whitespace().collect();
        let a0 = args.first().copied().unwrap_or("");
        let focus = self.focus.filter(|f| self.windows.contains_key(f));
        let ws = self.active_ws();

        match name {
            "spawn" | "exec" | "spawn-sh" => self.spawn(rest),
            "spawn-terminal" | "terminal" => {
                let t = self.cfg.general.terminal.clone();
                self.spawn(&t)
            }
            "close" | "close-window" | "killactive" => {
                if let Some(f) = focus {
                    self.close(f)
                }
            }
            "exit" | "quit" => {
                info!("выход");
                self.signal.stop();
            }
            "reload" | "reload-config" => crate::config_reload(self),
            "fullscreen" | "fullscreen-window" => {
                if let Some(f) = focus {
                    let on = !self.windows[&f].fullscreen;
                    self.set_fullscreen(f, on);
                }
            }
            "maximize" | "maximize-window-to-edges" | "toggle-maximize" => {
                if let Some(f) = focus {
                    let w = self.windows.get_mut(&f).unwrap();
                    w.maximized = !w.maximized;
                    self.relayout(ws);
                }
            }
            "toggle-float" | "toggle-window-floating" | "togglefloating" | "float" => {
                if let Some(f) = focus {
                    self.toggle_float(f)
                }
            }
            "switch-focus-between-floating-and-tiling" => {
                let want_float = !focus.is_some_and(|f| self.windows[&f].floating);
                let next = self.workspaces[ws as usize - 1]
                    .focus_hist
                    .iter()
                    .rev()
                    .copied()
                    .find(|i| self.windows.get(i).is_some_and(|w| w.floating == want_float));
                if next.is_some() {
                    self.focus_window(next, true);
                }
            }
            "center" | "center-window" => {
                if let Some(f) = focus.filter(|f| self.windows[f].floating) {
                    let (area, _) = self.ws_area(ws);
                    let w = self.windows.get_mut(&f).unwrap();
                    w.float_rect.x = area.x + (area.w - w.float_rect.w) / 2.0;
                    w.float_rect.y = area.y + (area.h - w.float_rect.h) / 2.0;
                    self.relayout(ws);
                } else {
                    self.layout_cmd("center-column", &[]);
                }
            }
            "focus" | "movefocus" => {
                if let Some(d) = Dir::parse(a0) {
                    self.focus_dir(d)
                }
            }
            "move" | "movewindow" | "swap" => {
                if let Some(d) = Dir::parse(a0) {
                    self.move_dir(d)
                }
            }
            "focus-column-left" | "focus-column-right" | "focus-window-up" | "focus-window-down"
            | "focus-column-first" | "focus-column-last" => {
                if !self.layout_cmd(name, &args) {
                    match name {
                        "focus-column-left" => self.focus_dir(Dir::Left),
                        "focus-column-right" => self.focus_dir(Dir::Right),
                        "focus-window-up" => self.focus_dir(Dir::Up),
                        "focus-window-down" => self.focus_dir(Dir::Down),
                        _ => {}
                    }
                }
            }
            "focus-window-or-workspace-down" | "focus-window-or-workspace-up" => {
                let before = self.focus;
                self.focus_dir(if name.ends_with("down") { Dir::Down } else { Dir::Up });
                if before == self.focus {
                    let t = self.ws_step(ws, if name.ends_with("down") { 1 } else { -1 });
                    self.switch_workspace(t);
                }
            }
            "move-column-left" | "move-column-right" | "move-window-up" | "move-window-down"
            | "move-column-to-first" | "move-column-to-last" => {
                if focus.is_some_and(|f| self.windows[&f].floating) || !self.layout_cmd(name, &args) {
                    match name {
                        "move-column-left" => self.move_dir(Dir::Left),
                        "move-column-right" => self.move_dir(Dir::Right),
                        "move-window-up" => self.move_dir(Dir::Up),
                        "move-window-down" => self.move_dir(Dir::Down),
                        _ => {}
                    }
                }
            }
            "resize" | "resizeactive" => {
                if let Some(f) = focus {
                    let dx: f64 = args.first().and_then(|v| v.trim_end_matches('%').parse().ok()).unwrap_or(0.0);
                    let dy: f64 = args.get(1).and_then(|v| v.trim_end_matches('%').parse().ok()).unwrap_or(0.0);
                    let (dx, dy) = if a0.ends_with('%') { (dx / 100.0, dy / 100.0) } else { (dx, dy) };
                    if self.windows[&f].floating {
                        let (area, _) = self.ws_area(ws);
                        let w = self.windows.get_mut(&f).unwrap();
                        let sx = if dx.abs() <= 1.0 { dx * area.w } else { dx };
                        let sy = if dy.abs() <= 1.0 { dy * area.h } else { dy };
                        w.float_rect.w = (w.float_rect.w + sx).max(50.0);
                        w.float_rect.h = (w.float_rect.h + sy).max(50.0);
                    } else {
                        let (area, gap) = self.ws_area(ws);
                        let ctx = Ctx { cfg: &self.cfg, area, gap };
                        self.workspaces[ws as usize - 1].layout.resize(&ctx, f, dx, dy);
                    }
                    self.relayout(ws);
                }
            }
            "workspace" | "focus-workspace" => {
                if let Some(t) = self.parse_ws(a0) {
                    if t == ws && self.cfg.general.cycle_focus {
                        return;
                    }
                    self.switch_workspace(t)
                }
            }
            "focus-workspace-down" | "focus-workspace-up" | "workspace-next" | "workspace-prev" => {
                let t = self.ws_step(ws, if name.ends_with("down") || name.ends_with("next") { 1 } else { -1 });
                self.switch_workspace(t);
            }
            "focus-workspace-previous" => {
                // simple: toggle with the last workspace we came from
                if let Some(s) = self.outputs.get(self.focused_output).and_then(|o| o.switch.as_ref().map(|s| s.from)) {
                    self.switch_workspace(s);
                }
            }
            "move-to-workspace" | "move-column-to-workspace" | "move-window-to-workspace" | "movetoworkspace"
            | "movetoworkspacesilent" => {
                if let (Some(f), Some(t)) = (focus, self.parse_ws(a0)) {
                    let follow = name != "movetoworkspacesilent"
                        && !args.iter().any(|a| a.contains("focus=false"));
                    self.move_to_workspace(f, t, follow);
                }
            }
            "move-column-to-workspace-down" | "move-column-to-workspace-up" | "move-window-to-workspace-down"
            | "move-window-to-workspace-up" => {
                if let Some(f) = focus {
                    let t = self.ws_step(ws, if name.ends_with("down") { 1 } else { -1 });
                    self.move_to_workspace(f, t, true);
                }
            }
            "move-workspace-down" | "move-workspace-up" => {
                let t = self.ws_step(ws, if name.ends_with("down") { 1 } else { -1 });
                self.swap_workspaces(ws, t);
            }
            "focus-monitor-left" | "focus-monitor-right" | "focus-monitor-up" | "focus-monitor-down" => {
                let d = Dir::parse(name.rsplit('-').next().unwrap_or("")).unwrap_or(Dir::Right);
                self.focus_output_dir(d);
            }
            "move-column-to-monitor-left" | "move-column-to-monitor-right" | "move-column-to-monitor-up"
            | "move-column-to-monitor-down" | "move-window-to-monitor-left" | "move-window-to-monitor-right" => {
                let d = Dir::parse(name.rsplit('-').next().unwrap_or("")).unwrap_or(Dir::Right);
                if let Some(f) = focus {
                    self.move_to_output_dir(f, d);
                }
            }
            "layout" | "set-layout" => {
                let cur = self.workspaces[ws as usize - 1].layout.name();
                let next = match a0 {
                    "next" | "" => {
                        let i = layout::LAYOUTS.iter().position(|l| *l == cur).unwrap_or(0);
                        layout::LAYOUTS[(i + 1) % (layout::LAYOUTS.len() - 1)]
                    }
                    "prev" => {
                        let i = layout::LAYOUTS.iter().position(|l| *l == cur).unwrap_or(0);
                        let n = layout::LAYOUTS.len() - 1;
                        layout::LAYOUTS[(i + n - 1) % n]
                    }
                    l => l,
                };
                let next = next.to_string();
                self.set_layout(ws, &next);
            }
            "switch-layout" | "switchxkblayout" | "keyboard-layout" => crate::input::switch_kb_layout(self, a0),
            "scroll" | "scroll-view" => {
                let px = match a0 {
                    "left" | "up" => -self.cfg.scroller.wheel_scroll_step,
                    "right" | "down" => self.cfg.scroller.wheel_scroll_step,
                    v => v.parse().unwrap_or(0.0),
                };
                self.scroll_view(px * self.cfg.scroller.scroll_speed, true);
            }
            "wallpaper" | "set-wallpaper" => {
                let p = rest.to_string();
                crate::wallpaper::set_wallpaper(self, &p);
            }
            "wallpaper-random" | "wallpaper-next" | "wallpaper-prev" => {
                crate::wallpaper::cycle_wallpaper(self, name);
            }
            "we" | "wallpaper-engine" => {
                let r = rest.trim();
                let r = if r.starts_with("we:") { r.to_string() } else { format!("we:{r}") };
                crate::wallpaper::set_wallpaper(self, &r);
            }
            "we-random" | "we-next" | "we-prev" => self.we_cycle(name),
            "we-restart" => self.we_restart(),
            "toggle-keyboard-shortcuts-inhibit" => {
                self.inhibit_shortcuts = !self.inhibit_shortcuts;
                info!("перехват хоткеев приложениями: {}", self.inhibit_shortcuts);
            }
            "screenshot" | "screenshot-screen" | "screenshot-window" => crate::screenshot::take(self, name),
            "game-mode" | "gaming-mode" | "toggle-game-mode" => self.toggle_game_mode(),
            "toggle-overview" | "overview" => {
                // Overview: temporarily switch to the scroller view of all windows isn't implemented yet;
                // show the next workspace grid via layout toggle instead of doing nothing.
                info!("overview пока не реализован");
            }
            "show-hotkeys" | "show-hotkey-overlay" => {
                let list = self.binds.iter().map(|b| format!("{} → {}", b.text, b.action)).collect::<Vec<_>>().join("\n");
                let body = list.lines().take(40).collect::<Vec<_>>().join("\n");
                let _ = std::process::Command::new("notify-send")
                    .env("WAYLAND_DISPLAY", &self.socket_name)
                    .args(["-a", "FerriumWM", "Горячие клавиши", &body])
                    .spawn();
            }
            "power-off-monitors" => crate::backend::udev::power_off_monitors(self),
            "pseudo" | "toggle-column-tabbed-display" => {}
            "noop" | "none" | "" => {}
            other => {
                if !self.layout_cmd(other, &args) {
                    warn!("неизвестное действие: {action}");
                }
            }
        }
        crate::ipc::event(self, "action");
    }

    pub fn scroll_view(&mut self, px: f64, snap: bool) {
        let ws = self.active_ws();
        let wsi = ws as usize - 1;
        let (area, gap) = self.ws_area(ws);
        let vertical = self.cfg.scroller.direction == "vertical";
        let (dx, dy) = if vertical { (0.0, px) } else { (px, 0.0) };
        let focus = {
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            let l = &mut self.workspaces[wsi].layout;
            let f = l.scroll(&ctx, dx, dy);
            if snap { l.scroll_end(&ctx).or(f) } else { f }
        };
        self.relayout(ws);
        if let Some(f) = focus {
            if Some(f) != self.focus {
                // focus without re-centering the view
                self.focus = Some(f);
                for w in self.windows.values() {
                    w.window.set_activated(Some(w.id) == self.focus);
                    if let Some(t) = w.window.toplevel() {
                        if t.is_initial_configure_sent() {
                            t.send_pending_configure();
                        }
                    }
                }
                if self.layer_focus.is_none() {
                    let s = self.windows[&f].window.toplevel().map(|t| t.wl_surface().clone());
                    let kb = self.seat.get_keyboard().unwrap();
                    kb.set_focus(self, s, SERIAL_COUNTER.next_serial());
                }
                let wsp = &mut self.workspaces[wsi];
                wsp.focus_hist.retain(|x| *x != f);
                wsp.focus_hist.push(f);
            }
        }
    }

    /// Advance animations; returns true while anything is still moving.
    pub fn tick_animations(&mut self, now: Instant) -> bool {
        let mut busy = false;
        for w in self.windows.values_mut() {
            busy |= w.rect.animating(now);
            if let Some(o) = &w.open {
                if o.done(now) {
                    w.open = None;
                } else {
                    busy = true;
                }
            }
            if let Some(b) = &mut w.border {
                if let Some(p) = &b.prog {
                    if p.done(now) {
                        b.prog = None;
                    } else {
                        busy = true;
                    }
                }
            }
        }
        for wsp in &mut self.workspaces {
            busy |= wsp.view_x.animating(now);
            busy |= wsp.view_y.animating(now);
        }
        let mut finished = Vec::new();
        for o in &mut self.outputs {
            if let Some(s) = &o.switch {
                if s.prog.done(now) {
                    finished.push(s.from);
                    o.switch = None;
                } else {
                    busy = true;
                }
            }
        }
        let any_finished = !finished.is_empty();
        for ws in finished {
            self.map_ws_in_space(ws);
        }
        busy |= self.wallpaper.animating(now);
        let closing_before = self.closing.len();
        busy |= self.tick_closing(now);
        if any_finished || self.closing.len() != closing_before {
            self.cleanup_workspaces();
        }
        busy
    }
}

// ---------------------------------------------------------------- xdg shell

impl XdgShellHandler for Fe {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = self.next_id;
        self.next_id += 1;
        let window = Window::new_wayland_window(surface);
        self.windows.insert(
            id,
            FeWindow {
                id,
                window,
                ws: self.active_ws(),
                mapped: false,
                floating: false,
                float_rect: RectF::default(),
                fullscreen: false,
                maximized: false,
                urgent: false,
                rect: AnimRect::new(RectF::default()),
                open: None,
                rule: RuleFx::default(),
                app_id: String::new(),
                title: String::new(),
                border: None,
                pending_size: None,
                last_frame: None,
            },
        );
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            tracing::debug!("окно {id}: toplevel уничтожен → снимок");
            self.snapshot_closing(id);
            self.remove_window(id);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        // menus: close on outside click, keyboard navigation inside the menu
        use smithay::desktop::{PopupKeyboardGrab, PopupPointerGrab, PopupUngrabStrategy};
        let Some(seat) = Seat::<Fe>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else { return };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Fe>::from_resource(&seat) else { return };
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            let ptr = seat.get_pointer().unwrap();
            if ptr.has_grab(serial) {
                if let Some(start) = ptr.grab_start_data() {
                    let grab = crate::input::WinGrab::new(self, id, crate::input::GrabKind::Move, start);
                    ptr.set_grab(self, grab, serial, Focus::Clear);
                }
            }
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
        let Some(seat) = Seat::<Fe>::from_resource(&seat) else { return };
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            let ptr = seat.get_pointer().unwrap();
            if ptr.has_grab(serial) {
                if let Some(start) = ptr.grab_start_data() {
                    let grab = crate::input::WinGrab::new(self, id, crate::input::GrabKind::Resize, start);
                    ptr.set_grab(self, grab, serial, Focus::Clear);
                }
            }
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            let w = self.windows.get_mut(&id).unwrap();
            w.maximized = true;
            let ws = w.ws;
            self.relayout(ws);
        }
        if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            let w = self.windows.get_mut(&id).unwrap();
            w.maximized = false;
            let ws = w.ws;
            self.relayout(ws);
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<wl_output::WlOutput>) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            self.set_fullscreen(id, true);
        } else {
            surface.with_pending_state(|s| s.states.set(xdg_toplevel::State::Fullscreen));
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            self.set_fullscreen(id, false);
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            let (a, t) = toplevel_app_id_title(&surface);
            let w = self.windows.get_mut(&id).unwrap();
            w.app_id = a;
            w.title = t;
            self.toplevel_list_update(id);
            self.queue_redraw();
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.title_changed(surface);
    }
}

impl Fe {
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        // toplevel parent
        if let Some(id) = self.window_for_surface(&root) {
            let Some((oi, r)) = self.window_visual_rect(id) else { return };
            let out = self.output_rect_local(oi);
            let mut target = Rectangle::new(
                ((out.x - r.x) as i32, (out.y - r.y) as i32).into(),
                (out.w as i32, out.h as i32).into(),
            );
            target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
            popup.with_pending_state(|s| s.geometry = s.positioner.get_unconstrained_geometry(target));
            return;
        }
        // layer-shell parent
        if let Some(output) = self.output_for_layer(&root) {
            let map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(&root, smithay::desktop::WindowSurfaceType::TOPLEVEL) {
                let lg = map.layer_geometry(layer).unwrap_or_default();
                let og = self.space.output_geometry(&output).unwrap_or_default();
                let mut target = Rectangle::from_size(og.size);
                target.loc -= lg.loc;
                target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
                popup.with_pending_state(|s| s.geometry = s.positioner.get_unconstrained_geometry(target));
            }
        }
    }
}
