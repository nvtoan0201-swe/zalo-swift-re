//! `NetworkProxy` — every http(s)/ws(s) request of the renderer. Port of `NetworkProxy.swift`.
//! Transport runs on the shared tokio runtime; results and socket events come back to the executor
//! (main thread) in order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures::StreamExt;
use futures::channel::mpsc as fmpsc;
use olaz_core::cookies::CookieJar;
use olaz_core::exec::{Exec, spawn};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use url::Url;

use crate::bridge::{PageSink, Replier};

pub const WEBSOCKET_ORIGIN: &str = "file://";
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// The process-wide tokio runtime for I/O (01 §3).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).thread_name("olaz-io").enable_all().build().expect("tokio runtime")
    })
}

/// `reqwest` cookie provider over the shared jar (Set-Cookie from every response, redirects included).
pub struct JarProvider(pub Arc<CookieJar>);

impl reqwest::cookie::CookieStore for JarProvider {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let headers: Vec<String> = cookie_headers.filter_map(|h| h.to_str().ok().map(str::to_string)).collect();
        self.0.store_response(url, headers.into_iter());
    }
    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.0.header_for(url).and_then(|h| HeaderValue::from_str(&h).ok())
    }
}

/// One form-data part (`formdata` body).
pub enum Part {
    Text { name: String, value: String },
    File { name: String, filename: String, content_type: String, data: Vec<u8> },
}

/// Multipart body: every part ends with CRLF; trailer `--<boundary>--\r\n`.
pub fn encode_multipart(parts: &[Part], boundary: &str) -> Vec<u8> {
    let mut data = Vec::new();
    for part in parts {
        data.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::File { name, filename, content_type, data: bytes } => {
                data.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n").as_bytes());
                data.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
                data.extend_from_slice(bytes);
            }
            Part::Text { name, value } => {
                data.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
                data.extend_from_slice(value.as_bytes());
            }
        }
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    data
}

pub fn new_boundary() -> String {
    olaz_core::fingerprint::chromium_form_boundary()
}

/// A request description from `znet` (`op: "http"`).
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: f64,
}

/// Builds the request ; `Err("bad url")` when the URL does not parse.
pub fn build_request(body: &Map<String, Value>) -> Result<HttpRequest, String> {
    let url = body.get("url").and_then(Value::as_str).and_then(|u| Url::parse(u).ok()).ok_or("bad url")?;
    let method = body.get("method").and_then(Value::as_str).unwrap_or("GET").to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    let set = |headers: &mut Vec<(String, String)>, k: &str, v: String| {
        headers.retain(|(name, _)| !name.eq_ignore_ascii_case(k));
        headers.push((k.to_string(), v));
    };
    if let Some(Value::Object(h)) = body.get("headers") {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                set(&mut headers, k, v.to_string());
            }
        }
    }
    let has = |headers: &Vec<(String, String)>, k: &str| headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(k));
    let mut data = None;
    if let Some(Value::Object(encoded)) = body.get("body") {
        if let Some(ct) = encoded.get("contentType").and_then(Value::as_str) {
            if !has(&headers, "Content-Type") {
                set(&mut headers, "Content-Type", ct.to_string());
            }
        }
        match encoded.get("kind").and_then(Value::as_str) {
            Some("text") => {
                data = Some(encoded.get("text").and_then(Value::as_str).unwrap_or("").as_bytes().to_vec());
                if !has(&headers, "Content-Type") {
                    set(&mut headers, "Content-Type", "text/plain;charset=UTF-8".into());
                }
            }
            Some("base64") => {
                data = STANDARD.decode(encoded.get("data").and_then(Value::as_str).unwrap_or("")).ok();
            }
            Some("formdata") => {
                let parts: Vec<Part> = encoded
                    .get("parts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_object)
                            .map(|p| {
                                let name = p.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                match p.get("filename").and_then(Value::as_str) {
                                    Some(f) => Part::File {
                                        name,
                                        filename: f.to_string(),
                                        content_type: p.get("type").and_then(Value::as_str).unwrap_or("application/octet-stream").to_string(),
                                        data: STANDARD.decode(p.get("data").and_then(Value::as_str).unwrap_or("")).unwrap_or_default(),
                                    },
                                    None => Part::Text { name, value: p.get("value").and_then(Value::as_str).unwrap_or("").to_string() },
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let boundary = new_boundary();
                data = Some(encode_multipart(&parts, &boundary));
                set(&mut headers, "Content-Type", format!("multipart/form-data; boundary={boundary}"));
            }
            _ => {}
        }
    }
    // Chromium 108's own request headers (client hints, sec-fetch, its Accept-Language / Accept-Encoding), which reqwest
    // would not send or would format differently; only names the page did not set.
    if !has(&headers, "User-Agent") {
        headers.push(("User-Agent".to_string(), olaz_core::fingerprint::user_agent()));
    }
    for (name, value) in olaz_core::fingerprint::client_headers(olaz_core::fingerprint::Flavour::Api) {
        if !has(&headers, name) {
            headers.push((name.to_string(), value));
        }
    }
    let timeout_ms = body.get("timeout").and_then(Value::as_f64).unwrap_or(0.0);
    Ok(HttpRequest { method, url, headers, body: data, timeout_ms })
}

pub struct HttpOutcome {
    pub status: u16,
    pub headers: Map<String, Value>,
    pub final_url: String,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct HttpFailure {
    pub message: String,
    pub timed_out: bool,
    /// URLError-like code (negative in `onRequestCompleted`).
    pub code: i64,
}

pub fn make_client(ua: &str, jar: Arc<CookieJar>) -> reqwest::Client {
    let mut defaults = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(ua) {
        defaults.insert(reqwest::header::USER_AGENT, v);
    }
    reqwest::Client::builder()
        .default_headers(defaults)
        .cookie_provider(Arc::new(JarProvider(jar)))
        .redirect(reqwest::redirect::Policy::limited(10))
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

pub async fn perform(client: reqwest::Client, req: HttpRequest) -> Result<HttpOutcome, HttpFailure> {
    let method = reqwest::Method::from_bytes(req.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut builder = client.request(method, req.url.clone());
    // URLSession's default request timeout is 60 s ("library default", not infinite).
    let timeout = if req.timeout_ms > 0.0 { Duration::from_secs_f64(req.timeout_ms / 1000.0) } else { Duration::from_secs(60) };
    builder = builder.timeout(timeout);
    for (k, v) in &req.headers {
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            builder = builder.header(name, value);
        }
    }
    if let Some(body) = req.body {
        builder = builder.body(body);
    }
    let fail = |e: reqwest::Error| {
        let timed_out = e.is_timeout();
        let code = if timed_out {
            1001
        } else if e.is_connect() {
            1004
        } else if e.is_redirect() {
            1007
        } else {
            1
        };
        let message = if timed_out { "The request timed out.".to_string() } else { error_chain(&e) };
        HttpFailure { message, timed_out, code }
    };
    let response = builder.send().await.map_err(fail)?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let mut headers = Map::new();
    for (k, v) in response.headers() {
        let key = k.as_str().to_lowercase();
        let value = String::from_utf8_lossy(v.as_bytes()).into_owned();
        match headers.get_mut(&key) {
            Some(Value::String(existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            _ => {
                headers.insert(key, Value::String(value));
            }
        }
    }
    let body = response.bytes().await.map_err(fail)?.to_vec();
    Ok(HttpOutcome { status, headers, final_url, body })
}

/// The fingerprint helper first (Chromium 108's TLS + HTTP/2 instead of rustls'); reqwest only when the helper cannot
/// serve the request. Cookies: attached from (and Set-Cookie harvested into) the shared jar, which reqwest's cookie
/// provider does on the other path.
pub async fn perform_fingerprinted(client: reqwest::Client, jar: Arc<CookieJar>, req: HttpRequest) -> Result<HttpOutcome, HttpFailure> {
    let proxy = crate::fingerprint::shared();
    if proxy.ready(Duration::from_secs(3)).await {
        let mut fp = crate::fingerprint::Request::get(req.url.as_str());
        fp.method = req.method.clone();
        fp.body = req.body.clone();
        fp.timeout_ms = if req.timeout_ms > 0.0 && req.timeout_ms < 600_000.0 { req.timeout_ms as u64 } else { 60_000 };
        // Chromium order: the page's headers, then the browser's defaults (both from `build_request`), then cookies.
        fp.headers = req.headers.clone();
        if let Some(cookie) = jar.header_for(&req.url) {
            fp.headers.push(("Cookie".into(), cookie));
        }
        match proxy.perform(&fp).await {
            Ok((head, body)) => {
                jar.store_response(&req.url, head.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.clone()));
                let mut headers = Map::new();
                for (k, v) in &head.headers {
                    let key = k.to_lowercase();
                    match headers.get_mut(&key) {
                        Some(Value::String(existing)) => {
                            existing.push_str(", ");
                            existing.push_str(v);
                        }
                        _ => {
                            headers.insert(key, Value::String(v.clone()));
                        }
                    }
                }
                let final_url = if head.url.is_empty() { req.url.to_string() } else { head.url };
                return Ok(HttpOutcome { status: head.status, headers, final_url, body });
            }
            Err(e) if e.falls_back() => olaz_core::log::write(format!("fingerprint: {e} — this request uses the built-in stack")),
            Err(e) => {
                let timed_out = e.is_timeout();
                return Err(HttpFailure { message: e.to_string(), timed_out, code: if timed_out { 1001 } else { 1 } });
            }
        }
    }
    perform(client, req).await
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

pub fn status_text(status: u16) -> String {
    reqwest::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or("").to_string()
}

// MARK: WebSockets

enum SocketCmd {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

#[derive(Debug)]
enum SocketEvent {
    Open(String),
    Text(String),
    Binary(Vec<u8>),
    /// Abnormal end: (close code or None → 1006, reason, description)
    Failed(Option<u16>, String, String),
    ServerClosed(u16, String),
}

struct SocketHandle {
    generation: u64,
    tx: tokio::sync::mpsc::UnboundedSender<SocketCmd>,
}

type CompletedHook = Rc<dyn Fn(&str, &Url, i64)>;
type ResponseObserver = Rc<dyn Fn(&Url, u16, &[u8])>;
type TraceHook = Rc<dyn Fn(String)>;

pub struct NetworkProxy {
    ua: String,
    jar: Arc<CookieJar>,
    client: reqwest::Client,
    exec: Exec,
    page: RefCell<Option<Weak<dyn PageSink>>>,
    sockets: RefCell<HashMap<i64, SocketHandle>>,
    frame_counts: RefCell<HashMap<i64, u32>>,
    next_generation: RefCell<u64>,
    events: fmpsc::UnboundedSender<(i64, u64, SocketEvent)>,
    pub on_request_completed: RefCell<Option<CompletedHook>>,
    pub response_observer: RefCell<Option<ResponseObserver>>,
    pub socket_trace: RefCell<Option<TraceHook>>,
}

impl NetworkProxy {
    pub fn new(ua: &str, jar: Arc<CookieJar>, exec: Exec) -> Rc<Self> {
        let client = {
            let _guard = runtime().enter();
            make_client(ua, jar.clone())
        };
        let (tx, mut rx) = fmpsc::unbounded::<(i64, u64, SocketEvent)>();
        let proxy = Rc::new(Self {
            ua: ua.to_string(),
            jar,
            client,
            exec: exec.clone(),
            page: RefCell::new(None),
            sockets: RefCell::new(HashMap::new()),
            frame_counts: RefCell::new(HashMap::new()),
            next_generation: RefCell::new(0),
            events: tx,
            on_request_completed: RefCell::new(None),
            response_observer: RefCell::new(None),
            socket_trace: RefCell::new(None),
        });
        // Socket events reach the page in receive order, on the main thread.
        let weak = Rc::downgrade(&proxy);
        spawn(&exec, async move {
            while let Some((id, generation, event)) = rx.next().await {
                let Some(proxy) = weak.upgrade() else { break };
                proxy.socket_event(id, generation, event);
            }
        });
        proxy
    }

    pub fn set_page(&self, page: Weak<dyn PageSink>) {
        *self.page.borrow_mut() = Some(page);
    }

    pub fn jar(&self) -> &Arc<CookieJar> {
        &self.jar
    }

    fn trace(&self, line: String) {
        let t = self.socket_trace.borrow().clone();
        if let Some(t) = t {
            t(line);
        }
    }

    /// A `znet` message.
    pub fn handle(self: &Rc<Self>, body: Value, reply: Replier) {
        let Some(obj) = body.as_object() else { return reply(None, Some("bad message".into())) };
        let Some(op) = obj.get("op").and_then(Value::as_str) else { return reply(None, Some("bad message".into())) };
        match op {
            "http" => {
                let this = self.clone();
                let obj = obj.clone();
                spawn(&self.exec, async move {
                    let value = this.perform_http(obj).await;
                    reply(Some(value), None);
                });
            }
            "ws.open" => {
                self.open_socket(obj);
                reply(None, None);
            }
            "ws.send" => {
                self.send_socket(obj);
                reply(None, None);
            }
            "ws.close" => {
                self.close_socket(obj);
                reply(None, None);
            }
            other => reply(None, Some(format!("unknown op {other}"))),
        }
    }

    async fn perform_http(&self, body: Map<String, Value>) -> Value {
        let req = match build_request(&body) {
            Ok(r) => r,
            Err(e) => return json!({"error": e}),
        };
        let (method, url) = (req.method.clone(), req.url.clone());
        let client = self.client.clone();
        let result = runtime().spawn(perform_fingerprinted(client, self.jar.clone(), req)).await;
        match result {
            Ok(Ok(out)) => {
                let completed = self.on_request_completed.borrow().clone();
                if let Some(h) = completed {
                    h(&method, &url, out.status as i64);
                }
                let observer = self.response_observer.borrow().clone();
                if let Some(o) = observer {
                    o(&url, out.status, &out.body);
                }
                json!({
                    "status": out.status,
                    "statusText": status_text(out.status),
                    "headers": Value::Object(out.headers),
                    "url": out.final_url,
                    "body": STANDARD.encode(&out.body),
                })
            }
            Ok(Err(fail)) => {
                olaz_core::log::write(format!("net error {} {}{} failed: {}", method, url.host_str().unwrap_or(""), url.path(), fail.code));
                let completed = self.on_request_completed.borrow().clone();
                if let Some(h) = completed {
                    h(&method, &url, -fail.code);
                }
                json!({"error": fail.message, "timedOut": fail.timed_out})
            }
            Err(join) => json!({"error": join.to_string()}),
        }
    }

    fn open_socket(&self, body: &Map<String, Value>) {
        let Some(id) = body.get("id").and_then(Value::as_f64).map(|f| f as i64) else { return };
        let Some(url) = body.get("url").and_then(Value::as_str).and_then(|u| Url::parse(u).ok()) else { return };
        let mut cookie_url = url.clone();
        let _ = cookie_url.set_scheme(if url.scheme() == "ws" { "http" } else { "https" });
        let cookie = self.jar.header_for(&cookie_url);
        match &cookie {
            Some(_) => {
                let names = self.jar.names_for(&cookie_url);
                self.trace(format!("ws#{id} attaching {} cookies: {:?}", names.len(), names));
            }
            None => self.trace(format!("ws#{id} no cookies for {}", url.host_str().unwrap_or(""))),
        }
        let protocols: Vec<String> = body
            .get("protocols")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let generation = {
            let mut g = self.next_generation.borrow_mut();
            *g += 1;
            *g
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.sockets.borrow_mut().insert(id, SocketHandle { generation, tx });
        let params: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        self.trace(format!(
            "ws#{id} open {}{} params={:?} open-sockets={}",
            url.host_str().unwrap_or(""),
            url.path(),
            params,
            self.sockets.borrow().len()
        ));
        let events = self.events.clone();
        let ua = self.ua.clone();
        runtime().spawn(socket_task(id, generation, url, ua, cookie, protocols, rx, events));
    }

    fn trace_frame(&self, id: i64, direction: &str, data: &[u8]) {
        let n = {
            let mut counts = self.frame_counts.borrow_mut();
            let c = counts.entry(id).or_default();
            *c += 1;
            *c
        };
        if n <= 80 {
            self.trace(format!("ws#{id} {direction} {}", describe_frame(data)));
        }
    }

    fn send_socket(&self, body: &Map<String, Value>) {
        let Some(id) = body.get("id").and_then(Value::as_f64).map(|f| f as i64) else { return };
        let sockets = self.sockets.borrow();
        let Some(handle) = sockets.get(&id) else { return };
        let cmd = if let Some(text) = body.get("text").and_then(Value::as_str) {
            SocketCmd::Text(text.to_string())
        } else {
            let data = STANDARD.decode(body.get("data").and_then(Value::as_str).unwrap_or("")).unwrap_or_default();
            drop(sockets);
            self.trace_frame(id, "out", &data);
            let sockets = self.sockets.borrow();
            if let Some(handle) = sockets.get(&id) {
                let _ = handle.tx.send(SocketCmd::Binary(data));
            }
            return;
        };
        let _ = handle.tx.send(cmd);
    }

    fn close_socket(&self, body: &Map<String, Value>) {
        let Some(id) = body.get("id").and_then(Value::as_f64).map(|f| f as i64) else { return };
        let Some(handle) = self.sockets.borrow_mut().remove(&id) else { return };
        let code = body.get("code").and_then(Value::as_f64).map(|c| c as i64).unwrap_or(1000);
        let reason = body.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
        self.trace(format!("ws#{id} closed-by-client code={code}"));
        let valid = if (1000..=4999).contains(&code) && ![1004, 1005, 1006, 1015].contains(&code) { code as u16 } else { 1000 };
        let _ = handle.tx.send(SocketCmd::Close(valid, reason.clone()));
        self.emit(id, "close", json!({"code": code, "reason": reason, "wasClean": true}));
    }

    /// Host shutdown: cancel with 1001, no events.
    pub fn close_all_sockets(&self) {
        for (_, handle) in self.sockets.borrow_mut().drain() {
            let _ = handle.tx.send(SocketCmd::Close(1001, String::new()));
        }
    }

    pub fn open_socket_count(&self) -> usize {
        self.sockets.borrow().len()
    }

    fn socket_event(&self, id: i64, generation: u64, event: SocketEvent) {
        // Only for the socket still in the table (`sockets[id] === task`): late events are dropped.
        if self.sockets.borrow().get(&id).map(|h| h.generation) != Some(generation) {
            return;
        }
        match event {
            SocketEvent::Open(protocol) => {
                self.trace(format!("ws#{id} handshake ok"));
                self.emit(id, "open", json!({"protocol": protocol}));
            }
            SocketEvent::Text(text) => self.emit(id, "text", json!({"text": text})),
            SocketEvent::Binary(data) => {
                self.trace_frame(id, "in ", &data);
                self.emit(id, "binary", json!({"data": STANDARD.encode(&data)}));
            }
            SocketEvent::ServerClosed(code, reason) => {
                self.sockets.borrow_mut().remove(&id);
                self.trace(format!("ws#{id} closed by server code={code} reason={}", reason.chars().take(80).collect::<String>()));
                self.emit(id, "close", json!({"code": code, "reason": reason, "wasClean": true}));
            }
            SocketEvent::Failed(code, reason, description) => {
                self.sockets.borrow_mut().remove(&id);
                let code = code.unwrap_or(1006);
                self.trace(format!("ws#{id} closed code={code} reason={} error={description}", reason.chars().take(80).collect::<String>()));
                if code == 1006 {
                    self.emit(id, "error", json!({}));
                }
                self.emit(id, "close", json!({"code": code, "reason": reason, "wasClean": code != 1006}));
            }
        }
    }

    fn emit(&self, id: i64, kind: &str, payload: Value) {
        let page = self.page.borrow().as_ref().and_then(Weak::upgrade);
        if let Some(page) = page {
            page.call("window.__zaloNet && window.__zaloNet.socketEvent(id, type, payload)", json!({"id": id, "type": kind, "payload": payload}));
        }
    }
}

/// `[version u8][cmd u16 LE][subCmd u8]` — headers only, never payloads.
pub fn describe_frame(data: &[u8]) -> String {
    if data.len() < 4 {
        return format!("len={}", data.len());
    }
    let cmd = data[1] as u16 | (data[2] as u16) << 8;
    format!("v={} cmd={} sub={} len={}", data[0], cmd, data[3], data.len())
}

#[allow(clippy::too_many_arguments)]
async fn socket_task(
    id: i64,
    generation: u64,
    url: Url,
    ua: String,
    cookie: Option<String>,
    protocols: Vec<String>,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<SocketCmd>,
    events: fmpsc::UnboundedSender<(i64, u64, SocketEvent)>,
) {
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode};
    let ev = |e: SocketEvent| {
        let _ = events.unbounded_send((id, generation, e));
    };
    // The fingerprint helper first (Chrome's ClientHello on the wss handshake); tungstenite only when it is unavailable.
    if crate::fingerprint::shared().ready(Duration::from_secs(3)).await {
        let mut headers = vec![("User-Agent".to_string(), ua.clone()), ("Origin".to_string(), WEBSOCKET_ORIGIN.to_string())];
        if let Some(c) = &cookie {
            headers.push(("Cookie".to_string(), c.clone()));
        }
        if helper_socket(url.as_str(), &headers, &protocols, &mut cmds, &ev).await {
            return;
        }
    }
    let mut request = match url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => return ev(SocketEvent::Failed(None, String::new(), e.to_string())),
    };
    let headers = request.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&ua) {
        headers.insert("User-Agent", v);
    }
    headers.insert("Origin", HeaderValue::from_static(WEBSOCKET_ORIGIN));
    if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        headers.insert("Cookie", c);
    }
    if !protocols.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&protocols.join(", ")) {
            headers.insert("Sec-WebSocket-Protocol", v);
        }
    }
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE);
    config.max_frame_size = Some(MAX_MESSAGE);
    // Commands sent before the handshake completes stay queued in `cmds` (like URLSession tasks).
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let (stream, response) = tokio::select! {
        r = connect => match r {
            Ok(ok) => ok,
            Err(e) => return ev(SocketEvent::Failed(None, String::new(), e.to_string())),
        },
        cmd = cmds.recv() => {
            // Closed (or dropped) before the handshake: nothing reaches the page (the client already got `close`).
            let _ = cmd;
            return;
        }
    };
    let protocol = response.headers().get("Sec-WebSocket-Protocol").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    ev(SocketEvent::Open(protocol));
    let (mut sink, mut source) = stream.split();
    use futures::SinkExt;
    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(SocketCmd::Text(t)) => {
                    if let Err(e) = sink.send(Message::Text(t.into())).await {
                        return ev(SocketEvent::Failed(None, String::new(), e.to_string()));
                    }
                }
                Some(SocketCmd::Binary(b)) => {
                    if let Err(e) = sink.send(Message::Binary(b.into())).await {
                        return ev(SocketEvent::Failed(None, String::new(), e.to_string()));
                    }
                }
                Some(SocketCmd::Close(code, reason)) => {
                    let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
                    let _ = tokio::time::timeout(Duration::from_secs(2), sink.send(Message::Close(Some(frame)))).await;
                    return;
                }
                None => return,
            },
            msg = source.next() => match msg {
                Some(Ok(Message::Text(t))) => ev(SocketEvent::Text(t.to_string())),
                Some(Ok(Message::Binary(b))) => ev(SocketEvent::Binary(b.to_vec())),
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new()));
                    return ev(SocketEvent::ServerClosed(code, reason));
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return ev(SocketEvent::Failed(None, String::new(), e.to_string())),
                None => return ev(SocketEvent::Failed(None, String::new(), "connection closed".into())),
            },
        }
    }
}

/// One websocket through the fingerprint helper. `false`: the helper could not open it (fall back to tungstenite).
async fn helper_socket(
    url: &str,
    headers: &[(String, String)],
    protocols: &[String],
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<SocketCmd>,
    ev: &impl Fn(SocketEvent),
) -> bool {
    use crate::fingerprint::WsEvent;
    let open = crate::fingerprint::shared().ws_open(url, headers, protocols);
    let (session, protocol, mut events) = tokio::select! {
        r = open => match r {
            Ok(ok) => ok,
            Err(e) if e.falls_back() => {
                olaz_core::log::write(format!("fingerprint: {e} — websocket uses the built-in stack"));
                return false;
            }
            Err(e) => {
                ev(SocketEvent::Failed(None, String::new(), e.to_string()));
                return true;
            }
        },
        cmd = cmds.recv() => {
            // Closed (or dropped) before the handshake: nothing reaches the page (the client already got `close`).
            let _ = cmd;
            return true;
        }
    };
    ev(SocketEvent::Open(protocol));
    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(SocketCmd::Text(t)) => {
                    if let Err(e) = session.send_text(&t) {
                        ev(SocketEvent::Failed(None, String::new(), e.to_string()));
                        return true;
                    }
                }
                Some(SocketCmd::Binary(b)) => {
                    if let Err(e) = session.send_binary(&b) {
                        ev(SocketEvent::Failed(None, String::new(), e.to_string()));
                        return true;
                    }
                }
                Some(SocketCmd::Close(code, _)) => {
                    session.close(code);
                    return true;
                }
                None => return true,
            },
            event = events.recv() => match event {
                Some(WsEvent::Text(t)) => ev(SocketEvent::Text(t)),
                Some(WsEvent::Binary(b)) => ev(SocketEvent::Binary(b)),
                // 1006: no close frame (the helper's reading of a broken connection).
                Some(WsEvent::Closed(1006, reason)) => {
                    ev(SocketEvent::Failed(None, reason, "connection closed".into()));
                    return true;
                }
                Some(WsEvent::Closed(code, reason)) => {
                    ev(SocketEvent::ServerClosed(code, reason));
                    return true;
                }
                Some(WsEvent::Error(m)) => {
                    ev(SocketEvent::Failed(None, String::new(), m));
                    return true;
                }
                None => {
                    ev(SocketEvent::Failed(None, String::new(), "helper exited".into()));
                    return true;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_bytes() {
        let parts = vec![
            Part::Text { name: "a".into(), value: "1".into() },
            Part::File { name: "f".into(), filename: "x.bin".into(), content_type: "application/octet-stream".into(), data: vec![0, 1] },
        ];
        let out = encode_multipart(&parts, "B");
        let expected = b"--B\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n\x00\x01\r\n--B--\r\n";
        assert_eq!(out, expected.to_vec());
        let b = new_boundary();
        assert!(b.starts_with("----WebKitFormBoundary") && b.len() == 22 + 16);
    }

    #[test]
    fn request_building() {
        let body = json!({"url": "https://x.zalo.me/a?b=1", "headers": {"content-type": "app/json"}, "body": {"kind": "text", "text": "hi", "contentType": "text/x"}});
        let r = build_request(body.as_object().unwrap()).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.headers[0], ("content-type".to_string(), "app/json".to_string()));
        // Chromium's request headers follow the page's own; no Origin / Referer.
        let names: Vec<&str> = r.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"sec-ch-ua") && names.contains(&"sec-fetch-mode") && names.contains(&"accept-encoding"));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("origin") || n.eq_ignore_ascii_case("referer")));
        assert_eq!(r.body.as_deref(), Some(&b"hi"[..]));
        let body = json!({"url": "https://x/", "body": {"kind": "text", "text": "hi"}});
        let r = build_request(body.as_object().unwrap()).unwrap();
        assert_eq!(r.headers[0], ("Content-Type".to_string(), "text/plain;charset=UTF-8".to_string()));
        let body = json!({"url": "https://x/", "headers": {"Content-Type": "x"}, "body": {"kind": "formdata", "parts": [{"name": "n", "value": "v"}]}});
        let r = build_request(body.as_object().unwrap()).unwrap();
        assert!(r.headers[0].1.starts_with("multipart/form-data; boundary=----WebKitFormBoundary"));
        assert_eq!(build_request(json!({"url": "::"}).as_object().unwrap()).unwrap_err(), "bad url");
    }

    #[test]
    fn frame_description() {
        assert_eq!(describe_frame(&[1, 2]), "len=2");
        assert_eq!(describe_frame(&[1, 0x01, 0x02, 3, 9]), "v=1 cmd=513 sub=3 len=5");
    }
}
