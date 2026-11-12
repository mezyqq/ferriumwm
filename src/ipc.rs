//! IPC: `fewm msg <action...>` over a unix socket; JSON state queries and an
//! event stream for bars/scripts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use serde_json::json;
use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};
use tracing::{info, warn};

use crate::state::Fe;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("FEWM_INSTANCE").unwrap_or_default();
    dir.join(format!("fewm{display}.sock"))
}

#[derive(Default)]
pub struct Ipc {
    pub subscribers: Vec<UnixStream>,
}

pub fn init(fe: &mut Fe) {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            warn!("ipc: {e}");
            return;
        }
    };
    listener.set_nonblocking(true).ok();
    info!("IPC: {}", path.display());
    unsafe { std::env::set_var("FEWM_SOCKET", &path) };
    fe.lh
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, l, fe| {
            while let Ok((stream, _)) = l.accept() {
                handle_client(fe, stream);
            }
            Ok(PostAction::Continue)
        })
        .ok();
}

fn handle_client(fe: &mut Fe, stream: UnixStream) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(std::time::Duration::from_millis(30))).ok();
    let mut line = String::new();
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let line = line.trim();
    let mut out = stream;
    let reply = match line.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line, "")) {
        ("windows", _) => windows(fe).to_string(),
        ("workspaces", _) => workspaces(fe).to_string(),
        ("outputs", _) => outputs(fe).to_string(),
        ("focused", _) | ("focused-window", _) => {
            fe.focus.and_then(|f| window_json(fe, f)).unwrap_or(serde_json::Value::Null).to_string()
        }
        ("colors", _) | ("palette", _) => serde_json::to_value(
            fe.palette.current().iter().map(|(k, v)| (k.clone(), v.hex())).collect::<std::collections::BTreeMap<_, _>>(),
        )
        .unwrap()
        .to_string(),
        ("config", _) => serde_json::to_string(&fe.cfg).unwrap_or_default(),
        ("we-list", _) => crate::wallpaper_engine::list_json().to_string(),
        ("events", _) | ("subscribe", _) => {
            let _ = writeln!(out, "{}", json!({"event": "hello", "workspaces": workspaces(fe)}));
            out.set_nonblocking(true).ok();
            fe.ipc.subscribers.push(out);
            return;
        }
        ("test-drag", a) => crate::drag::test_drag(fe, a),
        ("action", a) | ("do", a) => {
            fe.run_action(a);
            "ok".into()
        }
        (_, _) if !line.is_empty() => {
            fe.run_action(line);
            "ok".into()
        }
        _ => "error: empty".into(),
    };
    let _ = writeln!(out, "{reply}");
}

fn window_json(fe: &Fe, id: u64) -> Option<serde_json::Value> {
    let w = fe.windows.get(&id)?;
    let r = w.rect.target();
    Some(json!({
        "id": id, "app_id": w.app_id, "title": w.title, "workspace": w.ws,
        "floating": w.floating, "fullscreen": w.fullscreen, "maximized": w.maximized,
        "focused": fe.focus == Some(id), "urgent": w.urgent,
        "geometry": [r.x, r.y, r.w, r.h],
    }))
}

fn windows(fe: &Fe) -> serde_json::Value {
    let mut ids: Vec<u64> = fe.windows.keys().copied().collect();
    ids.sort();
    serde_json::Value::Array(ids.into_iter().filter_map(|i| window_json(fe, i)).collect())
}

fn workspaces(fe: &Fe) -> serde_json::Value {
    serde_json::Value::Array(
        fe.workspaces
            .iter()
            .map(|w| {
                let n = fe.windows.values().filter(|x| x.ws == w.id).count();
                json!({
                    "id": w.id, "layout": w.layout.name(), "windows": n,
                    "active": fe.active_ws() == w.id,
                    "visible": fe.output_of_ws(w.id).is_some(),
                    "output": fe.output_of_ws(w.id).map(|i| fe.outputs[i].output.name()),
                    "urgent": fe.windows.values().any(|x| x.ws == w.id && x.urgent),
                })
            })
            .collect(),
    )
}

fn outputs(fe: &Fe) -> serde_json::Value {
    serde_json::Value::Array(
        fe.outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let g = fe.output_geo(i).unwrap_or_default();
                json!({"name": o.output.name(), "workspace": o.ws, "focused": fe.focused_output == i,
                       "geometry": [g.loc.x, g.loc.y, g.size.w, g.size.h],
                       "scale": o.output.current_scale().fractional_scale()})
            })
            .collect(),
    )
}

pub fn event(fe: &mut Fe, kind: &str) {
    if fe.ipc.subscribers.is_empty() {
        return;
    }
    let msg = match kind {
        "workspace-changed" | "window-opened" | "window-closed" | "layout-changed" => {
            json!({"event": kind, "workspaces": workspaces(fe), "focused": fe.focus.and_then(|f| window_json(fe, f))})
        }
        "focus-changed" | "window-changed" => json!({"event": kind, "focused": fe.focus.and_then(|f| window_json(fe, f))}),
        "theme-changed" => json!({"event": kind, "wallpaper": fe.wallpaper.path}),
        "action" => return,
        _ => json!({"event": kind}),
    };
    let line = format!("{msg}\n");
    fe.ipc.subscribers.retain_mut(|s| match s.write(line.as_bytes()) {
        Ok(_) => true,
        Err(e) => e.kind() == std::io::ErrorKind::WouldBlock,
    });
}

/// Client side of `fewm msg ...`.
pub fn client(args: &[String]) -> i32 {
    let path = std::env::var("FEWM_SOCKET").map(PathBuf::from).unwrap_or_else(|_| socket_path());
    let mut s = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fewm: не подключиться к {}: {e}", path.display());
            return 1;
        }
    };
    let _ = writeln!(s, "{}", args.join(" "));
    let stream_mode = matches!(args.first().map(String::as_str), Some("events" | "subscribe"));
    let mut r = BufReader::new(s);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                print!("{line}");
                if !stream_mode {
                    break;
                }
            }
        }
    }
    0
}
