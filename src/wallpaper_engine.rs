//! Wallpaper Engine (Steam) wallpapers: scenes, videos and web wallpapers from the Workshop are
//! rendered by linux-wallpaperengine on the background layer of every output that uses one.
//! FeWM keeps showing the wallpaper's preview image underneath (it feeds the palette, the window
//! blur and covers the moment before the engine is up) and pauses the engine (SIGSTOP on its
//! process group) in game mode, on the lock screen and while windows cover the wallpaper.
//!
//! A wallpaper is chosen like any other: theme.wallpaper / outputs[].wallpaper = "we:<id>",
//! where <id> is a Workshop id, a built-in project folder name or a path to a project folder.

use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
