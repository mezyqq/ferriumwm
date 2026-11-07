//! Screenshots (native capture comes with the udev backend; nested uses grim on the host).

use crate::state::Fe;

pub fn take(fe: &mut Fe, kind: &str) {
    let dir = crate::config::expand_path(&fe.cfg.misc.screenshot_dir);
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("fewm-{}.png", chrono_like()));
    let f = file.to_string_lossy();
    let cmd = match kind {
        "screenshot-screen" => format!("grim '{f}' && wl-copy < '{f}'"),
        "screenshot-window" => format!("grim '{f}' && wl-copy < '{f}'"),
        _ => format!("grim -g \"$(slurp)\" '{f}' && wl-copy < '{f}'"),
    };
    fe.spawn(&cmd);
}

fn chrono_like() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    t.to_string()
}
