//! Python / Lua / Bash config front-ends: evaluate a file into JSON and
//! generate a file from JSON. Every language can express the full config, so
//! editing any one of them can regenerate the others.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};

use super::docs::doc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Python,
    Lua,
    Bash,
}

pub const ALL: [Lang; 3] = [Lang::Python, Lang::Lua, Lang::Bash];

impl Lang {
    pub fn file_name(self) -> &'static str {
        match self {
            Lang::Python => "config.py",
            Lang::Lua => "config.lua",
            Lang::Bash => "config.sh",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "Python",
            Lang::Lua => "Lua",
            Lang::Bash => "Bash",
        }
    }

    pub fn eval(self, path: &Path, schema: &Value) -> Result<Value> {
        match self {
            Lang::Python => eval_python(path),
            Lang::Lua => eval_lua(path),
            Lang::Bash => eval_bash(path, schema),
        }
    }

    pub fn generate(self, cfg: &Value, schema: &Value) -> String {
        match self {
            Lang::Python => gen_python(cfg),
            Lang::Lua => gen_lua(cfg),
            Lang::Bash => gen_bash(cfg, schema),
        }
    }
}

fn header(lang: Lang, comment: &str) -> String {
    let others: Vec<&str> = ALL.iter().filter(|l| **l != lang).map(|l| l.file_name()).collect();
    format!(
        "{c} FerriumWM (FeWM) — конфиг на {name}.\n\
         {c} Правь любой из config.py / config.lua / config.sh: при сохранении изменения\n\
         {c} применяются на лету и переписываются в {o1} и {o2}.\n\
         {c} Логика/функции допустимы, но в другие языки переносятся только итоговые значения.\n\
         {c} Внешний вид (рамки, скругления, отступы, тени, размытие) — в ../style.css\n\n",
        c = comment,
        name = lang.name(),
        o1 = others[0],
        o2 = others[1],
    )
}

// ---------------------------------------------------------------- running

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("не удалось запустить {:?}", cmd.get_program()))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stdout.read_to_end(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("конфиг выполнялся дольше {} с — убит", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    if !status.success() {
        bail!("{}", err.trim());
    }
    Ok(out)
}

const TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------- python

const PY_HARNESS: &str = r#"
import json, sys, types
p = sys.argv[1]
ns = {"__name__": "__fewm_config__", "__file__": p}
with open(p, encoding="utf-8") as f:
    src = f.read()
exec(compile(src, p, "exec"), ns)
out = {}
for k, v in ns.items():
    if k.startswith("_") or isinstance(v, (types.ModuleType, types.FunctionType, types.BuiltinFunctionType, type)):
        continue
    out[k] = v
def conv(o):
    if isinstance(o, (set, frozenset)):
        return list(o)
    return str(o)
sys.stdout.write(json.dumps(out, default=conv))
"#;

fn eval_python(path: &Path) -> Result<Value> {
    let mut cmd = Command::new("python3");
    cmd.arg("-I").arg("-c").arg(PY_HARNESS).arg(path);
    let out = run_with_timeout(cmd, TIMEOUT)?;
    serde_json::from_str(&out).context("python: неверный вывод")
}

fn py_value(v: &Value, indent: usize, out: &mut String, path: &str) {
    let pad = "    ".repeat(indent + 1);
    let pad_end = "    ".repeat(indent);
    match v {
        Value::Null => out.push_str("None"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Number(n) => out.push_str(&fmt_num(n)),
        Value::String(s) => out.push_str(&json_str(s)),
        Value::Array(a) => {
            if a.iter().all(|x| !x.is_object() && !x.is_array()) {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    py_value(x, indent, out, path);
                }
                out.push(']');
            } else {
                out.push_str("[\n");
                for x in a {
                    out.push_str(&pad);
                    py_value(x, indent + 1, out, path);
                    out.push_str(",\n");
                }
                out.push_str(&pad_end);
                out.push(']');
            }
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (k, x) in o {
                let p = join(path, k);
                if let Some(d) = doc(&p) {
                    out.push_str(&format!("{pad}# {d}\n"));
                }
                out.push_str(&format!("{pad}{}: ", json_str(k)));
                py_value(x, indent + 1, out, &p);
                out.push_str(",\n");
            }
            out.push_str(&pad_end);
            out.push('}');
        }
    }
}

fn gen_python(cfg: &Value) -> String {
    let mut out = header(Lang::Python, "#");
    if let Value::Object(o) = cfg {
        for (k, v) in o {
            if let Some(d) = doc(k) {
                out.push_str(&format!("# {d}\n"));
            }
            out.push_str(&format!("{k} = "));
            py_value(v, 0, &mut out, k);
            out.push_str("\n\n");
        }
    }
    out
}

// ---------------------------------------------------------------- lua

const LUA_HARNESS: &str = r#"
local path = os.getenv("FE_CONFIG")
local before = {}
for k in pairs(_G) do before[k] = true end
local chunk, err = loadfile(path)
if not chunk then io.stderr:write(err) os.exit(1) end
local ok, ret = pcall(chunk)
if not ok then io.stderr:write(tostring(ret)) os.exit(1) end
local out = {}
if type(ret) == "table" then for k, v in pairs(ret) do out[k] = v end end
for k, v in pairs(_G) do
  if not before[k] and type(v) ~= "function" and type(k) == "string" and k:sub(1,1) ~= "_" then out[k] = v end
end
local function esc(s)
  return '"' .. s:gsub('[%c"\\]', function(c)
    if c == '"' then return '\\"' elseif c == '\\' then return '\\\\'
    elseif c == '\n' then return '\\n' elseif c == '\t' then return '\\t'
    else return string.format('\\u%04x', c:byte()) end
  end) .. '"'
end
local enc
local function isint(v) if math.type then return math.type(v) == "integer" end return v == math.floor(v) and v < 2^53 end
enc = function(v, seen)
  local t = type(v)
  if t == "nil" then return "null"
  elseif t == "boolean" then return tostring(v)
  elseif t == "number" then
    if v ~= v or v == math.huge or v == -math.huge then return "null" end
    if isint(v) then return string.format("%d", v) end
    return string.format("%.10g", v)
  elseif t == "string" then return esc(v)
  elseif t == "table" then
    if seen[v] then error("цикл в таблице") end
    seen[v] = true
    local n, count = #v, 0
    for _ in pairs(v) do count = count + 1 end
    local parts = {}
    if n > 0 and n == count then
      for i = 1, n do parts[#parts+1] = enc(v[i], seen) end
      seen[v] = nil
      return "[" .. table.concat(parts, ",") .. "]"
    end
    for k, x in pairs(v) do
      if type(x) ~= "function" then parts[#parts+1] = esc(tostring(k)) .. ":" .. enc(x, seen) end
    end
    seen[v] = nil
    return "{" .. table.concat(parts, ",") .. "}"
  else return esc(tostring(v)) end
end
io.write(enc(out, {}))
"#;

fn lua_binary() -> &'static str {
    use std::sync::OnceLock;
    static BIN: OnceLock<&'static str> = OnceLock::new();
    BIN.get_or_init(|| {
        for b in ["lua5.4", "lua", "lua5.3", "luajit"] {
            if Command::new(b)
                .arg("-v")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
            {
                return b;
            }
        }
        "lua"
    })
}

fn eval_lua(path: &Path) -> Result<Value> {
    let mut cmd = Command::new(lua_binary());
    cmd.env("FE_CONFIG", path).arg("-e").arg(LUA_HARNESS);
    let out = run_with_timeout(cmd, TIMEOUT)?;
    serde_json::from_str(&out).context("lua: неверный вывод")
}

fn lua_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\{:03}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn lua_key(k: &str) -> String {
    let ident = !k.is_empty()
        && k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ident { k.to_string() } else { format!("[{}]", lua_str(k)) }
}

fn lua_value(v: &Value, indent: usize, out: &mut String, path: &str) {
    let pad = "  ".repeat(indent + 1);
    let pad_end = "  ".repeat(indent);
    match v {
        Value::Null => out.push_str("nil"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&fmt_num(n)),
        Value::String(s) => out.push_str(&lua_str(s)),
        Value::Array(a) => {
            if a.iter().all(|x| !x.is_object() && !x.is_array()) {
                out.push_str("{ ");
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    lua_value(x, indent, out, path);
                }
                out.push_str(if a.is_empty() { "}" } else { " }" });
            } else {
                out.push_str("{\n");
                for x in a {
                    out.push_str(&pad);
                    lua_value(x, indent + 1, out, path);
                    out.push_str(",\n");
                }
                out.push_str(&pad_end);
                out.push('}');
            }
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (k, x) in o {
                let p = join(path, k);
                if let Some(d) = doc(&p) {
                    out.push_str(&format!("{pad}-- {d}\n"));
                }
                out.push_str(&format!("{pad}{} = ", lua_key(k)));
                lua_value(x, indent + 1, out, &p);
                out.push_str(",\n");
            }
            out.push_str(&pad_end);
            out.push('}');
        }
    }
}

fn gen_lua(cfg: &Value) -> String {
    let mut out = header(Lang::Lua, "--");
    if let Value::Object(o) = cfg {
        for (k, v) in o {
            if let Some(d) = doc(k) {
                out.push_str(&format!("-- {d}\n"));
            }
            out.push_str(&format!("{k} = "));
            lua_value(v, 0, &mut out, k);
            out.push_str("\n\n");
        }
    }
    out
}

// ---------------------------------------------------------------- bash
//
// Mapping (derived from the schema shape):
//   struct at path a.b            -> declare -A a_b=([field]=value ...)
//   string map (binds, env)       -> declare -A binds=(['SUPER+Q']='close' ...)
//   list of scalars at a.b        -> a_b=(v1 v2 ...)
//   list of structs at a.b        -> declare -A a_b_0=(...) a_b_1=(...)
//   scalar field a.b.c may also be overridden as plain variable a_b_c=value

const BASH_HARNESS: &str = r#"
__fe_before=" $(compgen -A variable | tr '\n' ' ') "
source "$1" || exit 1
for __fe_v in $(compgen -A variable); do
  case "$__fe_before" in *" $__fe_v "*) continue;; esac
  case "$__fe_v" in __fe_*) continue;; esac
  __fe_decl=$(declare -p "$__fe_v" 2>/dev/null)
  case "$__fe_decl" in
    "declare -A"*|"declare -a"*)
      if [[ "$__fe_decl" == "declare -A"* ]]; then __fe_t=A; else __fe_t=a; fi
      declare -n __fe_ref="$__fe_v"
      printf '%s\0%s\0%s\0%s\0' "$__fe_t" "$__fe_v" "" ""
      for __fe_k in "${!__fe_ref[@]}"; do
        printf '%s\0%s\0%s\0%s\0' "$__fe_t" "$__fe_v" "$__fe_k" "${__fe_ref[$__fe_k]}"
      done
      unset -n __fe_ref
      ;;
    *) printf 's\0%s\0\0%s\0' "$__fe_v" "${!__fe_v}" ;;
  esac
done
"#;

#[derive(Debug, Default)]
struct BashVar {
    kind: char,
    scalar: String,
    entries: Vec<(String, String)>,
}

fn eval_bash(path: &Path, schema: &Value) -> Result<Value> {
    let mut cmd = Command::new("bash");
    cmd.arg("--noprofile").arg("--norc").arg("-c").arg(BASH_HARNESS).arg("fewm").arg(path);
    let out = run_with_timeout(cmd, TIMEOUT)?;
    let fields: Vec<&str> = out.split('\0').collect();
    let mut vars: HashMap<String, BashVar> = HashMap::new();
    for rec in fields.chunks(4) {
        if rec.len() < 4 {
            break;
        }
        let kind = rec[0].chars().next().unwrap_or('s');
        let var = vars.entry(rec[1].to_string()).or_insert_with(|| BashVar { kind, ..Default::default() });
        match kind {
            's' => var.scalar = rec[3].to_string(),
            _ => {
                if !rec[2].is_empty() {
                    var.entries.push((rec[2].to_string(), rec[3].to_string()));
                }
            }
        }
    }
    let Value::Object(s) = schema else { return Err(anyhow!("bad schema")) };
    Ok(Value::Object(bash_rebuild_obj("", s, &vars)))
}

fn bash_name(parent: &str, key: &str) -> String {
    if parent.is_empty() { key.to_string() } else { format!("{parent}_{key}") }
}

fn bash_rebuild_obj(name: &str, schema: &Map<String, Value>, vars: &HashMap<String, BashVar>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(v) = vars.get(name).filter(|v| v.kind == 'A') {
        for (k, val) in &v.entries {
            if schema.get(k).is_some_and(|s| !s.is_object() && !s.is_array()) {
                out.insert(k.clone(), Value::String(val.clone()));
            } else if !schema.contains_key(k) {
                // unknown key; let coercion report it
                out.insert(k.clone(), Value::String(val.clone()));
            }
        }
    }
    for (k, s) in schema {
        let child = bash_name(name, k);
        let path_is_dyn = super::DYN_MAPS.contains(&child.as_str());
        match s {
            _ if path_is_dyn => {
                if let Some(v) = vars.get(&child) {
                    let m: Map<String, Value> =
                        v.entries.iter().map(|(a, b)| (a.clone(), Value::String(b.clone()))).collect();
                    out.insert(k.clone(), Value::Object(m));
                }
            }
            Value::Object(so) => {
                let m = bash_rebuild_obj(&child, so, vars);
                if !m.is_empty() {
                    out.insert(k.clone(), Value::Object(m));
                }
            }
            Value::Array(sa) => {
                if let Some(Value::Object(elem)) = sa.first() {
                    let mut items = Vec::new();
                    let mut i = 0;
                    loop {
                        let n = format!("{child}_{i}");
                        if !vars.contains_key(&n) {
                            break;
                        }
                        items.push(Value::Object(bash_rebuild_obj(&n, elem, vars)));
                        i += 1;
                    }
                    if i > 0 || vars.contains_key(&child) {
                        out.insert(k.clone(), Value::Array(items));
                    }
                } else if let Some(v) = vars.get(&child) {
                    let mut e: Vec<(i64, String)> =
                        v.entries.iter().filter_map(|(a, b)| a.parse().ok().map(|i| (i, b.clone()))).collect();
                    e.sort_by_key(|x| x.0);
                    if v.kind == 's' {
                        e = if v.scalar.is_empty() { vec![] } else { vec![(0, v.scalar.clone())] };
                    }
                    out.insert(k.clone(), Value::Array(e.into_iter().map(|x| Value::String(x.1)).collect()));
                }
            }
            _ => {
                if let Some(v) = vars.get(&child).filter(|v| v.kind == 's') {
                    out.insert(k.clone(), Value::String(v.scalar.clone()));
                }
            }
        }
    }
    out
}

fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/:,%@+".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn sh_scalar(v: &Value) -> String {
    match v {
        Value::Null => "''".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => fmt_num(n),
        Value::String(s) => sh_quote(s),
        other => sh_quote(&other.to_string()),
    }
}

fn gen_bash(cfg: &Value, schema: &Value) -> String {
    let mut out = header(Lang::Bash, "#");
    out.push_str(
        "# Формат: секции — ассоциативные массивы (declare -A секция=([ключ]=значение)),\n\
         # вложенные секции — секция_подсекция, списки — массивы, элементы списков\n\
         # правил — список_0, список_1, ... Можно и просто: general_layout=scroller\n\n",
    );
    if let (Value::Object(o), Value::Object(s)) = (cfg, schema) {
        for (k, v) in o {
            if let Some(d) = doc(k) {
                out.push_str(&format!("# {d}\n"));
            }
            bash_emit(&mut out, k, k, v, s.get(k).unwrap_or(&Value::Null));
            out.push('\n');
        }
    }
    out
}

fn bash_emit(out: &mut String, name: &str, path: &str, v: &Value, schema: &Value) {
    let is_dyn = super::DYN_MAPS.contains(&path);
    match v {
        Value::Object(o) if is_dyn => {
            out.push_str(&format!("declare -A {name}=(\n"));
            for (k, x) in o {
                out.push_str(&format!("  [{}]={}\n", sh_quote_key(k), sh_scalar(x)));
            }
            out.push_str(")\n");
        }
        Value::Object(o) => {
            let scalars: Vec<_> = o.iter().filter(|(_, x)| !x.is_object() && !x.is_array()).collect();
            if !scalars.is_empty() || o.is_empty() {
                out.push_str(&format!("declare -A {name}=(\n"));
                for (k, x) in scalars {
                    let p = join(path, k);
                    out.push_str(&format!("  [{k}]={}", sh_scalar(x)));
                    if let Some(d) = doc(&p) {
                        out.push_str(&format!("  # {d}"));
                    }
                    out.push('\n');
                }
                out.push_str(")\n");
            }
            for (k, x) in o.iter().filter(|(_, x)| x.is_object() || x.is_array()) {
                let p = join(path, k);
                if let Some(d) = doc(&p) {
                    out.push_str(&format!("# {d}\n"));
                }
                bash_emit(out, &bash_name(name, k), &p, x, schema.get(k).unwrap_or(&Value::Null));
            }
        }
        Value::Array(a) => {
            let elem_is_obj = matches!(schema.as_array().and_then(|s| s.first()), Some(Value::Object(_)))
                || a.iter().any(|x| x.is_object());
            if elem_is_obj {
                if a.is_empty() {
                    out.push_str(&format!("# (пусто) пример: declare -A {name}_0=([ключ]=значение)\n"));
                }
                let elem_schema = schema.as_array().and_then(|s| s.first()).cloned().unwrap_or(Value::Null);
                for (i, x) in a.iter().enumerate() {
                    bash_emit(out, &format!("{name}_{i}"), path, x, &elem_schema);
                }
            } else {
                out.push_str(&format!("{name}=("));
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(&sh_scalar(x));
                }
                out.push_str(")\n");
            }
        }
        other => out.push_str(&format!("{name}={}\n", sh_scalar(other))),
    }
}

fn sh_quote_key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        k.to_string()
    } else {
        format!("'{}'", k.replace('\'', "'\\''"))
    }
}

// ---------------------------------------------------------------- helpers

fn join(path: &str, k: &str) -> String {
    if path.is_empty() { k.to_string() } else { format!("{path}.{k}") }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

pub fn fmt_num(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    let mut s = format!("{:.4}", f);
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.push('0');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, coerce::coerce};

    fn roundtrip(lang: Lang) {
        let cfg = Config::default();
        let schema = serde_json::to_value(Config::schema_sample()).unwrap();
        let v = serde_json::to_value(&cfg).unwrap();
        let text = lang.generate(&v, &schema);
        let dir = std::env::temp_dir().join(format!("fewm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(lang.file_name());
        std::fs::write(&p, &text).unwrap();
        let back = lang.eval(&p, &schema).unwrap_or_else(|e| panic!("{lang:?}: {e}\n{text}"));
        let mut w = Vec::new();
        let back = coerce(back, &schema, &mut w, "");
        let cfg2: Config = serde_json::from_value(back).unwrap();
        assert!(w.is_empty(), "{lang:?} warnings: {w:?}");
        let a = serde_json::to_value(&cfg).unwrap().to_string();
        let b = serde_json::to_value(&cfg2).unwrap().to_string();
        assert_eq!(a, b, "{lang:?}\n{text}");
    }

    #[test]
    fn python_roundtrip() {
        roundtrip(Lang::Python)
    }
    #[test]
    fn lua_roundtrip() {
        roundtrip(Lang::Lua)
    }
    #[test]
    fn bash_roundtrip() {
        roundtrip(Lang::Bash)
    }
}
