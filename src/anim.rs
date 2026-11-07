//! Animation curves (bezier, springs, presets) and animated values.

use std::time::{Duration, Instant};

use crate::config::Anim as AnimCfg;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    Bezier(f64, f64, f64, f64),
    /// damping ratio, stiffness (mass = 1)
    Spring(f64, f64),
}

impl Curve {
    pub fn parse(s: &str) -> Curve {
        let s = s.trim().to_ascii_lowercase();
        let args = |name: &str| -> Option<Vec<f64>> {
            let r = s.strip_prefix(name)?.trim().strip_prefix('(')?.strip_suffix(')')?;
            r.split(',').map(|x| x.trim().parse::<f64>().ok()).collect()
        };
        if let Some(a) = args("bezier").or_else(|| args("cubic-bezier")) {
            if a.len() == 4 {
                return Curve::Bezier(a[0], a[1], a[2], a[3]);
            }
        }
        if let Some(a) = args("spring") {
            if a.len() >= 2 {
                return Curve::Spring(a[0].max(0.05), a[1].max(1.0));
            }
        }
        match s.as_str() {
            "linear" => Curve::Linear,
            "ease" => Curve::Bezier(0.25, 0.1, 0.25, 1.0),
            "ease-in" => Curve::Bezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Curve::Bezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Curve::Bezier(0.42, 0.0, 0.58, 1.0),
            "ease-out-cubic" => Curve::Bezier(0.33, 1.0, 0.68, 1.0),
            "ease-out-quint" => Curve::Bezier(0.22, 1.0, 0.36, 1.0),
            "ease-out-expo" => Curve::Bezier(0.16, 1.0, 0.3, 1.0),
            "ease-out-back" => Curve::Bezier(0.34, 1.56, 0.64, 1.0),
            "ease-in-out-back" => Curve::Bezier(0.68, -0.6, 0.32, 1.6),
            "hyprland" | "overshot" => Curve::Bezier(0.05, 0.9, 0.1, 1.05),
            _ => Curve::Bezier(0.22, 1.0, 0.36, 1.0),
        }
    }

    /// Natural duration for springs (time to settle), None for time-based curves.
    fn spring_duration(self) -> Option<Duration> {
        let Curve::Spring(zeta, k) = self else { return None };
        let mut t = 0.0;
        let dt = 1.0 / 240.0;
        let mut settled_for = 0.0;
        while t < 5.0 {
            t += dt;
            let x = spring_pos(zeta, k, t);
            if (1.0 - x).abs() < 0.002 {
                settled_for += dt;
                if settled_for > 0.05 {
                    break;
                }
            } else {
                settled_for = 0.0;
            }
        }
        Some(Duration::from_secs_f64(t))
    }

    /// Progress (may overshoot) at normalized time `t` in [0, 1] for time curves,
    /// or at absolute seconds for springs.
    fn sample(self, t: f64, secs: f64) -> f64 {
        match self {
            Curve::Linear => t,
            Curve::Bezier(x1, y1, x2, y2) => bezier(x1, y1, x2, y2, t),
            Curve::Spring(z, k) => spring_pos(z, k, secs),
        }
    }
}

fn spring_pos(zeta: f64, k: f64, t: f64) -> f64 {
    let w0 = k.sqrt();
    if zeta < 1.0 {
        let wd = w0 * (1.0 - zeta * zeta).sqrt();
        1.0 - (-zeta * w0 * t).exp() * ((wd * t).cos() + (zeta * w0 / wd) * (wd * t).sin())
    } else if (zeta - 1.0).abs() < 1e-6 {
        1.0 - (-w0 * t).exp() * (1.0 + w0 * t)
    } else {
        let s = (zeta * zeta - 1.0).sqrt();
        let r1 = -w0 * (zeta - s);
        let r2 = -w0 * (zeta + s);
        let c2 = r1 / (r1 - r2);
        let c1 = 1.0 - c2;
        1.0 - (c1 * (r1 * t).exp() + c2 * (r2 * t).exp())
    }
}

fn bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bx = |t: f64| 3.0 * (1.0 - t).powi(2) * t * x1 + 3.0 * (1.0 - t) * t * t * x2 + t.powi(3);
    let by = |t: f64| 3.0 * (1.0 - t).powi(2) * t * y1 + 3.0 * (1.0 - t) * t * t * y2 + t.powi(3);
    // bisection on x(t)
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bx(mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    by((lo + hi) / 2.0)
}

/// Resolved animation parameters.
#[derive(Debug, Clone, Copy)]
pub struct AnimParams {
    pub curve: Curve,
    pub duration: Duration,
    pub enabled: bool,
}

impl AnimParams {
    pub fn from_cfg(c: &AnimCfg, global_enabled: bool, speed: f64) -> Self {
        let curve = Curve::parse(&c.curve);
        let speed = if speed <= 0.0 { 1.0 } else { speed };
        let base = curve
            .spring_duration()
            .unwrap_or_else(|| Duration::from_millis(c.duration_ms as u64));
        Self {
            curve,
            duration: base.div_f64(speed),
            enabled: global_enabled && c.enabled && c.duration_ms > 0,
        }
    }
    pub fn instant() -> Self {
        Self { curve: Curve::Linear, duration: Duration::ZERO, enabled: false }
    }
}

/// A 0→1 progress animation.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    start: Instant,
    params: AnimParams,
    /// spring time-scale (curve seconds per real second)
    time_scale: f64,
}

impl Progress {
    pub fn new(params: AnimParams, now: Instant) -> Self {
        let time_scale = match params.curve {
            Curve::Spring(..) => {
                let natural = params.curve.spring_duration().unwrap().as_secs_f64();
                natural / params.duration.as_secs_f64().max(1e-3)
            }
            _ => 1.0,
        };
        Self { start: now, params, time_scale }
    }
    pub fn value(&self, now: Instant) -> f64 {
        if !self.params.enabled || self.params.duration.is_zero() {
            return 1.0;
        }
        let el = now.saturating_duration_since(self.start).as_secs_f64();
        let t = (el / self.params.duration.as_secs_f64()).min(1.0);
        if t >= 1.0 {
            return 1.0;
        }
        self.params.curve.sample(t, el * self.time_scale)
    }
    pub fn done(&self, now: Instant) -> bool {
        !self.params.enabled || now.saturating_duration_since(self.start) >= self.params.duration
    }
}

/// An animated f64 that can be retargeted mid-flight.
#[derive(Debug, Clone, Copy)]
pub struct AnimF64 {
    from: f64,
    to: f64,
    prog: Option<Progress>,
}

impl AnimF64 {
    pub fn new(v: f64) -> Self {
        Self { from: v, to: v, prog: None }
    }
    pub fn value(&self, now: Instant) -> f64 {
        match &self.prog {
            Some(p) => self.from + (self.to - self.from) * p.value(now),
            None => self.to,
        }
    }
    pub fn target(&self) -> f64 {
        self.to
    }
    pub fn set(&mut self, to: f64, params: AnimParams, now: Instant) {
        if (to - self.to).abs() < 1e-9 {
            return;
        }
        let cur = self.value(now);
        self.from = cur;
        self.to = to;
        self.prog = if params.enabled { Some(Progress::new(params, now)) } else { None };
    }
    pub fn jump(&mut self, v: f64) {
        self.from = v;
        self.to = v;
        self.prog = None;
    }
    pub fn animating(&mut self, now: Instant) -> bool {
        if let Some(p) = &self.prog {
            if p.done(now) {
                self.prog = None;
                return false;
            }
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn lerp(self, o: RectF, t: f64) -> RectF {
        RectF {
            x: self.x + (o.x - self.x) * t,
            y: self.y + (o.y - self.y) * t,
            w: self.w + (o.w - self.w) * t,
            h: self.h + (o.h - self.h) * t,
        }
    }
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Animated rectangle (one shared progress, retargetable).
#[derive(Debug, Clone, Copy)]
pub struct AnimRect {
    from: RectF,
    to: RectF,
    prog: Option<Progress>,
}

impl AnimRect {
    pub fn new(r: RectF) -> Self {
        Self { from: r, to: r, prog: None }
    }
    pub fn value(&self, now: Instant) -> RectF {
        match &self.prog {
            Some(p) => self.from.lerp(self.to, p.value(now)),
            None => self.to,
        }
    }
    pub fn target(&self) -> RectF {
        self.to
    }
    pub fn set(&mut self, to: RectF, params: AnimParams, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.prog = if params.enabled { Some(Progress::new(params, now)) } else { None };
    }
    pub fn jump(&mut self, r: RectF) {
        self.from = r;
        self.to = r;
        self.prog = None;
    }
    pub fn animating(&mut self, now: Instant) -> bool {
        if let Some(p) = &self.prog {
            if p.done(now) {
                self.prog = None;
                return false;
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curves() {
        let b = Curve::parse("ease-out");
        assert!((b.sample(0.5, 0.0) - 0.5).abs() < 0.5);
        let s = Curve::parse("spring(0.8, 500)");
        assert!(s.spring_duration().unwrap().as_secs_f64() < 1.5);
        assert!((spring_pos(1.0, 400.0, 3.0) - 1.0).abs() < 1e-3);
        assert!((spring_pos(2.0, 400.0, 5.0) - 1.0).abs() < 1e-3);
    }
}
