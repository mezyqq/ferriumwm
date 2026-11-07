//! A tiny CSS engine for FeWM's look: borders, radius, gaps, shadows, blur,
//! opacity. Palette colors from the wallpaper are available as variables.
//!
//! ```css
//! workspace { gaps-in: 5px; gaps-out: 10px; }
//! window { border-width: 2px; border-color: var(--outline-variant); border-radius: 12px; }
//! window:focus { border-color: linear-gradient(45deg, var(--primary), var(--tertiary)); }
//! window[app_id~="kitty|foot"] { opacity: 0.92; blur: true; }
//! ```

use std::collections::HashMap;

use regex::Regex;

pub const DEFAULT_CSS: &str = r#"/*
 * FerriumWM (FeWM) — внешний вид. Изменения применяются сразу после сохранения.
 *
 * Цвета палитры из обоев (Material You) доступны как переменные:
 *   var(--primary) var(--on-primary) var(--primary-container) var(--secondary)
 *   var(--tertiary) var(--error) var(--surface) var(--surface-container)
 *   var(--surface-container-high) var(--on-surface) var(--outline) var(--outline-variant) ...
 * Функции цвета: alpha(цвет, 0.5)  mix(цвет1, цвет2, 0.3)  lighten(цвет, 0.1)  darken(цвет, 0.1)
 * Рамка может быть градиентом: linear-gradient(45deg, var(--primary), var(--tertiary))
 *
 * Селекторы: workspace, window, background
 *   состояния: :focus :urgent :floating :fullscreen :maximized :tiled :pinned
 *   атрибуты:  [app_id="точно"] [app_id~="регулярка"] [title^="начало"] [title*="подстрока"]
 *              [layout="scroller"] (для workspace и window)
 */

:root {
  --radius: 0px;         /* скругление углов окон: 0 — прямые углы, например 12px — скруглённые */
}

workspace {
  gaps-in: 6px;          /* между окнами */
  gaps-out: 12px;        /* от краёв экрана */
}

window {
  border-width: 2px;
  border-color: alpha(var(--outline-variant), 0.8);
  border-radius: var(--radius);
  opacity: 1;
  shadow: 0 6px 22px 0 rgba(0, 0, 0, 0.40);   /* x y размытие расширение цвет; none — без тени */
  blur: false;            /* размытие фона под полупрозрачным окном */
  dim: 0;                 /* затемнение окна, 0..1 */
}

window:focus {
  border-color: linear-gradient(45deg, var(--primary), var(--tertiary));
  shadow: 0 8px 28px 0 rgba(0, 0, 0, 0.55);
}

window:urgent {
  border-color: var(--error);
}

window:floating {
  shadow: 0 12px 40px 2px rgba(0, 0, 0, 0.55);
}

window:fullscreen {
  border-width: 0;
  border-radius: 0;
  shadow: none;
}

background {
  background-color: var(--surface);
}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color(pub [f32; 4]);

impl Color {
    pub const TRANSPARENT: Color = Color([0.0; 4]);

    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color([r, g, b, a])
    }
    pub fn lerp(self, o: Color, t: f32) -> Color {
        let mut c = [0.0; 4];
        for (i, v) in c.iter_mut().enumerate() {
            *v = self.0[i] + (o.0[i] - self.0[i]) * t;
        }
        Color(c)
    }
    pub fn premultiplied(self) -> [f32; 4] {
        let [r, g, b, a] = self.0;
        [r * a, g * a, b * a, a]
    }
    pub fn from_hex(s: &str) -> Option<Color> {
        let h = s.strip_prefix('#')?;
        let p = |i: usize, n: usize| -> Option<f32> {
            let v = u8::from_str_radix(&h[i..i + n], 16).ok()?;
            Some(if n == 1 { (v * 17) as f32 / 255.0 } else { v as f32 / 255.0 })
        };
        match h.len() {
            3 => Some(Color([p(0, 1)?, p(1, 1)?, p(2, 1)?, 1.0])),
            4 => Some(Color([p(0, 1)?, p(1, 1)?, p(2, 1)?, p(3, 1)?])),
            6 => Some(Color([p(0, 2)?, p(2, 2)?, p(4, 2)?, 1.0])),
            8 => Some(Color([p(0, 2)?, p(2, 2)?, p(4, 2)?, p(6, 2)?])),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear { angle_deg: f32, from: Color, to: Color },
}

impl Default for Paint {
    fn default() -> Self {
        Paint::Solid(Color::TRANSPARENT)
    }
}

impl Paint {
    /// (color1, color2, angle_radians)
    pub fn stops(&self) -> (Color, Color, f32) {
        match *self {
            Paint::Solid(c) => (c, c, 0.0),
            Paint::Linear { angle_deg, from, to } => (from, to, angle_deg.to_radians()),
        }
    }
    pub fn lerp(&self, other: &Paint, t: f32) -> Paint {
        let (a1, a2, aa) = self.stops();
        let (b1, b2, ba) = other.stops();
        Paint::Linear {
            angle_deg: (aa + (ba - aa) * t).to_degrees(),
            from: a1.lerp(b1, t),
            to: a2.lerp(b2, t),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WinStyle {
    pub border_width: f32,
    pub border_color: Paint,
    pub border_radius: f32,
    pub opacity: f32,
    pub shadow: Option<Shadow>,
    pub blur: bool,
    pub dim: f32,
}

impl Default for WinStyle {
    fn default() -> Self {
        Self {
            border_width: 2.0,
            border_color: Paint::Solid(Color::rgba(0.5, 0.5, 0.5, 1.0)),
            border_radius: 10.0,
            opacity: 1.0,
            shadow: None,
            blur: false,
            dim: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WsStyle {
    pub gaps_in: f32,
    pub gaps_out: f32,
}

impl Default for WsStyle {
    fn default() -> Self {
        Self { gaps_in: 5.0, gaps_out: 10.0 }
    }
}

#[derive(Debug, Default, Clone)]
pub struct WinQuery<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    pub layout: &'a str,
    pub focus: bool,
    pub urgent: bool,
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone)]
enum AttrOp {
    Eq(String),
    Regex(Regex),
    Prefix(String),
    Contains(String),
}

#[derive(Debug, Clone)]
struct Selector {
    elem: String,
    pseudos: Vec<String>,
    attrs: Vec<(String, AttrOp)>,
}

impl Selector {
    fn specificity(&self) -> u32 {
        (self.pseudos.len() + self.attrs.len()) as u32 * 10 + if self.elem == "*" { 0 } else { 1 }
    }
}

#[derive(Debug, Clone)]
struct Rule {
    selectors: Vec<Selector>,
    decls: Vec<(String, String)>,
    order: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
    root_vars: HashMap<String, String>,
    palette: HashMap<String, String>,
    pub errors: Vec<String>,
    cache: std::cell::RefCell<HashMap<String, WinStyle>>,
}

impl Stylesheet {
    pub fn parse(src: &str, palette: &HashMap<String, String>) -> Stylesheet {
        let mut sheet = Stylesheet {
            palette: palette.iter().map(|(k, v)| (format!("--{}", k.replace('_', "-")), v.clone())).collect(),
            ..Default::default()
        };
        let src = strip_comments(src);
        let mut rest = src.as_str();
        let mut order = 0;
        while let Some(open) = rest.find('{') {
            let sel_text = rest[..open].trim();
            let Some(close) = rest[open..].find('}') else {
                sheet.errors.push("незакрытая {".into());
                break;
            };
            let body = &rest[open + 1..open + close];
            rest = &rest[open + close + 1..];
            let decls: Vec<(String, String)> = split_top(body, ';')
                .into_iter()
                .filter_map(|d| {
                    let (k, v) = d.split_once(':')?;
                    Some((k.trim().to_ascii_lowercase(), v.trim().trim_end_matches("!important").trim().to_string()))
                })
                .filter(|(k, _)| !k.is_empty())
                .collect();
            if sel_text == ":root" || sel_text == "*" && decls.iter().all(|(k, _)| k.starts_with("--")) {
                for (k, v) in decls {
                    sheet.root_vars.insert(k, v);
                }
                continue;
            }
            let mut selectors = Vec::new();
            for s in split_top(sel_text, ',') {
                match parse_selector(s.trim()) {
                    Ok(sel) => selectors.push(sel),
                    Err(e) => sheet.errors.push(format!("селектор {s:?}: {e}")),
                }
            }
            if !selectors.is_empty() {
                sheet.rules.push(Rule { selectors, decls, order });
                order += 1;
            }
        }
        // Validate all values once so errors are reported on load.
        let mut errs = Vec::new();
        for r in &sheet.rules {
            for (k, v) in &r.decls {
                if k.starts_with("--") {
                    continue;
                }
                let resolved = sheet.resolve_vars(v, 0);
                let mut dummy_w = WinStyle::default();
                let mut dummy_ws = WsStyle::default();
                let mut dummy_bg = None;
                if let Err(e) = apply_decl(k, &resolved, &mut dummy_w, &mut dummy_ws, &mut dummy_bg) {
                    errs.push(format!("{k}: {e}"));
                }
            }
        }
        sheet.errors.extend(errs);
        sheet
    }

    fn var(&self, name: &str) -> Option<&String> {
        self.root_vars.get(name).or_else(|| self.palette.get(name))
    }

    fn resolve_vars(&self, v: &str, depth: u32) -> String {
        if depth > 8 || !v.contains("var(") {
            return v.to_string();
        }
        let mut out = String::new();
        let mut rest = v;
        while let Some(i) = rest.find("var(") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 4..];
            let end = matching_paren(after).unwrap_or(after.len());
            let inner = &after[..end];
            let (name, fallback) = match inner.split_once(',') {
                Some((n, f)) => (n.trim(), Some(f.trim())),
                None => (inner.trim(), None),
            };
            let val = self.var(name).cloned().or(fallback.map(str::to_string)).unwrap_or_default();
            out.push_str(&self.resolve_vars(&val, depth + 1));
            rest = after.get(end + 1..).unwrap_or("");
        }
        out.push_str(rest);
        out
    }

    fn matching<'a>(&'a self, elem: &str, check: impl Fn(&Selector) -> bool) -> Vec<&'a Rule> {
        let mut rules: Vec<(u32, &Rule)> = self
            .rules
            .iter()
            .filter_map(|r| {
                r.selectors
                    .iter()
                    .filter(|s| (s.elem == elem || s.elem == "*") && check(s))
                    .map(|s| s.specificity())
                    .max()
                    .map(|sp| (sp, r))
            })
            .collect();
        rules.sort_by_key(|(sp, r)| (*sp, r.order));
        rules.into_iter().map(|(_, r)| r).collect()
    }

    fn decls_vars<'a>(&self, rules: &[&'a Rule]) -> Vec<(&'a str, String)> {
        // rule-local custom properties override :root for later decls
        let mut out = Vec::new();
        for r in rules {
            for (k, v) in &r.decls {
                out.push((k.as_str(), v.clone()));
            }
        }
        out
    }

    pub fn window(&self, q: &WinQuery) -> WinStyle {
        let key = format!(
            "{}\u{1}{}\u{1}{}\u{1}{}{}{}{}{}{}",
            q.app_id, q.title, q.layout, q.focus as u8, q.urgent as u8, q.floating as u8, q.fullscreen as u8,
            q.maximized as u8, q.pinned as u8
        );
        if let Some(s) = self.cache.borrow().get(&key) {
            return s.clone();
        }
        let rules = self.matching("window", |s| {
            s.pseudos.iter().all(|p| match p.as_str() {
                "focus" | "focused" | "active" => q.focus,
                "urgent" => q.urgent,
                "floating" => q.floating,
                "tiled" => !q.floating,
                "fullscreen" => q.fullscreen,
                "maximized" => q.maximized,
                "pinned" => q.pinned,
                _ => false,
            }) && s.attrs.iter().all(|(a, op)| {
                let val = match a.as_str() {
                    "app_id" | "app-id" | "class" => q.app_id,
                    "title" => q.title,
                    "layout" => q.layout,
                    _ => return false,
                };
                op.matches(val)
            })
        });
        let mut st = WinStyle::default();
        let mut ws = WsStyle::default();
        let mut bg = None;
        for (k, v) in self.decls_vars(&rules) {
            if k.starts_with("--") {
                continue;
            }
            let _ = apply_decl(k, &self.resolve_vars(&v, 0), &mut st, &mut ws, &mut bg);
        }
        let mut cache = self.cache.borrow_mut();
        if cache.len() > 512 {
            cache.clear();
        }
        cache.insert(key, st.clone());
        st
    }

    pub fn workspace(&self, layout: &str) -> WsStyle {
        let rules = self.matching("workspace", |s| {
            s.pseudos.is_empty()
                && s.attrs.iter().all(|(a, op)| a == "layout" && op.matches(layout))
        });
        let mut st = WinStyle::default();
        let mut ws = WsStyle::default();
        let mut bg = None;
        for (k, v) in self.decls_vars(&rules) {
            let _ = apply_decl(k, &self.resolve_vars(&v, 0), &mut st, &mut ws, &mut bg);
        }
        ws
    }

    pub fn background(&self) -> Option<Color> {
        let rules = self.matching("background", |s| s.pseudos.is_empty() && s.attrs.is_empty());
        let mut st = WinStyle::default();
        let mut ws = WsStyle::default();
        let mut bg = None;
        for (k, v) in self.decls_vars(&rules) {
            let _ = apply_decl(k, &self.resolve_vars(&v, 0), &mut st, &mut ws, &mut bg);
        }
        bg
    }
}

impl AttrOp {
    fn matches(&self, v: &str) -> bool {
        match self {
            AttrOp::Eq(s) => v == s,
            AttrOp::Regex(r) => r.is_match(v),
            AttrOp::Prefix(s) => v.starts_with(s.as_str()),
            AttrOp::Contains(s) => v.contains(s.as_str()),
        }
    }
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            Some(j) => rest = &rest[i + 2 + j + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Split on `sep` outside of parentheses and quotes.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out.into_iter().filter(|x| !x.trim().is_empty()).collect()
}

fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn parse_selector(s: &str) -> Result<Selector, String> {
    let mut sel = Selector { elem: String::new(), pseudos: Vec::new(), attrs: Vec::new() };
    let mut chars = s.char_indices().peekable();
    let mut elem_end = s.len();
    while let Some(&(i, c)) = chars.peek() {
        if c == ':' || c == '[' {
            elem_end = i;
            break;
        }
        chars.next();
    }
    sel.elem = s[..elem_end].trim().to_string();
    if sel.elem.is_empty() {
        sel.elem = "*".into();
    }
    let mut rest = &s[elem_end..];
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix(':') {
            let end = r.find([':', '[']).unwrap_or(r.len());
            sel.pseudos.push(r[..end].trim().to_ascii_lowercase());
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']').ok_or("нет ]")?;
            let inner = &r[..end];
            rest = &r[end + 1..];
            let (name, op, val) = if let Some((n, v)) = inner.split_once("~=") {
                (n, "~=", v)
            } else if let Some((n, v)) = inner.split_once("^=") {
                (n, "^=", v)
            } else if let Some((n, v)) = inner.split_once("*=") {
                (n, "*=", v)
            } else if let Some((n, v)) = inner.split_once('=') {
                (n, "=", v)
            } else {
                return Err("ожидалось [attr=value]".into());
            };
            let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
            let op = match op {
                "~=" => AttrOp::Regex(Regex::new(&val).map_err(|e| e.to_string())?),
                "^=" => AttrOp::Prefix(val),
                "*=" => AttrOp::Contains(val),
                _ => AttrOp::Eq(val),
            };
            sel.attrs.push((name.trim().to_string(), op));
        } else {
            return Err(format!("непонятно: {rest}"));
        }
    }
    Ok(sel)
}

fn parse_len(v: &str) -> Result<f32, String> {
    let v = v.trim();
    let n = v.trim_end_matches("px").trim();
    n.parse::<f32>().map_err(|_| format!("ожидалась длина, получено {v:?}"))
}

fn parse_num(v: &str) -> Result<f32, String> {
    let v = v.trim();
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f32>().map(|x| x / 100.0).map_err(|_| format!("ожидалось число: {v:?}"));
    }
    v.parse::<f32>().map_err(|_| format!("ожидалось число: {v:?}"))
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.trim() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" | "none" => Ok(false),
        o => Err(format!("ожидалось true/false: {o:?}")),
    }
}

fn func<'a>(v: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let v = v.trim();
    let r = v.strip_prefix(name)?.trim_start().strip_prefix('(')?;
    let r = r.strip_suffix(')')?;
    Some(split_top(r, ',').into_iter().map(str::trim).collect())
}

pub fn parse_color(v: &str) -> Result<Color, String> {
    let v = v.trim();
    if v.starts_with('#') {
        return Color::from_hex(v).ok_or_else(|| format!("плохой hex-цвет {v:?}"));
    }
    if let Some(a) = func(v, "rgba").or_else(|| func(v, "rgb")) {
        let a: Vec<&str> = if a.len() == 1 { a[0].split_whitespace().filter(|x| *x != "/").collect() } else { a };
        if a.len() < 3 {
            return Err(format!("rgb(): мало аргументов {v:?}"));
        }
        let ch = |s: &str| -> Result<f32, String> {
            if let Some(p) = s.strip_suffix('%') {
                Ok(p.parse::<f32>().map_err(|e| e.to_string())? / 100.0)
            } else {
                Ok(s.parse::<f32>().map_err(|e| e.to_string())? / 255.0)
            }
        };
        let alpha = if a.len() > 3 { parse_num(a[3])? } else { 1.0 };
        return Ok(Color([ch(a[0])?, ch(a[1])?, ch(a[2])?, alpha]));
    }
    if let Some(a) = func(v, "alpha") {
        let c = parse_color(a.first().ok_or("alpha(): нет цвета")?)?;
        let t = parse_num(a.get(1).ok_or("alpha(): нет значения")?)?;
        return Ok(Color([c.0[0], c.0[1], c.0[2], c.0[3] * t]));
    }
    if let Some(a) = func(v, "mix") {
        let c1 = parse_color(a.first().ok_or("mix(): нет цвета")?)?;
        let c2 = parse_color(a.get(1).ok_or("mix(): нет второго цвета")?)?;
        let t = a.get(2).map(|x| parse_num(x)).transpose()?.unwrap_or(0.5);
        return Ok(c1.lerp(c2, t));
    }
    if let Some(a) = func(v, "lighten").or_else(|| func(v, "darken")) {
        let c = parse_color(a.first().ok_or("нет цвета")?)?;
        let t = parse_num(a.get(1).ok_or("нет значения")?)?;
        let target = if v.starts_with("lighten") { Color([1.0, 1.0, 1.0, c.0[3]]) } else { Color([0.0, 0.0, 0.0, c.0[3]]) };
        return Ok(c.lerp(target, t));
    }
    let named = match v.to_ascii_lowercase().as_str() {
        "transparent" | "none" => Color::TRANSPARENT,
        "black" => Color::rgba(0.0, 0.0, 0.0, 1.0),
        "white" => Color::rgba(1.0, 1.0, 1.0, 1.0),
        "red" => Color::rgba(1.0, 0.0, 0.0, 1.0),
        "green" => Color::rgba(0.0, 0.5, 0.0, 1.0),
        "lime" => Color::rgba(0.0, 1.0, 0.0, 1.0),
        "blue" => Color::rgba(0.0, 0.0, 1.0, 1.0),
        "yellow" => Color::rgba(1.0, 1.0, 0.0, 1.0),
        "cyan" => Color::rgba(0.0, 1.0, 1.0, 1.0),
        "magenta" => Color::rgba(1.0, 0.0, 1.0, 1.0),
        "orange" => Color::rgba(1.0, 0.65, 0.0, 1.0),
        "gray" | "grey" => Color::rgba(0.5, 0.5, 0.5, 1.0),
        _ => return Err(format!("неизвестный цвет {v:?}")),
    };
    Ok(named)
}

fn parse_paint(v: &str) -> Result<Paint, String> {
    if let Some(a) = func(v, "linear-gradient") {
        let (angle, colors) = match a.first() {
            Some(first) if first.ends_with("deg") => {
                (first.trim_end_matches("deg").trim().parse::<f32>().map_err(|e| e.to_string())?, &a[1..])
            }
            Some(first) if first.starts_with("to ") => {
                let ang = match first.trim() {
                    "to top" => 0.0,
                    "to right" => 90.0,
                    "to bottom" => 180.0,
                    "to left" => 270.0,
                    "to top right" | "to right top" => 45.0,
                    "to bottom right" | "to right bottom" => 135.0,
                    "to bottom left" | "to left bottom" => 225.0,
                    "to top left" | "to left top" => 315.0,
                    o => return Err(format!("направление {o:?}")),
                };
                (ang, &a[1..])
            }
            _ => (180.0, &a[..]),
        };
        if colors.len() < 2 {
            return Err("linear-gradient: нужно два цвета".into());
        }
        return Ok(Paint::Linear {
            angle_deg: angle,
            from: parse_color(colors[0])?,
            to: parse_color(colors[colors.len() - 1])?,
        });
    }
    Ok(Paint::Solid(parse_color(v)?))
}

fn parse_shadow(v: &str) -> Result<Option<Shadow>, String> {
    let v = v.trim();
    if v == "none" || v == "0" {
        return Ok(None);
    }
    // numbers first, color last (color may contain spaces inside parens)
    let mut nums = Vec::new();
    let mut rest = v;
    loop {
        let t = rest.trim_start();
        let end = t.find(char::is_whitespace).unwrap_or(t.len());
        let tok = &t[..end];
        if let Ok(n) = parse_len(tok) {
            nums.push(n);
            rest = &t[end..];
            if nums.len() == 4 {
                break;
            }
        } else {
            break;
        }
    }
    let color_str = rest.trim();
    let color = if color_str.is_empty() { Color::rgba(0.0, 0.0, 0.0, 0.5) } else { parse_color(color_str)? };
    let g = |i: usize| nums.get(i).copied().unwrap_or(0.0);
    Ok(Some(Shadow { x: g(0), y: g(1), blur: g(2), spread: g(3), color }))
}

fn apply_decl(
    k: &str,
    v: &str,
    w: &mut WinStyle,
    ws: &mut WsStyle,
    bg: &mut Option<Color>,
) -> Result<(), String> {
    match k {
        "border-width" => w.border_width = parse_len(v)?,
        "border-color" => w.border_color = parse_paint(v)?,
        "border-radius" => w.border_radius = parse_len(v)?,
        "border" => {
            // border: 2px <paint>
            let v = v.trim();
            let (first, rest) = v.split_once(char::is_whitespace).unwrap_or((v, ""));
            w.border_width = parse_len(first)?;
            if !rest.trim().is_empty() {
                w.border_color = parse_paint(rest)?;
            }
        }
        "opacity" => w.opacity = parse_num(v)?.clamp(0.0, 1.0),
        "shadow" | "box-shadow" => w.shadow = parse_shadow(v)?,
        "blur" => w.blur = parse_bool(v)?,
        "dim" => w.dim = parse_num(v)?.clamp(0.0, 1.0),
        "gaps-in" | "gap" | "gaps-inner" => ws.gaps_in = parse_len(v)?,
        "gaps-out" | "gaps-outer" | "padding" => ws.gaps_out = parse_len(v)?,
        "gaps" => {
            let parts: Vec<&str> = v.split_whitespace().collect();
            ws.gaps_in = parse_len(parts.first().ok_or("gaps: пусто")?)?;
            ws.gaps_out = parse_len(parts.get(1).unwrap_or(&parts[0]))?;
        }
        "background-color" | "background" => *bg = Some(parse_color(v)?),
        o => return Err(format!("неизвестное свойство {o}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_css_parses() {
        let mut pal = HashMap::new();
        for n in ["primary", "tertiary", "outline_variant", "error", "surface"] {
            pal.insert(n.to_string(), "#336699".to_string());
        }
        let s = Stylesheet::parse(DEFAULT_CSS, &pal);
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let w = s.window(&WinQuery { app_id: "kitty", focus: true, ..Default::default() });
        assert!(matches!(w.border_color, Paint::Linear { .. }));
        assert_eq!(w.border_radius, 0.0);
        let ws = s.workspace("dwindle");
        assert_eq!(ws.gaps_out, 12.0);
        let fs = s.window(&WinQuery { fullscreen: true, ..Default::default() });
        assert_eq!(fs.border_width, 0.0);
        assert!(fs.shadow.is_none());
    }

    #[test]
    fn attr_regex() {
        let s = Stylesheet::parse(
            "window[app_id~=\"^ki\"] { opacity: 50%; blur: true } window:focus[title*=\"vim\"] { border-width: 5px }",
            &HashMap::new(),
        );
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let w = s.window(&WinQuery { app_id: "kitty", title: "nvim", focus: true, ..Default::default() });
        assert_eq!(w.opacity, 0.5);
        assert!(w.blur);
        assert_eq!(w.border_width, 5.0);
    }
}
