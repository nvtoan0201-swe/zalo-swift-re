//! `FingerprintProxy` — Olaz's requests through `olaz-fingerprint-helper`, so that every connection handshakes with
//! Chromium 108's TLS ClientHello and HTTP/2 settings (the official Zalo PC client's, Electron 22.3.9) instead of rustls'.
//! Port of `FingerprintProxy.swift` + `FingerprintProxyConnection.swift`; the helper itself is upstream's Go program
//! (`linux-port/fingerprint-helper`, protocol in its `protocol.go`).
//!
//! Callers fall back to reqwest / tungstenite only when the helper cannot serve a request (`Error::falls_back`); an
//! upstream failure (timeout, refused connection) is a real result and is never retried on the other stack — a retry
//! that switched fingerprint is exactly what this exists to avoid. Everything here runs on the shared tokio runtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

pub const HELPER_NAME: &str = "olaz-fingerprint-helper";

/// One upstream request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: String,
    /// Sent in this order (Chromium sends the page's headers before its own defaults, cookies last).
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: u64,
    pub follow: bool,
    pub max_bytes: u64,
    pub idle_ms: u64,
    pub first_byte_ms: u64,
}

impl Request {
    pub fn get(url: &str) -> Self {
        Self {
            method: "GET".into(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
            timeout_ms: 0,
            follow: true,
            max_bytes: 0,
            idle_ms: 30_000,
            first_byte_ms: 0,
        }
    }

    fn to_json(&self, stream: bool) -> String {
        json!({
            "method": self.method,
            "url": self.url,
            "headers": self.headers.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
            "bodyB64": self.body.as_ref().map(|b| STANDARD.encode(b)).unwrap_or_default(),
            "timeoutMs": self.timeout_ms,
            "stream": stream,
            "follow": self.follow,
            "maxBytes": self.max_bytes,
            "idleMs": self.idle_ms,
            "firstByteMs": self.first_byte_ms,
        })
        .to_string()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The final URL (after redirects the helper followed).
    pub url: String,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The helper is not running (and could not be started): fall back.
    Unavailable(String),
    /// The helper went away mid-request: fall back for this request.
    ConnectionLost(String),
    /// The upstream request itself failed — a real result, do not fall back.
    Failed(String),
    TooLarge,
}

impl Error {
    pub fn falls_back(&self) -> bool {
        matches!(self, Error::Unavailable(_) | Error::ConnectionLost(_))
    }
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Failed(m) if { let m = m.to_lowercase(); m.contains("timeout") || m.contains("deadline exceeded") || m.contains("timed out") })
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unavailable(m) => write!(f, "fingerprint helper unavailable: {m}"),
            Error::ConnectionLost(m) => write!(f, "fingerprint helper connection lost: {m}"),
            Error::Failed(m) => f.write_str(m),
            Error::TooLarge => f.write_str("body too large"),
        }
    }
}

/// A streaming request's start: the helper answers a redirect it was told not to follow as a complete response.
pub enum StreamStart {
    Complete(Head, Vec<u8>),
    Body(Head, BodyStream),
}

/// Body chunks of a streaming response; `None` = finished. Dropping it cancels the upstream transfer.
pub struct BodyStream {
    rx: mpsc::UnboundedReceiver<Result<Vec<u8>, Error>>,
    _guard: CancelGuard,
}

impl BodyStream {
    pub async fn next(&mut self) -> Option<Result<Vec<u8>, Error>> {
        self.rx.recv().await
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    Text(String),
    Binary(Vec<u8>),
    Closed(u16, String),
    Error(String),
}

// MARK: Connection

enum StartMsg {
    Streaming(Head),
    Complete(Head, Vec<u8>),
}

enum Pending {
    Complete(oneshot::Sender<Result<(Head, Vec<u8>), Error>>),
    Stream { start: Option<oneshot::Sender<Result<StartMsg, Error>>>, chunks: mpsc::UnboundedSender<Result<Vec<u8>, Error>> },
    WsOpen(oneshot::Sender<Result<String, Error>>),
}

struct Connection {
    /// Frames to write, in order (a writer task owns the socket's write half).
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<String, Pending>>,
    sockets: Mutex<HashMap<String, mpsc::UnboundedSender<WsEvent>>>,
    hello: Mutex<Option<oneshot::Sender<String>>>,
    alive: AtomicBool,
}

impl Connection {
    fn send(&self, frame: Vec<u8>) -> Result<(), Error> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(Error::ConnectionLost("helper exited".into()));
        }
        self.out.send(frame).map_err(|_| Error::ConnectionLost("helper exited".into()))
    }

    fn send_line(&self, line: String) -> Result<(), Error> {
        self.send(format!("{line}\n").into_bytes())
    }

    fn fail_all(&self, why: &str) {
        self.alive.store(false, Ordering::SeqCst);
        let pending: Vec<Pending> = self.pending.lock().unwrap().drain().map(|(_, p)| p).collect();
        for p in pending {
            let e = Error::ConnectionLost(why.to_string());
            match p {
                Pending::Complete(tx) => {
                    let _ = tx.send(Err(e));
                }
                Pending::Stream { start: Some(tx), .. } => {
                    let _ = tx.send(Err(e));
                }
                Pending::Stream { start: None, chunks } => {
                    let _ = chunks.send(Err(e));
                }
                Pending::WsOpen(tx) => {
                    let _ = tx.send(Err(e));
                }
            }
        }
        for (_, tx) in self.sockets.lock().unwrap().drain() {
            let _ = tx.send(WsEvent::Error(why.to_string()));
        }
    }

    fn handle_json(&self, command: &str, id: &str, rest: &str) {
        let v: Value = serde_json::from_str(rest).unwrap_or(Value::Null);
        let error = || v.get("error").and_then(Value::as_str).unwrap_or("request failed").to_string();
        let as_error = |m: String| if m.contains("too large") { Error::TooLarge } else { Error::Failed(m) };
        match command {
            "RES" | "RESHEAD" => {
                let head = parse_head(&v);
                let body = || v.get("bodyB64").and_then(Value::as_str).and_then(|b| STANDARD.decode(b).ok()).unwrap_or_default();
                let mut pending = self.pending.lock().unwrap();
                match pending.remove(id) {
                    Some(Pending::Complete(tx)) => {
                        let _ = tx.send(Ok((head, body())));
                    }
                    Some(Pending::Stream { start: Some(tx), chunks }) => {
                        if command == "RES" {
                            let _ = tx.send(Ok(StartMsg::Complete(head, body())));
                        } else {
                            let _ = tx.send(Ok(StartMsg::Streaming(head)));
                            pending.insert(id.to_string(), Pending::Stream { start: None, chunks });
                        }
                    }
                    Some(other) => {
                        pending.insert(id.to_string(), other);
                    }
                    None => {}
                }
            }
            "ERR" => match self.pending.lock().unwrap().remove(id) {
                Some(Pending::Complete(tx)) => {
                    let _ = tx.send(Err(as_error(error())));
                }
                Some(Pending::Stream { start: Some(tx), .. }) => {
                    let _ = tx.send(Err(as_error(error())));
                }
                Some(Pending::Stream { start: None, chunks }) => {
                    let _ = chunks.send(Err(as_error(error())));
                }
                _ => {}
            },
            "WSOPENED" | "WSERR" => {
                if let Some(Pending::WsOpen(tx)) = self.pending.lock().unwrap().remove(id) {
                    let _ = tx.send(if command == "WSOPENED" {
                        Ok(v.get("protocol").and_then(Value::as_str).unwrap_or("").to_string())
                    } else {
                        Err(Error::Failed(error()))
                    });
                }
                if command == "WSERR" {
                    self.sockets.lock().unwrap().remove(id);
                }
            }
            _ => {}
        }
    }
}

fn parse_head(v: &Value) -> Head {
    let headers = v
        .get("headers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    let pair = h.as_array()?;
                    Some((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Head {
        status: v.get("status").and_then(Value::as_u64).unwrap_or(0) as u16,
        headers,
        url: v.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
    }
}

/// "REQ <id> <json…>" → ("REQ", "<id>", "<json…>")
fn split_command(line: &str) -> (&str, &str, &str) {
    let Some((command, remainder)) = line.split_once(' ') else { return (line, "", "") };
    match remainder.split_once(' ') {
        Some((id, rest)) => (command, id, rest),
        None => (command, remainder, ""),
    }
}

async fn read_loop(conn: Arc<Connection>, read: tokio::net::unix::OwnedReadHalf) {
    let mut reader = BufReader::with_capacity(64 << 10, read);
    let mut line = Vec::new();
    let why = loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break "helper exited".to_string(),
            Err(e) => break e.to_string(),
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(&line)).into_owned();
        let (command, id, rest) = split_command(&text);
        match command {
            "OK" => {
                if let Some(tx) = conn.hello.lock().unwrap().take() {
                    let _ = tx.send(format!("{id} {rest}"));
                }
            }
            "CHUNK" | "WSMSG" => {
                // CHUNK <id> <len> / WSMSG <id> text|binary <len>, then exactly <len> bytes.
                let (kind, len) = if command == "CHUNK" { ("", rest) } else { rest.split_once(' ').unwrap_or(("binary", "0")) };
                let Ok(len) = len.trim().parse::<usize>() else { break format!("bad frame: {text}") };
                let mut payload = vec![0u8; len];
                if let Err(e) = reader.read_exact(&mut payload).await {
                    break e.to_string();
                }
                if command == "CHUNK" {
                    if let Some(Pending::Stream { chunks, .. }) = conn.pending.lock().unwrap().get(id) {
                        let _ = chunks.send(Ok(payload));
                    }
                } else if let Some(tx) = conn.sockets.lock().unwrap().get(id) {
                    let _ = tx.send(if kind == "text" { WsEvent::Text(String::from_utf8_lossy(&payload).into_owned()) } else { WsEvent::Binary(payload) });
                }
            }
            "END" => {
                // Dropping the chunk sender ends the stream.
                conn.pending.lock().unwrap().remove(id);
            }
            "WSCLOSED" => {
                // WSCLOSED <id> <code> {json}
                let (code, json) = rest.split_once(' ').unwrap_or((rest, "{}"));
                let reason = serde_json::from_str::<Value>(json).ok().and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_string)).unwrap_or_default();
                if let Some(tx) = conn.sockets.lock().unwrap().remove(id) {
                    let _ = tx.send(WsEvent::Closed(code.parse().unwrap_or(1006), reason));
                }
            }
            "BYE" => break "shut down".to_string(),
            _ => conn.handle_json(command, id, rest),
        }
    };
    olaz_core::log::write(format!("fingerprint: connection closed ({why})"));
    conn.fail_all(&why);
}

/// Sends `CANCEL <id>` when dropped before the request finished.
struct CancelGuard {
    conn: Arc<Connection>,
    id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.conn.pending.lock().unwrap().remove(&self.id).is_some() {
            let _ = self.conn.send_line(format!("CANCEL {}", self.id));
        }
    }
}

/// An open websocket session in the helper. Dropping it closes the session (1001) unless `close` ran.
pub struct WsSession {
    conn: Arc<Connection>,
    id: String,
    closed: bool,
}

impl WsSession {
    fn frame(&self, kind: &str, payload: &[u8]) -> Result<(), Error> {
        let mut frame = format!("WSSEND {} {kind} {}\n", self.id, payload.len()).into_bytes();
        frame.extend_from_slice(payload);
        self.conn.send(frame)
    }
    pub fn send_text(&self, text: &str) -> Result<(), Error> {
        self.frame("text", text.as_bytes())
    }
    pub fn send_binary(&self, data: &[u8]) -> Result<(), Error> {
        self.frame("binary", data)
    }
    pub fn close(mut self, code: u16) {
        self.closed = true;
        self.conn.sockets.lock().unwrap().remove(&self.id);
        let _ = self.conn.send_line(format!("WSCLOSE {} {code}", self.id));
    }
}

impl Drop for WsSession {
    fn drop(&mut self) {
        if !self.closed {
            self.conn.sockets.lock().unwrap().remove(&self.id);
            self.conn.pending.lock().unwrap().remove(&self.id);
            let _ = self.conn.send_line(format!("WSCLOSE {} 1001", self.id));
        }
    }
}

// MARK: Proxy

enum State {
    Idle,
    Ready(Arc<Connection>),
    Failed { retry_at: Instant },
    Disabled,
}

pub struct FingerprintProxy {
    state: tokio::sync::Mutex<(State, u32)>,
    pid: Mutex<Option<u32>>,
    next_id: AtomicU64,
}

/// The process-wide proxy.
pub fn shared() -> &'static FingerprintProxy {
    static PROXY: OnceLock<FingerprintProxy> = OnceLock::new();
    PROXY.get_or_init(|| FingerprintProxy { state: tokio::sync::Mutex::new((State::Idle, 0)), pid: Mutex::new(None), next_id: AtomicU64::new(1) })
}

/// The helper binary: `OLAZ_FINGERPRINT_HELPER` (`off` disables it), next to the executable, `../libexec/olaz/`
/// (packaged installs), then the dev build in the repo.
pub fn helper_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("OLAZ_FINGERPRINT_HELPER") {
        if p == "off" || p == "0" {
            return None;
        }
        candidates.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(HELPER_NAME));
        candidates.push(dir.join("../libexec/olaz").join(HELPER_NAME));
    }
    candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fingerprint-helper/build").join(HELPER_NAME));
    candidates.into_iter().find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn socket_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir);
    base.join("olaz")
}

/// Sockets of helpers whose Olaz process is gone (`fp-helper-<pid>-…`).
fn sweep_stale_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("fp-helper-").filter(|_| name.ends_with(".sock")) else { continue };
        let pid: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(pid) = pid.parse::<i32>() {
            if pid > 0 && unsafe { libc::kill(pid, 0) } != 0 {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

impl FingerprintProxy {
    /// Starts the helper now (engine boot), so the first request does not wait for it.
    pub fn warm_up(&'static self) {
        crate::net::runtime().spawn(async move {
            let _ = self.connection().await;
        });
    }

    async fn connection(&self) -> Option<Arc<Connection>> {
        let mut guard = self.state.lock().await;
        let (state, attempts) = &mut *guard;
        match state {
            State::Ready(c) if c.alive.load(Ordering::SeqCst) => return Some(c.clone()),
            State::Ready(_) => {
                // The helper died: back off before the next start.
                *attempts += 1;
                *state = State::Failed { retry_at: Instant::now() + retry_delay(*attempts) };
                return None;
            }
            State::Failed { retry_at } if *retry_at > Instant::now() => return None,
            State::Disabled => return None,
            State::Idle | State::Failed { .. } => {}
        }
        let Some(helper) = helper_path() else {
            olaz_core::log::write("fingerprint: helper not installed — requests use the built-in TLS stack");
            *state = State::Disabled;
            return None;
        };
        match self.start(&helper).await {
            Ok(c) => {
                *attempts = 0;
                *state = State::Ready(c.clone());
                Some(c)
            }
            Err(e) => {
                olaz_core::log::write(format!("fingerprint: start failed — {e}"));
                *attempts += 1;
                *state = State::Failed { retry_at: Instant::now() + retry_delay(*attempts) };
                None
            }
        }
    }

    async fn start(&self, helper: &Path) -> Result<Arc<Connection>, String> {
        let dir = socket_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        sweep_stale_sockets(&dir);
        let socket = dir.join(format!("fp-helper-{}-{}.sock", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let pid = spawn_helper(helper, &socket)?;
        *self.pid.lock().unwrap() = Some(pid);

        let mut waited = 0;
        while !socket.exists() && waited < 50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += 1;
        }
        let stream = UnixStream::connect(&socket).await.map_err(|e| {
            kill_pid(pid);
            format!("connect: {e}")
        })?;
        let (read, mut write) = stream.into_split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (hello_tx, hello_rx) = oneshot::channel();
        let conn = Arc::new(Connection {
            out,
            pending: Mutex::new(HashMap::new()),
            sockets: Mutex::new(HashMap::new()),
            hello: Mutex::new(Some(hello_tx)),
            alive: AtomicBool::new(true),
        });
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if write.write_all(&frame).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(read_loop(conn.clone(), read));
        conn.send_line("HELLO".into()).map_err(|e| e.to_string())?;
        match tokio::time::timeout(Duration::from_secs(5), hello_rx).await {
            Ok(Ok(profile)) => {
                olaz_core::log::write(format!("fingerprint: helper ready ({})", profile.trim()));
                Ok(conn)
            }
            _ => {
                conn.fail_all("handshake failed");
                kill_pid(pid);
                Err("handshake failed".into())
            }
        }
    }

    /// Waits for readiness (starting the helper if needed) for at most `timeout`.
    pub async fn ready(&self, timeout: Duration) -> bool {
        matches!(tokio::time::timeout(timeout, self.connection()).await, Ok(Some(_)))
    }

    async fn live(&self) -> Result<Arc<Connection>, Error> {
        self.connection().await.ok_or_else(|| Error::Unavailable("helper not running".into()))
    }

    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// A complete (buffered, ≤ 64 MB) request.
    pub async fn perform(&self, request: &Request) -> Result<(Head, Vec<u8>), Error> {
        let conn = self.live().await?;
        let id = self.next_id("r");
        let (tx, rx) = oneshot::channel();
        conn.pending.lock().unwrap().insert(id.clone(), Pending::Complete(tx));
        let guard = CancelGuard { conn: conn.clone(), id: id.clone() };
        conn.send_line(format!("REQ {id} {}", request.to_json(false)))?;
        let result = rx.await.unwrap_or_else(|_| Err(Error::ConnectionLost("helper exited".into())));
        drop(guard);
        result
    }

    /// A streaming request: the head and the body stream, or a complete response for an unfollowed redirect.
    pub async fn stream(&self, request: &Request) -> Result<StreamStart, Error> {
        let conn = self.live().await?;
        let id = self.next_id("s");
        let (tx, rx) = oneshot::channel();
        let (chunks, chunk_rx) = mpsc::unbounded_channel();
        conn.pending.lock().unwrap().insert(id.clone(), Pending::Stream { start: Some(tx), chunks });
        let guard = CancelGuard { conn: conn.clone(), id: id.clone() };
        conn.send_line(format!("REQ {id} {}", request.to_json(true)))?;
        match rx.await.unwrap_or_else(|_| Err(Error::ConnectionLost("helper exited".into())))? {
            StartMsg::Complete(head, body) => Ok(StreamStart::Complete(head, body)),
            StartMsg::Streaming(head) => Ok(StreamStart::Body(head, BodyStream { rx: chunk_rx, _guard: guard })),
        }
    }

    /// Opens a websocket through the helper (Chrome's ClientHello on the wss handshake too). Returns the session, the
    /// negotiated subprotocol and the session's events.
    pub async fn ws_open(
        &self,
        url: &str,
        headers: &[(String, String)],
        protocols: &[String],
    ) -> Result<(WsSession, String, mpsc::UnboundedReceiver<WsEvent>), Error> {
        let conn = self.live().await?;
        let id = self.next_id("w");
        let (events, events_rx) = mpsc::unbounded_channel();
        let (tx, rx) = oneshot::channel();
        conn.sockets.lock().unwrap().insert(id.clone(), events);
        conn.pending.lock().unwrap().insert(id.clone(), Pending::WsOpen(tx));
        let session = WsSession { conn: conn.clone(), id: id.clone(), closed: false };
        let spec = json!({
            "url": url,
            "headers": headers.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
            "protocols": protocols,
        });
        conn.send_line(format!("WSOPEN {id} {spec}"))?;
        let protocol = rx.await.unwrap_or_else(|_| Err(Error::ConnectionLost("helper exited".into())))?;
        Ok((session, protocol, events_rx))
    }

    /// App exit: the helper also dies with us (PR_SET_PDEATHSIG), this is the polite path.
    pub fn shutdown(&self) {
        if let Some(pid) = self.pid.lock().unwrap().take() {
            kill_pid(pid);
        }
    }
}

fn retry_delay(attempts: u32) -> Duration {
    Duration::from_secs((1u64 << attempts.min(6)).min(60))
}

fn kill_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Spawns the helper from a dedicated thread that lives as long as the child: PR_SET_PDEATHSIG fires when the spawning
/// *thread* exits, so the helper dies with Olaz (even on a crash) but not with some short-lived runtime thread.
fn spawn_helper(helper: &Path, socket: &Path) -> Result<u32, String> {
    use std::io::BufRead;
    use std::os::unix::process::CommandExt;
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let helper = helper.to_path_buf();
    let socket = socket.to_path_buf();
    std::thread::Builder::new()
        .name("olaz-fp-helper".into())
        .spawn(move || {
            let mut cmd = std::process::Command::new(&helper);
            cmd.arg("-socket").arg(&socket).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("spawn {}: {e}", helper.display())));
                    return;
                }
            };
            let _ = tx.send(Ok(child.id()));
            let mut tail: Vec<String> = Vec::new();
            if let Some(stderr) = child.stderr.take() {
                for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                    tail.push(line);
                    if tail.len() > 6 {
                        tail.remove(0);
                    }
                }
            }
            let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
            let _ = std::fs::remove_file(&socket);
            olaz_core::log::write(format!("fingerprint: helper exited ({status}) {}", tail.join(" | ")));
        })
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split() {
        assert_eq!(split_command("REQ r1 {\"a\": 1}"), ("REQ", "r1", "{\"a\": 1}"));
        assert_eq!(split_command("END r1"), ("END", "r1", ""));
        assert_eq!(split_command("BYE"), ("BYE", "", ""));
    }

    #[test]
    fn request_json() {
        let mut r = Request::get("https://x/");
        r.headers = vec![("b".into(), "2".into()), ("a".into(), "1".into())];
        r.body = Some(b"hi".to_vec());
        let v: Value = serde_json::from_str(&r.to_json(true)).unwrap();
        assert_eq!(v["headers"], json!([["b", "2"], ["a", "1"]]));
        assert_eq!(v["bodyB64"], "aGk=");
        assert_eq!(v["stream"], true);
        assert_eq!(v["follow"], true);
    }

    #[test]
    fn errors() {
        assert!(Error::Unavailable("x".into()).falls_back() && Error::ConnectionLost("x".into()).falls_back());
        assert!(!Error::Failed("x".into()).falls_back());
        assert!(Error::Failed("request: context deadline exceeded".into()).is_timeout());
    }

    /// Live: needs the built helper and network (`OLAZ_FP_LIVE=1`).
    #[test]
    fn live_fingerprint() {
        if std::env::var_os("OLAZ_FP_LIVE").is_none() || helper_path().is_none() {
            return;
        }
        crate::net::runtime().block_on(async {
            let proxy = shared();
            assert!(proxy.ready(Duration::from_secs(5)).await);
            let mut r = Request::get("https://tls.peet.ws/api/clean");
            r.headers = vec![("User-Agent".into(), olaz_core::fingerprint::user_agent()), ("accept-encoding".into(), "gzip, deflate, br".into())];
            let (head, body) = proxy.perform(&r).await.unwrap();
            assert_eq!(head.status, 200);
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["ja4"], "t13d1516h2_8daaf6152771_e5627efa2ab1");
            assert_eq!(v["akamai"], "1:65536;2:0;3:1000;4:6291456;6:262144|15663105|0|m,a,s,p");
            // Streaming path.
            let StreamStart::Body(head, mut body) = proxy.stream(&Request::get("https://tls.peet.ws/api/clean")).await.unwrap() else { panic!() };
            assert_eq!(head.status, 200);
            let mut n = 0;
            while let Some(chunk) = body.next().await {
                n += chunk.unwrap().len();
            }
            assert!(n > 100);
            proxy.shutdown();
        });
    }
}
