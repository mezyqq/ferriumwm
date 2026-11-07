//! Wallpaper → palette → templates for apps.

pub mod palette;
pub mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::{Config, Template, expand_path};
use palette::Palette;

#[allow(dead_code)]
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::home().join(".cache"));
    base.join("FerriumWM")
}

pub fn load_image(path: &Path) -> Result<image::RgbaImage> {
    let img = image::ImageReader::open(path)
        .with_context(|| format!("не открыть {}", path.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("не декодировать {}", path.display()))?;
    Ok(img.to_rgba8())
}

pub fn palette_for(cfg: &Config, img: Option<&image::RgbaImage>) -> Palette {
    let dark = cfg.theme.mode != "light";
    match img {
        Some(img) if cfg.theme.generate_colors => Palette::from_image(img, &cfg.theme.scheme, dark, cfg.theme.contrast),
        _ => {
            let src = crate::style::Color::from_hex(&cfg.theme.background_color)
                .map(|c| palette::Rgb8((c.0[0] * 255.0) as u8, (c.0[1] * 255.0) as u8, (c.0[2] * 255.0) as u8))
                .unwrap_or(palette::Rgb8(0x7a, 0x8f, 0xc4));
            Palette::from_source(src, &cfg.theme.scheme, dark, cfg.theme.contrast)
        }
    }
}

// ------------------------------------------------------------ builtins

const FEWM_JSON: &str = r#"{
  "wallpaper": "{{image}}",
  "mode": "{{mode}}",
  "source": "{{source_color}}",
  "primary": "{{colors.primary.default.hex}}",
  "on_primary": "{{colors.on_primary.default.hex}}",
  "primary_container": "{{colors.primary_container.default.hex}}",
  "on_primary_container": "{{colors.on_primary_container.default.hex}}",
  "secondary": "{{colors.secondary.default.hex}}",
  "on_secondary": "{{colors.on_secondary.default.hex}}",
  "secondary_container": "{{colors.secondary_container.default.hex}}",
  "tertiary": "{{colors.tertiary.default.hex}}",
  "on_tertiary": "{{colors.on_tertiary.default.hex}}",
  "tertiary_container": "{{colors.tertiary_container.default.hex}}",
  "error": "{{colors.error.default.hex}}",
  "on_error": "{{colors.on_error.default.hex}}",
  "surface": "{{colors.surface.default.hex}}",
  "on_surface": "{{colors.on_surface.default.hex}}",
  "on_surface_variant": "{{colors.on_surface_variant.default.hex}}",
  "surface_container_lowest": "{{colors.surface_container_lowest.default.hex}}",
  "surface_container_low": "{{colors.surface_container_low.default.hex}}",
  "surface_container": "{{colors.surface_container.default.hex}}",
  "surface_container_high": "{{colors.surface_container_high.default.hex}}",
  "surface_container_highest": "{{colors.surface_container_highest.default.hex}}",
  "outline": "{{colors.outline.default.hex}}",
  "outline_variant": "{{colors.outline_variant.default.hex}}",
  "shadow": "{{colors.shadow.default.hex}}"
}
"#;

const PYWAL_JSON: &str = r#"{
  "wallpaper": "{{image}}",
  "alpha": "100",
  "special": {
    "background": "{{colors.surface.default.hex}}",
    "foreground": "{{colors.on_surface.default.hex}}",
    "cursor": "{{colors.primary.default.hex}}"
  },
  "colors": {
    "color0": "{{colors.surface.default.hex}}",
    "color1": "{{colors.terminal_normal_red.default.hex}}",
    "color2": "{{colors.terminal_normal_green.default.hex}}",
    "color3": "{{colors.primary.default.hex}}",
    "color4": "{{colors.terminal_normal_blue.default.hex}}",
    "color5": "{{colors.tertiary.default.hex}}",
    "color6": "{{colors.terminal_normal_cyan.default.hex}}",
    "color7": "{{colors.surface_container_high.default.hex}}",
    "color8": "{{colors.outline_variant.default.hex}}",
    "color9": "{{colors.terminal_bright_red.default.hex}}",
    "color10": "{{colors.primary.default.hex}}",
    "color11": "{{colors.terminal_bright_yellow.default.hex}}",
    "color12": "{{colors.terminal_bright_blue.default.hex}}",
    "color13": "{{colors.secondary.default.hex}}",
    "color14": "{{colors.terminal_bright_cyan.default.hex}}",
    "color15": "{{colors.on_surface.default.hex}}"
  }
}
"#;

const KITTY_CONF: &str = r#"# FerriumWM colors
foreground {{colors.terminal_foreground.default.hex}}
background {{colors.terminal_background.default.hex}}
cursor {{colors.terminal_cursor.default.hex}}
cursor_text_color {{colors.terminal_cursor_text.default.hex}}
selection_foreground {{colors.terminal_selection_fg.default.hex}}
selection_background {{colors.terminal_selection_bg.default.hex}}
url_color {{colors.primary.default.hex}}
active_border_color {{colors.primary.default.hex}}
inactive_border_color {{colors.outline_variant.default.hex}}
active_tab_foreground {{colors.on_primary.default.hex}}
active_tab_background {{colors.primary.default.hex}}
inactive_tab_foreground {{colors.on_surface_variant.default.hex}}
inactive_tab_background {{colors.surface_container.default.hex}}
color0 {{colors.terminal_normal_black.default.hex}}
color1 {{colors.terminal_normal_red.default.hex}}
color2 {{colors.terminal_normal_green.default.hex}}
color3 {{colors.terminal_normal_yellow.default.hex}}
color4 {{colors.terminal_normal_blue.default.hex}}
color5 {{colors.terminal_normal_magenta.default.hex}}
color6 {{colors.terminal_normal_cyan.default.hex}}
color7 {{colors.terminal_normal_white.default.hex}}
color8 {{colors.terminal_bright_black.default.hex}}
color9 {{colors.terminal_bright_red.default.hex}}
color10 {{colors.terminal_bright_green.default.hex}}
color11 {{colors.terminal_bright_yellow.default.hex}}
color12 {{colors.terminal_bright_blue.default.hex}}
color13 {{colors.terminal_bright_magenta.default.hex}}
color14 {{colors.terminal_bright_cyan.default.hex}}
color15 {{colors.terminal_bright_white.default.hex}}
"#;

fn builtin(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // (template, output, post_hook)
    Some(match name {
        "fewm" => (FEWM_JSON, "$XDG_CACHE_HOME/FerriumWM/colors.json", ""),
        "pywal" => (PYWAL_JSON, "$XDG_CACHE_HOME/wal/colors.json", "pywalfox update >/dev/null 2>&1 || true"),
        "kitty" => (
            KITTY_CONF,
            "$XDG_CONFIG_HOME/kitty/themes/fewm.conf",
            "pkill -USR1 -x kitty >/dev/null 2>&1 || true",
        ),
        _ => return None,
    })
}

// ------------------------------------------------------------ apply

pub struct Job {
    pub name: String,
    pub text: Result<String, String>,
    pub output: PathBuf,
    pub post_hook: String,
}

/// Render all templates now (cheap); write files and run hooks in a thread.
pub fn apply_templates(cfg: &Config, pal: &Palette, wallpaper: &str) {
    if std::env::var_os("FEWM_NO_TEMPLATES").is_some() {
        info!("шаблоны цветов пропущены (FEWM_NO_TEMPLATES)");
        return;
    }
    let mut vars = HashMap::new();
    vars.insert("image".to_string(), wallpaper.to_string());
    let mut jobs = Vec::new();
    for name in &cfg.theme.builtin_templates {
        let Some((tpl, out, hook)) = builtin(name) else {
            warn!("нет встроенного шаблона {name}");
            continue;
        };
        let ctx = template::Ctx { palette: pal, vars: vars.clone() };
        jobs.push(Job {
            name: name.clone(),
            text: template::render(tpl, &ctx),
            output: expand_path(out),
            post_hook: hook.into(),
        });
    }
    for t in cfg.theme.templates.iter().filter(|t| t.enabled && !t.input.is_empty() && !t.output.is_empty()) {
        let input = expand_path(&t.input);
        let mut v = vars.clone();
        v.insert(
            "config_dir".into(),
            input.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        );
        let ctx = template::Ctx { palette: pal, vars: v };
        let text = std::fs::read_to_string(&input)
            .map_err(|e| format!("{}: {e}", input.display()))
            .and_then(|src| template::render(&src, &ctx));
        let hook = template::render(&t.post_hook, &ctx).unwrap_or_else(|_| t.post_hook.clone());
        jobs.push(Job { name: t.name.clone(), text, output: expand_path(&t.output), post_hook: hook });
    }
    std::thread::spawn(move || {
        for job in jobs {
            match job.text {
                Ok(text) => {
                    if let Some(dir) = job.output.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if std::fs::read_to_string(&job.output).ok().as_deref() == Some(text.as_str()) {
                        continue;
                    }
                    if let Err(e) = std::fs::write(&job.output, text) {
                        warn!("шаблон {}: {e}", job.name);
                        continue;
                    }
                    if !job.post_hook.is_empty() {
                        let _ = Command::new("sh").arg("-c").arg(&job.post_hook).spawn().map(|mut c| {
                            std::thread::spawn(move || c.wait());
                        });
                    }
                }
                Err(e) => warn!("шаблон {}: {e}", job.name),
            }
        }
        info!("шаблоны цветов применены");
    });
}

// ------------------------------------------------------------ noctalia import

/// Convert the user's noctalia theme templates (builtin, community and user
/// ones) into FeWM template entries, reusing noctalia's template files.
pub fn import_noctalia() -> Vec<Template> {
    let home = crate::config::home();
    let settings = home.join(".local/state/noctalia/settings.toml");
    let Ok(text) = std::fs::read_to_string(&settings) else { return Vec::new() };
    let Ok(root) = text.parse::<toml::Table>() else { return Vec::new() };
    let tpl = root
        .get("theme")
        .and_then(|t| t.get("templates"))
        .and_then(|t| t.as_table())
        .cloned()
        .unwrap_or_default();
    let ids = |k: &str| -> Vec<String> {
        tpl.get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    let skip = ["niri", "hyprland", "sway", "labwc", "mango", "scroll"];

    let mut add_from = |dir: &Path, toml_file: &Path, wanted: &[String], prefix: &str| {
        let Ok(t) = std::fs::read_to_string(toml_file) else { return };
        let Ok(t) = t.parse::<toml::Table>() else { return };
        let Some(templates) = t.get("templates").and_then(|x| x.as_table()) else { return };
        for (id, def) in templates {
            let base = id.split('-').next().unwrap_or(id);
            let tid = if prefix == "builtin" {
                id.as_str()
            } else {
                base
            };
            if (prefix == "builtin" && !wanted.iter().any(|w| w == tid || w == id)) || skip.contains(&tid) {
                continue;
            }
            let Some(input) = def.get("input_path").and_then(|v| v.as_str()) else { continue };
            let in_path = if input.starts_with('/') || input.starts_with('~') {
                input.to_string()
            } else {
                // template files live next to their template.toml / in a per-id dir
                let rel = input.trim_start_matches("./");
                let p = dir.join(rel);
                p.to_string_lossy().into_owned()
            };
            // noctalia's {{ config_dir }} is the directory holding the .toml
            let cfg_dir = dir.to_string_lossy().into_owned();
            let mut hook = def.get("post_hook").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match def.get("post_action").and_then(|v| v.as_str()) {
                Some("firefox-theme") => hook = "pywalfox update >/dev/null 2>&1 || true".into(),
                Some("kde-color-scheme") => {
                    hook = "plasma-apply-colorscheme noctalia >/dev/null 2>&1 || true".into()
                }
                _ => {}
            }
            hook = hook.replace("{{ config_dir }}", &cfg_dir).replace("{{config_dir}}", &cfg_dir);
            let outputs: Vec<String> = match def.get("output_path") {
                Some(toml::Value::String(s)) => vec![s.clone()],
                Some(toml::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                _ => continue,
            };
            for (i, o) in outputs.iter().enumerate() {
                // only write outputs whose app is actually installed/configured
                let expanded = expand_path(o);
                let app_dir_exists = expanded
                    .parent()
                    .and_then(|p| if p.ends_with("themes") || p.ends_with("colors") { p.parent() } else { Some(p) })
                    .is_some_and(|p| p.exists());
                if prefix != "builtin" && !app_dir_exists {
                    continue;
                }
                out.push(Template {
                    name: format!("{prefix}:{id}{}", if i > 0 { format!("#{i}") } else { String::new() }),
                    enabled: true,
                    input: in_path.clone(),
                    output: o.clone(),
                    post_hook: if i + 1 == outputs.len() { hook.clone() } else { String::new() },
                });
            }
        }
    };

    if tpl.get("enable_builtin_templates").and_then(|v| v.as_bool()).unwrap_or(true) {
        for share in ["/usr/local/share/noctalia/assets/templates", "/usr/share/noctalia/assets/templates"] {
            let d = Path::new(share);
            if d.exists() {
                // gtk3/gtk4 are separate ids sharing the gtk dir
                add_from(d, &d.join("builtin.toml"), &ids("builtin_ids"), "builtin");
                break;
            }
        }
    }
    if tpl.get("enable_community_templates").and_then(|v| v.as_bool()).unwrap_or(true) {
        let cdir = home.join(".local/state/noctalia/community-templates");
        for id in ids("community_ids") {
            let d = cdir.join(&id);
            add_from(&d, &d.join("template.toml"), std::slice::from_ref(&id), "community");
        }
    }
    if let Some(user) = tpl.get("user").and_then(|u| u.as_table()) {
        for (id, def) in user {
            if def.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }
            let get = |k: &str| def.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if get("input_path").is_empty() || get("output_path").is_empty() {
                continue;
            }
            out.push(Template {
                name: format!("user:{id}"),
                enabled: true,
                input: get("input_path"),
                output: get("output_path"),
                post_hook: get("post_hook"),
            });
        }
    }
    out
}

/// noctalia's current wallpaper and scheme, if any.
pub fn noctalia_wallpaper() -> Option<(String, String, String)> {
    let home = crate::config::home();
    let text = std::fs::read_to_string(home.join(".local/state/noctalia/settings.toml")).ok()?;
    let root = text.parse::<toml::Table>().ok()?;
    let wp = root.get("wallpaper")?;
    let path = wp
        .get("last")
        .and_then(|l| l.get("path"))
        .or_else(|| wp.get("default").and_then(|l| l.get("path")))
        .and_then(|v| v.as_str())?
        .to_string();
    let dir = wp.get("directory").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let scheme = root
        .get("theme")
        .and_then(|t| t.get("wallpaper_scheme"))
        .and_then(|v| v.as_str())
        .unwrap_or("m3-content")
        .trim_start_matches("m3-")
        .to_string();
    Some((path, dir, scheme))
}
