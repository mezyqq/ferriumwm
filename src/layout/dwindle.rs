//! Dwindle: binary space partitioning like Hyprland.

use super::{CmdResult, Ctx, Dir, Layout, WinId};
use crate::anim::RectF;

#[derive(Debug, Clone)]
enum Node {
    Leaf(WinId),
    Split {
        /// Some(true): side by side (split along x); Some(false): stacked; None: auto
        side_by_side: Option<bool>,
        ratio: f64,
        a: Box<Node>,
        b: Box<Node>,
    },
}

#[derive(Default)]
pub struct Dwindle {
    root: Option<Node>,
}

impl Node {
    fn contains(&self, id: WinId) -> bool {
        match self {
            Node::Leaf(w) => *w == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }
    fn first_leaf(&self) -> WinId {
        match self {
            Node::Leaf(w) => *w,
            Node::Split { a, .. } => a.first_leaf(),
        }
    }
    fn leaves(&self, out: &mut Vec<WinId>) {
        match self {
            Node::Leaf(w) => out.push(*w),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }
    fn leaf_mut(&mut self, id: WinId) -> Option<&mut Node> {
        match self {
            Node::Leaf(w) if *w == id => Some(self),
            Node::Leaf(_) => None,
            Node::Split { a, b, .. } => {
                if a.contains(id) {
                    a.leaf_mut(id)
                } else {
                    b.leaf_mut(id)
                }
            }
        }
    }
    /// Remove leaf `id`; returns the replacement for this node.
    fn remove(self, id: WinId) -> Option<Node> {
        match self {
            Node::Leaf(w) if w == id => None,
            Node::Leaf(_) => Some(self),
            Node::Split { side_by_side, ratio, a, b } => {
                if a.contains(id) {
                    match a.remove(id) {
                        None => Some(*b),
                        Some(na) => Some(Node::Split { side_by_side, ratio, a: Box::new(na), b }),
                    }
                } else if b.contains(id) {
                    match b.remove(id) {
                        None => Some(*a),
                        Some(nb) => Some(Node::Split { side_by_side, ratio, a, b: Box::new(nb) }),
                    }
                } else {
                    Some(Node::Split { side_by_side, ratio, a, b })
                }
            }
        }
    }
    fn swap_ids(&mut self, x: WinId, y: WinId) {
        match self {
            Node::Leaf(w) => {
                if *w == x {
                    *w = y
                } else if *w == y {
                    *w = x
                }
            }
            Node::Split { a, b, .. } => {
                a.swap_ids(x, y);
                b.swap_ids(x, y);
            }
        }
    }
}

fn auto_side(r: RectF, mult: f64) -> bool {
    r.w * mult.max(0.1) > r.h
}

fn arrange_node(n: &Node, r: RectF, gap: f64, mult: f64, out: &mut Vec<(WinId, RectF)>) {
    match n {
        Node::Leaf(w) => out.push((*w, r)),
        Node::Split { side_by_side, ratio, a, b } => {
            let sbs = side_by_side.unwrap_or_else(|| auto_side(r, mult));
            let ratio = ratio.clamp(0.05, 0.95);
            if sbs {
                let wa = (r.w - gap) * ratio;
                arrange_node(a, RectF::new(r.x, r.y, wa, r.h), gap, mult, out);
                arrange_node(b, RectF::new(r.x + wa + gap, r.y, r.w - wa - gap, r.h), gap, mult, out);
            } else {
                let ha = (r.h - gap) * ratio;
                arrange_node(a, RectF::new(r.x, r.y, r.w, ha), gap, mult, out);
                arrange_node(b, RectF::new(r.x, r.y + ha + gap, r.w, r.h - ha - gap), gap, mult, out);
            }
        }
    }
}

/// Walk to the leaf; on the way record (split node path, which side, its rect).
#[allow(clippy::too_many_arguments)]
fn resize_node(n: &mut Node, id: WinId, r: RectF, ws: RectF, gap: f64, mult: f64, dx: f64, dy: f64) -> (bool, bool) {
    // returns (handled_x, handled_y) so inner splits get priority
    let Node::Split { side_by_side, ratio, a, b } = n else { return (false, false) };
    let sbs = side_by_side.unwrap_or_else(|| auto_side(r, mult));
    let in_a = a.contains(id);
    let (ra, rb) = if sbs {
        let wa = (r.w - gap) * *ratio;
        (RectF::new(r.x, r.y, wa, r.h), RectF::new(r.x + wa + gap, r.y, r.w - wa - gap, r.h))
    } else {
        let ha = (r.h - gap) * *ratio;
        (RectF::new(r.x, r.y, r.w, ha), RectF::new(r.x, r.y + ha + gap, r.w, r.h - ha - gap))
    };
    let (mut hx, mut hy) = if in_a {
        resize_node(a, id, ra, ws, gap, mult, dx, dy)
    } else {
        resize_node(b, id, rb, ws, gap, mult, dx, dy)
    };
    let delta = if sbs { dx } else { dy };
    let handled = if sbs { hx } else { hy };
    if !handled && delta != 0.0 {
        // dx/dy are fractions of the workspace; convert to a ratio of this split
        let (total, ws_len) = if sbs { (r.w, ws.w) } else { (r.h, ws.h) };
        let d = (delta * ws_len / total.max(1.0)).clamp(-0.5, 0.5);
        *ratio = if in_a { *ratio + d } else { *ratio - d }.clamp(0.05, 0.95);
        if sbs {
            hx = true
        } else {
            hy = true
        }
    }
    (hx, hy)
}

impl Dwindle {
    fn mult(ctx: &Ctx) -> f64 {
        ctx.cfg.dwindle.split_width_multiplier
    }

    fn rects(&self, ctx: &Ctx) -> Vec<(WinId, RectF)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            arrange_node(root, ctx.area, ctx.gap, Self::mult(ctx), &mut out);
        }
        out
    }
}

impl Layout for Dwindle {
    fn name(&self) -> &'static str {
        "dwindle"
    }

    fn add(&mut self, ctx: &Ctx, id: WinId, focus: Option<WinId>, cursor: Option<(f64, f64)>) {
        let rects = self.rects(ctx);
        let Some(root) = &mut self.root else {
            self.root = Some(Node::Leaf(id));
            return;
        };
        let target = focus.filter(|f| root.contains(*f)).unwrap_or_else(|| {
            let mut l = Vec::new();
            root.leaves(&mut l);
            *l.last().unwrap()
        });
        let target_rect = rects.iter().find(|(w, _)| *w == target).map(|(_, r)| *r).unwrap_or(ctx.area);
        let cfg = &ctx.cfg.dwindle;
        let side_by_side = match cfg.split.as_str() {
            "horizontal" | "h" => Some(true),
            "vertical" | "v" => Some(false),
            _ => {
                if cfg.preserve_split {
                    Some(auto_side(target_rect, Self::mult(ctx)))
                } else {
                    None
                }
            }
        };
        let sbs = side_by_side.unwrap_or_else(|| auto_side(target_rect, Self::mult(ctx)));
        let before = match cfg.new_window_side.as_str() {
            "before" | "left" | "top" => true,
            "cursor" => cursor.is_some_and(|(x, y)| {
                if sbs { x < target_rect.x + target_rect.w / 2.0 } else { y < target_rect.y + target_rect.h / 2.0 }
            }),
            _ => false,
        };
        let smart = cfg.smart_split && cursor.is_some();
        let side_by_side = if smart {
            // pick the split by which edge of the tile the cursor is closest to
            let (x, y) = cursor.unwrap();
            let rx = ((x - target_rect.x) / target_rect.w - 0.5).abs();
            let ry = ((y - target_rect.y) / target_rect.h - 0.5).abs();
            Some(rx > ry)
        } else {
            side_by_side
        };
        let ratio = cfg.default_ratio;
        if let Some(leaf) = root.leaf_mut(target) {
            let old = std::mem::replace(leaf, Node::Leaf(0));
            let new = Node::Leaf(id);
            let (a, b) = if before { (new, old) } else { (old, new) };
            *leaf = Node::Split { side_by_side, ratio, a: Box::new(a), b: Box::new(b) };
        }
    }

    fn remove(&mut self, _ctx: &Ctx, id: WinId) {
        if let Some(root) = self.root.take() {
            self.root = root.remove(id);
        }
    }

    fn windows(&self) -> Vec<WinId> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut out);
        }
        out
    }

    fn arrange(&self, ctx: &Ctx) -> Vec<(WinId, RectF)> {
        let mut out = self.rects(ctx);
        if out.len() == 1 && ctx.cfg.dwindle.no_gaps_when_only {
            out[0].1 = ctx.area;
        }
        out
    }

    fn move_dir(&mut self, ctx: &Ctx, id: WinId, dir: Dir) -> bool {
        let rects = self.rects(ctx);
        if let Some(n) = super::geometric_neighbor(&rects, id, dir) {
            self.swap(id, n);
            true
        } else {
            false
        }
    }

    fn resize(&mut self, ctx: &Ctx, id: WinId, dx: f64, dy: f64) {
        let mult = Self::mult(ctx);
        if let Some(root) = &mut self.root {
            resize_node(root, id, ctx.area, ctx.area, ctx.gap, mult, dx, dy);
        }
    }

    fn swap(&mut self, a: WinId, b: WinId) {
        if let Some(r) = &mut self.root {
            r.swap_ids(a, b);
        }
    }

    fn cmd(&mut self, ctx: &Ctx, focus: Option<WinId>, cmd: &str, _args: &[&str]) -> CmdResult {
        let Some(f) = focus else { return CmdResult::Unhandled };
        match cmd {
            "toggle-split" | "togglesplit" => {
                let mult = Self::mult(ctx);
                fn walk(n: &mut Node, id: WinId, r: RectF, gap: f64, mult: f64) -> bool {
                    let Node::Split { side_by_side, a, b, ratio } = n else { return false };
                    let sbs = side_by_side.unwrap_or_else(|| auto_side(r, mult));
                    let direct = matches!(**a, Node::Leaf(w) if w == id) || matches!(**b, Node::Leaf(w) if w == id);
                    if direct {
                        *side_by_side = Some(!sbs);
                        return true;
                    }
                    let (ra, rb) = if sbs {
                        let wa = (r.w - gap) * *ratio;
                        (RectF::new(r.x, r.y, wa, r.h), RectF::new(r.x + wa + gap, r.y, r.w - wa - gap, r.h))
                    } else {
                        let ha = (r.h - gap) * *ratio;
                        (RectF::new(r.x, r.y, r.w, ha), RectF::new(r.x, r.y + ha + gap, r.w, r.h - ha - gap))
                    };
                    if a.contains(id) { walk(a, id, ra, gap, mult) } else { walk(b, id, rb, gap, mult) }
                }
                if let Some(root) = &mut self.root {
                    walk(root, f, ctx.area, ctx.gap, mult);
                }
                CmdResult::Done
            }
            "swap-split" | "swapsplit" => {
                fn walk(n: &mut Node, id: WinId) -> bool {
                    let Node::Split { a, b, .. } = n else { return false };
                    let direct = matches!(**a, Node::Leaf(w) if w == id) || matches!(**b, Node::Leaf(w) if w == id);
                    if direct {
                        std::mem::swap(a, b);
                        return true;
                    }
                    if a.contains(id) { walk(a, id) } else { walk(b, id) }
                }
                if let Some(root) = &mut self.root {
                    walk(root, f);
                }
                CmdResult::Done
            }
            "focus-first" | "focus-column-first" => {
                self.root.as_ref().map(|r| CmdResult::Focus(r.first_leaf())).unwrap_or(CmdResult::Done)
            }
            "focus-last" | "focus-column-last" => self
                .windows()
                .last()
                .map(|w| CmdResult::Focus(*w))
                .unwrap_or(CmdResult::Done),
            _ => CmdResult::Unhandled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn dwindle_basic() {
        let cfg = Config::default();
        let ctx = Ctx { cfg: &cfg, area: RectF::new(0.0, 0.0, 1000.0, 500.0), gap: 10.0 };
        let mut d = Dwindle::default();
        d.add(&ctx, 1, None, None);
        d.add(&ctx, 2, Some(1), None);
        d.add(&ctx, 3, Some(2), None);
        let r = d.arrange(&ctx);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].1.w, 495.0);
        assert_eq!(r[1].1.h, 245.0);
        assert_eq!(d.neighbor(&ctx, 1, Dir::Right), Some(2));
        d.resize(&ctx, 1, 0.1, 0.0);
        assert!(d.arrange(&ctx)[0].1.w > 495.0);
        d.remove(&ctx, 2);
        assert_eq!(d.windows(), vec![1, 3]);
    }
}
