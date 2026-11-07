//! Material You palette from a wallpaper (same color roles as matugen/noctalia).

use std::collections::BTreeMap;

use material_colors::{
    blend::harmonize,
    color::Rgb,
    dynamic_color::DynamicScheme,
    hct::Hct,
    image::extract_color,
    scheme::{
        Scheme,
        variant::{
            SchemeContent, SchemeExpressive, SchemeFidelity, SchemeFruitSalad, SchemeMonochrome, SchemeNeutral,
            SchemeRainbow, SchemeTonalSpot, SchemeVibrant,
        },
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb8(pub u8, pub u8, pub u8);

impl Rgb8 {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
    pub fn to_f32(self) -> [f32; 3] {
        [self.0 as f32 / 255.0, self.1 as f32 / 255.0, self.2 as f32 / 255.0]
    }
}

impl From<Rgb> for Rgb8 {
    fn from(c: Rgb) -> Self {
        Rgb8(c.red, c.green, c.blue)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub source: Rgb8,
    pub dark: BTreeMap<String, Rgb8>,
    pub light: BTreeMap<String, Rgb8>,
    pub is_dark: bool,
}

impl Palette {
    pub fn current(&self) -> &BTreeMap<String, Rgb8> {
        if self.is_dark { &self.dark } else { &self.light }
    }

    /// name -> "#rrggbb" for the active mode (used for CSS variables)
    pub fn css_vars(&self) -> std::collections::HashMap<String, String> {
        self.current().iter().map(|(k, v)| (k.clone(), v.hex())).collect()
    }

    pub fn fallback() -> Palette {
        Palette::from_source(Rgb8(0x7a, 0x8f, 0xc4), "tonal-spot", true, 0.0)
    }

    pub fn from_source(source: Rgb8, scheme: &str, is_dark: bool, contrast: f64) -> Palette {
        let src = Rgb::new(source.0, source.1, source.2);
        // dark and light schemes are independent: build them on two cores
        let mut schemes = crate::parallel::map(2, |i| build(src, scheme, i == 0, contrast));
        let light = schemes.pop().unwrap();
        let dark = schemes.pop().unwrap();
        Palette { source, dark, light, is_dark }
    }

    pub fn from_image(img: &image::RgbaImage, scheme: &str, is_dark: bool, contrast: f64) -> Palette {
        // downscale to ~128px on the long side for quantization
        let (w, h) = img.dimensions();
        let scale = (128.0 / w.max(h) as f32).min(1.0);
        let small = image::imageops::resize(
            img,
            ((w as f32 * scale) as u32).max(1),
            ((h as f32 * scale) as u32).max(1),
            image::imageops::FilterType::Triangle,
        );
        let pixels: Vec<Rgb> = small
            .pixels()
            .filter(|p| p.0[3] > 200)
            .map(|p| Rgb::new(p.0[0], p.0[1], p.0[2]))
            .collect();
        let source = if pixels.is_empty() { Rgb::new(0x7a, 0x8f, 0xc4) } else { extract_color(&pixels) };
        Palette::from_source(source.into(), scheme, is_dark, contrast)
    }
}

fn dynamic(src: Rgb, scheme: &str, dark: bool, contrast: f64) -> DynamicScheme {
    let hct = Hct::new(src);
    let c = Some(contrast);
    match scheme.trim().trim_start_matches("m3-").to_ascii_lowercase().replace('_', "-").as_str() {
        "content" => SchemeContent::new(hct, dark, c).scheme,
        "vibrant" => SchemeVibrant::new(hct, dark, c).scheme,
        "expressive" => SchemeExpressive::new(hct, dark, c).scheme,
        "fidelity" => SchemeFidelity::new(hct, dark, c).scheme,
        "neutral" => SchemeNeutral::new(hct, dark, c).scheme,
        "monochrome" => SchemeMonochrome::new(hct, dark, c).scheme,
        "rainbow" => SchemeRainbow::new(hct, dark, c).scheme,
        "fruit-salad" => SchemeFruitSalad::new(hct, dark, c).scheme,
        _ => SchemeTonalSpot::new(hct, dark, c).scheme,
    }
}

fn build(src: Rgb, scheme: &str, dark: bool, contrast: f64) -> BTreeMap<String, Rgb8> {
    let s: Scheme = dynamic(src, scheme, dark, contrast).into();
    let mut m: BTreeMap<String, Rgb8> = s.into_iter().map(|(k, v)| (k.to_string(), v.into())).collect();
    m.insert("source_color".into(), src.into());

    // Terminal palette: classic hues harmonized toward the wallpaper color.
    let tone = |base: Rgb, t: f64, chroma: f64| -> Rgb8 {
        let h = Hct::new(harmonize(base, src));
        Rgb::from(Hct::from(h.get_hue(), chroma, t)).into()
    };
    let bases = [
        ("red", Rgb::new(0xe0, 0x50, 0x50)),
        ("green", Rgb::new(0x50, 0xb8, 0x60)),
        ("yellow", Rgb::new(0xd8, 0xb0, 0x40)),
        ("blue", Rgb::new(0x50, 0x80, 0xe0)),
        ("magenta", Rgb::new(0xc0, 0x60, 0xc0)),
        ("cyan", Rgb::new(0x40, 0xb8, 0xc0)),
    ];
    let (n_tone, b_tone) = if dark { (72.0, 82.0) } else { (40.0, 50.0) };
    for (name, base) in bases {
        m.insert(format!("terminal_normal_{name}"), tone(base, n_tone, 48.0));
        m.insert(format!("terminal_bright_{name}"), tone(base, b_tone, 56.0));
    }
    let g = |k: &str| m[k];
    let (black, bright_black, white, bright_white) = if dark {
        (g("surface_container_high"), g("outline_variant"), g("on_surface_variant"), g("on_surface"))
    } else {
        (g("on_surface"), g("on_surface_variant"), g("surface_container_high"), g("surface_container_lowest"))
    };
    let extra = [
        ("terminal_normal_black", black),
        ("terminal_bright_black", bright_black),
        ("terminal_normal_white", white),
        ("terminal_bright_white", bright_white),
        ("terminal_background", g("surface")),
        ("terminal_foreground", g("on_surface")),
        ("terminal_cursor", g("primary")),
        ("terminal_cursor_text", g("on_primary")),
        ("terminal_selection_bg", g("primary_container")),
        ("terminal_selection_fg", g("on_primary_container")),
    ];
    for (k, v) in extra {
        m.insert(k.into(), v);
    }
    m
}

// ---- small color math used by template filters

pub fn rgb_to_hsl(c: Rgb8) -> (f32, f32, f32) {
    let [r, g, b] = c.to_f32();
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Rgb8 {
    let h = (h % 360.0 + 360.0) % 360.0 / 360.0;
    if s <= 0.0 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Rgb8(v, v, v);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0
        }
        if t > 1.0 {
            t -= 1.0
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Rgb8(f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}
