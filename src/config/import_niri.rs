//! Import keybindings from a niri config (`binds { ... }` in config.kdl).
//! niri action names are understood natively by FeWM, so only key names and
//! spawn arguments need translating.

use std::collections::BTreeMap;
use std::path::Path;

pub struct Imported {
    pub binds: BTreeMap<String, String>,
    pub mouse_binds: BTreeMap<String, String>,
    pub autostart: Vec<String>,
    pub kb_layout: Option<String>,
    pub kb_options: Option<String>,
}

pub fn import(path: &Path) -> Option<Imported> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = strip_kdl_comments(&text);
    let mut binds = BTreeMap::new();
    let mut mouse_binds = BTreeMap::new();

    let body = block(&text, "binds")?;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(open) = line.find('{') else { continue };
        let Some(close) = line.rfind('}') else { continue };
        let head = &line[..open];
        let action = line[open + 1..close].trim().trim_end_matches(';').trim();
        let combo = head.split_whitespace().next().unwrap_or("");
        let Some((key, is_mouse)) = translate_combo(combo) else { continue };
        let Some(action) = translate_action(action) else { continue };
        if is_mouse {
            mouse_binds.insert(key, action);
        } else {
            binds.insert(key, action);
        }
    }

    let mut autostart = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("spawn-sh-at-startup ") {
            autostart.push(unquote_all(rest).join(" "));
        } else if let Some(rest) = l.strip_prefix("spawn-at-startup ") {
            autostart.push(shell_join(&unquote_all(rest)));
        }
    }

    let xkb = block(&text, "xkb");
    let get = |k: &str| -> Option<String> {
        xkb.as_ref()?
            .lines()
            .find_map(|l| l.trim().strip_prefix(k).map(|r| unquote_all(r).join("")))
            .filter(|s| !s.is_empty())
    };
    Some(Imported {
        binds,
        mouse_binds,
        autostart,
        kb_layout: get("layout "),
        kb_options: get("options "),
    })
}

fn strip_kdl_comments(s: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in s.lines() {
        let mut l = line;
        if in_block {
            if let Some(i) = l.find("*/") {
                l = &l[i + 2..];
                in_block = false;
            } else {
                continue;
            }
        }
        if let Some(i) = l.find("/*") {
            if let Some(j) = l[i..].find("*/") {
                let mut s = l[..i].to_string();
                s.push_str(&l[i + j + 2..]);
                out.push_str(&s);
                out.push('\n');
                continue;
            }
            l = &l[..i];
            in_block = true;
        }
        // `//` outside of strings
        let mut in_str = false;
        let mut cut = l.len();
        let b = l.as_bytes();
        for i in 0..b.len() {
            match b[i] {
                b'"' if i == 0 || b[i - 1] != b'\\' => in_str = !in_str,
                b'/' if !in_str && i + 1 < b.len() && b[i + 1] == b'/' => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&l[..cut]);
        out.push('\n');
    }
    out
}

/// Content of the first top-level-ish block named `name { ... }`.
fn block(text: &str, name: &str) -> Option<String> {
    let mut search = 0;
    while let Some(i) = text[search..].find(name) {
        let start = search + i;
        let before_ok = start == 0 || text[..start].ends_with(char::is_whitespace);
        let after = &text[start + name.len()..];
        if before_ok && after.trim_start().starts_with('{') {
            let open = start + name.len() + after.find('{')? + 1;
            let mut depth = 1;
            let mut in_str = false;
            let bytes = text.as_bytes();
            let mut j = open;
            while j < bytes.len() {
                match bytes[j] {
                    b'"' if bytes[j - 1] != b'\\' => in_str = !in_str,
                    b'{' if !in_str => depth += 1,
                    b'}' if !in_str => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(text[open..j].to_string());
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            return None;
        }
        search = start + name.len();
    }
    None
}

fn translate_combo(combo: &str) -> Option<(String, bool)> {
    let parts: Vec<&str> = combo.split('+').collect();
    let (key, mods) = parts.split_last()?;
    let mut out: Vec<String> = Vec::new();
    for m in mods {
        let m = match m.to_ascii_lowercase().as_str() {
            "mod" | "super" | "win" | "mod4" => "SUPER",
            "ctrl" | "control" => "CTRL",
            "shift" => "SHIFT",
            "alt" | "mod1" => "ALT",
            "iso_level3_shift" | "mod5" => "ALTGR",
            _ => return None,
        };
        out.push(m.into());
    }
    let (key, mouse) = match *key {
        "WheelScrollDown" => ("wheel_down".to_string(), true),
        "WheelScrollUp" => ("wheel_up".to_string(), true),
        "WheelScrollLeft" => ("wheel_left".to_string(), true),
        "WheelScrollRight" => ("wheel_right".to_string(), true),
        "TouchpadScrollDown" | "TouchpadScrollUp" | "TouchpadScrollLeft" | "TouchpadScrollRight" => return None,
        "MouseLeft" => ("mouse_left".to_string(), true),
        "MouseRight" => ("mouse_right".to_string(), true),
        "MouseMiddle" => ("mouse_middle".to_string(), true),
        k => (k.to_string(), false),
    };
    out.push(key);
    Some((out.join("+"), mouse))
}

fn unquote_all(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut cur = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            cur.push(match n {
                                'n' => '\n',
                                't' => '\t',
                                o => o,
                            });
                        }
                    }
                    '"' => break,
                    o => cur.push(o),
                }
            }
            out.push(cur);
        } else {
            let mut cur = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                cur.push(c);
                chars.next();
            }
            if !cur.contains('=') {
                out.push(cur);
            }
        }
    }
    out
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:+%@,".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn translate_action(a: &str) -> Option<String> {
    let (name, rest) = a.split_once(char::is_whitespace).unwrap_or((a, ""));
    let args = unquote_all(rest);
    Some(match name {
        "spawn" => format!("spawn {}", shell_join(&args)),
        "spawn-sh" => format!("spawn {}", args.join(" ")),
        "quit" => "exit".into(),
        "show-hotkey-overlay" => "show-hotkeys".into(),
        _ if args.is_empty() => name.to_string(),
        _ => format!("{name} {}", args.join(" ")),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_user_niri() {
        let p = crate::config::home().join(".config/niri/config.kdl");
        if !p.exists() {
            return;
        }
        let imp = super::import(&p).unwrap();
        assert!(imp.binds.len() > 20, "{:?}", imp.binds);
        assert_eq!(imp.binds.get("SUPER+Q").map(String::as_str), Some("close-window"));
        assert!(imp.mouse_binds.contains_key("SUPER+wheel_down"));
    }
}
