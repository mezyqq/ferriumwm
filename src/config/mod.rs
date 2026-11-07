//! FerriumWM configuration model.
//!
//! The canonical form is JSON (serde). Each language file in
//! `~/.config/FerriumWM/configs/` (config.py, config.lua, config.sh) is evaluated
//! by its own interpreter into JSON, coerced against the schema and deserialized
//! into [`Config`]. See `langs.rs` and `sync.rs`.

pub mod coerce;
pub mod docs;
pub mod import_niri;
pub mod langs;
pub mod sync;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("FerriumWM")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand `~` and `$VARS` in a path-ish string.
pub fn expand_path(s: &str) -> PathBuf {
    let mut out = String::new();
    let s = if let Some(rest) = s.strip_prefix("~/") {
        out.push_str(&home().to_string_lossy());
        out.push('/');
        rest
    } else if s == "~" {
        return home();
    } else {
        s
    };
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let braced = chars.peek() == Some(&'{');
            if braced {
                chars.next();
            }
            let mut name = String::new();
            while let Some(&n) = chars.peek() {
                if n.is_ascii_alphanumeric() || n == '_' {
                    name.push(n);
                    chars.next();
                } else {
                    break;
                }
            }
            if braced && chars.peek() == Some(&'}') {
                chars.next();
            }
            let val = std::env::var(&name).ok().or_else(|| match name.as_str() {
                "XDG_CONFIG_HOME" => Some(home().join(".config").to_string_lossy().into_owned()),
                "XDG_DATA_HOME" => Some(home().join(".local/share").to_string_lossy().into_owned()),
                "XDG_CACHE_HOME" => Some(home().join(".cache").to_string_lossy().into_owned()),
                "XDG_STATE_HOME" => Some(home().join(".local/state").to_string_lossy().into_owned()),
                _ => None,
            });
            out.push_str(&val.unwrap_or_default());
        } else {
            out.push(c);
        }
    }
    PathBuf::from(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub input: Input,
    pub dwindle: Dwindle,
    pub master: Master,
    pub scroller: Scroller,
    pub animations: Animations,
    pub gestures: Gestures,
    pub theme: Theme,
    pub cursor: Cursor,
    pub misc: Misc,
    pub binds: BTreeMap<String, String>,
    pub mouse_binds: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub autostart: Vec<String>,
    pub window_rules: Vec<WindowRule>,
    pub workspace_rules: Vec<WorkspaceRule>,
    pub outputs: Vec<OutputConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    /// dwindle | master | scroller | floating
    pub layout: String,
    pub workspaces: u32,
    /// niri-style: empty workspaces vanish, there's always one empty at the end
    pub dynamic_workspaces: bool,
    /// dragged window held at the top/bottom screen edge switches workspace after N ms (0 = off)
    pub drag_edge_switch_ms: u32,
    pub focus_follows_mouse: bool,
    pub warp_mouse_to_focus: bool,
    pub focus_new_windows: bool,
    pub auto_float_dialogs: bool,
    pub float_new_windows: bool,
    pub cycle_focus: bool,
    pub resize_step: f64,
    pub move_floating_step: i32,
    pub terminal: String,
    pub hot_reload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Input {
    pub kb_layout: String,
    pub kb_variant: String,
    pub kb_options: String,
    pub kb_model: String,
    pub repeat_rate: i32,
    pub repeat_delay: i32,
    pub numlock: bool,
    pub accel_speed: f64,
    /// adaptive | flat
    pub accel_profile: String,
    pub natural_scroll: bool,
    pub scroll_factor: f64,
    pub left_handed: bool,
    pub touchpad_natural_scroll: bool,
    pub touchpad_tap_to_click: bool,
    pub touchpad_drag: bool,
    pub touchpad_disable_while_typing: bool,
    pub touchpad_scroll_factor: f64,
    pub touchpad_accel_speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Dwindle {
    /// auto | horizontal | vertical
    pub split: String,
    pub default_ratio: f64,
    /// after | before | cursor
    pub new_window_side: String,
    pub preserve_split: bool,
    pub smart_split: bool,
    pub no_gaps_when_only: bool,
    pub split_width_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Master {
    /// left | right | top | bottom | center
    pub orientation: String,
    pub master_ratio: f64,
    pub master_count: u32,
    pub new_on_top: bool,
    pub new_is_master: bool,
    pub no_gaps_when_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Scroller {
    /// horizontal | vertical
    pub direction: String,
    pub column_width: f64,
    pub column_width_presets: Vec<f64>,
    pub window_height_presets: Vec<f64>,
    /// never | always | on-overflow
    pub center_focused: String,
    pub center_single_column: bool,
    /// after-focused | end | start
    pub new_column_position: String,
    pub scroll_speed: f64,
    pub wheel_scroll_step: f64,
    pub wheel_scrolls_view: bool,
    pub touchpad_scrolls_view: bool,
    pub snap_to_column: bool,
    pub focus_follows_view: bool,
    pub max_windows_per_column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Anim {
    pub enabled: bool,
    pub duration_ms: u32,
    /// linear | ease | ease-out | ease-in-out | ease-out-quint | ease-out-back |
    /// bezier(x1, y1, x2, y2) | spring(damping_ratio, stiffness)
    pub curve: String,
    /// open/close: popin | slide | fade | none ; workspaces: slide | slide-vertical | fade | none
    pub style: String,
    pub from_scale: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Animations {
    pub enabled: bool,
    pub speed: f64,
    pub windows_move: Anim,
    pub windows_open: Anim,
    pub windows_close: Anim,
    pub workspaces: Anim,
    pub scroll: Anim,
    pub border_color: Anim,
    pub fade: Anim,
    pub wallpaper: Anim,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gestures {
    pub workspace_swipe: bool,
    pub workspace_swipe_fingers: u32,
    pub workspace_swipe_vertical: bool,
    pub workspace_swipe_distance: f64,
    pub scroller_swipe_fingers: u32,
    pub swipe_invert: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Template {
    pub name: String,
    pub enabled: bool,
    pub input: String,
    pub output: String,
    pub post_hook: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Theme {
    pub wallpaper: String,
    pub wallpaper_dir: String,
    /// fill | fit | center | stretch | tile
    pub wallpaper_mode: String,
    pub background_color: String,
    pub generate_colors: bool,
    /// content | tonal-spot | vibrant | expressive | fidelity | neutral | monochrome | rainbow | fruit-salad
    pub scheme: String,
    /// dark | light
    pub mode: String,
    pub contrast: f64,
    /// Built-in templates to apply: kitty, gtk3, gtk4, qt, btop, pywal, fewm
    pub builtin_templates: Vec<String>,
    pub templates: Vec<Template>,
    pub blur_passes: u32,
    pub blur_radius: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cursor {
    pub theme: String,
    pub size: u32,
    pub hide_after_ms: u32,
    pub hide_while_typing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Misc {
    pub xwayland: bool,
    pub xwayland_satellite: String,
    pub screenshot_dir: String,
    /// shell command run when game mode turns on / off
    pub game_mode_on: String,
    pub game_mode_off: String,
    pub notify_config_errors: bool,
    pub vrr: bool,
    pub disable_power_key: bool,
    pub background_dim_on_fullscreen: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct WindowRule {
    /// regex on app_id
    pub app_id: String,
    /// regex on title
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub float: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximize: Option<bool>,
    /// "800x600" or "50%x60%"
    pub size: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_shadow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_anim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blur: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_width: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct WorkspaceRule {
    pub workspace: u32,
    pub layout: String,
    pub name: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutputConfig {
    pub name: String,
    pub enabled: bool,
    /// "1920x1080@60" or "preferred"
    pub mode: String,
    pub scale: f64,
    /// "auto" or "1920x0"
    pub position: String,
    /// normal | 90 | 180 | 270 | flipped | flipped-90 | flipped-180 | flipped-270
    pub transform: String,
    pub wallpaper: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            mode: "preferred".into(),
            scale: 1.0,
            position: "auto".into(),
            transform: "normal".into(),
            wallpaper: String::new(),
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            layout: "dwindle".into(),
            workspaces: 10,
            dynamic_workspaces: true,
            drag_edge_switch_ms: 400,
            focus_follows_mouse: true,
            warp_mouse_to_focus: false,
            focus_new_windows: true,
            auto_float_dialogs: true,
            float_new_windows: false,
            cycle_focus: false,
            resize_step: 0.05,
            move_floating_step: 40,
            terminal: "kitty".into(),
            hot_reload: true,
        }
    }
}

impl Default for Input {
    fn default() -> Self {
        Self {
            kb_layout: "us,ru".into(),
            kb_variant: String::new(),
            kb_options: "grp:alt_shift_toggle".into(),
            kb_model: String::new(),
            repeat_rate: 25,
            repeat_delay: 600,
            numlock: false,
            accel_speed: 0.0,
            accel_profile: "adaptive".into(),
            natural_scroll: false,
            scroll_factor: 1.0,
            left_handed: false,
            touchpad_natural_scroll: true,
            touchpad_tap_to_click: true,
            touchpad_drag: true,
            touchpad_disable_while_typing: true,
            touchpad_scroll_factor: 1.0,
            touchpad_accel_speed: 0.2,
        }
    }
}

impl Default for Dwindle {
    fn default() -> Self {
        Self {
            split: "auto".into(),
            default_ratio: 0.5,
            new_window_side: "after".into(),
            preserve_split: false,
            smart_split: false,
            no_gaps_when_only: false,
            split_width_multiplier: 1.0,
        }
    }
}

impl Default for Master {
    fn default() -> Self {
        Self {
            orientation: "left".into(),
            master_ratio: 0.55,
            master_count: 1,
            new_on_top: false,
            new_is_master: false,
            no_gaps_when_only: false,
        }
    }
}

impl Default for Scroller {
    fn default() -> Self {
        Self {
            direction: "horizontal".into(),
            column_width: 0.5,
            column_width_presets: vec![0.3333, 0.5, 0.6667, 1.0],
            window_height_presets: vec![0.3333, 0.5, 0.6667, 1.0],
            center_focused: "never".into(),
            center_single_column: true,
            new_column_position: "after-focused".into(),
            scroll_speed: 1.0,
            wheel_scroll_step: 120.0,
            wheel_scrolls_view: true,
            touchpad_scrolls_view: true,
            snap_to_column: true,
            focus_follows_view: true,
            max_windows_per_column: 0,
        }
    }
}

impl Anim {
    fn new(duration_ms: u32, curve: &str, style: &str) -> Self {
        Self {
            enabled: true,
            duration_ms,
            curve: curve.into(),
            style: style.into(),
            from_scale: 0.85,
        }
    }
}

impl Default for Anim {
    fn default() -> Self {
        Anim::new(250, "ease-out-quint", "")
    }
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            enabled: true,
            speed: 1.0,
            windows_move: Anim::new(250, "spring(1.0, 800)", ""),
            windows_open: Anim::new(150, "ease-out-expo", "popin"),
            windows_close: Anim::new(150, "ease-out", "popin"),
            workspaces: Anim::new(250, "spring(1.0, 1000)", "slide"),
            scroll: Anim::new(250, "spring(1.0, 800)", ""),
            border_color: Anim::new(200, "ease-out", ""),
            fade: Anim::new(200, "ease-out", ""),
            wallpaper: Anim::new(600, "ease-in-out", "fade"),
        }
    }
}

impl Default for Gestures {
    fn default() -> Self {
        Self {
            workspace_swipe: true,
            workspace_swipe_fingers: 3,
            workspace_swipe_vertical: false,
            workspace_swipe_distance: 300.0,
            scroller_swipe_fingers: 3,
            swipe_invert: false,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            wallpaper: String::new(),
            wallpaper_dir: "~/Pictures/Wallpapers".into(),
            wallpaper_mode: "fill".into(),
            background_color: "#131314".into(),
            generate_colors: true,
            scheme: "content".into(),
            mode: "dark".into(),
            contrast: 0.0,
            builtin_templates: vec!["fewm".into()],
            templates: Vec::new(),
            blur_passes: 3,
            blur_radius: 24.0,
        }
    }
}

impl Default for Template {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            input: String::new(),
            output: String::new(),
            post_hook: String::new(),
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            theme: String::new(),
            size: 24,
            hide_after_ms: 0,
            hide_while_typing: false,
        }
    }
}

impl Default for Misc {
    fn default() -> Self {
        Self {
            xwayland: true,
            xwayland_satellite: "xwayland-satellite".into(),
            screenshot_dir: "~/Pictures/Screenshots".into(),
            game_mode_on: r#"d="${XDG_RUNTIME_DIR:-/tmp}/fewm-game"; mkdir -p "$d"; powerprofilesctl get > "$d/profile" 2>/dev/null && powerprofilesctl set performance 2>/dev/null; if command -v noctalia >/dev/null; then noctalia msg notification-dnd-status > "$d/dnd" 2>/dev/null; noctalia msg status 2>/dev/null | jq -r .barVisible > "$d/bar" 2>/dev/null; noctalia msg panel-close; noctalia msg bar-hide; noctalia msg notification-dnd-set on; fi >/dev/null 2>&1; qs ipc -c fewm-launcher call launcher hide >/dev/null 2>&1; true"#.into(),
            game_mode_off: r#"d="${XDG_RUNTIME_DIR:-/tmp}/fewm-game"; [ -s "$d/profile" ] && powerprofilesctl set "$(cat "$d/profile")" 2>/dev/null; if command -v noctalia >/dev/null; then [ "$(cat "$d/bar" 2>/dev/null)" != false ] && noctalia msg bar-show; [ -s "$d/dnd" ] && noctalia msg notification-dnd-set "$(cat "$d/dnd")"; fi >/dev/null 2>&1; true"#.into(),
            notify_config_errors: true,
            vrr: false,
            disable_power_key: false,
            background_dim_on_fullscreen: 0.0,
        }
    }
}

fn binds(list: &[(&str, &str)]) -> BTreeMap<String, String> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

impl Default for Config {
    fn default() -> Self {
        let mut b = vec![
            ("SUPER+Return", "spawn kitty"),
            ("SUPER+T", "spawn kitty"),
            ("SUPER+Q", "close"),
            ("SUPER+SHIFT+E", "exit"),
            ("SUPER+F", "fullscreen"),
            ("CTRL+SHIFT+F", "game-mode"),
            ("SUPER+M", "maximize"),
            ("SUPER+V", "toggle-float"),
            ("SUPER+C", "center"),
            ("SUPER+P", "pseudo"),
            ("SUPER+J", "toggle-split"),
            ("SUPER+R", "cycle-column-width"),
            ("SUPER+SHIFT+R", "reload"),
            ("SUPER+L", "layout next"),
            ("SUPER+Left", "focus left"),
            ("SUPER+Right", "focus right"),
            ("SUPER+Up", "focus up"),
            ("SUPER+Down", "focus down"),
            ("SUPER+SHIFT+Left", "move left"),
            ("SUPER+SHIFT+Right", "move right"),
            ("SUPER+SHIFT+Up", "move up"),
            ("SUPER+SHIFT+Down", "move down"),
            ("SUPER+CTRL+Left", "resize -0.05 0"),
            ("SUPER+CTRL+Right", "resize 0.05 0"),
            ("SUPER+CTRL+Up", "resize 0 -0.05"),
            ("SUPER+CTRL+Down", "resize 0 0.05"),
            ("SUPER+Tab", "workspace next"),
            ("SUPER+SHIFT+Tab", "workspace prev"),
            ("SUPER+Page_Down", "workspace next"),
            ("SUPER+Page_Up", "workspace prev"),
            ("SUPER+bracketleft", "consume-or-expel left"),
            ("SUPER+bracketright", "consume-or-expel right"),
            ("SUPER+W", "spawn fewm msg wallpaper-random"),
            ("Print", "spawn sh -c 'grim ~/Pictures/Screenshots/$(date +%F_%T).png'"),
            ("XF86AudioRaiseVolume", "spawn wpctl set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 5%+"),
            ("XF86AudioLowerVolume", "spawn wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
            ("XF86AudioMute", "spawn wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
            ("XF86MonBrightnessUp", "spawn brightnessctl set 5%+"),
            ("XF86MonBrightnessDown", "spawn brightnessctl set 5%-"),
        ];
        let ws: Vec<(String, String)> = (1..=10)
            .flat_map(|i| {
                let key = if i == 10 { "0".to_string() } else { i.to_string() };
                [
                    (format!("SUPER+{key}"), format!("workspace {i}")),
                    (format!("SUPER+SHIFT+{key}"), format!("move-to-workspace {i}")),
                ]
            })
            .collect();
        let mut binds_map = binds(&b);
        for (k, v) in ws {
            binds_map.insert(k, v);
        }
        b.clear();

        Self {
            general: General::default(),
            input: Input::default(),
            dwindle: Dwindle::default(),
            master: Master::default(),
            scroller: Scroller::default(),
            animations: Animations::default(),
            gestures: Gestures::default(),
            theme: Theme::default(),
            cursor: Cursor::default(),
            misc: Misc::default(),
            binds: binds_map,
            mouse_binds: binds(&[
                ("SUPER+mouse_left", "move"),
                ("SUPER+mouse_right", "resize"),
                ("SUPER+wheel_down", "scroll right"),
                ("SUPER+wheel_up", "scroll left"),
                ("SUPER+SHIFT+wheel_down", "workspace next"),
                ("SUPER+SHIFT+wheel_up", "workspace prev"),
            ]),
            env: BTreeMap::new(),
            autostart: Vec::new(),
            window_rules: vec![
                WindowRule {
                    app_id: "^(pavucontrol|org.pulseaudio.pavucontrol|nm-connection-editor|blueman-manager)$".into(),
                    float: Some(true),
                    size: "50%x60%".into(),
                    center: Some(true),
                    ..Default::default()
                },
                WindowRule {
                    app_id: "^kitty$".into(),
                    blur: Some(true),
                    ..Default::default()
                },
            ],
            workspace_rules: Vec::new(),
            outputs: Vec::new(),
        }
    }
}

impl Config {
    /// A config where every optional value and every list has one typed sample
    /// element. Used as the schema for type coercion and bash name mapping.
    pub fn schema_sample() -> Self {
        let mut c = Config::default();
        c.window_rules = vec![WindowRule {
            app_id: String::new(),
            title: String::new(),
            float: Some(false),
            fullscreen: Some(false),
            maximize: Some(false),
            size: String::new(),
            center: Some(false),
            workspace: Some(1),
            opacity: Some(1.0),
            no_border: Some(false),
            no_shadow: Some(false),
            no_anim: Some(false),
            blur: Some(false),
            column_width: Some(0.5),
        }];
        c.workspace_rules = vec![WorkspaceRule::default()];
        c.outputs = vec![OutputConfig::default()];
        c.theme.templates = vec![Template::default()];
        c.autostart = vec![String::new()];
        c
    }

    pub fn workspace_layout(&self, ws: u32) -> String {
        self.workspace_rules
            .iter()
            .find(|r| r.workspace == ws && !r.layout.is_empty())
            .map(|r| r.layout.clone())
            .unwrap_or_else(|| self.general.layout.clone())
    }
}

/// Paths that are free-form string maps (not structs).
pub const DYN_MAPS: &[&str] = &["binds", "mouse_binds", "env"];
