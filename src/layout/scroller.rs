//! Scroller: an infinite strip of columns (niri-style). Horizontal or vertical.

use super::{CmdResult, Ctx, Dir, Layout, WinId, split_sizes};
use crate::anim::RectF;
use crate::config::Config;

#[derive(Debug, Clone)]
struct Column {
    wins: Vec<WinId>,
    heights: Vec<f64>,
    width: f64,
    active: usize,
    full: bool,
}

impl Column {
    fn new(id: WinId, width: f64) -> Self {
        Self { wins: vec![id], heights: vec![1.0], width, active: 0, full: false }
    }
}

pub struct Scroller {
    cols: Vec<Column>,
    active: usize,
    view: f64,
    /// Next column width override for the next added window.
    pub next_width: Option<f64>,
}

/// Coordinates in "main/cross" space so vertical mode reuses the same logic.
struct Axes {
    vertical: bool,
    main_pos: f64,
    main_len: f64,
    cross_pos: f64,
    cross_len: f64,
}

impl Axes {
    fn new(ctx: &Ctx) -> Self {
        let vertical = ctx.cfg.scroller.direction == "vertical";
        let a = ctx.area;
        if vertical {
            Axes { vertical, main_pos: a.y, main_len: a.h, cross_pos: a.x, cross_len: a.w }
        } else {
            Axes { vertical, main_pos: a.x, main_len: a.w, cross_pos: a.y, cross_len: a.h }
        }
    }
    fn rect(&self, main: f64, main_len: f64, cross: f64, cross_len: f64) -> RectF {
        if self.vertical {
            RectF::new(cross, main, cross_len, main_len)
        } else {
            RectF::new(main, cross, main_len, cross_len)
        }
    }
    /// Map a screen direction to (along_main, positive).
    fn dir(&self, d: Dir) -> (bool, bool) {
        let along_main = if self.vertical { !d.horizontal() } else { d.horizontal() };
        (along_main, d.positive())
    }
}

fn parse_amount(s: &str, cur: f64, main_len: f64) -> Option<f64> {
    let s = s.trim().trim_matches('"');
    let (rel, body) = match s.chars().next()? {
        '+' => (1.0, &s[1..]),
        '-' => (-1.0, &s[1..]),
        _ => (0.0, s),
    };
    let v = if let Some(p) = body.strip_suffix('%') {
        p.trim().parse::<f64>().ok()? / 100.0
    } else {
        let n = body.parse::<f64>().ok()?;
        if n > 1.0 { n / main_len.max(1.0) } else { n }
    };
    Some(if rel == 0.0 { v } else { cur + rel * v })
}

impl Scroller {
    pub fn new(_cfg: &Config) -> Self {
        Self { cols: Vec::new(), active: 0, view: 0.0, next_width: None }
    }

    fn col_px(&self, c: &Column, ax: &Axes, gap: f64) -> f64 {
        if c.full { ax.main_len } else { (c.width * (ax.main_len + gap) - gap).max(20.0) }
    }

    fn col_starts(&self, ax: &Axes, gap: f64) -> Vec<(f64, f64)> {
        let mut x = 0.0;
        self.cols
            .iter()
            .map(|c| {
                let w = self.col_px(c, ax, gap);
                let r = (x, w);
                x += w + gap;
                r
            })
            .collect()
    }

    fn find(&self, id: WinId) -> Option<(usize, usize)> {
        self.cols.iter().enumerate().find_map(|(ci, c)| c.wins.iter().position(|w| *w == id).map(|wi| (ci, wi)))
    }

    fn fit(&mut self, ctx: &Ctx) {
        let ax = Axes::new(ctx);
        let starts = self.col_starts(&ax, ctx.gap);
        let Some(&(s, w)) = starts.get(self.active) else {
            self.view = 0.0;
            return;
        };
        let center = |v: &mut f64| *v = s + w / 2.0 - ax.main_len / 2.0;
        let sc = &ctx.cfg.scroller;
        if self.cols.len() == 1 && sc.center_single_column {
            center(&mut self.view);
            return;
        }
        match sc.center_focused.as_str() {
            "always" => center(&mut self.view),
            "on-overflow" => {
                if s < self.view || s + w > self.view + ax.main_len {
                    center(&mut self.view)
                }
            }
            _ => {
                if s < self.view {
                    self.view = s;
                } else if s + w > self.view + ax.main_len {
                    self.view = s + w - ax.main_len;
                }
            }
        }
    }

    fn active_win(&self) -> Option<WinId> {
        let c = self.cols.get(self.active)?;
        c.wins.get(c.active).copied()
    }

    fn focus_col(&self, i: usize) -> CmdResult {
        self.cols.get(i).and_then(|c| c.wins.get(c.active)).map(|w| CmdResult::Focus(*w)).unwrap_or(CmdResult::Done)
    }

    fn presets(ctx: &Ctx, height: bool) -> Vec<f64> {
        let p = if height { &ctx.cfg.scroller.window_height_presets } else { &ctx.cfg.scroller.column_width_presets };
        if p.is_empty() { vec![0.3333, 0.5, 0.6667, 1.0] } else { p.clone() }
    }

    /// Column whose center is closest to the view center.
    fn col_at_view_center(&self, ctx: &Ctx) -> Option<usize> {
        let ax = Axes::new(ctx);
        let mid = self.view + ax.main_len / 2.0;
        self.col_starts(&ax, ctx.gap)
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let da = (a.1.0 + a.1.1 / 2.0 - mid).abs();
                let db = (b.1.0 + b.1.1 / 2.0 - mid).abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
    }

    fn expel(&mut self, ctx: &Ctx, id: WinId, positive: bool) {
        let Some((ci, wi)) = self.find(id) else { return };
        if self.cols[ci].wins.len() < 2 {
            return;
        }
        let col = &mut self.cols[ci];
        col.wins.remove(wi);
        col.heights.remove(wi);
        col.active = col.active.min(col.wins.len() - 1);
        let width = ctx.cfg.scroller.column_width;
        let at = if positive { ci + 1 } else { ci };
        self.cols.insert(at, Column::new(id, width));
        self.active = at;
    }

    fn consume_into(&mut self, id: WinId, positive: bool, max: u32) {
        let Some((ci, wi)) = self.find(id) else { return };
        let target = if positive { ci + 1 } else { ci.wrapping_sub(1) };
        if target >= self.cols.len() {
            return;
        }
        if max > 0 && self.cols[target].wins.len() >= max as usize {
            return;
        }
        let h = self.cols[ci].heights.remove(wi);
        self.cols[ci].wins.remove(wi);
        let tc = &mut self.cols[target];
        tc.wins.push(id);
        tc.heights.push(h);
        tc.active = tc.wins.len() - 1;
        if self.cols[ci].wins.is_empty() {
            self.cols.remove(ci);
            self.active = if target > ci { target - 1 } else { target };
        } else {
            let c = &mut self.cols[ci];
            c.active = c.active.min(c.wins.len() - 1);
            self.active = target;
        }
    }
}

impl Layout for Scroller {
    fn name(&self) -> &'static str {
        "scroller"
    }

    fn add(&mut self, ctx: &Ctx, id: WinId, focus: Option<WinId>, _cursor: Option<(f64, f64)>) {
        let width = self.next_width.take().unwrap_or(ctx.cfg.scroller.column_width).clamp(0.05, 1.0);
        let col = Column::new(id, width);
        let at = match ctx.cfg.scroller.new_column_position.as_str() {
            "end" => self.cols.len(),
            "start" => 0,
            _ => focus.and_then(|f| self.find(f)).map(|(ci, _)| ci + 1).unwrap_or(self.cols.len()),
        };
        self.cols.insert(at, col);
        self.active = at;
        self.fit(ctx);
    }

    fn remove(&mut self, ctx: &Ctx, id: WinId) {
        let Some((ci, wi)) = self.find(id) else { return };
        let c = &mut self.cols[ci];
        c.wins.remove(wi);
        c.heights.remove(wi);
        if c.wins.is_empty() {
            self.cols.remove(ci);
            if self.active > ci || self.active >= self.cols.len() {
                self.active = self.active.saturating_sub(1);
            }
        } else {
            c.active = c.active.min(c.wins.len() - 1);
        }
        self.fit(ctx);
    }

    fn windows(&self) -> Vec<WinId> {
        self.cols.iter().flat_map(|c| c.wins.iter().copied()).collect()
    }

    fn arrange(&self, ctx: &Ctx) -> Vec<(WinId, RectF)> {
        let ax = Axes::new(ctx);
        let starts = self.col_starts(&ax, ctx.gap);
        let mut out = Vec::new();
        for (c, (s, w)) in self.cols.iter().zip(starts) {
            let hs = split_sizes(ax.cross_len, ctx.gap, &c.heights);
            let mut y = ax.cross_pos;
            for (win, h) in c.wins.iter().zip(hs) {
                out.push((*win, ax.rect(ax.main_pos + s, w, y, h)));
                y += h + ctx.gap;
            }
        }
        out
    }

    fn view_offset(&self, ctx: &Ctx) -> (f64, f64) {
        if ctx.cfg.scroller.direction == "vertical" { (0.0, self.view) } else { (self.view, 0.0) }
    }

    fn focus_changed(&mut self, ctx: &Ctx, id: WinId) {
        if let Some((ci, wi)) = self.find(id) {
            self.active = ci;
            self.cols[ci].active = wi;
            self.fit(ctx);
        }
    }

    fn neighbor(&self, ctx: &Ctx, id: WinId, dir: Dir) -> Option<WinId> {
        let ax = Axes::new(ctx);
        let (ci, wi) = self.find(id)?;
        let (along_main, pos) = ax.dir(dir);
        if along_main {
            let ni = if pos { ci + 1 } else { ci.checked_sub(1)? };
            let c = self.cols.get(ni)?;
            c.wins.get(c.active).copied()
        } else {
            let c = &self.cols[ci];
            let nw = if pos { wi + 1 } else { wi.checked_sub(1)? };
            c.wins.get(nw).copied()
        }
    }

    fn move_dir(&mut self, ctx: &Ctx, id: WinId, dir: Dir) -> bool {
        let ax = Axes::new(ctx);
        let Some((ci, wi)) = self.find(id) else { return false };
        let (along_main, pos) = ax.dir(dir);
        if along_main {
            let ni = if pos { ci + 1 } else { match ci.checked_sub(1) { Some(i) => i, None => return false } };
            if ni >= self.cols.len() {
                return false;
            }
            self.cols.swap(ci, ni);
            self.active = ni;
        } else {
            let c = &mut self.cols[ci];
            let nw = if pos { wi + 1 } else { match wi.checked_sub(1) { Some(i) => i, None => return false } };
            if nw >= c.wins.len() {
                return false;
            }
            c.wins.swap(wi, nw);
            c.heights.swap(wi, nw);
            c.active = nw;
        }
        self.fit(ctx);
        true
    }

    fn resize(&mut self, ctx: &Ctx, id: WinId, dx: f64, dy: f64) {
        let ax = Axes::new(ctx);
        let (dmain, dcross) = if ax.vertical { (dy, dx) } else { (dx, dy) };
        let Some((ci, wi)) = self.find(id) else { return };
        let c = &mut self.cols[ci];
        if dmain != 0.0 {
            c.full = false;
            c.width = (c.width + dmain).clamp(0.05, 1.0);
        }
        if dcross != 0.0 && c.wins.len() > 1 {
            let sum: f64 = c.heights.iter().sum();
            let n = c.heights.len() as f64;
            c.heights[wi] = (c.heights[wi] + dcross * sum * n / n).max(0.1);
        }
        self.fit(ctx);
    }

    fn swap(&mut self, a: WinId, b: WinId) {
        let (Some((ca, wa)), Some((cb, wb))) = (self.find(a), self.find(b)) else { return };
        self.cols[ca].wins[wa] = b;
        self.cols[cb].wins[wb] = a;
    }

    fn scroll(&mut self, ctx: &Ctx, dx: f64, dy: f64) -> Option<WinId> {
        let ax = Axes::new(ctx);
        let d = if ax.vertical { if dy != 0.0 { dy } else { dx } } else if dx != 0.0 { dx } else { dy };
        let starts = self.col_starts(&ax, ctx.gap);
        let total = starts.last().map(|(s, w)| s + w).unwrap_or(0.0);
        self.view = (self.view + d).clamp(-ax.main_len * 0.5, (total - ax.main_len * 0.5).max(0.0));
        if ctx.cfg.scroller.focus_follows_view {
            if let Some(i) = self.col_at_view_center(ctx) {
                if i != self.active {
                    self.active = i;
                    return self.active_win();
                }
            }
        }
        None
    }

    fn scroll_end(&mut self, ctx: &Ctx) -> Option<WinId> {
        if ctx.cfg.scroller.snap_to_column {
            if let Some(i) = self.col_at_view_center(ctx) {
                self.active = i;
            }
            self.fit(ctx);
        }
        self.active_win()
    }

    fn set_next_width(&mut self, w: f64) {
        self.next_width = Some(w);
    }

    fn cmd(&mut self, ctx: &Ctx, focus: Option<WinId>, cmd: &str, args: &[&str]) -> CmdResult {
        let ax = Axes::new(ctx);
        let focus_ci = focus.and_then(|f| self.find(f)).map(|(ci, _)| ci).unwrap_or(self.active);
        let maxw = ctx.cfg.scroller.max_windows_per_column;
        let r = match cmd {
            "focus-column-left" | "focus-column-up-scroller" => {
                return self.focus_col(focus_ci.saturating_sub(1));
            }
            "focus-column-right" => return self.focus_col((focus_ci + 1).min(self.cols.len().saturating_sub(1))),
            "focus-column-first" => return self.focus_col(0),
            "focus-column-last" => return self.focus_col(self.cols.len().saturating_sub(1)),
            "focus-window-down" | "focus-window-up" => {
                let Some(f) = focus else { return CmdResult::Done };
                let d = if cmd.ends_with("down") { Dir::Down } else { Dir::Up };
                let d = if ax.vertical { if d == Dir::Down { Dir::Right } else { Dir::Left } } else { d };
                return self.neighbor(ctx, f, d).map(CmdResult::Focus).unwrap_or(CmdResult::Done);
            }
            "move-column-left" | "move-column-right" => {
                let pos = cmd.ends_with("right");
                let ni = if pos { focus_ci + 1 } else { focus_ci.wrapping_sub(1) };
                if ni < self.cols.len() {
                    self.cols.swap(focus_ci, ni);
                    self.active = ni;
                }
                CmdResult::Done
            }
            "move-column-to-first" => {
                if focus_ci < self.cols.len() {
                    let c = self.cols.remove(focus_ci);
                    self.cols.insert(0, c);
                    self.active = 0;
                }
                CmdResult::Done
            }
            "move-column-to-last" => {
                if focus_ci < self.cols.len() {
                    let c = self.cols.remove(focus_ci);
                    self.cols.push(c);
                    self.active = self.cols.len() - 1;
                }
                CmdResult::Done
            }
            "move-window-up" | "move-window-down" => {
                let Some(f) = focus else { return CmdResult::Done };
                let d = if cmd.ends_with("down") { Dir::Down } else { Dir::Up };
                let d = if ax.vertical { if d == Dir::Down { Dir::Right } else { Dir::Left } } else { d };
                self.move_dir(ctx, f, d);
                CmdResult::Done
            }
            "consume-or-expel-window-left" | "consume-or-expel-window-right" | "consume-or-expel" => {
                let Some(f) = focus else { return CmdResult::Done };
                let pos = cmd.ends_with("right") || args.first().is_some_and(|a| *a == "right" || *a == "down");
                let Some((ci, _)) = self.find(f) else { return CmdResult::Done };
                if self.cols[ci].wins.len() > 1 {
                    self.expel(ctx, f, pos);
                } else {
                    self.consume_into(f, pos, maxw);
                }
                CmdResult::Done
            }
            "consume-window-into-column" => {
                // pull the first window of the next column into the focused column
                let next = focus_ci + 1;
                if let Some(id) = self.cols.get(next).and_then(|c| c.wins.first().copied()) {
                    self.consume_into(id, false, maxw);
                }
                CmdResult::Done
            }
            "expel-window-from-column" => {
                if let Some(f) = focus {
                    self.expel(ctx, f, true);
                }
                CmdResult::Done
            }
            "switch-preset-column-width" | "cycle-column-width" | "switch-preset-column-width-back" => {
                let back = cmd.ends_with("back");
                let presets = Self::presets(ctx, false);
                if let Some(c) = self.cols.get_mut(focus_ci) {
                    c.full = false;
                    let cur = c.width;
                    let next = if back {
                        presets.iter().rev().find(|p| **p < cur - 0.01).copied().unwrap_or(*presets.last().unwrap())
                    } else {
                        presets.iter().find(|p| **p > cur + 0.01).copied().unwrap_or(presets[0])
                    };
                    c.width = next;
                }
                CmdResult::Done
            }
            "switch-preset-window-height" | "cycle-window-height" => {
                let presets = Self::presets(ctx, true);
                if let (Some(f), Some(c)) = (focus, self.cols.get_mut(focus_ci)) {
                    if c.wins.len() > 1 {
                        if let Some(wi) = c.wins.iter().position(|w| *w == f) {
                            let sum: f64 = c.heights.iter().sum();
                            let cur = c.heights[wi] / sum;
                            let next = presets.iter().find(|p| **p > cur + 0.01).copied().unwrap_or(presets[0]);
                            let others = sum - c.heights[wi];
                            // solve h/(h+others) = next
                            c.heights[wi] = (next * others / (1.0 - next).max(0.05)).max(0.05);
                        }
                    }
                }
                CmdResult::Done
            }
            "reset-window-height" => {
                if let Some(c) = self.cols.get_mut(focus_ci) {
                    for h in &mut c.heights {
                        *h = 1.0;
                    }
                }
                CmdResult::Done
            }
            "set-column-width" => {
                if let (Some(c), Some(a)) = (self.cols.get_mut(focus_ci), args.first()) {
                    if let Some(v) = parse_amount(a, c.width, ax.main_len) {
                        c.full = false;
                        c.width = v.clamp(0.05, 1.0);
                    }
                }
                CmdResult::Done
            }
            "set-window-height" => {
                if let (Some(f), Some(c), Some(a)) = (focus, self.cols.get_mut(focus_ci), args.first()) {
                    if let Some(wi) = c.wins.iter().position(|w| *w == f) {
                        if c.wins.len() > 1 {
                            let sum: f64 = c.heights.iter().sum();
                            let cur = c.heights[wi] / sum;
                            if let Some(v) = parse_amount(a, cur, ax.cross_len) {
                                let v = v.clamp(0.05, 0.95);
                                let others = sum - c.heights[wi];
                                c.heights[wi] = v * others / (1.0 - v);
                            }
                        }
                    }
                }
                CmdResult::Done
            }
            "maximize-column" => {
                if let Some(c) = self.cols.get_mut(focus_ci) {
                    c.full = !c.full;
                }
                CmdResult::Done
            }
            "expand-column-to-available-width" => {
                // grow to fill whatever the other visible columns leave
                let starts = self.col_starts(&ax, ctx.gap);
                let mut used = 0.0;
                for (i, (s, w)) in starts.iter().enumerate() {
                    if i != focus_ci && *s >= self.view - 1.0 && s + w <= self.view + ax.main_len + 1.0 {
                        used += w + ctx.gap;
                    }
                }
                if let Some(c) = self.cols.get_mut(focus_ci) {
                    let px = (ax.main_len - used).max(50.0);
                    c.full = false;
                    c.width = ((px + ctx.gap) / (ax.main_len + ctx.gap)).clamp(0.05, 1.0);
                }
                CmdResult::Done
            }
            "center-column" | "center" => {
                let starts = self.col_starts(&ax, ctx.gap);
                if let Some((s, w)) = starts.get(focus_ci) {
                    self.view = s + w / 2.0 - ax.main_len / 2.0;
                }
                return CmdResult::Done;
            }
            "center-visible-columns" => {
                let starts = self.col_starts(&ax, ctx.gap);
                let vis: Vec<&(f64, f64)> = starts
                    .iter()
                    .filter(|(s, w)| *s >= self.view - 1.0 && s + w <= self.view + ax.main_len + 1.0)
                    .collect();
                if let (Some(f), Some(l)) = (vis.first(), vis.last()) {
                    let span = l.0 + l.1 - f.0;
                    self.view = f.0 + span / 2.0 - ax.main_len / 2.0;
                }
                return CmdResult::Done;
            }
            _ => CmdResult::Unhandled,
        };
        self.fit(ctx);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroller_basic() {
        let cfg = Config::default();
        let ctx = Ctx { cfg: &cfg, area: RectF::new(0.0, 0.0, 1000.0, 500.0), gap: 10.0 };
        let mut s = Scroller::new(&cfg);
        s.add(&ctx, 1, None, None);
        s.add(&ctx, 2, Some(1), None);
        s.add(&ctx, 3, Some(2), None);
        let r = s.arrange(&ctx);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].1.x, 505.0);
        // third column is off-screen -> view scrolled
        assert!(s.view > 0.0);
        assert_eq!(s.neighbor(&ctx, 2, Dir::Left), Some(1));
        s.cmd(&ctx, Some(3), "consume-or-expel-window-left", &[]);
        assert_eq!(s.cols.len(), 2);
        assert_eq!(s.cols[1].wins, vec![2, 3]);
        s.cmd(&ctx, Some(3), "consume-or-expel-window-right", &[]);
        assert_eq!(s.cols.len(), 3);
    }
}
