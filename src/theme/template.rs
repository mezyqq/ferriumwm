//! matugen/noctalia-compatible template engine.
//!
//! `{{ colors.<name>.<default|dark|light>.<format> | filter arg | ... }}`
//! formats: hex hex_stripped rgb rgba rgb_csv red green blue alpha hue saturation lightness hsl hsla
//! filters: darken N, lighten N, set_alpha N, saturate N, desaturate N, set_lightness N,
//!          set_hue N, rotate N, invert, grayscale, blend <#hex> N
//! other: {{ image }} / {{ wallpaper }}, {{ mode }}, {{ config_dir }}, {{ source_color }}

use std::collections::HashMap;

use super::palette::{Palette, Rgb8, hsl_to_rgb, rgb_to_hsl};

pub struct Ctx<'a> {
    pub palette: &'a Palette,
    pub vars: HashMap<String, String>,
}

#[derive(Clone, Copy)]
struct Col {
    c: Rgb8,
    a: f32,
}

fn parse_num(s: &str) -> Option<f32> {
    let s = s.trim().trim_matches('"');
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().map(|v| v / 100.0);
    }
    s.parse().ok()
}

fn apply_filter(col: &mut Col, name: &str, arg: &str) -> Result<(), String> {
    let num = || parse_num(arg).ok_or_else(|| format!("фильтр {name}: нужен аргумент-число"));
    let (h, s, l) = rgb_to_hsl(col.c);
    match name {
        "darken" => {
            let n = num()?;
            let n = if n > 1.0 { n / 100.0 } else { n };
            col.c = hsl_to_rgb(h, s, (l - n).clamp(0.0, 1.0));
        }
        "lighten" => {
            let n = num()?;
            let n = if n > 1.0 { n / 100.0 } else { n };
            col.c = hsl_to_rgb(h, s, (l + n).clamp(0.0, 1.0));
        }
        "saturate" => {
            let n = num()?;
            let n = if n > 1.0 { n / 100.0 } else { n };
            col.c = hsl_to_rgb(h, (s + n).clamp(0.0, 1.0), l);
        }
        "desaturate" => {
            let n = num()?;
            let n = if n > 1.0 { n / 100.0 } else { n };
            col.c = hsl_to_rgb(h, (s - n).clamp(0.0, 1.0), l);
        }
        "set_lightness" => {
            let n = num()?;
            let n = if n > 1.0 { n / 100.0 } else { n };
            col.c = hsl_to_rgb(h, s, n.clamp(0.0, 1.0));
        }
        "set_hue" => col.c = hsl_to_rgb(num()?, s, l),
        "rotate" | "rotate_hue" => col.c = hsl_to_rgb(h + num()?, s, l),
        "set_alpha" => {
            let n = num()?;
            col.a = if n > 1.0 { n / 255.0 } else { n }.clamp(0.0, 1.0);
        }
        "invert" => col.c = Rgb8(255 - col.c.0, 255 - col.c.1, 255 - col.c.2),
        "grayscale" => {
            let y = (0.299 * col.c.0 as f32 + 0.587 * col.c.1 as f32 + 0.114 * col.c.2 as f32).round() as u8;
            col.c = Rgb8(y, y, y);
        }
        "blend" => {
            let mut parts = arg.split_whitespace();
            let hex = parts.next().ok_or("blend: нужен цвет")?;
            let t = parts.next().and_then(parse_num).unwrap_or(0.5);
            let o = crate::style::Color::from_hex(hex.trim_matches('"')).ok_or("blend: плохой цвет")?;
            let mix = |a: u8, b: f32| (a as f32 + (b * 255.0 - a as f32) * t).round() as u8;
            col.c = Rgb8(mix(col.c.0, o.0[0]), mix(col.c.1, o.0[1]), mix(col.c.2, o.0[2]));
        }
        o => return Err(format!("неизвестный фильтр {o}")),
    }
    Ok(())
}

fn format(col: Col, fmt: &str) -> Result<String, String> {
    let Rgb8(r, g, b) = col.c;
    let (h, s, l) = rgb_to_hsl(col.c);
    Ok(match fmt {
        "hex" => {
            if col.a < 1.0 {
                format!("#{r:02x}{g:02x}{b:02x}{:02x}", (col.a * 255.0).round() as u8)
            } else {
                col.c.hex()
            }
        }
        "hex_stripped" => col.c.hex()[1..].to_string(),
        "rgb" => format!("rgb({r}, {g}, {b})"),
        "rgba" => format!("rgba({r}, {g}, {b}, {})", trim_f(col.a)),
        "rgb_csv" => format!("{r},{g},{b}"),
        "red" => r.to_string(),
        "green" => g.to_string(),
        "blue" => b.to_string(),
        "alpha" => trim_f(col.a),
        "hue" => format!("{}", h.round()),
        "saturation" => format!("{}", (s * 100.0).round()),
        "lightness" => format!("{}", (l * 100.0).round()),
        "hsl" => format!("hsl({}, {}%, {}%)", h.round(), (s * 100.0).round(), (l * 100.0).round()),
        "hsla" => format!(
            "hsla({}, {}%, {}%, {})",
            h.round(),
            (s * 100.0).round(),
            (l * 100.0).round(),
            trim_f(col.a)
        ),
        o => return Err(format!("неизвестный формат {o}")),
    })
}

fn trim_f(f: f32) -> String {
    let s = format!("{:.3}", f);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.into() }
}

fn eval(expr: &str, ctx: &Ctx) -> Result<String, String> {
    let mut parts = expr.split('|');
    let head = parts.next().unwrap_or("").trim();
    let filters: Vec<(&str, &str)> = parts
        .map(|f| {
            let f = f.trim();
            let (n, a) = f.split_once([' ', ':']).unwrap_or((f, ""));
            (n.trim(), a.trim())
        })
        .collect();

    if let Some(rest) = head.strip_prefix("colors.") {
        let segs: Vec<&str> = rest.split('.').collect();
        let (name, mode, fmt) = match segs.as_slice() {
            [n, m, f] => (*n, *m, *f),
            [n, m] => (*n, *m, "hex"),
            [n] => (*n, "default", "hex"),
            _ => return Err(format!("непонятное выражение {head}")),
        };
        let map = match mode {
            "dark" => &ctx.palette.dark,
            "light" => &ctx.palette.light,
            _ => ctx.palette.current(),
        };
        let c = *map.get(name).ok_or_else(|| format!("нет цвета {name}"))?;
        let mut col = Col { c, a: 1.0 };
        for (n, a) in filters {
            apply_filter(&mut col, n, a)?;
        }
        return format(col, fmt);
    }
    match head {
        "image" | "wallpaper" | "mode" | "config_dir" | "source_color" | "is_dark" => {
            let v = match head {
                "image" | "wallpaper" => ctx.vars.get("image").cloned().unwrap_or_default(),
                "mode" => if ctx.palette.is_dark { "dark".into() } else { "light".into() },
                "is_dark" => ctx.palette.is_dark.to_string(),
                "source_color" => ctx.palette.source.hex(),
                k => ctx.vars.get(k).cloned().unwrap_or_default(),
            };
            Ok(v)
        }
        k => ctx.vars.get(k).cloned().ok_or_else(|| format!("неизвестная переменная {k}")),
    }
}

pub fn render(src: &str, ctx: &Ctx) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        out.push_str(&eval(after[..end].trim(), ctx)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders() {
        let p = Palette::from_source(Rgb8(0x33, 0x66, 0x99), "content", true, 0.0);
        let ctx = Ctx { palette: &p, vars: HashMap::from([("image".into(), "/a.png".into())]) };
        let out = render(
            "a {{colors.primary.default.hex}} b {{ colors.surface.dark.rgb_csv }} c {{colors.primary.default.hex | darken 0.1}} {{image}} {{colors.error.default.rgba | set_alpha: 0.5}} {{colors.terminal_normal_red.default.hex_stripped}}",
            &ctx,
        )
        .unwrap();
        assert!(out.starts_with("a #"), "{out}");
        assert!(out.contains("/a.png"));
        assert!(out.contains(", 0.5)"));
    }
}
