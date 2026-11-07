//! Master/stack layout.

use super::{CmdResult, Ctx, Dir, Layout, WinId, split_sizes};
use crate::anim::RectF;
use crate::config::Config;

pub struct Master {
    wins: Vec<WinId>,
    ratio: f64,
    count: usize,
}

impl Master {
    pub fn new(cfg: &Config) -> Self {
        Self { wins: Vec::new(), ratio: cfg.master.master_ratio, count: cfg.master.master_count.max(1) as usize }
    }
}

impl Layout for Master {
    fn name(&self) -> &'static str {
        "master"
    }

    fn add(&mut self, ctx: &Ctx, id: WinId, focus: Option<WinId>, _cursor: Option<(f64, f64)>) {
        let m = &ctx.cfg.master;
        if m.new_is_master {
            self.wins.insert(0, id);
        } else if m.new_on_top {
            let idx = self.count.min(self.wins.len());
            self.wins.insert(idx, id);
        } else if let Some(i) = focus.and_then(|f| self.wins.iter().position(|w| *w == f)) {
            self.wins.insert((i + 1).max(self.count.min(self.wins.len())), id);
        } else {
            self.wins.push(id);
        }
    }

    fn remove(&mut self, _ctx: &Ctx, id: WinId) {
        self.wins.retain(|w| *w != id);
    }

    fn windows(&self) -> Vec<WinId> {
        self.wins.clone()
    }

    fn arrange(&self, ctx: &Ctx) -> Vec<(WinId, RectF)> {
        let a = ctx.area;
        let g = ctx.gap;
        let n = self.wins.len();
        if n == 0 {
            return Vec::new();
        }
        let mc = self.count.min(n);
        let masters = &self.wins[..mc];
        let stack = &self.wins[mc..];
        let mut out = Vec::new();
        let orient = ctx.cfg.master.orientation.as_str();
        if stack.is_empty() {
            let r = a;
            let hs = split_sizes(r.h, g, &vec![1.0; mc]);
            let mut y = r.y;
            for (w, h) in masters.iter().zip(hs) {
                out.push((*w, RectF::new(r.x, y, r.w, h)));
                y += h + g;
            }
            return out;
        }
        let horizontal = matches!(orient, "left" | "right" | "center");
        let main_len = if horizontal { a.w } else { a.h };
        let master_len = (main_len - g) * self.ratio.clamp(0.05, 0.95);
        let stack_len = main_len - g - master_len;

        let place = |ids: &[WinId], r: RectF, along_y: bool, out: &mut Vec<(WinId, RectF)>| {
            let sizes = split_sizes(if along_y { r.h } else { r.w }, g, &vec![1.0; ids.len()]);
            let mut p = if along_y { r.y } else { r.x };
            for (w, s) in ids.iter().zip(sizes) {
                let rr = if along_y { RectF::new(r.x, p, r.w, s) } else { RectF::new(p, r.y, s, r.h) };
                out.push((*w, rr));
                p += s + g;
            }
        };

        match orient {
            "center" if stack.len() >= 2 => {
                let side = (a.w - master_len - 2.0 * g) / 2.0;
                let mr = RectF::new(a.x + side + g, a.y, master_len, a.h);
                place(masters, mr, true, &mut out);
                let mut left = Vec::new();
                let mut right = Vec::new();
                for (i, w) in stack.iter().enumerate() {
                    if i % 2 == 0 {
                        right.push(*w)
                    } else {
                        left.push(*w)
                    }
                }
                place(&left, RectF::new(a.x, a.y, side, a.h), true, &mut out);
                place(&right, RectF::new(a.x + side + g + master_len + g, a.y, side, a.h), true, &mut out);
            }
            "right" => {
                place(masters, RectF::new(a.x + stack_len + g, a.y, master_len, a.h), true, &mut out);
                place(stack, RectF::new(a.x, a.y, stack_len, a.h), true, &mut out);
            }
            "top" => {
                place(masters, RectF::new(a.x, a.y, a.w, master_len), false, &mut out);
                place(stack, RectF::new(a.x, a.y + master_len + g, a.w, stack_len), false, &mut out);
            }
            "bottom" => {
                place(masters, RectF::new(a.x, a.y + stack_len + g, a.w, master_len), false, &mut out);
                place(stack, RectF::new(a.x, a.y, a.w, stack_len), false, &mut out);
            }
            _ => {
                place(masters, RectF::new(a.x, a.y, master_len, a.h), true, &mut out);
                place(stack, RectF::new(a.x + master_len + g, a.y, stack_len, a.h), true, &mut out);
            }
        }
        out
    }

    fn move_dir(&mut self, ctx: &Ctx, id: WinId, dir: Dir) -> bool {
        let rects = self.arrange(ctx);
        if let Some(n) = super::geometric_neighbor(&rects, id, dir) {
            self.swap(id, n);
            return true;
        }
        false
    }

    fn resize(&mut self, ctx: &Ctx, id: WinId, dx: f64, dy: f64) {
        let horizontal = matches!(ctx.cfg.master.orientation.as_str(), "left" | "right" | "center");
        let d = if horizontal { dx } else { dy };
        let is_master = self.wins.iter().position(|w| *w == id).is_some_and(|i| i < self.count);
        let sign = match ctx.cfg.master.orientation.as_str() {
            "right" | "bottom" => -1.0,
            _ => 1.0,
        };
        let d = if is_master { d * sign } else { d * sign };
        self.ratio = (self.ratio + d).clamp(0.05, 0.95);
    }

    fn swap(&mut self, a: WinId, b: WinId) {
        let ia = self.wins.iter().position(|w| *w == a);
        let ib = self.wins.iter().position(|w| *w == b);
        if let (Some(ia), Some(ib)) = (ia, ib) {
            self.wins.swap(ia, ib);
        }
    }

    fn cmd(&mut self, _ctx: &Ctx, focus: Option<WinId>, cmd: &str, _args: &[&str]) -> CmdResult {
        match cmd {
            "swap-with-master" | "swapwithmaster" => {
                if let (Some(f), Some(m)) = (focus, self.wins.first().copied()) {
                    if f == m {
                        if self.wins.len() > 1 {
                            self.wins.swap(0, 1);
                        }
                    } else {
                        self.swap(f, m);
                    }
                }
                CmdResult::Done
            }
            "focus-master" | "focus-column-first" | "focus-first" => {
                self.wins.first().map(|w| CmdResult::Focus(*w)).unwrap_or(CmdResult::Done)
            }
            "focus-column-last" | "focus-last" => {
                self.wins.last().map(|w| CmdResult::Focus(*w)).unwrap_or(CmdResult::Done)
            }
            "add-master" => {
                self.count = (self.count + 1).min(self.wins.len().max(1));
                CmdResult::Done
            }
            "remove-master" => {
                self.count = self.count.saturating_sub(1).max(1);
                CmdResult::Done
            }
            _ => CmdResult::Unhandled,
        }
    }
}

