//! Wallpaper Engine (Steam) wallpapers: scenes, videos and web wallpapers from the Workshop are
//! rendered by linux-wallpaperengine on the background layer of every output that uses one.
//! FeWM keeps showing the wallpaper's preview image underneath (it feeds the palette, the window
//! blur and covers the moment before the engine is up) and pauses the engine (SIGSTOP on its
//! process group) in game mode, on the lock screen and while windows cover the wallpaper.
//!
//! A wallpaper is chosen like any other: theme.wallpaper / outputs[].wallpaper = "we:<id>",
//! where <id> is a Workshop id, a built-in project folder name or a path to a project folder.
//! Picking a wallpaper in the Wallpaper Engine app itself (Proton) works too: FeWM watches the app's
//! config.json and applies its selection. The cursor position is published to the engine through a
//! small file (FEWM_CURSOR_FILE), so wallpapers follow the mouse over windows too, like on Windows.

use std::collections::VecDeque;
use std::os::unix::fs::FileExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use tracing::{info, warn};

use crate::state::Fe;

const APP_ID: &str = "431960";

/// "we:123" / "wallpaperengine:123" / a folder with project.json → what linux-wallpaperengine takes as a background.
pub fn parse_ref(s: &str) -> Option<String> {
    let s = s.trim();
    for p in ["we:", "wallpaperengine:", "wallpaper-engine:"] {
        if let Some(r) = s.strip_prefix(p) {
            let r = r.trim();
            return (!r.is_empty()).then(|| r.to_string());
        }
    }
    let p = crate::config::expand_path(s);
    if !s.is_empty() && p.join("project.json").is_file() {
        return Some(p.to_string_lossy().into_owned());
    }
    None
}

#[derive(Debug, Clone)]
pub struct Item {
    /// what goes after "we:"
    pub id: String,
    pub dir: PathBuf,
    pub title: String,
    /// scene | video | web | application
    pub kind: String,
    pub preview: Option<PathBuf>,
}

/// Steam library folders (the main one + everything in libraryfolders.vdf).
pub fn steam_libraries() -> Vec<PathBuf> {
    let home = crate::config::home();
    let mut libs: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(c) = p.canonicalize() {
            if c.join("steamapps").is_dir() && !libs.contains(&c) {
                libs.push(c);
            }
        }
    };
    for root in [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam"] {
        let root = home.join(root);
        add(root.clone());
        if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            for line in vdf.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("\"path\"") {
                    let v = rest.trim().trim_matches('"').replace("\\\\", "\\");
                    add(PathBuf::from(v));
                }
            }
        }
    }
    libs
}

fn project_dirs() -> Vec<(PathBuf, bool)> {
    // (folder with one project per subfolder, is_workshop)
    let mut v = Vec::new();
    for lib in steam_libraries() {
        v.push((lib.join("steamapps/workshop/content").join(APP_ID), true));
        let we = lib.join("steamapps/common/wallpaper_engine/projects");
        v.push((we.join("defaultprojects"), false));
        v.push((we.join("myprojects"), false));
    }
    v
}

fn read_item(dir: &Path, id: String) -> Option<Item> {
    let text = std::fs::read_to_string(dir.join("project.json")).ok()?;
    let j: Value = serde_json::from_str(&text).ok()?;
    let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = Some(s("title")).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| id.clone());
    let preview = Some(s("preview")).filter(|p| !p.is_empty()).map(|p| dir.join(p)).filter(|p| p.is_file());
    Some(Item { id, dir: dir.to_path_buf(), title: title.trim().to_string(), kind: s("type").to_lowercase(), preview })
}

pub fn find_item(r: &str) -> Option<Item> {
    let p = Path::new(r);
    if p.is_absolute() {
        return read_item(p, r.to_string());
    }
    project_dirs().into_iter().find_map(|(d, _)| {
        let dir = d.join(r);
        dir.is_dir().then(|| read_item(&dir, r.to_string())).flatten()
    })
}

/// Every installed wallpaper: Workshop subscriptions first (newest first), then the built-in ones.
pub fn list_items() -> Vec<Item> {
    let mut out: Vec<(bool, std::time::SystemTime, Item)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (d, workshop) in project_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let dir = e.path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(it) = read_item(&dir, name) {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                out.push((workshop, t, it));
            }
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    out.into_iter().map(|(_, _, i)| i).collect()
}

/// The image FeWM itself shows / takes colours from for a wallpaper setting: the preview of a
/// Wallpaper Engine wallpaper, otherwise the path as is.
/// The PNG noctalia gets for a "we:" wallpaper (the preview's first frame: noctalia then builds the
/// same palette as FeWM for the bar, the launcher and its own app templates).
pub fn noctalia_image_for(wallpaper: &str) -> Option<PathBuf> {
    let r = parse_ref(wallpaper)?;
    let name: String = r.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    Some(crate::config::home().join(format!(".cache/FerriumWM/we-preview/{name}.png")))
}

/// Tell noctalia about the wallpaper (on a worker thread, after the image is decoded).
pub fn sync_noctalia(wallpaper: &str, image: Option<&image::RgbaImage>) {
    // a nested test instance must not touch the host session's shell
    if std::env::var_os("FEWM_NO_TEMPLATES").is_some() {
        return;
    }
    let target = match noctalia_image_for(wallpaper) {
        Some(png) => {
            let Some(img) = image else { return };
            if !png.is_file() {
                let _ = std::fs::create_dir_all(png.parent().unwrap());
                let tmp = png.with_extension("tmp.png");
                if let Err(e) = img.save(&tmp).and_then(|_| std::fs::rename(&tmp, &png).map_err(Into::into)) {
                    warn!("wallpaper engine: превью для noctalia {}: {e}", png.display());
                    return;
                }
            }
            png
        }
        None => crate::config::expand_path(wallpaper),
    };
    let target = target.to_string_lossy().into_owned();
    if target.is_empty() || crate::theme::noctalia_wallpaper().is_some_and(|(cur, _, _)| cur == target) {
        return;
    }
    let ok = Command::new("noctalia")
        .args(["msg", "wallpaper-set", &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        info!("noctalia: обои → {target}");
    }
}

pub fn image_for(wallpaper: &str) -> String {
    match parse_ref(wallpaper) {
        Some(r) => find_item(&r).and_then(|i| i.preview).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        None => wallpaper.to_string(),
    }
}

fn find_assets(cfg_dir: &str) -> Option<PathBuf> {
    if !cfg_dir.trim().is_empty() {
        return Some(crate::config::expand_path(cfg_dir));
    }
    steam_libraries().into_iter().map(|l| l.join("steamapps/common/wallpaper_engine/assets")).find(|p| p.is_dir())
}

// ---------------------------------------------------------------- the Wallpaper Engine app (Proton)

/// The app's config.json (it lives next to wallpaper64.exe).
fn app_config() -> Option<PathBuf> {
    steam_libraries().into_iter().map(|l| l.join("steamapps/common/wallpaper_engine/config.json")).find(|p| p.is_file())
}

/// "S:/steamapps/workshop/content/431960/123/project.json" → a Linux path, through the drive letters
/// of the app's Proton prefix (any library may hold the prefix).
fn wine_to_linux(p: &str) -> Option<PathBuf> {
    let p = p.replace('\\', "/");
    if p.starts_with('/') {
        return Some(PathBuf::from(p));
    }
    let (drive, rest) = p.split_once(':')?;
    if drive.len() != 1 {
        return None;
    }
    let letter = format!("{}:", drive.to_ascii_lowercase());
    steam_libraries().into_iter().find_map(|l| {
        let d = l.join(format!("steamapps/compatdata/{APP_ID}/pfx/dosdevices")).join(&letter);
        let root = d.canonicalize().ok()?;
        Some(root.join(rest.trim_start_matches('/')))
    })
}

/// What the app shows on its first monitor, as a "we:" reference.
fn app_selection(cfg: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cfg).ok()?).ok()?;
    let mut picks: Vec<(String, String)> = Vec::new();
    for user in v.as_object()?.values() {
        let Some(sel) = user.pointer("/general/wallpaperconfig/selectedwallpapers").and_then(|s| s.as_object()) else { continue };
        for (mon, w) in sel {
            if let Some(f) = w.get("file").and_then(|f| f.as_str()).filter(|f| !f.is_empty()) {
                picks.push((mon.clone(), f.to_string()));
            }
        }
    }
    picks.sort();
    app_ref(&picks.first()?.1)
}

/// A wallpaper file as the app names it ("S:/steamapps/workshop/content/431960/<id>/project.json",
/// ".../scene.pkg", ...) → a "we:" reference.
fn app_ref(file: &str) -> Option<String> {
    let path = wine_to_linux(file)?;
    let dir = if path.is_dir() { path } else { path.parent()?.to_path_buf() };
    let id = dir.file_name()?.to_string_lossy().into_owned();
    // Workshop items by id (that is what we-pick and the config use), anything else by path
    if dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == APP_ID) {
        return Some(format!("we:{id}"));
    }
    dir.join("project.json").is_file().then(|| format!("we:{}", dir.display()))
}

/// Port the app's UI reports picks to (see patch_ui).
const UI_PORT: u16 = 47431;
const UI_MARK: &str = "/*FEWM*/";

fn ui_script() -> Option<PathBuf> {
    steam_libraries()
        .into_iter()
        .map(|l| l.join("steamapps/common/wallpaper_engine/ui/dist/scripts/scripts.js"))
        .find(|p| p.is_file())
}

/// Under Proton the app can't put a wallpaper on the desktop, so it never even saves the pick.
/// Its UI is a web page: next to its native selectWallpaper(file, monitor) call add a request to
/// FeWM with the same arguments. The original file is kept as scripts.js.fewm-orig; a Steam update
/// brings back the unpatched file and this runs again.
fn patch_ui(p: &Path) -> Result<bool, String> {
    let s = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
    if s.contains(UI_MARK) {
        return Ok(false);
    }
    let needle = r#".callDeferred("browseWallpaperObject","selectWallpaper","#;
    let i = s.find(needle).ok_or("нет вызова selectWallpaper (новая версия интерфейса?)")?;
    let start = s[..i].rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')).map(|k| k + 1).unwrap_or(0);
    let mut args = s[i + needle.len()..].splitn(3, ',');
    let file = args.next().filter(|a| !a.is_empty() && !a.contains(['(', ')'])).ok_or("не разобрал аргументы")?;
    let mon = args.next().filter(|a| !a.is_empty() && !a.contains(['(', ')'])).ok_or("не разобрал аргументы")?;
    let hook = format!(
        "{UI_MARK}(function(f,m){{try{{(new Image).src=\"http://127.0.0.1:{UI_PORT}/fewm-select?m=\"+encodeURIComponent(m)+\"&f=\"+encodeURIComponent(f)}}catch(x){{}}}})({file},{mon}),"
    );
    let out = format!("{}{hook}{}", &s[..start], &s[start..]);
    let _ = std::fs::write(p.with_file_name("scripts.js.fewm-orig"), &s);
    std::fs::write(p, out).map_err(|e| e.to_string())?;
    Ok(true)
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Tiny HTTP listener for the patched UI: GET /fewm-select?m=<monitor>&f=<file> → the file.
fn ui_listener(tx: smithay::reexports::calloop::channel::Sender<String>) {
    use std::io::{Read, Write};
    let l = match std::net::TcpListener::bind(("127.0.0.1", UI_PORT)) {
        Ok(l) => l,
        Err(e) => {
            // a nested test instance next to the session: the session has it
            info!("wallpaper engine: порт {UI_PORT} занят ({e}), выбор в приложении не слушаю");
            return;
        }
    };
    std::thread::Builder::new()
        .name("we-ui".into())
        .spawn(move || {
            for c in l.incoming().flatten() {
                let mut c = c;
                let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 8192];
                let n = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("");
                let _ = c.write_all(b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n");
                if let Some(q) = path.strip_prefix("/fewm-select?") {
                    if let Some(f) = q.split('&').find_map(|kv| kv.strip_prefix("f=")) {
                        if tx.send(url_decode(f)).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .ok();
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------- the engine process

#[derive(Default)]
pub struct Engine {
    child: Option<Child>,
    args: Vec<String>,
    paused: bool,
    crashes: VecDeque<Instant>,
    /// gave up after crashing repeatedly with these args (until they change)
    failed: Option<Vec<String>>,
    assets: Option<(String, Option<PathBuf>)>,
    warned_assets: bool,
    /// FEWM_CURSOR_FILE: the global cursor position (two f64, logical layout coords) for the engine
    cursor: Option<(PathBuf, std::fs::File)>,
    /// the Wallpaper Engine app's config.json and its last seen mtime / selection
    app_cfg: Option<(PathBuf, Option<SystemTime>)>,
    app_selection: Option<String>,
    /// the app's UI script and its mtime when last checked / patched
    ui: Option<(PathBuf, Option<SystemTime>)>,
}

impl Engine {
    fn signal(&self, sig: i32) {
        if let Some(c) = &self.child {
            // the whole process group: web wallpapers run CEF helper processes
            unsafe { libc::kill(-(c.id() as i32), sig) };
        }
    }

    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let pid = c.id() as i32;
            unsafe {
                libc::kill(-pid, libc::SIGCONT);
                libc::kill(-pid, libc::SIGTERM);
            }
            // reap without blocking the compositor
            std::thread::spawn(move || {
                for _ in 0..30 {
                    if let Ok(Some(_)) = c.try_wait() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                let _ = c.wait();
            });
        }
        self.paused = false;
    }
}

fn log_path() -> PathBuf {
    crate::config::home().join(".cache/FerriumWM/wallpaperengine.log")
}

impl Fe {
    /// What linux-wallpaperengine should run with right now; empty = nothing to show.
    fn we_desired_args(&mut self) -> Vec<String> {
        let we = self.cfg.wallpaper_engine.clone();
        let mut bgs: Vec<String> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for o in &self.outputs {
            let name = o.output.name();
            let per = self.cfg.outputs.iter().find(|c| c.name == name).map(|c| c.wallpaper.clone()).filter(|w| !w.trim().is_empty());
            let wp = per.unwrap_or_else(|| self.cfg.theme.wallpaper.clone());
            if let Some(r) = parse_ref(&wp) {
                bgs.extend(["--screen-root".into(), name, "--bg".into(), r.clone()]);
                bgs.extend(["--scaling".into(), we.scaling.clone(), "--clamp".into(), we.clamp.clone()]);
                ids.push(r);
            }
        }
        if bgs.is_empty() {
            return Vec::new();
        }
        let mut a: Vec<String> = vec!["--layer".into(), "background".into(), "--fps".into(), we.fps.max(1).to_string()];
        a.extend(["--hwdec".into(), we.hwdec.clone()]);
        if we.volume == 0 {
            a.push("--silent".into());
        } else {
            a.extend(["--volume".into(), we.volume.min(100).to_string()]);
        }
        if !we.automute {
            a.push("--noautomute".into());
        }
        if !we.audio_reactive {
            a.push("--no-audio-processing".into());
        }
        if !we.mouse {
            a.push("--disable-mouse".into());
        }
        if !we.parallax {
            a.push("--disable-parallax".into());
        }
        if !we.particles {
            a.push("--disable-particles".into());
        }
        // FeWM pauses the engine itself (it knows about game mode, the lock screen and its own fullscreen)
        a.push("--no-fullscreen-pause".into());
        let cached = self.we.assets.as_ref().filter(|(k, _)| *k == we.assets_dir).map(|(_, v)| v.clone());
        let assets = cached.unwrap_or_else(|| {
            let v = find_assets(&we.assets_dir);
            self.we.assets = Some((we.assets_dir.clone(), v.clone()));
            v
        });
        match assets {
            Some(p) => a.extend(["--assets-dir".into(), p.to_string_lossy().into_owned()]),
            None if !self.we.warned_assets => {
                self.we.warned_assets = true;
                crate::notify(self, "Wallpaper Engine", "Не нашёл assets: установи Wallpaper Engine в Steam (или укажи wallpaper_engine.assets_dir)");
            }
            None => {}
        }
        for (k, v) in &we.properties {
            match k.split_once('.') {
                Some((id, prop)) if ids.iter().any(|i| i == id) => a.extend(["--set-property".into(), format!("{prop}={v}")]),
                Some(_) => {}
                None => a.extend(["--set-property".into(), format!("{k}={v}")]),
            }
        }
        a.extend(bgs);
        a
    }

    /// Start / restart / stop the engine to match the config and the outputs.
    pub fn we_sync(&mut self) {
        let args = self.we_desired_args();
        if args.is_empty() {
            if self.we.child.is_some() {
                info!("wallpaper engine: выключен");
                self.we.stop();
            }
            self.we.args.clear();
            return;
        }
        if self.we.failed.as_ref() == Some(&args) {
            return;
        }
        self.we.failed = None;
        if self.we.child.is_some() && self.we.args == args {
            return;
        }
        self.we.stop();
        self.we_spawn(args);
    }

    fn we_spawn(&mut self, args: Vec<String>) {
        let bin = crate::config::expand_path(&self.cfg.wallpaper_engine.binary);
        let log = log_path();
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let out = std::fs::File::create(&log).ok();
        let mut c = Command::new(&bin);
        c.args(&args);
        c.env("WAYLAND_DISPLAY", &self.socket_name);
        c.env("XDG_SESSION_TYPE", "wayland");
        c.env("XDG_CURRENT_DESKTOP", "FerriumWM");
        c.env_remove("DISPLAY");
        if let Some(path) = self.we_cursor_file() {
            c.env("FEWM_CURSOR_FILE", path);
        }
        c.stdin(Stdio::null());
        match out.as_ref().and_then(|f| f.try_clone().ok().zip(f.try_clone().ok())) {
            Some((o, e)) => {
                c.stdout(o).stderr(e);
            }
            None => {
                c.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        c.process_group(0);
        unsafe {
            c.pre_exec(|| {
                // dies with FeWM
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        match c.spawn() {
            Ok(child) => {
                info!("wallpaper engine: {} {}", bin.display(), args.join(" "));
                self.we.child = Some(child);
                self.we.args = args;
                self.we.paused = false;
            }
            Err(e) => {
                warn!("wallpaper engine: не запустить {}: {e}", bin.display());
                crate::notify(self, "Wallpaper Engine", &format!("Не запустить {}: {e}", bin.display()));
                self.we.failed = Some(args);
            }
        }
    }

    /// Whether the wallpapers can't be seen right now.
    fn we_hidden(&self) -> bool {
        if self.game.is_some() || self.lock.locked {
            return true;
        }
        let mode = self.cfg.wallpaper_engine.pause.as_str();
        if mode == "never" {
            return false;
        }
        // every output that shows an engine wallpaper has to be covered
        let mut any = false;
        for o in &self.outputs {
            let name = o.output.name();
            if !self.we.args.windows(2).any(|w| w[0] == "--screen-root" && w[1] == name) {
                continue;
            }
            any = true;
            if o.switch.is_some() {
                return false;
            }
            let covered = self.windows.values().any(|w| {
                w.ws == o.ws && w.mapped && (w.fullscreen || (mode == "windows" && !w.floating))
            });
            if !covered {
                return false;
            }
        }
        any
    }

    fn we_tick(&mut self) {
        self.we_follow_app();
        // crashed?
        let exited = self.we.child.as_mut().and_then(|c| c.try_wait().ok().flatten());
        if let Some(status) = exited {
            self.we.child = None;
            let now = Instant::now();
            self.we.crashes.push_back(now);
            while self.we.crashes.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                self.we.crashes.pop_front();
            }
            warn!("wallpaper engine завершился: {status}");
            if self.we.crashes.len() >= 3 {
                self.we.failed = Some(self.we.args.clone());
                self.we.crashes.clear();
                let tail = std::fs::read_to_string(log_path())
                    .map(|t| t.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                crate::notify(self, "Wallpaper Engine упал", &format!("{tail}\nЛог: ~/.cache/FerriumWM/wallpaperengine.log"));
            }
            self.we.args.clear();
        }
        self.we_sync();
        let hide = self.we_hidden();
        if self.we.child.is_some() && hide != self.we.paused {
            self.we.signal(if hide { libc::SIGSTOP } else { libc::SIGCONT });
            self.we.paused = hide;
        }
    }

    /// The cursor file, created on first use; filled with the current cursor position.
    fn we_cursor_file(&mut self) -> Option<PathBuf> {
        if self.we.cursor.is_none() {
            let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let path = dir.join(format!("fewm-we-cursor-{}", self.socket_name));
            match std::fs::OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&path) {
                Ok(f) => self.we.cursor = Some((path, f)),
                Err(e) => {
                    warn!("wallpaper engine: курсор-файл {}: {e}", path.display());
                    return None;
                }
            }
            if let Some(p) = self.seat.get_pointer() {
                self.we_cursor(p.current_location());
            }
        }
        self.we.cursor.as_ref().map(|(p, _)| p.clone())
    }

    /// Called on every pointer move: tell the engine where the cursor is (also over windows).
    pub fn we_cursor(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        if let Some((_, f)) = &self.we.cursor {
            let mut buf = [0u8; 16];
            buf[..8].copy_from_slice(&pos.x.to_ne_bytes());
            buf[8..].copy_from_slice(&pos.y.to_ne_bytes());
            let _ = f.write_at(&buf, 0);
        }
    }

    /// Keep the app's UI hooked (patch_ui); rechecked whenever its script changes.
    fn we_patch_ui(&mut self) {
        if self.we.ui.is_none() {
            let Some(p) = ui_script() else { return };
            self.we.ui = Some((p, None));
        }
        let Some((p, seen)) = self.we.ui.clone() else { return };
        let now = mtime(&p);
        if now == seen {
            return;
        }
        match patch_ui(&p) {
            Ok(true) => info!("wallpaper engine: интерфейс приложения подключён ({}), открой его окно заново", p.display()),
            Ok(false) => {}
            Err(e) => {
                warn!("wallpaper engine: не подключил интерфейс приложения: {e}");
                crate::notify(self, "Wallpaper Engine", &format!("Не подключился к приложению: {e}"));
            }
        }
        self.we.ui = Some((p.clone(), mtime(&p)));
    }

    /// A pick reported by the app's UI.
    fn we_app_pick(&mut self, file: &str) {
        if !self.cfg.wallpaper_engine.follow_steam_app {
            return;
        }
        let Some(r) = app_ref(file) else {
            warn!("wallpaper engine: не нашёл обои из приложения: {file}");
            return;
        };
        self.we.app_selection = Some(r.clone());
        if r != self.cfg.theme.wallpaper {
            info!("wallpaper engine: выбрано в приложении: {r}");
            crate::wallpaper::set_wallpaper(self, &r);
        }
    }

    /// A wallpaper picked in the Wallpaper Engine app → our wallpaper.
    fn we_follow_app(&mut self) {
        if !self.cfg.wallpaper_engine.follow_steam_app {
            return;
        }
        self.we_patch_ui();
        if self.we.app_cfg.is_none() {
            // remember the current state; only later changes are applied
            let Some(p) = app_config() else { return };
            self.we.app_selection = app_selection(&p);
            self.we.app_cfg = Some((p.clone(), mtime(&p)));
            return;
        }
        let Some((path, seen)) = self.we.app_cfg.clone() else { return };
        let now = mtime(&path);
        if now == seen {
            return;
        }
        self.we.app_cfg = Some((path.clone(), now));
        let sel = app_selection(&path);
        if sel.is_none() || sel == self.we.app_selection {
            return;
        }
        self.we.app_selection = sel.clone();
        let sel = sel.unwrap();
        if sel != self.cfg.theme.wallpaper {
            info!("wallpaper engine: выбрано в приложении: {sel}");
            crate::wallpaper::set_wallpaper(self, &sel);
        }
    }

    pub fn we_restart(&mut self) {
        self.we.stop();
        self.we.failed = None;
        self.we.args.clear();
        self.we.assets = None;
        self.we.warned_assets = false;
        self.we_sync();
    }

    /// we-random / we-next / we-prev
    pub fn we_cycle(&mut self, how: &str) {
        let items = list_items();
        if items.is_empty() {
            crate::notify(self, "Wallpaper Engine", "Нет обоев: подпишись на обои в Мастерской Steam");
            return;
        }
        let cur = parse_ref(&self.cfg.theme.wallpaper);
        let idx = cur.and_then(|c| items.iter().position(|i| i.id == c));
        let n = items.len();
        let next = match how {
            "we-next" => idx.map(|i| (i + 1) % n).unwrap_or(0),
            "we-prev" => idx.map(|i| (i + n - 1) % n).unwrap_or(0),
            _ => {
                let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                let mut k = (seed % n as u128) as usize;
                if Some(k) == idx && n > 1 {
                    k = (k + 1) % n;
                }
                k
            }
        };
        let id = items[next].id.clone();
        crate::wallpaper::set_wallpaper(self, &format!("we:{id}"));
    }
}

pub fn init(fe: &mut Fe) {
    let (tx, rx) = smithay::reexports::calloop::channel::channel::<String>();
    let _ = fe.lh.insert_source(rx, |ev, _, fe| {
        if let smithay::reexports::calloop::channel::Event::Msg(file) = ev {
            fe.we_app_pick(&file);
        }
    });
    ui_listener(tx);
    let _ = fe.lh.insert_source(Timer::from_duration(Duration::from_millis(300)), |_, _, fe| {
        fe.we_tick();
        TimeoutAction::ToDuration(Duration::from_millis(400))
    });
}

/// JSON list for `fewm msg we-list`.
pub fn list_json() -> Value {
    Value::Array(
        list_items()
            .into_iter()
            .map(|i| {
                serde_json::json!({
                    "id": i.id, "title": i.title, "type": i.kind,
                    "preview": i.preview.map(|p| p.to_string_lossy().into_owned()),
                    "path": i.dir.to_string_lossy(),
                })
            })
            .collect(),
    )
}

/// `fewm we-pick`: choose a wallpaper in fuzzel and apply it through IPC.
pub fn pick() -> i32 {
    let items = list_items();
    if items.is_empty() {
        let _ = Command::new("notify-send").args(["Wallpaper Engine", "Нет обоев: установи Wallpaper Engine и подпишись на обои в Мастерской"]).status();
        return 1;
    }
    let kind = |k: &str| match k {
        "scene" => "сцена",
        "video" => "видео",
        "web" => "веб",
        "application" => "программа",
        other => other,
    }
    .to_string();
    let mut input = String::new();
    for i in &items {
        input.push_str(&format!("{}  ·  {}", i.title, kind(&i.kind)));
        // fuzzel shows PNG/SVG icons only
        if let Some(p) = i.preview.as_ref().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))) {
            input.push_str(&format!("\0icon\x1f{}", p.display()));
        }
        input.push('\n');
    }
    let child = Command::new("fuzzel")
        .args(["--dmenu", "--index", "--prompt", "обои › ", "--width", "60", "--lines", "15"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("нужен fuzzel");
        return 1;
    };
    {
        use std::io::Write;
        let _ = child.stdin.take().map(|mut s| s.write_all(input.as_bytes()));
    }
    let Ok(out) = child.wait_with_output() else { return 1 };
    let Some(idx) = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>().ok() else { return 1 };
    let Some(it) = items.get(idx) else { return 1 };
    crate::ipc::client(&["wallpaper".to_string(), format!("we:{}", it.id)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_ui_real_script() {
        let Some(orig) = ui_script().map(|p| p.with_file_name("scripts.js.fewm-orig")).filter(|p| p.is_file()) else { return };
        let dir = std::env::temp_dir().join(format!("fewm-ui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("scripts.js");
        std::fs::copy(&orig, &p).unwrap();
        assert_eq!(patch_ui(&p), Ok(true));
        assert_eq!(patch_ui(&p), Ok(false));
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains(r#"/*FEWM*/(function(f,m){try{(new Image).src="http://127.0.0.1:47431/fewm-select?m="+encodeURIComponent(m)+"&f="+encodeURIComponent(f)}catch(x){}})(e.file,t),F.callDeferred("browseWallpaperObject","selectWallpaper",e.file,t,"#), "{}", &s[s.find("/*FEWM*/").unwrap()-40..s.find("/*FEWM*/").unwrap()+300]);
        let _ = std::fs::remove_dir_all(&dir);
        // the pick from the real app
        assert_eq!(app_ref("S:/steamapps/workshop/content/431960/3793885664/gifscene.pkg").as_deref(), Some("we:3793885664"));
    }

    #[test]
    fn url_decode_works() {
        assert_eq!(url_decode("S%3A%2Fsteamapps%2Fa%20b+c%2"), "S:/steamapps/a b c%2");
    }

    #[test]
    fn app_selection_formats() {
        let dir = std::env::temp_dir().join(format!("fewm-we-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        // what the app writes (Windows paths through the Proton drive letters)
        let Some(lib) = steam_libraries().into_iter().find(|l| l.join(format!("steamapps/workshop/content/{APP_ID}")).is_dir()) else { return };
        let ws = lib.join(format!("steamapps/workshop/content/{APP_ID}"));
        let Some(item) = std::fs::read_dir(&ws).unwrap().flatten().map(|e| e.path()).find(|p| p.join("project.json").is_file()) else { return };
        let id = item.file_name().unwrap().to_string_lossy().into_owned();
        let json = format!(
            r#"{{"steamuser":{{"general":{{"wallpaperconfig":{{"selectedwallpapers":{{"Monitor0":{{"file":"S:/steamapps/workshop/content/{APP_ID}/{id}/project.json"}}}}}}}}}}}}"#
        );
        std::fs::write(&cfg, json).unwrap();
        assert_eq!(app_selection(&cfg), Some(format!("we:{id}")));
        std::fs::write(&cfg, r#"{"steamuser":{"general":{"wallpaperconfig":{"selectedwallpapers":null}}}}"#).unwrap();
        assert_eq!(app_selection(&cfg), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
