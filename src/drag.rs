//! Dragging a window with the mouse (Super+LMB), niri-like: the window sticks
//! to the cursor, comes along when the workspace changes (keyboard, Super+wheel
//! or holding it at the top/bottom screen edge) and is dropped into the layout
//! of whatever workspace is shown on release.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use smithay::{
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point},
};

use crate::{
    anim::RectF,
    layout::WinId,
    state::Fe,
};

pub struct Drag {
    pub id: WinId,
    /// where the cursor holds the window, as a fraction of its size
    pub fx: f64,
    pub fy: f64,
    /// cursor, global logical
    pub pos: Point<f64, Logical>,
    /// tiled windows follow `pos`; floating ones move their float_rect
    pub tiled: bool,
    /// cursor pressed against the top (-1) / bottom (1) edge, with a generation
    /// so a stale timer does nothing
    edge: Option<(i32, u64)>,
    edge_gen: u64,
    /// recent cursor positions, for the speed a window is thrown with
    samples: VecDeque<(Instant, Point<f64, Logical>)>,
}

impl Drag {
    pub fn new(id: WinId, visual: RectF, out_origin: Point<f64, Logical>, pos: Point<f64, Logical>, tiled: bool) -> Self {
        let lx = pos.x - out_origin.x;
        let ly = pos.y - out_origin.y;
        let fx = ((lx - visual.x) / visual.w.max(1.0)).clamp(0.0, 1.0);
        let fy = ((ly - visual.y) / visual.h.max(1.0)).clamp(0.0, 1.0);
        Self { id, fx, fy, pos, tiled, edge: None, edge_gen: 0, samples: VecDeque::from([(Instant::now(), pos)]) }
    }

    fn sample(&mut self, pos: Point<f64, Logical>) {
        let now = Instant::now();
        self.samples.push_back((now, pos));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now - *t > Duration::from_millis(120)) {
            self.samples.pop_front();
        }
    }

    /// Cursor speed over the last ~80 ms, px/s; 0 when the mouse stopped
    /// before letting go.
    pub fn velocity(&self, now: Instant) -> (f64, f64) {
        let Some(&(t1, p1)) = self.samples.back() else { return (0.0, 0.0) };
        if now.saturating_duration_since(t1) > Duration::from_millis(60) {
            return (0.0, 0.0);
        }
        let Some(&(t0, p0)) = self.samples.iter().find(|(t, _)| t1.saturating_duration_since(*t) <= Duration::from_millis(80)) else {
            return (0.0, 0.0);
        };
        let dt = t1.saturating_duration_since(t0).as_secs_f64();
        if dt < 0.004 {
            return (0.0, 0.0);
        }
        ((p1.x - p0.x) / dt, (p1.y - p0.y) / dt)
    }

    fn rect_in(&self, out_origin: Point<f64, Logical>, size: (f64, f64)) -> RectF {
        let (lx, ly) = (self.pos.x - out_origin.x, self.pos.y - out_origin.y);
        RectF::new(lx - self.fx * size.0, ly - self.fy * size.1, size.0, size.1)
    }
}

impl Fe {
    pub fn dragged_window(&self) -> Option<WinId> {
        self.drag.as_ref().map(|d| d.id)
    }

    /// Output-local rect of a dragged tiled window: under the cursor, at the
    /// same relative spot it was grabbed, whatever size the layout gives it.
    pub fn drag_rect(&self, id: WinId, oi: usize, size: (f64, f64)) -> Option<RectF> {
        let d = self.drag.as_ref().filter(|d| d.id == id && d.tiled)?;
        Some(d.rect_in(self.output_geo(oi)?.loc.to_f64(), size))
    }

    /// Pointer moved while dragging: follow it, and switch workspaces when it is
    /// held at the top/bottom edge of the screen.
    pub fn drag_motion(&mut self, pos: Point<f64, Logical>) {
        let Some(d) = self.drag.as_mut() else { return };
        d.pos = pos;
        d.sample(pos);
        let delay = self.cfg.general.drag_edge_switch_ms;
        let dir = match self.output_at(pos).and_then(|oi| self.output_geo(oi)) {
            Some(g) if delay > 0 => {
                let ly = pos.y - g.loc.y as f64;
                if ly <= 2.0 {
                    Some(-1)
                } else if ly >= g.size.h as f64 - 3.0 {
                    Some(1)
                } else {
                    None
                }
            }
            _ => None,
        };
        let d = self.drag.as_mut().unwrap();
        match (dir, d.edge) {
            (None, _) => d.edge = None,
            (Some(a), Some((b, _))) if a == b => {}
            (Some(a), _) => {
                d.edge_gen += 1;
                let g = d.edge_gen;
                d.edge = Some((a, g));
                let first = Duration::from_millis(delay as u64);
                let again = Duration::from_millis((delay as u64 * 2).max(500));
                let _ = self.lh.insert_source(Timer::from_duration(first), move |_, _, fe| {
                    let live = fe.drag.as_ref().is_some_and(|d| d.edge == Some((a, g)));
                    if !live || !fe.drag_edge_switch(a) {
                        return TimeoutAction::Drop;
                    }
                    TimeoutAction::ToDuration(again)
                });
            }
        }
    }

    /// One workspace up/down with the dragged window. false when there is
    /// nowhere useful to go.
    fn drag_edge_switch(&mut self, dir: i32) -> bool {
        if self.drag.is_none() {
            return false;
        }
        let cur = self.active_ws();
        let target = cur as i64 + dir as i64;
        if target < 1 || (target as u32 > self.ws_count() && self.dynamic_ws()) {
            return false;
        }
        let target = target as u32;
        // the only window of the last workspace moving into the trailing empty
        // one changes nothing: the old one would just vanish
        if self.dynamic_ws() && target == self.ws_count() && self.windows.values().filter(|w| w.ws == cur).count() <= 1 {
            return false;
        }
        self.switch_workspace(target);
        self.active_ws() != cur
    }

    /// Workspace switch while dragging: take the window to `target` first.
    /// Returns the (possibly renumbered) target.
    pub fn drag_carry(&mut self, target: u32) -> u32 {
        let Some(id) = self.dragged_window() else { return target };
        let cur = self.active_ws();
        let on_cur = self.windows.get(&id).is_some_and(|w| w.ws == cur);
        if !on_cur || target == cur || self.output_of_ws(target).is_some() {
            return target;
        }
        self.move_to_workspace(id, target, false);
        self.windows.get(&id).map(|w| w.ws).unwrap_or(target)
    }

    /// Mouse released: put a tiled window back into the layout from where it
    /// was dropped (swapping with the window under the cursor), so it glides
    /// into its slot.
    pub fn drag_drop(&mut self) {
        let Some(d) = self.drag.take() else { return };
        self.queue_redraw();
        if !d.tiled {
            // sandbox: the window flies on with the speed of the hand
            self.physics_throw(d.id, d.velocity(Instant::now()));
            return;
        }
        let Some(w) = self.windows.get(&d.id) else { return };
        let ws = w.ws;
        let now = Instant::now();
        let cur = w.rect.value(now);
        let og = self.output_of_ws(ws).and_then(|oi| self.output_geo(oi));
        if let (Some(og), Some(wsp)) = (og, self.workspaces.get(ws as usize - 1)) {
            let (vx, vy) = (wsp.view_x.value(now), wsp.view_y.value(now));
            let r = d.rect_in(og.loc.to_f64(), (cur.w, cur.h));
            if let Some(w) = self.windows.get_mut(&d.id) {
                // +0.01: a jump to the exact target would not animate
                w.rect.jump(RectF::new(r.x + vx + 0.01, r.y + vy, r.w, r.h));
            }
            // dropped onto another tiled window = swap places
            let (x, y) = (d.pos.x - og.loc.x as f64, d.pos.y - og.loc.y as f64);
            let target = self.stacking(ws).into_iter().rev().filter(|t| *t != d.id).find(|t| {
                self.windows.get(t).is_some_and(|w| !w.floating) && self.window_visual_rect(*t).is_some_and(|(_, r)| r.contains(x, y))
            });
            if let Some(target) = target {
                self.workspaces[ws as usize - 1].layout.swap(d.id, target);
            }
        }
        self.relayout(ws);
    }
}

/// `fewm msg test-drag begin X Y | move X Y | drop`: drives the drag logic
/// without a mouse, for testing in a nested session.
pub fn test_drag(fe: &mut Fe, args: &str) -> String {
    let mut it = args.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let nums: Vec<f64> = it.filter_map(|s| s.parse().ok()).collect();
    let pos = Point::<f64, Logical>::from((nums.first().copied().unwrap_or(0.0), nums.get(1).copied().unwrap_or(0.0)));
    match cmd {
        "begin" => {
            let Some(id) = fe.window_under(pos) else { return "error: no window there".into() };
            let Some((oi, r)) = fe.window_visual_rect(id) else { return "error: not visible".into() };
            let og = fe.output_geo(oi).map(|g| g.loc.to_f64()).unwrap_or_default();
            let tiled = !fe.windows[&id].floating;
            fe.focus_window(Some(id), false);
            fe.drag = Some(Drag::new(id, r, og, pos, tiled));
            format!("dragging {id}")
        }
        "move" => {
            fe.drag_motion(pos);
            fe.queue_redraw();
            "ok".into()
        }
        "drop" => {
            fe.drag_drop();
            "ok".into()
        }
        _ => "usage: test-drag begin X Y | move X Y | drop".into(),
    }
}
