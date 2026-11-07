//! Keeps config.py / config.lua / config.sh in sync and hot-reloads them.
//!
//! A background thread polls the files (robust against editors that replace
//! files via rename). When one changes and its content isn't something we
//! wrote ourselves, it is evaluated, validated, and the other two files are
//! regenerated from the result. The new config is sent to the compositor.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde_json::Value;
use smithay::reexports::calloop::channel::Sender;
use tracing::{info, warn};

use super::Config;
use super::coerce::coerce;
use super::langs::{ALL, Lang};

pub enum SyncEvent {
    Config(Box<Config>, Option<Lang>, Vec<String>),
    Style(String),
    Error(String),
    /// noctalia switched the wallpaper: follow it (blur + palette)
    Wallpaper(String),
}

pub fn configs_dir() -> PathBuf {
    super::config_dir().join("configs")
}

pub fn style_path() -> PathBuf {
    super::config_dir().join("style.css")
}

fn hash(s: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn schema() -> Value {
    serde_json::to_value(Config::schema_sample()).unwrap()
}

/// Evaluate a language file into a validated config.
pub fn eval_file(lang: Lang, path: &Path) -> Result<(Config, Vec<String>)> {
    let schema = schema();
    let raw = lang.eval(path, &schema)?;
    let mut warnings = Vec::new();
    let v = coerce(raw, &schema, &mut warnings, "");
    let cfg: Config = serde_json::from_value(v).context("конфиг не прошёл проверку")?;
    Ok((cfg, warnings))
}

fn write_if_changed(path: &Path, text: &str) -> Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(false);
    }
    let tmp = path.with_extension("fewm-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Write all language files (except `skip`) from `cfg`. Returns content hashes.
pub fn write_all(cfg: &Config, skip: Option<Lang>) -> Result<HashMap<Lang, u64>> {
    let dir = configs_dir();
    std::fs::create_dir_all(&dir)?;
    let schema = schema();
    let v = serde_json::to_value(cfg)?;
    let mut hashes = HashMap::new();
    for lang in ALL {
        if Some(lang) == skip {
            continue;
        }
        let text = lang.generate(&v, &schema);
        write_if_changed(&dir.join(lang.file_name()), &text)?;
        hashes.insert(lang, hash(text.as_bytes()));
    }
    Ok(hashes)
}

fn mtime(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Load the config at startup: newest language file wins and the others are
/// regenerated. If nothing exists, defaults (+ `first_run` tweaks) are written.
pub fn load_initial(first_run: impl FnOnce(&mut Config)) -> (Config, Vec<String>) {
    let dir = configs_dir();
    let mut newest: Option<(SystemTime, Lang)> = None;
    for lang in ALL {
        if let Some((t, _)) = mtime(&dir.join(lang.file_name())) {
            if newest.is_none_or(|(nt, _)| t > nt) {
                newest = Some((t, lang));
            }
        }
    }
    let mut msgs = Vec::new();
    let Some((_, lang)) = newest else {
        let mut cfg = Config::default();
        first_run(&mut cfg);
        if let Err(e) = write_all(&cfg, None) {
            msgs.push(format!("не удалось записать конфиги: {e:#}"));
        }
        info!("создал конфиги в {}", dir.display());
        return (cfg, msgs);
    };
    // Try newest first, then fall back to the others.
    let mut order = vec![lang];
    order.extend(ALL.iter().copied().filter(|l| *l != lang));
    for l in order {
        match eval_file(l, &dir.join(l.file_name())) {
            Ok((cfg, warnings)) => {
                msgs.extend(warnings.into_iter().map(|w| format!("{}: {w}", l.file_name())));
                if let Err(e) = write_all(&cfg, Some(l)) {
                    msgs.push(format!("синхронизация: {e:#}"));
                }
                return (cfg, msgs);
            }
            Err(e) => msgs.push(format!("{}: {e:#}", l.file_name())),
        }
    }
    (Config::default(), msgs)
}

pub fn spawn_watcher(tx: Sender<SyncEvent>) {
    std::thread::Builder::new()
        .name("fewm-config-sync".into())
        .spawn(move || watcher(tx))
        .expect("spawn config watcher");
}

fn watcher(tx: Sender<SyncEvent>) {
    let dir = configs_dir();
    let style = style_path();
    let noctalia = super::home().join(".local/state/noctalia/settings.toml");
    let mut last_wallpaper = String::new();
    let mut seen: HashMap<PathBuf, Option<(SystemTime, u64)>> = HashMap::new();
    let mut ours: HashMap<Lang, u64> = HashMap::new();
    for lang in ALL {
        let p = dir.join(lang.file_name());
        seen.insert(p.clone(), mtime(&p));
        if let Ok(s) = std::fs::read(&p) {
            ours.insert(lang, hash(&s));
        }
    }
    seen.insert(style.clone(), mtime(&style));

    loop {
        std::thread::sleep(Duration::from_millis(250));

        let m = mtime(&noctalia);
        if m.is_some() && seen.get(&noctalia) != Some(&m) {
            seen.insert(noctalia.clone(), m);
            if let Some((wp, _, _)) = crate::theme::noctalia_wallpaper() {
                if wp != last_wallpaper {
                    last_wallpaper = wp.clone();
                    if tx.send(SyncEvent::Wallpaper(wp)).is_err() {
                        return;
                    }
                }
            }
        }

        let m = mtime(&style);
        if seen.get(&style) != Some(&m) {
            seen.insert(style.clone(), m);
            if let Ok(text) = std::fs::read_to_string(&style) {
                if tx.send(SyncEvent::Style(text)).is_err() {
                    return;
                }
            }
        }

        for lang in ALL {
            let p = dir.join(lang.file_name());
            let m = mtime(&p);
            if seen.get(&p) == Some(&m) {
                continue;
            }
            seen.insert(p.clone(), m);
            // Editors often write in several steps; let the file settle.
            std::thread::sleep(Duration::from_millis(60));
            let Ok(content) = std::fs::read(&p) else { continue };
            let h = hash(&content);
            if ours.get(&lang) == Some(&h) {
                continue;
            }
            ours.insert(lang, h);
            info!("{} изменён, синхронизирую", lang.file_name());
            match eval_file(lang, &p) {
                Ok((cfg, warnings)) => {
                    match write_all(&cfg, Some(lang)) {
                        Ok(hashes) => {
                            for (l, h) in hashes {
                                ours.insert(l, h);
                                let lp = dir.join(l.file_name());
                                seen.insert(lp.clone(), mtime(&lp));
                            }
                        }
                        Err(e) => warn!("синхронизация: {e:#}"),
                    }
                    let warnings = warnings.into_iter().map(|w| format!("{}: {w}", lang.file_name())).collect();
                    if tx.send(SyncEvent::Config(Box::new(cfg), Some(lang), warnings)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let msg = format!("{}: {e:#}", lang.file_name());
                    warn!("{msg}");
                    if tx.send(SyncEvent::Error(msg)).is_err() {
                        return;
                    }
                }
            }
        }
    }
}
