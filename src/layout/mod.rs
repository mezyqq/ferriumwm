//! Tiling layouts. Every layout works in workspace-local coordinates.

pub mod dwindle;
pub mod master;
pub mod scroller;

use crate::anim::RectF;
use crate::config::Config;

pub type WinId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "l" | "left" | "west" => Dir::Left,
            "r" | "right" | "east" => Dir::Right,
            "u" | "up" | "top" | "north" => Dir::Up,
            "d" | "down" | "bottom" | "south" => Dir::Down,
            _ => return None,
        })
    }
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
    pub fn positive(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    /// usable area (outer gaps and exclusive zones already removed)
    pub area: RectF,
    pub gap: f64,
}

pub enum CmdResult {
    Unhandled,
    Done,
    Focus(WinId),
}

pub trait Layout {
    fn name(&self) -> &'static str;
    fn add(&mut self, ctx: &Ctx, id: WinId, focus: Option<WinId>, cursor: Option<(f64, f64)>);
    fn remove(&mut self, ctx: &Ctx, id: WinId);
    fn windows(&self) -> Vec<WinId>;
    fn arrange(&self, ctx: &Ctx) -> Vec<(WinId, RectF)>;
    /// Scroll offset of the view (for scrolling layouts).
    fn view_offset(&self, _ctx: &Ctx) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn focus_changed(&mut self, _ctx: &Ctx, _id: WinId) {}
    fn neighbor(&self, ctx: &Ctx, id: WinId, dir: Dir) -> Option<WinId> {
        geometric_neighbor(&self.arrange(ctx), id, dir)
    }
    /// Move the window in `dir`. Returns false when already at the edge.
    fn move_dir(&mut self, ctx: &Ctx, id: WinId, dir: Dir) -> bool;
    fn resize(&mut self, ctx: &Ctx, id: WinId, dx: f64, dy: f64);
    fn swap(&mut self, a: WinId, b: WinId);
    /// Scroll the view by pixels. Returns window that should be focused (scroller).
    fn scroll(&mut self, _ctx: &Ctx, _dx: f64, _dy: f64) -> Option<WinId> {
        None
    }
    /// Finish an interactive scroll (snap).
    fn scroll_end(&mut self, _ctx: &Ctx) -> Option<WinId> {
        None
    }
    /// Width hint for the next added window (scroller column width rule).
    fn set_next_width(&mut self, _w: f64) {}
    fn cmd(&mut self, _ctx: &Ctx, _focus: Option<WinId>, _cmd: &str, _args: &[&str]) -> CmdResult {
        CmdResult::Unhandled
    }
}

pub fn make(name: &str, cfg: &Config) -> Box<dyn Layout> {
    match name {
        "scroller" | "scrolling" | "niri" => Box::new(scroller::Scroller::new(cfg)),
        "master" | "master-stack" => Box::new(master::Master::new(cfg)),
        "floating" | "float" => Box::new(Floating::default()),
        _ => Box::new(dwindle::Dwindle::default()),
    }
}

pub const LAYOUTS: &[&str] = &["dwindle", "master", "scroller", "floating"];

pub fn geometric_neighbor(rects: &[(WinId, RectF)], id: WinId, dir: Dir) -> Option<WinId> {
    let (_, cur) = rects.iter().find(|(w, _)| *w == id)?;
    let (cx, cy) = cur.center();
    let mut best: Option<(f64, WinId)> = None;
    for (w, r) in rects {
        if *w == id {
            continue;
        }
        let (x, y) = r.center();
        let ok = match dir {
            Dir::Left => r.x + r.w <= cur.x + 1.0,
            Dir::Right => r.x >= cur.x + cur.w - 1.0,
            Dir::Up => r.y + r.h <= cur.y + 1.0,
            Dir::Down => r.y >= cur.y + cur.h - 1.0,
        };
        if !ok {
            continue;
        }
        // overlap on the perpendicular axis is strongly preferred
        let overlap = if dir.horizontal() {
            (cur.y + cur.h).min(r.y + r.h) - cur.y.max(r.y)
        } else {
            (cur.x + cur.w).min(r.x + r.w) - cur.x.max(r.x)
        };
        let (main, perp) = if dir.horizontal() { ((x - cx).abs(), (y - cy).abs()) } else { ((y - cy).abs(), (x - cx).abs()) };
        let score = main + perp * 0.5 + if overlap > 0.0 { 0.0 } else { 100000.0 };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, *w));
        }
    }
    best.map(|(_, w)| w)
}

/// All windows float; nothing is tiled.
#[derive(Default)]
pub struct Floating {
    wins: Vec<WinId>,
}

impl Layout for Floating {
    fn name(&self) -> &'static str {
        "floating"
    }
    fn add(&mut self, _: &Ctx, id: WinId, _: Option<WinId>, _: Option<(f64, f64)>) {
        self.wins.push(id);
    }
    fn remove(&mut self, _: &Ctx, id: WinId) {
        self.wins.retain(|w| *w != id);
    }
    fn windows(&self) -> Vec<WinId> {
        self.wins.clone()
    }
    fn arrange(&self, _: &Ctx) -> Vec<(WinId, RectF)> {
        Vec::new()
    }
    fn move_dir(&mut self, _: &Ctx, _: WinId, _: Dir) -> bool {
        false
    }
    fn resize(&mut self, _: &Ctx, _: WinId, _: f64, _: f64) {}
    fn swap(&mut self, _: WinId, _: WinId) {}
}

/// Split `total` into `n` sizes by weights with `gap` between.
pub fn split_sizes(total: f64, gap: f64, weights: &[f64]) -> Vec<f64> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let avail = (total - gap * (n as f64 - 1.0)).max(1.0);
    let sum: f64 = weights.iter().map(|w| w.max(0.01)).sum();
    weights.iter().map(|w| avail * w.max(0.01) / sum).collect()
}
