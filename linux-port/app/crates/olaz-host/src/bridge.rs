//! `ZaloBridge` — the Electron preload + `ipcMain` emulation. Port of `ZaloBridge.swift`.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::{Rc, Weak};

use olaz_core::exec::{Exec, LocalBoxFuture, spawn};
use olaz_core::value::{BV, undefined_marker};
use serde_json::{Map, Value, json};

pub type BridgeResult = Result<BV, String>;
pub type AsyncHandler = Rc<dyn Fn(Vec<BV>) -> LocalBoxFuture<BridgeResult>>;
pub type SyncHandler = Rc<dyn Fn(Vec<BV>) -> BridgeResult>;
/// `reply(value, error)` of a `zbridge` message.
pub type Replier = Box<dyn FnOnce(Option<Value>, Option<String>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Async,
    Sync,
    Event,
    Off,
    Noop,
    NoopAsync,
    Value,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Async => "async",
            Mode::Sync => "sync",
            Mode::Event => "event",
            Mode::Off => "off",
            Mode::Noop => "noop",
            Mode::NoopAsync => "noop-async",
            Mode::Value => "value",
        }
    }
}

/// What the bridge needs from its page (implemented by `ClientHost`).
pub trait PageSink {
    /// Fire-and-forget page call: an async function body with named arguments.
    fn call(&self, body: &str, args: Value);
    /// Evaluate an async function body with named arguments; resolves with the JSON result.
    fn eval(&self, body: &str, args: Value) -> LocalBoxFuture<Result<Value, String>>;
}

#[derive(Default)]
struct Inner {
    globals: BTreeSet<String>,
    modes: HashMap<String, Mode>,
    values: HashMap<String, BV>,
    async_handlers: HashMap<String, AsyncHandler>,
    sync_handlers: HashMap<String, SyncHandler>,
    ipc_table: HashMap<String, (String, String)>,
    channel_handlers: HashMap<String, AsyncHandler>,
    channel_sync_handlers: HashMap<String, SyncHandler>,
    subscriptions: HashSet<String>,
    subscribed_channels: HashSet<String>,
    provided_channels: HashSet<String>,
    unhandled_channels: HashMap<String, u32>,
}

type Trace = Rc<dyn Fn(&str, &[BV])>;
type ErrorHook = Rc<dyn Fn(&str, &str)>;
type ChannelHook = Rc<dyn Fn(&str)>;

pub struct ZaloBridge {
    inner: RefCell<Inner>,
    page: RefCell<Option<Weak<dyn PageSink>>>,
    exec: Exec,
    /// Called for every bridge call (path or `ipc:<channel>`, args) — diagnostics.
    pub trace: RefCell<Option<Trace>>,
    /// Handler failures (key, message) — diagnostics.
    pub on_handler_error: RefCell<Option<ErrorHook>>,
    /// The renderer started listening on a channel or provides a handler.
    pub on_channel_activity: RefCell<Option<ChannelHook>>,
}

impl ZaloBridge {
    pub fn new(exec: Exec) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::default(),
            page: RefCell::new(None),
            exec,
            trace: RefCell::new(None),
            on_handler_error: RefCell::new(None),
            on_channel_activity: RefCell::new(None),
        })
    }

    pub fn exec(&self) -> &Exec {
        &self.exec
    }

    pub fn set_page(&self, page: Weak<dyn PageSink>) {
        *self.page.borrow_mut() = Some(page);
    }

    pub fn clear_page(&self) {
        *self.page.borrow_mut() = None;
    }

    fn page(&self) -> Option<Rc<dyn PageSink>> {
        self.page.borrow().as_ref().and_then(Weak::upgrade)
    }

    // MARK: registration — path-keyed members

    pub fn declare_global(&self, name: &str) {
        self.inner.borrow_mut().globals.insert(name.to_string());
    }

    fn register(&self, path: &str, mode: Mode) {
        let mut inner = self.inner.borrow_mut();
        inner.modes.insert(path.to_string(), mode);
        if let Some(global) = path.split('.').next().filter(|g| !g.is_empty()) {
            inner.globals.insert(global.to_string());
        }
    }

    pub fn value(&self, path: &str, value: BV) {
        self.register(path, Mode::Value);
        self.inner.borrow_mut().values.insert(path.to_string(), value);
    }

    pub fn r#async(&self, path: &str, handler: impl Fn(Vec<BV>) -> LocalBoxFuture<BridgeResult> + 'static) {
        self.register(path, Mode::Async);
        self.inner.borrow_mut().async_handlers.insert(path.to_string(), Rc::new(handler));
    }

    /// An async member whose answer is immediate.
    pub fn async_now(&self, path: &str, handler: impl Fn(Vec<BV>) -> BridgeResult + 'static) {
        let handler = Rc::new(handler);
        self.r#async(path, move |args| {
            let r = handler(args);
            Box::pin(async move { r })
        });
    }

    pub fn sync(&self, path: &str, handler: impl Fn(Vec<BV>) -> BridgeResult + 'static) {
        self.register(path, Mode::Sync);
        self.inner.borrow_mut().sync_handlers.insert(path.to_string(), Rc::new(handler));
    }

    pub fn event(&self, path: &str) {
        self.register(path, Mode::Event);
    }
    pub fn off(&self, path: &str) {
        self.register(path, Mode::Off);
    }
    pub fn noop(&self, paths: &[&str]) {
        paths.iter().for_each(|p| self.register(p, Mode::Noop));
    }
    pub fn noop_async(&self, paths: &[&str]) {
        paths.iter().for_each(|p| self.register(p, Mode::NoopAsync));
    }

    // MARK: registration — IPC channels

    /// `[member: {channel, type}]`, merged; the global is the part before the first `.`.
    pub fn load_ipc_table(&self, table: impl IntoIterator<Item = (String, String, String)>) {
        let mut inner = self.inner.borrow_mut();
        for (member, channel, kind) in table {
            if let Some(global) = member.split('.').next().filter(|g| !g.is_empty()) {
                inner.globals.insert(global.to_string());
            }
            inner.ipc_table.insert(member, (channel, kind));
        }
    }

    /// `ipcMain.handle(channel)` — answers invoke and send.
    pub fn handle(&self, channel: &str, handler: impl Fn(Vec<BV>) -> LocalBoxFuture<BridgeResult> + 'static) {
        self.inner.borrow_mut().channel_handlers.insert(channel.to_string(), Rc::new(handler));
    }

    /// An IPC handler whose answer is immediate (still the async kind: answers `invoke`, not `sendSync`).
    pub fn handle_now(&self, channel: &str, handler: impl Fn(Vec<BV>) -> BridgeResult + 'static) {
        let handler = Rc::new(handler);
        self.handle(channel, move |args| {
            let r = handler(args);
            Box::pin(async move { r })
        });
    }

    pub fn handle_sync(&self, channel: &str, handler: impl Fn(Vec<BV>) -> BridgeResult + 'static) {
        self.inner.borrow_mut().channel_sync_handlers.insert(channel.to_string(), Rc::new(handler));
    }

    /// Channels fine to acknowledge with `undefined` (only when no handler exists).
    pub fn acknowledge(&self, channels: &[&str]) {
        let mut inner = self.inner.borrow_mut();
        for c in channels {
            if !inner.channel_handlers.contains_key(*c) && !inner.channel_sync_handlers.contains_key(*c) {
                inner.channel_sync_handlers.insert(c.to_string(), Rc::new(|_| Ok(BV::Undefined)));
            }
        }
    }

    /// `window.__zaloBridgeSpec = {...};` — generated once, after every member is registered.
    pub fn spec_script(&self) -> String {
        let inner = self.inner.borrow();
        let members: Map<String, Value> = inner.modes.iter().map(|(k, m)| (k.clone(), Value::String(m.as_str().into()))).collect();
        let values: Map<String, Value> = inner.values.iter().map(|(k, v)| (k.clone(), v.to_js())).collect();
        let ipc: Map<String, Value> =
            inner.ipc_table.iter().map(|(k, (c, t))| (k.clone(), json!({"channel": c, "type": t}))).collect();
        let spec = json!({"globals": inner.globals.iter().collect::<Vec<_>>(), "members": members, "values": values, "ipc": ipc});
        format!("window.__zaloBridgeSpec = {spec};")
    }

    // MARK: introspection

    pub fn subscribed_channels(&self) -> HashSet<String> {
        self.inner.borrow().subscribed_channels.clone()
    }
    pub fn is_subscribed(&self, channel: &str) -> bool {
        self.inner.borrow().subscribed_channels.contains(channel)
    }
    pub fn provided_channels(&self) -> HashSet<String> {
        self.inner.borrow().provided_channels.clone()
    }
    pub fn subscriptions(&self) -> HashSet<String> {
        self.inner.borrow().subscriptions.clone()
    }
    pub fn unhandled_channels(&self) -> HashMap<String, u32> {
        self.inner.borrow().unhandled_channels.clone()
    }
    pub fn globals(&self) -> BTreeSet<String> {
        self.inner.borrow().globals.clone()
    }
    /// `(name, kind)` of every answered member/channel — for `--dump-bridge-registry`.
    pub fn registry(&self) -> Vec<(String, String)> {
        let inner = self.inner.borrow();
        let mut out: Vec<(String, String)> = inner.modes.iter().map(|(k, m)| (k.clone(), m.as_str().to_string())).collect();
        out.extend(inner.channel_handlers.keys().map(|c| (format!("ipc:{c}"), "async".to_string())));
        out.extend(inner.channel_sync_handlers.keys().map(|c| (format!("ipc:{c}"), "sync".to_string())));
        out.sort();
        out
    }

    // MARK: native → JS

    /// `webContents.send(channel, ...args)`; returns whether the renderer subscribed (does not wait for JS).
    pub fn send(&self, channel: &str, args: Vec<BV>) -> bool {
        let Some(page) = self.page() else { return false };
        page.call(
            "return window.__zaloBridge.ipcEmit(channel, args)",
            json!({"channel": channel, "args": Value::Array(args.iter().map(BV::to_js).collect())}),
        );
        self.is_subscribed(channel)
    }

    /// `webContents.invoke(channel, ...args)` — main asks a renderer PROVIDE handler.
    pub fn invoke_renderer(&self, channel: &str, args: Vec<BV>) -> LocalBoxFuture<BridgeResult> {
        let Some(page) = self.page() else { return Box::pin(async { Err("no renderer".to_string()) }) };
        let fut = page.eval(
            "return await window.__zaloBridge.ipcInvokeRenderer(channel, args)",
            json!({"channel": channel, "args": Value::Array(args.iter().map(BV::to_js).collect())}),
        );
        Box::pin(async move { fut.await.map(BV::from_js) })
    }

    /// Events for path-keyed members; `false` if never subscribed or no page.
    pub fn emit(&self, path: &str, args: Vec<BV>, with_event: bool) -> bool {
        if !self.inner.borrow().subscriptions.contains(path) {
            return false;
        }
        let Some(page) = self.page() else { return false };
        let body = if with_event { "window.__zaloBridge.emit(path, args)" } else { "window.__zaloBridge.emitRaw(path, args)" };
        page.call(body, json!({"path": path, "args": Value::Array(args.iter().map(BV::to_js).collect())}));
        true
    }

    // MARK: JS → native

    /// A `zbridge` message.
    pub fn handle_message(&self, body: Value, reply: Replier) {
        let Some(obj) = body.as_object() else { return reply(None, Some("bad bridge message".into())) };
        let Some(kind) = obj.get("kind").and_then(Value::as_str).map(str::to_string) else {
            return reply(None, Some("bad bridge message".into()));
        };
        let args: Vec<BV> = match obj.get("args") {
            Some(Value::Array(a)) => a.iter().cloned().map(BV::from_js).collect(),
            _ => Vec::new(),
        };

        if let Some(channel) = obj.get("channel").and_then(Value::as_str).map(str::to_string) {
            match kind.as_str() {
                "subscribe" => {
                    self.inner.borrow_mut().subscribed_channels.insert(channel.clone());
                    self.channel_activity(&channel);
                    reply(None, None);
                }
                "provide" => {
                    self.inner.borrow_mut().provided_channels.insert(channel.clone());
                    self.channel_activity(&channel);
                    reply(None, None);
                }
                _ => {
                    let key = format!("ipc:{channel}");
                    self.trace_call(&key, &args);
                    let (async_h, sync_h) = {
                        let inner = self.inner.borrow();
                        (inner.channel_handlers.get(&channel).cloned(), inner.channel_sync_handlers.get(&channel).cloned())
                    };
                    if let Some(h) = async_h {
                        let fut = h(args);
                        let hook = self.on_handler_error.borrow().clone();
                        spawn(&self.exec, async move {
                            match fut.await {
                                Ok(v) => reply(Some(v.to_js()), None),
                                Err(e) => {
                                    if let Some(hook) = hook {
                                        hook(&key, &e);
                                    }
                                    reply(None, Some(e));
                                }
                            }
                        });
                    } else if let Some(h) = sync_h {
                        match h(args) {
                            Ok(v) => reply(Some(v.to_js()), None),
                            Err(e) => {
                                self.handler_error(&key, &e);
                                reply(None, Some(e));
                            }
                        }
                    } else {
                        *self.inner.borrow_mut().unhandled_channels.entry(channel.clone()).or_default() += 1;
                        olaz_core::log::write(format!("bridge unhandled ipc {kind} {channel}"));
                        reply(Some(undefined_marker()), None);
                    }
                }
            }
            return;
        }

        let Some(path) = obj.get("path").and_then(Value::as_str).map(str::to_string) else {
            return reply(None, Some("bad bridge message".into()));
        };
        if kind == "subscribe" {
            self.inner.borrow_mut().subscriptions.insert(path);
            return reply(None, None);
        }
        self.trace_call(&path, &args);
        let (async_h, sync_h) = {
            let inner = self.inner.borrow();
            (inner.async_handlers.get(&path).cloned(), inner.sync_handlers.get(&path).cloned())
        };
        if let Some(h) = async_h {
            let fut = h(args);
            spawn(&self.exec, async move {
                match fut.await {
                    Ok(v) => reply(Some(v.to_js()), None),
                    Err(e) => reply(None, Some(e)),
                }
            });
        } else if let Some(h) = sync_h {
            match h(args) {
                Ok(v) => reply(Some(v.to_js()), None),
                Err(e) => reply(None, Some(e)),
            }
        } else {
            olaz_core::log::write(format!("bridge unhandled async bridge call {path}"));
            reply(Some(undefined_marker()), None);
        }
    }

    /// Synchronous calls (`bridge-sync`) — runs now, never waits on async work.
    pub fn handle_sync_request(&self, body: &[u8]) -> Vec<u8> {
        let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(body) else {
            return serde_json::to_vec(&json!({"error": "bad sync request"})).unwrap();
        };
        let args: Vec<BV> = match obj.get("args") {
            Some(Value::Array(a)) => a.iter().cloned().map(BV::from_js).collect(),
            _ => Vec::new(),
        };
        let (key, handler) = if let Some(channel) = obj.get("channel").and_then(Value::as_str) {
            let handler = self.inner.borrow().channel_sync_handlers.get(channel).cloned();
            if handler.is_none() {
                *self.inner.borrow_mut().unhandled_channels.entry(channel.to_string()).or_default() += 1;
            }
            (format!("ipc:{channel}"), handler)
        } else {
            let key = obj.get("path").and_then(Value::as_str).unwrap_or("?").to_string();
            let handler = self.inner.borrow().sync_handlers.get(&key).cloned();
            (key, handler)
        };
        self.trace_call(&key, &args);
        let mut response = Map::new();
        match handler {
            Some(h) => match h(args) {
                Ok(v) => {
                    response.insert("value".into(), v.to_js());
                }
                Err(e) => {
                    self.handler_error(&key, &e);
                    response.insert("error".into(), Value::String(e));
                }
            },
            None => {
                olaz_core::log::write(format!("bridge unhandled sync bridge call {key}"));
                response.insert("value".into(), undefined_marker());
            }
        }
        serde_json::to_vec(&Value::Object(response)).unwrap_or_else(|_| b"{}".to_vec())
    }

    fn trace_call(&self, key: &str, args: &[BV]) {
        let t = self.trace.borrow().clone();
        if let Some(t) = t {
            t(key, args);
        }
    }
    fn handler_error(&self, key: &str, e: &str) {
        let h = self.on_handler_error.borrow().clone();
        if let Some(h) = h {
            h(key, e);
        }
    }
    fn channel_activity(&self, channel: &str) {
        let h = self.on_channel_activity.borrow().clone();
        if let Some(h) = h {
            h(channel);
        }
    }
}

/// Helper for handlers: `Ok(BV::Undefined)` (Swift `return nil`).
pub fn none() -> BridgeResult {
    Ok(BV::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use olaz_core::exec::TestExecutor;
    use std::cell::RefCell as Cell;

    struct RecordingPage {
        calls: Cell<Vec<(String, Value)>>,
    }
    impl PageSink for RecordingPage {
        fn call(&self, body: &str, args: Value) {
            self.calls.borrow_mut().push((body.into(), args));
        }
        fn eval(&self, _body: &str, args: Value) -> LocalBoxFuture<Result<Value, String>> {
            Box::pin(async move { Ok(json!({"echo": args["channel"]})) })
        }
    }

    fn reply_into(slot: &Rc<Cell<Option<(Option<Value>, Option<String>)>>>) -> Replier {
        let slot = slot.clone();
        Box::new(move |v, e| *slot.borrow_mut() = Some((v, e)))
    }

    #[test]
    fn spec_script_contains_everything() {
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        b.declare_global("$zcommon");
        b.value("$zenv.isRenderer", BV::Bool(true));
        b.sync("$znode.fs.existsSync", |_| Ok(BV::Bool(true)));
        b.load_ipc_table([("$zapp.getVersion".to_string(), "_electron_get-app-version".to_string(), "REQUEST".to_string())]);
        let s = b.spec_script();
        let json: Value = serde_json::from_str(s.strip_prefix("window.__zaloBridgeSpec = ").unwrap().strip_suffix(';').unwrap()).unwrap();
        assert_eq!(json["globals"], json!(["$zapp", "$zcommon", "$zenv", "$znode"]));
        assert_eq!(json["members"]["$znode.fs.existsSync"], "sync");
        assert_eq!(json["values"]["$zenv.isRenderer"], true);
        assert_eq!(json["ipc"]["$zapp.getVersion"], json!({"channel": "_electron_get-app-version", "type": "REQUEST"}));
    }

    #[test]
    fn invoke_without_handler_is_undefined_and_counted() {
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        let slot = Rc::new(Cell::new(None));
        b.handle_message(json!({"kind": "invoke", "channel": "nope", "args": []}), reply_into(&slot));
        assert_eq!(slot.borrow().clone(), Some((Some(json!({"__undefined": true})), None)));
        assert_eq!(b.unhandled_channels().get("nope"), Some(&1));
    }

    #[test]
    fn async_and_sync_channel_handlers() {
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        b.handle_now("a", |args| Ok(BV::Array(args)));
        b.handle_sync("s", |_| Err("ENOENT: nope".into()));
        let errors = Rc::new(Cell::new(Vec::new()));
        let e2 = errors.clone();
        *b.on_handler_error.borrow_mut() = Some(Rc::new(move |k: &str, m: &str| e2.borrow_mut().push(format!("{k} {m}"))));
        let slot = Rc::new(Cell::new(None));
        b.handle_message(json!({"kind": "invoke", "channel": "a", "args": [{"__bytes": "aGk="}]}), reply_into(&slot));
        assert!(slot.borrow().is_none()); // async: answered by the executor
        t.run_until_stalled();
        assert_eq!(slot.borrow().clone(), Some((Some(json!([{"__bytes": "aGk="}])), None)));
        b.handle_message(json!({"kind": "invoke", "channel": "s"}), reply_into(&slot));
        assert_eq!(slot.borrow().clone(), Some((None, Some("ENOENT: nope".into()))));
        assert_eq!(errors.borrow().clone(), vec!["ipc:s ENOENT: nope".to_string()]);
        // sync request path: channel with only an async handler is "unhandled" for sendSync
        let out: Value = serde_json::from_slice(&b.handle_sync_request(br#"{"channel":"a","args":[]}"#)).unwrap();
        assert_eq!(out, json!({"value": {"__undefined": true}}));
        let out: Value = serde_json::from_slice(&b.handle_sync_request(br#"{"channel":"s"}"#)).unwrap();
        assert_eq!(out, json!({"error": "ENOENT: nope"}));
        let out: Value = serde_json::from_slice(&b.handle_sync_request(b"[1]")).unwrap();
        assert_eq!(out, json!({"error": "bad sync request"}));
    }

    #[test]
    fn path_members_and_subscriptions() {
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        let page = Rc::new(RecordingPage { calls: Cell::new(vec![]) });
        let weak: Weak<dyn PageSink> = Rc::downgrade(&(page.clone() as Rc<dyn PageSink>));
        b.set_page(weak);
        b.sync("$x.f", |args| Ok(args.first().cloned().unwrap_or(BV::Null)));
        let slot = Rc::new(Cell::new(None));
        b.handle_message(json!({"kind": "call", "path": "$x.f", "args": [5]}), reply_into(&slot));
        assert_eq!(slot.borrow().clone(), Some((Some(json!(5)), None)));
        assert!(!b.emit("$x.onY", vec![], true));
        b.handle_message(json!({"kind": "subscribe", "path": "$x.onY"}), reply_into(&slot));
        assert!(b.emit("$x.onY", vec![BV::Bytes(vec![1])], true));
        assert_eq!(page.calls.borrow().last().unwrap().0, "window.__zaloBridge.emit(path, args)");
        assert_eq!(page.calls.borrow().last().unwrap().1, json!({"path": "$x.onY", "args": [{"__bytes": "AQ=="}]}));
        assert!(!b.send("chan", vec![]));
        b.handle_message(json!({"kind": "subscribe", "channel": "chan"}), reply_into(&slot));
        assert!(b.send("chan", vec![BV::Undefined]));
        assert_eq!(page.calls.borrow().last().unwrap().1, json!({"channel": "chan", "args": [{"__undefined": true}]}));
        b.handle_message(json!({"nokind": 1}), reply_into(&slot));
        assert_eq!(slot.borrow().clone(), Some((None, Some("bad bridge message".into()))));
        b.handle_message(json!({"kind": "call"}), reply_into(&slot));
        assert_eq!(slot.borrow().clone(), Some((None, Some("bad bridge message".into()))));
    }

    #[test]
    fn invoke_renderer_round_trip() {
        let t = TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        let page = Rc::new(RecordingPage { calls: Cell::new(vec![]) });
        let page_dyn: Rc<dyn PageSink> = page.clone();
        b.set_page(Rc::downgrade(&page_dyn));
        let rx = olaz_core::exec::spawn_with_result(&t.exec(), b.invoke_renderer("c1", vec![]));
        t.run_until_stalled();
        let got = futures::executor::block_on(rx).unwrap();
        assert_eq!(got, Ok(BV::obj([("echo", BV::str("c1"))])));
    }
}
