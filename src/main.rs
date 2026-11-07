//! FerriumWM (FeWM) — a tiling Wayland compositor.

mod anim;
mod backend;
mod capture;
mod closing;
mod game;
mod config;
mod cursor;
mod drag;
mod input;
mod ipc;
mod layout;
mod lock;
mod render;
mod screenshot;
mod shell;
mod state;
mod style;
mod theme;
mod wallpaper;

use std::time::Duration;

use smithay::reexports::{
    calloop::{EventLoop, channel::{Event as ChanEvent, channel}},
    wayland_server::Display,
};
use tracing::{error, info, warn};

use config::{Config, sync::SyncEvent};
use state::Fe;

const HELP: &str = "FerriumWM (FeWM) — тайлинговый Wayland-композитор

Использование:
  fewm                 запустить (внутри другого композитора — окном, из TTY — сессией)
  fewm --nested        запустить окном внутри текущей сессии
  fewm --session       запустить на DRM/KMS (из TTY)
  fewm msg <действие>  отправить действие запущенному FeWM, например: fewm msg workspace 2
  fewm msg windows|workspaces|outputs|focused|colors|config|events
  fewm import-niri     заново импортировать хоткеи из ~/.config/niri/config.kdl
  fewm sync            перегенерировать config.py/.lua/.sh из самого свежего
  fewm check           проверить конфиги и style.css

Конфиги: ~/.config/FerriumWM/configs/config.{py,lua,sh}, вид: ~/.config/FerriumWM/style.css";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("msg") => std::process::exit(ipc::client(&args[1..])),
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            return;
        }
        Some("-V" | "--version") => {
            println!("fewm {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("import-niri") => {
            init_logging();
            let (mut cfg, _) = config::sync::load_initial(|_| {});
            import_niri(&mut cfg);
            match config::sync::write_all(&cfg, None) {
                Ok(_) => println!("импортировал {} хоткеев", cfg.binds.len() + cfg.mouse_binds.len()),
                Err(e) => eprintln!("{e:#}"),
            }
            return;
        }
        Some("sync") => {
            init_logging();
            let (cfg, msgs) = config::sync::load_initial(first_run);
            for m in msgs {
                eprintln!("{m}");
            }
            println!("ok: раскладка {}, {} хоткеев", cfg.general.layout, cfg.binds.len());
            return;
        }
        Some("check") => {
            init_logging();
            let mut ok = true;
            for l in config::langs::ALL {
                let p = config::sync::configs_dir().join(l.file_name());
                match config::sync::eval_file(l, &p) {
                    Ok((_, w)) => {
                        println!("{}: ok", l.file_name());
                        for x in w {
                            println!("  предупреждение: {x}");
                        }
                    }
                    Err(e) => {
                        ok = false;
                        println!("{}: {e:#}", l.file_name())
                    }
                }
            }
            let css = std::fs::read_to_string(config::sync::style_path()).unwrap_or_default();
            let s = style::Stylesheet::parse(&css, &theme::palette::Palette::fallback().css_vars());
            if s.errors.is_empty() {
                println!("style.css: ok");
            } else {
                ok = false;
                for e in s.errors {
                    println!("style.css: {e}");
                }
            }
            std::process::exit(if ok { 0 } else { 1 });
        }
        _ => {}
    }
    init_logging();
    let nested = if args.iter().any(|a| a == "--nested" || a == "--winit") {
        true
    } else if args.iter().any(|a| a == "--session" || a == "--tty" || a == "--udev") {
        false
    } else {
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
    };
    if let Err(e) = run(nested) {
        error!("{e:#}");
        std::process::exit(1);
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("FEWM_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,smithay=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}

fn import_niri(cfg: &mut Config) {
    let p = config::home().join(".config/niri/config.kdl");
    if let Some(imp) = config::import_niri::import(&p) {
        if !imp.binds.is_empty() {
            cfg.binds = imp.binds;
        }
        if !imp.mouse_binds.is_empty() {
            // keep FeWM's drag/resize with SUPER+mouse
            let mut m = imp.mouse_binds;
            m.entry("SUPER+mouse_left".into()).or_insert_with(|| "move".into());
            m.entry("SUPER+mouse_right".into()).or_insert_with(|| "resize".into());
            cfg.mouse_binds = m;
        }
        if !imp.autostart.is_empty() {
            cfg.autostart = imp.autostart;
        }
        if let Some(l) = imp.kb_layout {
            cfg.input.kb_layout = l;
            cfg.input.kb_options = imp.kb_options.unwrap_or_default();
        }
        info!("импортировал хоткеи из {}", p.display());
    }
}

/// Personalize defaults on the very first start: niri binds, noctalia theming.
fn first_run(cfg: &mut Config) {
    import_niri(cfg);
    let templates = theme::import_noctalia();
    if !templates.is_empty() {
        info!("импортировал {} шаблонов цветов из noctalia", templates.len());
        cfg.theme.templates = templates;
    }
    if let Some((wp, dir, scheme)) = theme::noctalia_wallpaper() {
        cfg.theme.wallpaper = wp;
        if !dir.is_empty() {
            cfg.theme.wallpaper_dir = dir;
        }
        cfg.theme.scheme = scheme;
    }
}

pub fn notify(fe: &Fe, title: &str, body: &str) {
    if !fe.cfg.misc.notify_config_errors {
        return;
    }
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "FerriumWM", "-u", "normal", title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut c| std::thread::spawn(move || c.wait()));
}

/// Rebuild the stylesheet (palette or CSS changed).
pub fn apply_style(fe: &mut Fe) {
    let sheet = style::Stylesheet::parse(&fe.css_src, &fe.palette.css_vars());
    if !sheet.errors.is_empty() {
        let msg = sheet.errors.join("\n");
        warn!("style.css: {msg}");
        notify(fe, "FerriumWM: style.css", &msg);
    }
    fe.style = sheet;
    fe.relayout_all();
    fe.queue_redraw();
}

pub fn apply_config(fe: &mut Fe, new: Config) {
    let old = std::mem::replace(&mut fe.cfg, new);
    fe.reapply_game_mode();
    fe.binds = input::parse_binds(&fe.cfg);
    if old.input != fe.cfg.input {
        let kb = fe.seat.get_keyboard().unwrap();
        let cfg_copy = fe.cfg.clone();
        let xkb = input::xkb_config(&cfg_copy);
        if let Err(e) = kb.set_xkb_config(fe, xkb) {
            warn!("раскладка: {e:?}");
            notify(fe, "FerriumWM", "не удалось применить раскладку клавиатуры");
        }
        kb.change_repeat_info(fe.cfg.input.repeat_rate, fe.cfg.input.repeat_delay);
        backend::udev::reconfigure_inputs(fe);
    }
    fe.cursor.configure(&fe.cfg);
    fe.ensure_workspaces();
    // layouts: follow general.layout / workspace rules when they changed
    for ws in 1..=fe.workspaces.len() as u32 {
        let want = fe.cfg.workspace_layout(ws);
        let was = old.workspace_layout(ws);
        let cur = fe.workspaces[ws as usize - 1].layout.name().to_string();
        if want != was && cur != want {
            fe.set_layout(ws, &want);
        }
    }
    let t_old = &old.theme;
    let t_new = &fe.cfg.theme;
    if t_old.wallpaper != t_new.wallpaper
        || t_old.scheme != t_new.scheme
        || t_old.mode != t_new.mode
        || t_old.contrast != t_new.contrast
        || t_old.generate_colors != t_new.generate_colors
        || t_old.blur_radius != t_new.blur_radius
        || t_old.blur_passes != t_new.blur_passes
        || t_old.background_color != t_new.background_color
    {
        let wp = fe.cfg.theme.wallpaper.clone();
        wallpaper::load_async(fe, &wp);
    } else if t_old.templates != t_new.templates || t_old.builtin_templates != t_new.builtin_templates {
        let wp = config::expand_path(&fe.wallpaper.path).to_string_lossy().into_owned();
        theme::apply_templates(&fe.cfg, &fe.palette, &wp);
    }
    if fe.cfg.misc.xwayland && !old.misc.xwayland {
        start_xwayland(fe);
    }
    fe.relayout_all();
    fe.queue_redraw();
}

pub fn config_reload(fe: &mut Fe) {
    let (cfg, msgs) = config::sync::load_initial(|_| {});
    if !msgs.is_empty() {
        notify(fe, "FerriumWM: конфиг", &msgs.join("\n"));
    }
    apply_config(fe, cfg);
    fe.css_src = std::fs::read_to_string(config::sync::style_path()).unwrap_or_else(|_| style::DEFAULT_CSS.into());
    apply_style(fe);
    info!("конфиг перезагружен");
}

fn start_xwayland(fe: &mut Fe) {
    if !fe.cfg.misc.xwayland || fe.xwayland_display.is_some() {
        return;
    }
    let bin = fe.cfg.misc.xwayland_satellite.clone();
    // find a free X display
    let n = (1..64)
        .find(|n| {
            !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                && !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists()
        })
        .unwrap_or(42);
    let disp = format!(":{n}");
    let r = std::process::Command::new(&bin)
        .arg(&disp)
        .env("WAYLAND_DISPLAY", &fe.socket_name)
        .env_remove("DISPLAY")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match r {
        Ok(child) => {
            info!("xwayland-satellite на {disp}");
            fe.xwayland_display = Some(disp.clone());
            std::thread::spawn(move || {
                let mut c = child;
                let _ = c.wait();
            });
        }
        Err(e) => warn!("xwayland-satellite не запустился ({bin}): {e}"),
    }
}

fn run(nested: bool) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<'static, Fe> = EventLoop::try_new()?;
    let display: Display<Fe> = Display::new()?;

    let (cfg, msgs) = config::sync::load_initial(first_run);
    for m in &msgs {
        warn!("{m}");
    }
    // default stylesheet
    let style_path = config::sync::style_path();
    if !style_path.exists() {
        let _ = std::fs::create_dir_all(style_path.parent().unwrap());
        let _ = std::fs::write(&style_path, style::DEFAULT_CSS);
    }

    let mut fe = Fe::new(display, event_loop.handle(), event_loop.get_signal(), cfg);
    if !msgs.is_empty() {
        notify(&fe, "FerriumWM: конфиг", &msgs.join("\n"));
    }

    if nested {
        backend::winit::init(&mut event_loop, &mut fe)?;
    } else {
        backend::udev::init(&mut event_loop, &mut fe)?;
    }

    // after the backend: children must talk to us
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &fe.socket_name);
        std::env::set_var("XDG_CURRENT_DESKTOP", "FerriumWM");
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::remove_var("DISPLAY");
    }
    if nested && std::env::var_os("FEWM_NESTED_TEMPLATES").is_none() {
        // don't touch the host session's app themes while testing
        unsafe { std::env::set_var("FEWM_NO_TEMPLATES", "1") };
    }
    if nested {
        // nested instance gets its own IPC socket name
        unsafe { std::env::set_var("FEWM_INSTANCE", format!("-{}", fe.socket_name)) };
    }
    ipc::init(&mut fe);

    // config + style hot reload
    let (tx, rx) = channel::<SyncEvent>();
    event_loop
        .handle()
        .insert_source(rx, |ev, _, fe| {
            if let ChanEvent::Msg(ev) = ev {
                match ev {
                    SyncEvent::Config(cfg, lang, warnings) => {
                        if !fe.cfg.general.hot_reload {
                            return;
                        }
                        if let Some(l) = lang {
                            info!("применяю {}", l.file_name());
                        }
                        if !warnings.is_empty() {
                            notify(fe, "FerriumWM: конфиг", &warnings.join("\n"));
                        }
                        apply_config(fe, *cfg);
                    }
                    SyncEvent::Style(css) => {
                        fe.css_src = css;
                        apply_style(fe);
                    }
                    SyncEvent::Error(e) => notify(fe, "FerriumWM: ошибка в конфиге", &e),
                    SyncEvent::Wallpaper(path) => {
                        if path != fe.cfg.theme.wallpaper && !path.is_empty() {
                            info!("noctalia сменила обои → {path}");
                            wallpaper::set_wallpaper(fe, &path);
                        }
                    }
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    config::sync::spawn_watcher(tx);

    // wallpaper loading
    let (wtx, wrx) = channel::<wallpaper::Loaded>();
    fe.wallpaper.tx = Some(wtx);
    event_loop
        .handle()
        .insert_source(wrx, |ev, _, fe| {
            if let ChanEvent::Msg(l) = ev {
                wallpaper::loaded(fe, l);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let wp = fe.cfg.theme.wallpaper.clone();
    wallpaper::load_async(&mut fe, &wp);

    start_xwayland(&mut fe);
    if !nested {
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg("dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE DISPLAY 2>/dev/null; systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP DISPLAY 2>/dev/null")
            .env("DISPLAY", fe.xwayland_display.clone().unwrap_or_default())
            .spawn();
    }
    if !nested || std::env::var_os("FEWM_NESTED_AUTOSTART").is_some() {
        for cmd in fe.cfg.autostart.clone() {
            fe.spawn(&cmd);
        }
    }
    if let Ok(cmd) = std::env::var("FEWM_STARTUP") {
        fe.spawn(&cmd);
    }

    info!("FerriumWM запущен ({})", if nested { "вложенный" } else { "сессия" });
    event_loop.run(Some(Duration::from_millis(16)), &mut fe, |fe| {
        if fe.redraw || fe.tick_animations(std::time::Instant::now()) {
            fe.queue_redraw();
        }
        backend::udev::watchdog(fe);
        // deliver input and protocol events right away, not only after a frame
        let _ = fe.dh.flush_clients();
    })?;
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}
