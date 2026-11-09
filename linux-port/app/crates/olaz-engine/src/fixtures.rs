//! Fixture loader: recorded `zstate` payloads replayed into a host-less engine, for the visual tests and the
//! payload-parsing tests: `requires` first (in order, once each), then `states`; tokens
//! `{"$ago": s}` / `{"$in": s}` → epoch ms relative to the capture clock, `"fixture:<path>"` → `file://` URL.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::engine::{LoginState, LoginStep, Phase, ZaloEngine};

#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub self_conversation_id: Option<String>,
    /// ShellModel state after the payloads (`destination`, `selection`, `quickSwitcher`, `inspector`).
    pub shell: Map<String, Value>,
    pub states: Vec<Map<String, Value>>,
    /// login.json only: `{capture, login}` per capture.
    pub phases: Vec<Map<String, Value>>,
    /// Fake renderer answers keyed `"__zaloX.method"`.
    pub renderer: Map<String, Value>,
}

/// Answers the engine's renderer calls (`window.__zaloX.method(...)`) from fixtures instead of a webview:
/// a plain value on every call; `{"$once": v}` first call then never resolves; `{"$seq": [..]}` one per call then
/// never; `{"$hang": true}` never. Calls without an answer return `null` and are listed in `unanswered`.
#[derive(Default)]
pub struct FakeRenderer {
    answers: std::cell::RefCell<std::collections::HashMap<String, Answer>>,
    pub unanswered: std::cell::RefCell<Vec<String>>,
}

enum Answer {
    Always(Value),
    Seq(std::collections::VecDeque<Value>),
    Hang,
}

/// `"__zaloX.method"` of an engine script (`const c = window.__zaloX; … c.method(...args)`).
pub fn call_key(script: &str) -> Option<String> {
    static RE: std::sync::OnceLock<(regex::Regex, regex::Regex)> = std::sync::OnceLock::new();
    let (object, method) = RE.get_or_init(|| {
        (regex::Regex::new(r"const (\w+) = window\.(__zalo\w+);").unwrap(), regex::Regex::new(r"\b(\w+)\.(\w+)\(\.\.\.").unwrap())
    });
    let o = object.captures(script)?;
    let var = o.get(1)?.as_str();
    let m = method.captures_iter(script).find(|c| c.get(1).map(|v| v.as_str()) == Some(var))?;
    Some(format!("{}.{}", o.get(2)?.as_str(), m.get(2)?.as_str()))
}

impl FakeRenderer {
    pub fn new(answers: &Map<String, Value>) -> Self {
        let parsed = answers
            .iter()
            .map(|(k, v)| {
                let a = match v {
                    Value::Object(o) if o.len() == 1 && o.contains_key("$once") => Answer::Seq([o["$once"].clone()].into_iter().collect()),
                    Value::Object(o) if o.len() == 1 && o.contains_key("$seq") => Answer::Seq(o["$seq"].as_array().cloned().unwrap_or_default().into_iter().collect()),
                    Value::Object(o) if o.len() == 1 && o.contains_key("$hang") => Answer::Hang,
                    other => Answer::Always(other.clone()),
                };
                (k.clone(), a)
            })
            .collect();
        FakeRenderer { answers: std::cell::RefCell::new(parsed), unanswered: Default::default() }
    }

    /// `Some(answer)` resolves the call; `None` = never resolves.
    pub fn answer(&self, script: &str) -> Option<Value> {
        let Some(key) = call_key(script) else {
            self.unanswered.borrow_mut().push(script.chars().take(80).collect());
            return Some(Value::Null);
        };
        match self.answers.borrow_mut().get_mut(&key) {
            Some(Answer::Always(v)) => Some(v.clone()),
            Some(Answer::Seq(q)) => q.pop_front(),
            Some(Answer::Hang) => None,
            None => {
                let mut u = self.unanswered.borrow_mut();
                if !u.contains(&key) {
                    u.push(key);
                }
                Some(Value::Null)
            }
        }
    }
}

/// `fixture:<path>` → absolute `file://` URL under `dir` (percent-encoding the path like `olaz_media` file URIs).
pub fn fixture_url(dir: &Path, rel: &str) -> String {
    let path = dir.join(rel);
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Resolves the fixture tokens (`$ago`, `$in`, `fixture:`) in place.
pub fn resolve_tokens(v: &mut Value, dir: &Path, now_ms: i64) {
    match v {
        Value::Object(map) => {
            if map.len() == 1 {
                let token = map.get("$ago").map(|s| -1.0 * s.as_f64().unwrap_or(0.0)).or_else(|| map.get("$in").and_then(Value::as_f64));
                if let Some(delta_s) = token {
                    *v = Value::from(now_ms + (delta_s * 1000.0) as i64);
                    return;
                }
            }
            for x in map.values_mut() {
                resolve_tokens(x, dir, now_ms);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| resolve_tokens(x, dir, now_ms)),
        Value::String(s) => {
            if let Some(rel) = s.strip_prefix("fixture:") {
                *s = fixture_url(dir, rel);
            }
        }
        _ => {}
    }
}

fn read(dir: &Path, name: &str, now_ms: i64) -> Result<(Vec<String>, Fixture), String> {
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut json: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    resolve_tokens(&mut json, dir, now_ms);
    let objs = |key: &str| -> Vec<Map<String, Value>> {
        json.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect()).unwrap_or_default()
    };
    let requires = json.get("requires").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    Ok((
        requires,
        Fixture {
            name: json.get("name").and_then(Value::as_str).unwrap_or(name).to_string(),
            self_conversation_id: json.get("selfConversationId").and_then(Value::as_str).map(str::to_string),
            shell: json.get("shell").and_then(Value::as_object).cloned().unwrap_or_default(),
            states: objs("states"),
            phases: objs("phases"),
            renderer: json.get("renderer").and_then(Value::as_object).cloned().unwrap_or_default(),
        },
    ))
}

/// `name` and everything it requires, in application order (requirements first, each once).
pub fn load(dir: &Path, name: &str, now_ms: i64) -> Result<Vec<Fixture>, String> {
    fn visit(dir: &Path, name: &str, now_ms: i64, seen: &mut Vec<String>, out: &mut Vec<Fixture>) -> Result<(), String> {
        if seen.iter().any(|s| s == name) {
            return Ok(());
        }
        seen.push(name.to_string());
        let (requires, fixture) = read(dir, name, now_ms)?;
        for r in &requires {
            visit(dir, r, now_ms, seen, out)?;
        }
        out.push(fixture);
        Ok(())
    }
    let mut out = Vec::new();
    visit(dir, name, now_ms, &mut Vec::new(), &mut out)?;
    Ok(out)
}

/// The fixture directory of this checkout (`linux-port/fixtures`).
pub fn default_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures");
    dir.canonicalize().unwrap_or(dir)
}

/// LoginState for one `phases[]` entry of login.json.
pub fn login_state(phase: &Map<String, Value>) -> Option<LoginState> {
    let login = phase.get("login")?.as_object()?;
    let s = |k: &str| login.get(k).and_then(Value::as_str).map(str::to_string);
    let step = match login.get("step")?.as_str()? {
        "generating" => LoginStep::Generating,
        "waitingScan" => LoginStep::WaitingScan,
        "waitingConfirm" => LoginStep::WaitingConfirm { name: s("name").unwrap_or_default(), avatar: s("avatar") },
        "expired" => LoginStep::Expired,
        "failed" => LoginStep::Failed(s("message").unwrap_or_default()),
        _ => return None,
    };
    let qr_png = s("qr").and_then(|u| file_url_path(&u)).and_then(|p| std::fs::read(p).ok());
    Some(LoginState { step, qr_png, is_second_qr: login.get("isSecondQR").and_then(Value::as_bool).unwrap_or(false) })
}

fn file_url_path(url: &str) -> Option<PathBuf> {
    let raw = url.strip_prefix("file://")?;
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

impl ZaloEngine {
    /// Replays fixtures into a host-less engine and shows the main UI (no webhost is started). Renderer calls are
    /// answered from the fixtures' `renderer` blocks (later fixtures override earlier keys).
    pub fn apply_fixtures(&self, fixtures: &[Fixture]) {
        let mut answers = Map::new();
        for f in fixtures {
            answers.extend(f.renderer.clone());
        }
        *self.fake_renderer.borrow_mut() = Some(std::rc::Rc::new(FakeRenderer::new(&answers)));
        for f in fixtures {
            if let Some(id) = &f.self_conversation_id {
                self.state.borrow_mut().self_conversation_id = Some(id.clone());
            }
            for state in &f.states {
                self.handle_state(state.clone());
            }
        }
        self.set_phase(Phase::Ready);
    }

    /// Shows the login screen in a given state (login scenes).
    pub fn show_login_state(&self, login: LoginState) {
        self.set_phase(Phase::Login(login));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_resolve() {
        let dir = Path::new("/fx dir");
        let mut v = serde_json::json!({"a": {"$ago": 60}, "b": [{"$in": 2}], "c": "fixture:avatars/x.png", "d": {"$ago": 1, "e": 2}});
        resolve_tokens(&mut v, dir, 1_000_000);
        assert_eq!(v["a"], 940_000);
        assert_eq!(v["b"][0], 1_002_000);
        assert_eq!(v["c"], "file:///fx%20dir/avatars/x.png");
        // Only single-key objects are tokens.
        assert!(v["d"].is_object());
        assert_eq!(file_url_path("file:///fx%20dir/a%C3%A2.png").unwrap(), PathBuf::from("/fx dir/aâ.png"));
    }

    #[test]
    fn every_fixture_loads_with_requirements_first() {
        let dir = default_dir();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = p.file_stem().unwrap().to_str().unwrap().to_string();
            let set = load(&dir, &name, 1_790_000_000_000).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(set.last().unwrap().name, name);
            if name == "chat-basic" {
                assert_eq!(set.first().unwrap().name, "base-account");
            }
        }
        let login = load(&dir, "login", 0).unwrap();
        let phases = &login.last().unwrap().phases;
        assert_eq!(phases.len(), 6);
        let states: Vec<LoginState> = phases.iter().map(|p| login_state(p).expect("phase")).collect();
        assert!(states[1].qr_png.as_ref().is_some_and(|b| b.starts_with(b"\x89PNG")));
        assert!(states[2].is_second_qr);
        assert!(matches!(&states[3].step, LoginStep::WaitingConfirm { name, .. } if name == "Minh Anh"));
        assert!(matches!(&states[5].step, LoginStep::Failed(m) if m == "Vui lòng thử lại QR mới."));
    }

    #[test]
    fn base_account_replays_into_the_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let t = olaz_core::exec::TestExecutor::new();
        let e = ZaloEngine::new(
            t.exec(),
            olaz_core::paths::XdgPaths::rooted(tmp.path()),
            crate::Services {
                media: None,
                secrets: std::sync::Arc::new(olaz_main::secrets::MemoryStore::default()),
                open_url: std::rc::Rc::new(|_| {}),
                badge: None,
                webhost_binary: "/nonexistent".into(),
                debug: false,
            },
        );
        let set = load(&default_dir(), "chat-basic", 1_790_000_000_000).unwrap();
        e.apply_fixtures(&set);
        assert_eq!(e.phase(), Phase::Ready);
        let s = e.state.borrow();
        assert_eq!(s.self_conversation_id.as_deref(), Some("1000000000000000001"));
        assert_eq!(s.conversations.len(), 7);
        assert_eq!(s.profile.as_ref().map(|p| p.display_name.as_str()), Some("Minh Anh"));
        assert!(s.messages.values().any(|m| m.len() > 5), "chat-basic messages were routed");
    }

    #[test]
    fn fake_renderer_answers_by_key() {
        let script = |o: &str, m: &str| format!("const c = window.{o}; if (!c || !c.{m}) return null; return await c.{m}(...[\"a\",1]);");
        assert_eq!(call_key(&script("__zaloChat", "reactors")).as_deref(), Some("__zaloChat.reactors"));
        assert_eq!(call_key("const s = window.__zaloMediaStore; if (!s || !s.page) return null; return await s.page(...[1]);").as_deref(), Some("__zaloMediaStore.page"));
        assert_eq!(
            call_key("return (() => { const c = window.__zaloPoll; if (!c || !c.watch) return 'missing'; c.watch(...[\"77\"]); return 'ok'; })()").as_deref(),
            Some("__zaloPoll.watch")
        );
        let answers = serde_json::json!({
            "__zaloChat.reactors": {"tabs": []},
            "__zaloPoll.voters": {"$once": {"n": 1}},
            "__zaloCloud.list": {"$seq": [1, 2]},
            "__zaloSticker.cate": {"$hang": true}
        });
        let f = FakeRenderer::new(answers.as_object().unwrap());
        assert_eq!(f.answer(&script("__zaloChat", "reactors")), Some(serde_json::json!({"tabs": []})));
        assert_eq!(f.answer(&script("__zaloChat", "reactors")), Some(serde_json::json!({"tabs": []})));
        assert_eq!(f.answer(&script("__zaloPoll", "voters")), Some(serde_json::json!({"n": 1})));
        assert_eq!(f.answer(&script("__zaloPoll", "voters")), None);
        assert_eq!(f.answer(&script("__zaloCloud", "list")), Some(serde_json::json!(1)));
        assert_eq!(f.answer(&script("__zaloCloud", "list")), Some(serde_json::json!(2)));
        assert_eq!(f.answer(&script("__zaloCloud", "list")), None);
        assert_eq!(f.answer(&script("__zaloSticker", "cate")), None);
        assert_eq!(f.answer(&script("__zaloX", "nope")), Some(Value::Null));
        assert_eq!(f.unanswered.borrow().as_slice(), ["__zaloX.nope"]);
    }
}