//! Coerce loosely-typed JSON (from bash strings, lua tables, python dicts)
//! into the shape of the schema sample so serde can deserialize it.

use serde_json::{Map, Value};

pub fn coerce(value: Value, schema: &Value, warnings: &mut Vec<String>, path: &str) -> Value {
    match schema {
        Value::Bool(_) => match &value {
            Value::Bool(_) => value,
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Value::Bool(true),
                "false" | "no" | "off" | "0" | "" => Value::Bool(false),
                _ => {
                    warnings.push(format!("{path}: ожидалось true/false, получено {s:?}"));
                    schema.clone()
                }
            },
            Value::Number(n) => Value::Bool(n.as_f64().unwrap_or(0.0) != 0.0),
            _ => {
                warnings.push(format!("{path}: ожидалось true/false"));
                schema.clone()
            }
        },
        Value::Number(sn) => {
            let want_int = sn.is_i64() || sn.is_u64();
            let f = match &value {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                _ => None,
            };
            match f {
                Some(f) if want_int => {
                    if f.fract() != 0.0 {
                        warnings.push(format!("{path}: ожидалось целое, округляю {f}"));
                    }
                    if f < 0.0 {
                        Value::from(f.round() as i64)
                    } else {
                        Value::from(f.round() as u64)
                    }
                }
                Some(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
                None => {
                    warnings.push(format!("{path}: ожидалось число, получено {value}"));
                    schema.clone()
                }
            }
        }
        Value::String(_) => match value {
            Value::String(_) => value,
            Value::Number(n) => Value::String(n.to_string()),
            Value::Bool(b) => Value::String(b.to_string()),
            Value::Null => Value::String(String::new()),
            other => {
                warnings.push(format!("{path}: ожидалась строка"));
                Value::String(other.to_string())
            }
        },
        Value::Array(sarr) => {
            let elem_schema = sarr.first().cloned().unwrap_or(Value::String(String::new()));
            let items: Vec<Value> = match value {
                Value::Array(a) => a,
                // Lua/bash style: {"1": x, "2": y} or {"0": x}
                Value::Object(o) => {
                    let mut kv: Vec<(i64, Value)> = o
                        .into_iter()
                        .filter_map(|(k, v)| k.parse::<i64>().ok().map(|i| (i, v)))
                        .collect();
                    kv.sort_by_key(|(i, _)| *i);
                    kv.into_iter().map(|(_, v)| v).collect()
                }
                Value::Null => Vec::new(),
                Value::String(s) if s.is_empty() => Vec::new(),
                single => vec![single],
            };
            Value::Array(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| coerce(v, &elem_schema, warnings, &format!("{path}[{i}]")))
                    .collect(),
            )
        }
        Value::Object(sobj) => {
            let obj = match value {
                Value::Object(o) => o,
                Value::Array(a) if a.is_empty() => Map::new(),
                Value::Null => Map::new(),
                _ => {
                    warnings.push(format!("{path}: ожидалась таблица/словарь"));
                    return schema.clone();
                }
            };
            let is_dyn = super::DYN_MAPS.contains(&path);
            let mut out = Map::new();
            for (k, v) in obj {
                if is_dyn {
                    out.insert(k, coerce(v, &Value::String(String::new()), warnings, path));
                    continue;
                }
                match sobj.get(&k) {
                    Some(s) => {
                        let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                        let v = coerce(v, s, warnings, &p);
                        out.insert(k, v);
                    }
                    None => {
                        let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                        warnings.push(format!("{p}: неизвестная опция, пропускаю"));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Null => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn coerce_bash_strings() {
        let schema = serde_json::to_value(Config::schema_sample()).unwrap();
        let v = serde_json::json!({
            "general": {"layout": "scroller", "workspaces": "5", "focus_follows_mouse": "false"},
            "scroller": {"column_width_presets": {"0": "0.25", "1": "0.75"}},
            "window_rules": [{"app_id": "x", "float": "true", "opacity": "0.9", "workspace": "3"}],
            "binds": {"SUPER+A": "spawn a"},
            "bogus": 1
        });
        let mut w = Vec::new();
        let c = coerce(v, &schema, &mut w, "");
        let cfg: Config = serde_json::from_value(c).unwrap();
        assert_eq!(cfg.general.layout, "scroller");
        assert_eq!(cfg.general.workspaces, 5);
        assert!(!cfg.general.focus_follows_mouse);
        assert_eq!(cfg.scroller.column_width_presets, vec![0.25, 0.75]);
        assert_eq!(cfg.window_rules[0].float, Some(true));
        assert_eq!(cfg.window_rules[0].workspace, Some(3));
        assert_eq!(cfg.binds.len(), 1);
        assert_eq!(w.len(), 1);
    }
}
