//! Sandbox mode (action `physics`, Super+B): the windows of a workspace leave
//! the layout and become physical bodies. Thrown with Super+LMB they keep their
//! momentum, bounce off the screen edges and off each other; with gravity on
//! they fall and pile up. Pressing it again glides every window back into the
//! layout slot it came from.
//!
//! Windows stay in their layout the whole time, only flagged `floating`, so the
//! layout keeps their slots and nothing has to be re-inserted on the way back.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::{
    anim::RectF,
    layout::{Ctx, WinId},
    state::Fe,
};

/// fixed simulation step: stable bounces whatever the frame rate
const STEP: f64 = 1.0 / 240.0;
/// below this a bounce or a collision just stops (no endless micro-jitter)
const REST: f64 = 25.0;
const MAX_SPEED: f64 = 9000.0;

pub struct Body {
    pub vx: f64,
    pub vy: f64,
    /// was floating before the sandbox (stays floating after it)
    was_floating: bool,
    /// not simulated before this (lets the shrink animation play)
    wake: Instant,
}

#[derive(Default)]
pub struct Physics {
    /// workspaces in sandbox mode
    pub sandbox: HashSet<u32>,
    pub bodies: HashMap<WinId, Body>,
    gravity: bool,
    /// last plain `physics` press (time, workspace), for the double tap
    last_tap: Option<(Instant, u32)>,
    last: Option<Instant>,
    acc: f64,
    seed: u64,
}

impl Physics {
    fn rand(&mut self) -> f64 {
        if self.seed == 0 {
            self.seed = Instant::now().elapsed().as_nanos() as u64 ^ 0x9E37_79B9_7F4A_7C15 ^ std::process::id() as u64;
            self.seed |= 1;
        }
        // xorshift64*
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        (self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Body as simulated during one tick.
struct Sim {
    id: WinId,
    r: RectF,
    vx: f64,
    vy: f64,
    /// inverse mass; 0 = moved by the mouse, pushes but can't be pushed
    im: f64,
    asleep: bool,
}

impl Fe {
    fn gravity_on(&self) -> bool {
        self.physics.gravity
    }

    /// `physics [toggle|on|off|gravity|space|shake]`. A plain press opens the
    /// sandbox with gravity or closes it; a second press right after it
    /// (Super+B B) switches to zero gravity instead.
    pub fn physics_action(&mut self, args: &[&str]) {
        let ws = self.active_ws();
        let on = self.physics.sandbox.contains(&ws);
        match args.first().copied().unwrap_or("toggle") {
            "on" if !on => self.sandbox_enter(ws),
            "off" if on => self.sandbox_exit(ws),
            "toggle" | "" => {
                let now = Instant::now();
                let window = Duration::from_millis(self.cfg.physics.double_tap_ms as u64);
                let double = self.physics.last_tap.is_some_and(|(t, w)| w == ws && now.saturating_duration_since(t) <= window);
                if double {
                    self.physics.last_tap = None;
                    self.physics_space(ws);
                } else {
                    self.physics.last_tap = Some((now, ws));
                    if on {
                        self.sandbox_exit(ws)
                    } else {
                        self.physics.gravity = true;
                        self.sandbox_enter(ws)
                    }
                }
            }
            "space" | "zero-g" => self.physics_space(ws),
            "gravity" => {
                self.physics.gravity = !self.physics.gravity;
                if !on && self.physics.gravity {
                    self.sandbox_enter(ws);
                }
            }
            "shake" | "kick" => {
                if !on {
                    self.sandbox_enter(ws);
                }
                self.physics_kick(ws, 1400.0, 1400.0);
            }
            _ => {}
        }
        self.queue_redraw();
    }

    /// Zero gravity: everything floats off in random directions and keeps
    /// bumping into each other.
    fn physics_space(&mut self, ws: u32) {
        self.physics.gravity = false;
        if !self.physics.sandbox.contains(&ws) {
            self.sandbox_enter(ws);
        }
        let s = self.cfg.physics.pop.max(300.0);
        self.physics_kick(ws, s, s);
    }

    /// Random push for every body of `ws`, speed in [min, min + spread).
    fn physics_kick(&mut self, ws: u32, min: f64, spread: f64) {
        let ids: Vec<WinId> = self.physics.bodies.keys().copied().filter(|id| self.windows.get(id).is_some_and(|w| w.ws == ws)).collect();
        let up = if self.gravity_on() { -900.0 } else { 0.0 };
        for id in ids {
            let a = self.physics.rand() * std::f64::consts::TAU;
            let s = min + self.physics.rand() * spread;
            if let Some(b) = self.physics.bodies.get_mut(&id) {
                b.vx = a.cos() * s;
                b.vy = a.sin() * s + up;
            }
        }
    }

    fn sandbox_enter(&mut self, ws: u32) {
        self.physics.sandbox.insert(ws);
        let ids: Vec<WinId> = self.windows.values().filter(|w| w.ws == ws).map(|w| w.id).collect();
        for id in ids {
            self.physics_adopt(id, true);
        }
        self.relayout(ws);
    }

    fn sandbox_exit(&mut self, ws: u32) {
        self.physics.sandbox.remove(&ws);
        let ids: Vec<WinId> = self.physics.bodies.keys().copied().filter(|id| self.windows.get(id).is_some_and(|w| w.ws == ws)).collect();
        for id in ids {
            self.physics_release(id);
        }
        // tiled ones glide back into their slots with the usual move animation
        self.relayout(ws);
    }

    /// Turn a window into a body: tiled ones float at their current spot,
    /// shrinking around their center.
    fn physics_adopt(&mut self, id: WinId, pop: bool) {
        if self.physics.bodies.contains_key(&id) {
            return;
        }
        let Some(w) = self.windows.get(&id) else { return };
        if !w.mapped || w.fullscreen || w.maximized {
            return;
        }
        let ws = w.ws;
        let was_floating = w.floating;
        let now = Instant::now();
        // where it is on screen right now (it may still be gliding back after
        // a quick off/on)
        let Some(wsp) = self.workspaces.get(ws as usize - 1) else { return };
        let mut vis = w.rect.value(now);
        if !w.floating {
            vis.x -= wsp.view_x.value(now);
            vis.y -= wsp.view_y.value(now);
        }
        let (area, _) = self.ws_area(ws);
        let mut wake = now;
        if !was_floating {
            let k = self.cfg.physics.shrink.clamp(0.15, 1.0);
            let (cx, cy) = vis.center();
            let (nw, nh) = ((vis.w * k).max(120.0).min(area.w), (vis.h * k).max(80.0).min(area.h));
            let w = self.windows.get_mut(&id).unwrap();
            w.floating = true;
            w.float_rect = RectF::new(cx - nw / 2.0, cy - nh / 2.0, nw, nh);
            let wsp = &mut self.workspaces[ws as usize - 1];
            wsp.float_stack.retain(|x| *x != id);
            wsp.float_stack.push(id);
            let p = self.anim(&self.cfg.animations.windows_move);
            if p.enabled && k < 1.0 {
                wake = now + p.duration.min(Duration::from_millis(600));
            }
        }
        let (mut vx, mut vy) = (0.0, 0.0);
        let speed = self.cfg.physics.pop.max(0.0);
        if pop && speed > 0.0 {
            // away from the middle of the screen, a bit randomized
            let (cx, cy) = vis.center();
            let (mx, my) = area.center();
            let base = if (cx - mx).abs() + (cy - my).abs() < 1.0 { self.physics.rand() * std::f64::consts::TAU } else { (cy - my).atan2(cx - mx) };
            let a = base + (self.physics.rand() - 0.5) * 1.2;
            let s = speed * (0.6 + self.physics.rand() * 0.8);
            vx = a.cos() * s;
            vy = a.sin() * s;
            if self.gravity_on() {
                vy -= speed;
            }
        }
        self.physics.bodies.insert(id, Body { vx, vy, was_floating, wake });
    }

    /// Body → normal window again (back into the layout if it was tiled).
    fn physics_release(&mut self, id: WinId) {
        let Some(b) = self.physics.bodies.remove(&id) else { return };
        if b.was_floating {
            return;
        }
        let Some(w) = self.windows.get_mut(&id) else { return };
        w.floating = false;
        let (ws, mapped) = (w.ws, w.mapped);
        let (area, gap) = self.ws_area(ws);
        let Some(wsp) = self.workspaces.get_mut(ws as usize - 1) else { return };
        wsp.float_stack.retain(|x| *x != id);
        // it may have left the layout meanwhile (moved to another workspace, hidden)
        if mapped && !wsp.layout.windows().contains(&id) {
            let focus = wsp.focus_hist.iter().rev().copied().find(|f| *f != id);
            let ctx = Ctx { cfg: &self.cfg, area, gap };
            wsp.layout.add(&ctx, id, focus, None);
        }
    }

    /// A window was let go with the mouse: it keeps flying with the speed it had.
    pub fn physics_throw(&mut self, id: WinId, (vx, vy): (f64, f64)) {
        let k = self.cfg.physics.throw;
        let Some(b) = self.physics.bodies.get_mut(&id) else { return };
        let (vx, vy) = (vx * k, vy * k);
        let s = (vx * vx + vy * vy).sqrt();
        let c = if s > MAX_SPEED { MAX_SPEED / s } else { 1.0 };
        b.vx = vx * c;
        b.vy = vy * c;
        b.wake = Instant::now();
        self.queue_redraw();
    }

    /// Advance the simulation to `now`. true while something moves.
    pub fn tick_physics(&mut self, now: Instant) -> bool {
        if self.physics.sandbox.is_empty() && self.physics.bodies.is_empty() {
            self.physics.last = None;
            return false;
        }
        // bodies whose window closed or left the sandbox
        let stale: Vec<(WinId, Option<u32>)> = self
            .physics
            .bodies
            .keys()
            .filter_map(|id| match self.windows.get(id) {
                None => Some((*id, None)),
                Some(w) if !self.physics.sandbox.contains(&w.ws) => Some((*id, Some(w.ws))),
                _ => None,
            })
            .collect();
        let mut relayout: HashSet<u32> = HashSet::new();
        for (id, ws) in stale {
            self.physics_release(id);
            relayout.extend(ws);
        }
        // newcomers: windows opened on (or moved to) a sandbox workspace
        let fresh: Vec<(WinId, u32)> = self
            .windows
            .values()
            .filter(|w| self.physics.sandbox.contains(&w.ws) && w.mapped && !w.fullscreen && !w.maximized && !self.physics.bodies.contains_key(&w.id))
            .map(|w| (w.id, w.ws))
            .collect();
        for (id, ws) in fresh {
            self.physics_adopt(id, false);
            relayout.insert(ws);
        }
        for ws in relayout {
            self.relayout(ws);
        }

        let dt = self.physics.last.map(|l| now.saturating_duration_since(l).as_secs_f64()).unwrap_or(0.0).min(0.05);
        self.physics.last = Some(now);
        self.physics.acc += dt;
        let mut steps = 0;
        while self.physics.acc >= STEP && steps < 16 {
            self.physics.acc -= STEP;
            steps += 1;
        }
        if steps == 16 {
            self.physics.acc = 0.0;
        }

        let mut busy = false;
        let shown: Vec<u32> = self.physics.sandbox.iter().copied().filter(|ws| self.output_of_ws(*ws).is_some()).collect();
        for ws in shown {
            busy |= self.simulate_ws(ws, steps, now);
        }
        busy
    }

    fn simulate_ws(&mut self, ws: u32, steps: usize, now: Instant) -> bool {
        let (area, _) = self.ws_area(ws);
        let held = self.drag.as_ref().map(|d| (d.id, d.velocity(now)));
        let mut sims: Vec<Sim> = Vec::new();
        let mut waiting = false;
        for (id, b) in &self.physics.bodies {
            let Some(w) = self.windows.get(id) else { continue };
            if w.ws != ws || !w.mapped || w.fullscreen || w.maximized {
                continue;
            }
            if b.wake > now {
                waiting = true;
                continue;
            }
            let r = w.float_rect;
            match held {
                Some((h, (hvx, hvy))) if h == *id => sims.push(Sim { id: *id, r, vx: hvx, vy: hvy, im: 0.0, asleep: false }),
                _ => sims.push(Sim { id: *id, r, vx: b.vx, vy: b.vy, im: 1.0 / (r.w * r.h).max(1.0), asleep: false }),
            }
        }
        if sims.is_empty() {
            return waiting;
        }
        let before: Vec<RectF> = sims.iter().map(|s| s.r).collect();
        let c = &self.cfg.physics;
        // zero gravity without inertia loss: perfectly elastic, nothing ever slows down
        let lossless = !self.gravity_on() && !c.space_inertia_loss;
        let (g, drag, bounce, friction) = if self.gravity_on() {
            (c.gravity, c.drag, c.bounce, c.friction)
        } else if lossless {
            (0.0, 0.0, 1.0, 0.0)
        } else {
            (0.0, c.space_drag, c.space_bounce, c.friction * 0.25)
        };
        for _ in 0..steps {
            step(&mut sims, area, g, drag.max(0.0), bounce.clamp(0.0, 1.0), friction.max(0.0), c.collisions, lossless);
        }

        let mut moving = waiting || held.is_some_and(|(h, _)| sims.iter().any(|s| s.id == h));
        let mut moved_any = false;
        for (s, b0) in sims.iter().zip(before) {
            let d = (s.r.x - b0.x).abs() + (s.r.y - b0.y).abs();
            // without inertia loss any speed counts: slow windows keep drifting
            let fast = if lossless { s.vx != 0.0 || s.vy != 0.0 } else { s.vx.abs() + s.vy.abs() > REST };
            if s.im > 0.0 {
                if let Some(b) = self.physics.bodies.get_mut(&s.id) {
                    // a body that barely moves for a whole tick goes to sleep
                    let asleep = !lossless && (s.asleep || (d < 0.05 && !fast));
                    b.vx = if asleep { 0.0 } else { s.vx };
                    b.vy = if asleep { 0.0 } else { s.vy };
                }
                if d > 0.001 {
                    if let Some(w) = self.windows.get_mut(&s.id) {
                        w.float_rect = s.r;
                        w.rect.jump(s.r);
                    }
                    moved_any = true;
                }
            }
            moving |= d > 0.05 || (fast && (steps == 0 || lossless));
        }
        if moved_any {
            self.map_ws_in_space(ws);
            self.queue_redraw();
        }
        moving
    }
}

/// One fixed step for every body of a workspace.
#[allow(clippy::too_many_arguments)]
fn step(b: &mut [Sim], area: RectF, g: f64, drag: f64, bounce: f64, friction: f64, collide: bool, lossless: bool) {
    let h = STEP;
    let air = (-drag * h).exp();
    for s in b.iter_mut().filter(|s| s.im > 0.0) {
        s.vy += g * h;
        s.vx *= air;
        s.vy *= air;
        s.r.x += s.vx * h;
        s.r.y += s.vy * h;
    }
    // a few relaxation passes: windows pushed into a wall push back the next one
    for _ in 0..4 {
        if collide {
            for i in 0..b.len() {
                for j in i + 1..b.len() {
                    let (l, r) = b.split_at_mut(j);
                    collide_pair(&mut l[i], &mut r[0], bounce, friction, h, lossless);
                }
            }
        }
        for s in b.iter_mut().filter(|s| s.im > 0.0) {
            walls(s, area, bounce, if g > 0.0 { friction } else { 0.0 }, h, lossless);
        }
    }
    for s in b.iter_mut().filter(|s| s.im > 0.0) {
        s.asleep = false;
        let v = (s.vx * s.vx + s.vy * s.vy).sqrt();
        if v > MAX_SPEED {
            s.vx *= MAX_SPEED / v;
            s.vy *= MAX_SPEED / v;
        }
    }
}

fn walls(s: &mut Sim, a: RectF, bounce: f64, floor_friction: f64, h: f64, lossless: bool) {
    let hit = |v: &mut f64| {
        *v = -*v * bounce;
        if !lossless && v.abs() < REST {
            *v = 0.0;
        }
    };
    if s.r.w >= a.w {
        s.r.x = a.x + (a.w - s.r.w) / 2.0;
        s.vx = 0.0;
    } else if s.r.x < a.x {
        s.r.x = a.x;
        if s.vx < 0.0 {
            hit(&mut s.vx);
        }
    } else if s.r.x + s.r.w > a.x + a.w {
        s.r.x = a.x + a.w - s.r.w;
        if s.vx > 0.0 {
            hit(&mut s.vx);
        }
    }
    if s.r.h >= a.h {
        s.r.y = a.y + (a.h - s.r.h) / 2.0;
        s.vy = 0.0;
    } else if s.r.y < a.y {
        s.r.y = a.y;
        if s.vy < 0.0 {
            hit(&mut s.vy);
        }
    } else if s.r.y + s.r.h > a.y + a.h {
        s.r.y = a.y + a.h - s.r.h;
        if s.vy > 0.0 {
            hit(&mut s.vy);
        }
        // sliding on the floor
        s.vx *= (-floor_friction * h).exp();
        if s.vx.abs() < REST * 0.2 {
            s.vx = 0.0;
        }
    }
}

/// Axis-aligned boxes: separate along the shallow axis, exchange momentum
/// along it (heavier = bigger window), rub along the other one.
fn collide_pair(a: &mut Sim, b: &mut Sim, bounce: f64, friction: f64, h: f64, lossless: bool) {
    let ox = (a.r.x + a.r.w).min(b.r.x + b.r.w) - a.r.x.max(b.r.x);
    let oy = (a.r.y + a.r.h).min(b.r.y + b.r.h) - a.r.y.max(b.r.y);
    if ox <= 0.0 || oy <= 0.0 {
        return;
    }
    let im = a.im + b.im;
    if im <= 0.0 {
        return;
    }
    let (ac, bc) = (a.r.center(), b.r.center());
    let x_axis = ox < oy;
    let (pen, sign) = if x_axis { (ox, if ac.0 < bc.0 { 1.0 } else { -1.0 }) } else { (oy, if ac.1 < bc.1 { 1.0 } else { -1.0 }) };
    // windows that start out overlapping slide apart instead of exploding
    let corr = pen.min(1800.0 * h);
    let (ka, kb) = (a.im / im, b.im / im);
    // relative velocity along the normal (a → b)
    let (va, vb, ta, tb) = if x_axis { (a.vx, b.vx, a.vy, b.vy) } else { (a.vy, b.vy, a.vx, b.vx) };
    let rel = (vb - va) * sign;
    let (mut na, mut nb) = (va, vb);
    if rel < 0.0 {
        let e = if !lossless && -rel < REST * 2.0 { 0.0 } else { bounce };
        let j = -(1.0 + e) * rel / im;
        na -= j * a.im * sign;
        nb += j * b.im * sign;
    }
    // rubbing: tangential speeds pull towards each other
    let f = 1.0 - (-friction * h).exp();
    let avg = if a.im == 0.0 { ta } else if b.im == 0.0 { tb } else { (ta * b.im + tb * a.im) / im };
    let (nta, ntb) = (ta + (avg - ta) * f * (a.im > 0.0) as u8 as f64, tb + (avg - tb) * f * (b.im > 0.0) as u8 as f64);
    if x_axis {
        a.r.x -= corr * ka * sign;
        b.r.x += corr * kb * sign;
        (a.vx, b.vx, a.vy, b.vy) = (na, nb, nta, ntb);
    } else {
        a.r.y -= corr * ka * sign;
        b.r.y += corr * kb * sign;
        (a.vy, b.vy, a.vx, b.vx) = (na, nb, nta, ntb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(id: WinId, x: f64, y: f64, vx: f64, vy: f64) -> Sim {
        Sim { id, r: RectF::new(x, y, 100.0, 100.0), vx, vy, im: 1.0 / 10000.0, asleep: false }
    }

    #[test]
    fn bounces_off_walls_and_stays_inside() {
        let area = RectF::new(0.0, 0.0, 1000.0, 800.0);
        let mut b = vec![sim(1, 850.0, 300.0, 3000.0, -2000.0)];
        for _ in 0..2000 {
            step(&mut b, area, 0.0, 0.9, 0.7, 4.0, true, false);
            let r = b[0].r;
            assert!(r.x >= -0.01 && r.x + r.w <= 1000.01 && r.y >= -0.01 && r.y + r.h <= 800.01);
        }
        assert!(b[0].vx.abs() + b[0].vy.abs() < 600.0, "drag slows it down");
    }

    #[test]
    fn gravity_settles_on_floor() {
        let area = RectF::new(0.0, 0.0, 1000.0, 800.0);
        let mut b = vec![sim(1, 100.0, 0.0, 0.0, 0.0), sim(2, 120.0, 200.0, 0.0, 0.0)];
        for _ in 0..240 * 6 {
            step(&mut b, area, 3200.0, 0.9, 0.7, 4.0, true, false);
        }
        for s in &b {
            assert!(s.vy.abs() < REST * 2.0, "settled: {}", s.vy);
        }
        // stacked: one on the floor, the other on top of it
        let (lo, hi) = if b[0].r.y > b[1].r.y { (&b[0], &b[1]) } else { (&b[1], &b[0]) };
        assert!((lo.r.y - 700.0).abs() < 2.0);
        assert!((hi.r.y - 600.0).abs() < 4.0, "on top: {}", hi.r.y);
    }

    #[test]
    fn head_on_collision_swaps_momentum() {
        let area = RectF::new(0.0, 0.0, 2000.0, 800.0);
        let mut b = vec![sim(1, 300.0, 300.0, 1000.0, 0.0), sim(2, 600.0, 300.0, 0.0, 0.0)];
        for _ in 0..60 {
            step(&mut b, area, 0.0, 0.0, 1.0, 0.0, true, false);
        }
        assert!(b[0].vx.abs() < 50.0 && b[1].vx > 900.0, "{} {}", b[0].vx, b[1].vx);
    }

    #[test]
    fn zero_g_without_inertia_loss_keeps_flying() {
        // slow and fast windows bouncing off walls and each other for 10 s
        let area = RectF::new(0.0, 0.0, 1000.0, 800.0);
        let mut b = vec![sim(1, 100.0, 100.0, 700.0, 300.0), sim(2, 600.0, 400.0, -400.0, 0.0), sim(3, 400.0, 650.0, 12.0, -9.0)];
        let speed = |b: &[Sim]| b.iter().map(|s| s.vx.abs() + s.vy.abs()).sum::<f64>();
        let e0: f64 = b.iter().map(|s| s.vx * s.vx + s.vy * s.vy).sum();
        for _ in 0..240 * 10 {
            step(&mut b, area, 0.0, 0.0, 1.0, 0.0, true, true);
        }
        let e1: f64 = b.iter().map(|s| s.vx * s.vx + s.vy * s.vy).sum();
        assert!((e1 / e0 - 1.0).abs() < 0.02, "energy kept: {e0} -> {e1}");
        assert!(speed(&b) > 0.0);
        for s in &b {
            assert!(s.vx != 0.0 || s.vy != 0.0, "nobody stops: {} {}", s.vx, s.vy);
        }
    }
}
