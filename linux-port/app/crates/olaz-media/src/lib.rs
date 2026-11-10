//! MediaLoader: Zalo CDN media for the native UI — memory + disk cache, de-duplicated flights,
//! a concurrency cap, per-key failure backoff. No cookies (its session has none), UA `ZaloPC-darwin-23v694`.
//! Decoding happens in the UI (GPUI decodes the bytes); this crate deals in bytes and files.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, Shared};
use futures::FutureExt;
use olaz_core::fingerprint::Flavour;
use olaz_host::fingerprint::{Error as FpError, Request as FpRequest, StreamStart};

pub const MAX_CONCURRENT: usize = 6;
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(20);
pub const IMAGE_DEADLINE: Duration = Duration::from_secs(25);
pub const FILE_DEADLINE: Duration = Duration::from_secs(120);
pub const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 2 << 30;
const DOH_SUFFIXES: [&str; 7] = ["zadn.vn", "zdn.vn", "zalo.me", "zaloapp.com", "zingcdn.me", "zalo.vn", "zing.vn"];

#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    BadUrl,
    Http(u16),
    Timeout,
    Network(String),
    TooLarge,
    Backoff,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The headers Chromium 108 sends with the equivalent load (`<img>` → `Image`, renderer `fetch()` → `Cors`), plus the
/// User-Agent of the runtime in use; names in `except` are left to the caller.
fn with_client_headers(mut req: reqwest::RequestBuilder, flavour: Flavour, except: &[(String, String)]) -> reqwest::RequestBuilder {
    let set = |n: &str| except.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    if !set("User-Agent") {
        req = req.header(reqwest::header::USER_AGENT, olaz_core::fingerprint::user_agent());
    }
    for (name, value) in olaz_core::fingerprint::client_headers(flavour) {
        if !set(name) {
            req = req.header(name, value);
        }
    }
    req
}

/// `with_client_headers` as a list, for the fingerprint helper (which sends exactly these, in this order).
fn client_header_list(flavour: Flavour, own: &[(String, String)]) -> Vec<(String, String)> {
    let set = |n: &str| own.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    let mut out: Vec<(String, String)> = own.to_vec();
    if !set("User-Agent") {
        out.push(("User-Agent".into(), olaz_core::fingerprint::user_agent()));
    }
    for (name, value) in olaz_core::fingerprint::client_headers(flavour) {
        if !set(name) {
            out.push((name.to_string(), value));
        }
    }
    out
}

fn fp_error(e: FpError) -> LoadError {
    match e {
        FpError::TooLarge => LoadError::TooLarge,
        e if e.is_timeout() => LoadError::Timeout,
        e => LoadError::Network(e.to_string()),
    }
}

/// A GET through the fingerprint helper. `Ok(None)`: the helper cannot serve it — the caller uses reqwest. An
/// upstream failure is a real result (never retried on the other stack).
async fn helper_stream(request: FpRequest) -> Result<Option<StreamStart>, LoadError> {
    let proxy = olaz_host::fingerprint::shared();
    if !proxy.ready(Duration::from_secs(3)).await {
        return Ok(None);
    }
    match proxy.stream(&request).await {
        Ok(start) => Ok(Some(start)),
        Err(e) if e.falls_back() => {
            olaz_core::log::write(format!("fingerprint: {e} — media request uses the built-in stack"));
            Ok(None)
        }
        Err(e) => Err(fp_error(e)),
    }
}

/// A response body from either stack.
enum Body {
    Helper(olaz_host::fingerprint::BodyStream),
    Buffered(Option<Vec<u8>>),
    Reqwest(futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>),
}

impl Body {
    async fn next(&mut self) -> Option<Result<Vec<u8>, LoadError>> {
        use futures::StreamExt;
        match self {
            Body::Helper(s) => s.next().await.map(|r| r.map_err(fp_error)),
            Body::Buffered(b) => b.take().map(Ok),
            Body::Reqwest(s) => s.next().await.map(|r| r.map(|b| b.to_vec()).map_err(map_err)),
        }
    }
}

pub fn is_zalo_cdn(host: &str) -> bool {
    DOH_SUFFIXES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// FNV-1a 64 of the normalized URL, `String(hash, radix: 16)` (no zero padding) — file cache names.
pub fn fnv1a64(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:x}")
}

/// http(s) with a host only; http upgraded to https; `file://` passes through for allowed roots.
pub fn normalize(url: &str) -> Option<url::Url> {
    let mut u = url::Url::parse(url.trim()).ok()?;
    match u.scheme() {
        "https" => {}
        "http" => {
            u.set_scheme("https").ok()?;
        }
        "file" => return Some(u),
        _ => return None,
    }
    u.host_str().filter(|h| !h.is_empty())?;
    Some(u)
}

type Flight = Shared<BoxFuture<'static, Result<Arc<Vec<u8>>, LoadError>>>;

struct Memory {
    map: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl Memory {
    fn get(&mut self, k: &str) -> Option<Arc<Vec<u8>>> {
        let v = self.map.get(k).cloned()?;
        if let Some(pos) = self.order.iter().position(|x| x == k) {
            let key = self.order.remove(pos).unwrap();
            self.order.push_back(key);
        }
        Some(v)
    }
    fn put(&mut self, k: String, v: Arc<Vec<u8>>) {
        if v.len() > MEMORY_LIMIT / 4 {
            return;
        }
        if let Some(old) = self.map.insert(k.clone(), v.clone()) {
            self.bytes -= old.len();
            self.order.retain(|x| x != &k);
        }
        self.bytes += v.len();
        self.order.push_back(k);
        while self.bytes > MEMORY_LIMIT {
            let Some(oldest) = self.order.pop_front() else { break };
            if let Some(old) = self.map.remove(&oldest) {
                self.bytes -= old.len();
            }
        }
    }
}

struct Inner {
    memory: Memory,
    flights: HashMap<String, Flight>,
    backoff: HashMap<String, Instant>,
}

pub struct MediaLoader {
    client: reqwest::Client,
    ua: String,
    cache_dir: PathBuf,
    allowed_roots: Vec<PathBuf>,
    inner: Mutex<Inner>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl MediaLoader {
    pub fn new(cache_root: &Path, allowed_roots: Vec<PathBuf>, ua: &str) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .user_agent(ua)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .expect("media client");
        let cache_dir = cache_root.join("media");
        let _ = std::fs::create_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(cache_root.join("files"));
        Arc::new(Self {
            client,
            ua: ua.to_string(),
            cache_dir,
            allowed_roots,
            inner: Mutex::new(Inner {
                memory: Memory { map: HashMap::new(), order: VecDeque::new(), bytes: 0 },
                flights: HashMap::new(),
                backoff: HashMap::new(),
            }),
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)),
        })
    }

    /// Bytes from the memory cache only (synchronous probe at view construction).
    pub fn cached(&self, url: &str) -> Option<Arc<Vec<u8>>> {
        let key = normalize(url)?.to_string();
        let mut inner = self.inner.lock().unwrap();
        if let Some(hit) = inner.memory.get(&key) {
            return Some(hit);
        }
        drop(inner);
        let disk = self.cache_dir.join(format!("{}.bin", fnv1a64(&key)));
        let data = std::fs::read(disk).ok().map(Arc::new)?;
        self.inner.lock().unwrap().memory.put(key, data.clone());
        Some(data)
    }

    /// Explicit retry: drop the backoff of `url`.
    pub fn forget_failure(&self, url: &str) {
        if let Some(u) = normalize(url) {
            self.inner.lock().unwrap().backoff.remove(&u.to_string());
        }
    }

    /// Fetches `url` (memory → disk → network), de-duplicated, bounded by the deadline. Must be awaited on any
    /// executor; network I/O runs on the shared tokio runtime.
    pub fn data(self: &Arc<Self>, url: &str) -> BoxFuture<'static, Result<Arc<Vec<u8>>, LoadError>> {
        let Some(u) = normalize(url) else { return async { Err(LoadError::BadUrl) }.boxed() };
        if u.scheme() == "file" {
            let path = u.to_file_path().unwrap_or_default();
            let allowed = self.allowed_roots.iter().any(|r| path.starts_with(r));
            return async move {
                if !allowed {
                    return Err(LoadError::BadUrl);
                }
                std::fs::read(&path).map(Arc::new).map_err(|e| LoadError::Network(e.to_string()))
            }
            .boxed();
        }
        let key = u.to_string();
        if let Some(hit) = self.cached(&key) {
            return async move { Ok(hit) }.boxed();
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.backoff.get(&key).is_some_and(|until| *until > Instant::now()) {
            return async { Err(LoadError::Backoff) }.boxed();
        }
        if let Some(f) = inner.flights.get(&key) {
            return f.clone().boxed();
        }
        let this = self.clone();
        let k = key.clone();
        let fut: BoxFuture<'static, Result<Arc<Vec<u8>>, LoadError>> = async move {
            let handle = olaz_host::net::runtime().spawn({
                let this = this.clone();
                let k = k.clone();
                async move {
                    let _permit = this.slots.clone().acquire_owned().await.map_err(|_| LoadError::Network("closed".into()))?;
                    tokio::time::timeout(IMAGE_DEADLINE, this.fetch(&k)).await.map_err(|_| LoadError::Timeout)?
                }
            });
            let result = handle.await.map_err(|e| LoadError::Network(e.to_string())).and_then(|r| r);
            let mut inner = this.inner.lock().unwrap();
            inner.flights.remove(&k);
            match &result {
                Ok(data) => {
                    inner.memory.put(k.clone(), data.clone());
                    let path = this.cache_dir.join(format!("{}.bin", fnv1a64(&k)));
                    let _ = olaz_core::paths::write_atomic(&path, data);
                }
                Err(e) => {
                    inner.backoff.insert(k.clone(), Instant::now() + FAILURE_BACKOFF);
                    if inner.backoff.len() > 1000 {
                        let now = Instant::now();
                        inner.backoff.retain(|_, t| *t > now);
                    }
                    let host = url::Url::parse(&k).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                    olaz_core::log::write(format!("media fail kind=image host={host} class={}", class_of(e)));
                }
            }
            result
        }
        .boxed();
        let shared = fut.shared();
        inner.flights.insert(key, shared.clone());
        shared.boxed()
    }

    async fn fetch(&self, url: &str) -> Result<Arc<Vec<u8>>, LoadError> {
        // The fingerprint helper first: Chromium 108's TLS + HTTP/2 (an `<img>` load of the official renderer).
        let proxy = olaz_host::fingerprint::shared();
        if proxy.ready(Duration::from_secs(3)).await {
            let mut r = FpRequest::get(url);
            r.headers = client_header_list(Flavour::Image, &[]);
            r.timeout_ms = 10_000;
            match proxy.perform(&r).await {
                Ok((head, body)) if (200..300).contains(&head.status) => return Ok(Arc::new(body)),
                Ok((head, _)) => return Err(LoadError::Http(head.status)),
                Err(e) if e.falls_back() => olaz_core::log::write(format!("fingerprint: {e} — image uses the built-in stack")),
                Err(e) => return Err(fp_error(e)),
            }
        }
        let req = with_client_headers(self.client.get(url), Flavour::Image, &[]);
        let resp = req.timeout(Duration::from_secs(10)).send().await.map_err(map_err)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(LoadError::Http(status));
        }
        if resp.content_length().is_some_and(|l| l > MAX_FILE_BYTES) {
            return Err(LoadError::TooLarge);
        }
        let bytes = resp.bytes().await.map_err(map_err)?;
        Ok(Arc::new(bytes.to_vec()))
    }

    /// A local copy of `url` in the files cache (voice, files, videos), with progress 0…1.
    pub fn local_file(self: &Arc<Self>, url: &str, progress: impl Fn(f64) + Send + 'static) -> BoxFuture<'static, Result<PathBuf, LoadError>> {
        let Some(u) = normalize(url) else { return async { Err(LoadError::BadUrl) }.boxed() };
        if u.scheme() == "file" {
            return async move { u.to_file_path().map_err(|_| LoadError::BadUrl) }.boxed();
        }
        let ext: String = Path::new(u.path())
            .extension()
            .map(|e| e.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect())
            .unwrap_or_default();
        let name = if ext.is_empty() { fnv1a64(u.as_str()) } else { format!("{}.{}", fnv1a64(u.as_str()), ext) };
        let target = self.cache_dir.parent().unwrap().join("files").join(name);
        let client = self.client.clone();
        let url = u.to_string();
        async move {
            if target.is_file() {
                progress(1.0);
                return Ok(target);
            }
            let handle = olaz_host::net::runtime().spawn(async move {
                use futures::StreamExt;
                use std::io::Write;
                let mut r = FpRequest::get(&url);
                r.headers = client_header_list(Flavour::Cors, &[]);
                r.max_bytes = MAX_FILE_BYTES;
                r.idle_ms = 60_000;
                let (status, total, mut stream) = match helper_stream(r).await? {
                    Some(StreamStart::Body(head, body)) => (head.status, head.header("content-length").and_then(|l| l.parse::<u64>().ok()), Body::Helper(body)),
                    Some(StreamStart::Complete(head, body)) => (head.status, Some(body.len() as u64), Body::Buffered(Some(body))),
                    None => {
                        let resp = with_client_headers(client.get(&url), Flavour::Cors, &[]).send().await.map_err(map_err)?;
                        (resp.status().as_u16(), resp.content_length(), Body::Reqwest(resp.bytes_stream().boxed()))
                    }
                };
                if !(200..300).contains(&status) {
                    return Err(LoadError::Http(status));
                }
                let part = target.with_file_name(format!(".{}.part", uuid::Uuid::new_v4()));
                let mut f = std::fs::File::create(&part).map_err(|e| LoadError::Network(e.to_string()))?;
                let mut received = 0u64;
                while let Some(chunk) = stream.next().await {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = std::fs::remove_file(&part);
                            return Err(e);
                        }
                    };
                    received += chunk.len() as u64;
                    if received > MAX_FILE_BYTES {
                        let _ = std::fs::remove_file(&part);
                        return Err(LoadError::TooLarge);
                    }
                    f.write_all(&chunk).map_err(|e| LoadError::Network(e.to_string()))?;
                    if let Some(t) = total {
                        progress(received as f64 / t.max(1) as f64);
                    }
                }
                // The helper ends a stalled transfer like a finished one: a short body is a failure, not a file.
                if total.is_some_and(|t| received < t) {
                    let _ = std::fs::remove_file(&part);
                    return Err(LoadError::Network("transfer ended early".into()));
                }
                f.sync_all().map_err(|e| LoadError::Network(e.to_string()))?;
                std::fs::rename(&part, &target).map_err(|e| LoadError::Network(e.to_string()))?;
                Ok(target)
            });
            handle.await.map_err(|e| LoadError::Network(e.to_string()))?
        }
        .boxed()
    }
}

pub mod decode;
pub mod download;
pub mod player;
pub mod secure;

const FORBIDDEN_HEADERS: [&str; 11] =
    ["connection", "keep-alive", "proxy-authenticate", "te", "trailer", "transfer-encoding", "upgrade", "host", "content-length", "expect", "accept-encoding"];
const SENSITIVE_HEADERS: [&str; 4] = ["cloud-viewer-key", "cookie", "authorization", "proxy-authorization"];

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Header sanitizing for `downloadWithHeaders`: RFC 7230 tokens, no CR/LF/controls (TAB allowed),
/// hop-by-hop headers dropped, sensitive headers only to https Zalo hosts.
pub fn sanitize_headers(url: &url::Url, headers: &[(String, String)]) -> Vec<(String, String)> {
    let zalo_https = url.scheme() == "https" && url.host_str().is_some_and(is_zalo_cdn);
    headers
        .iter()
        .filter(|(n, v)| {
            let lower = n.to_lowercase();
            is_token(n)
                && !v.chars().any(|c| c.is_control() && c != '\t')
                && !FORBIDDEN_HEADERS.contains(&lower.as_str())
                && (zalo_https || !SENSITIVE_HEADERS.contains(&lower.as_str()))
        })
        .cloned()
        .collect()
}

impl MediaLoader {
    /// Zalo Cloud downloads (`cloud-viewer-key`): no URL cache, capped size, optional time-to-first-byte;
    /// sensitive headers dropped when a redirect changes host. Returns a temp file the caller owns.
    pub fn download_with_headers(
        self: &Arc<Self>,
        url: &str,
        headers: Vec<(String, String)>,
        max_bytes: u64,
        ttfb: Option<Duration>,
        temp_dir: &Path,
    ) -> BoxFuture<'static, Result<PathBuf, LoadError>> {
        let Some(u) = normalize(url).filter(|u| u.scheme() == "https") else { return async { Err(LoadError::BadUrl) }.boxed() };
        let temp_dir = temp_dir.to_path_buf();
        let ua = self.ua.clone();
        async move {
            let handle = olaz_host::net::runtime().spawn(async move {
                use futures::StreamExt;
                use std::io::Write;
                let original_host = u.host_str().unwrap_or("").to_string();
                let clean = sanitize_headers(&u, &headers);
                let sensitive = clean.iter().any(|(n, _)| SENSITIVE_HEADERS.contains(&n.to_lowercase().as_str()));
                let (status, total, mut stream) = match helper_download(&u, &clean, sensitive, ttfb).await? {
                    Some(found) => found,
                    None => {
                        let client = reqwest::Client::builder()
                            .user_agent(ua)
                            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                                if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" { attempt.stop() } else { attempt.follow() }
                            }))
                            .build()
                            .map_err(|e| LoadError::Network(e.to_string()))?;
                        let mut req = with_client_headers(client.get(u.clone()), Flavour::Cors, &clean);
                        for (n, v) in &clean {
                            req = req.header(n.as_str(), v.as_str());
                        }
                        let send = req.send();
                        let resp = match ttfb {
                            Some(t) => tokio::time::timeout(t, send).await.map_err(|_| LoadError::Timeout)?,
                            None => send.await,
                        }
                        .map_err(map_err)?;
                        // reqwest already dropped Authorization/Cookie on cross-host redirects; other sensitive headers
                        // (cloud-viewer-key) must not follow either: refuse such a redirect.
                        if resp.url().host_str() != Some(original_host.as_str()) && sensitive {
                            return Err(LoadError::Http(403));
                        }
                        (resp.status().as_u16(), resp.content_length(), Body::Reqwest(resp.bytes_stream().boxed()))
                    }
                };
                if !(200..300).contains(&status) {
                    return Err(LoadError::Http(status));
                }
                if total.is_some_and(|l| l > max_bytes) {
                    return Err(LoadError::TooLarge);
                }
                std::fs::create_dir_all(&temp_dir).map_err(|e| LoadError::Network(e.to_string()))?;
                let path = temp_dir.join(format!("{}.part", uuid::Uuid::new_v4()));
                let mut f = std::fs::File::create(&path).map_err(|e| LoadError::Network(e.to_string()))?;
                let mut received = 0u64;
                loop {
                    let next = tokio::time::timeout(Duration::from_secs(30), stream.next()).await;
                    let chunk = match next {
                        Err(_) => {
                            let _ = std::fs::remove_file(&path);
                            return Err(LoadError::Timeout);
                        }
                        Ok(None) => break,
                        Ok(Some(Ok(c))) => c,
                        Ok(Some(Err(e))) => {
                            let _ = std::fs::remove_file(&path);
                            return Err(e);
                        }
                    };
                    received += chunk.len() as u64;
                    if received > max_bytes {
                        let _ = std::fs::remove_file(&path);
                        return Err(LoadError::TooLarge);
                    }
                    f.write_all(&chunk).map_err(|e| LoadError::Network(e.to_string()))?;
                }
                if total.is_some_and(|t| received < t) {
                    let _ = std::fs::remove_file(&path);
                    return Err(LoadError::Network("transfer ended early".into()));
                }
                Ok(path)
            });
            handle.await.map_err(|e| LoadError::Network(e.to_string()))?
        }
        .boxed()
    }
}

/// `download_with_headers` through the fingerprint helper: redirects are followed here (https only, at most 5 hops, a
/// host change refused while sensitive headers ride along — the reqwest policy), so per-hop headers stay ours.
/// `Ok(None)`: the helper cannot serve it.
async fn helper_download(
    url: &url::Url,
    clean: &[(String, String)],
    sensitive: bool,
    ttfb: Option<Duration>,
) -> Result<Option<(u16, Option<u64>, Body)>, LoadError> {
    let original_host = url.host_str().unwrap_or("").to_string();
    let mut current = url.clone();
    for _ in 0..=5 {
        let mut r = FpRequest::get(current.as_str());
        r.headers = client_header_list(Flavour::Cors, clean);
        r.follow = false;
        // Our own 30 s idle check runs on the chunks; the helper's watchdog stays out of its way.
        r.idle_ms = 45_000;
        let start = match ttfb {
            Some(t) => {
                r.first_byte_ms = t.as_millis() as u64;
                tokio::time::timeout(t, helper_stream(r)).await.map_err(|_| LoadError::Timeout)??
            }
            None => helper_stream(r).await?,
        };
        let Some(start) = start else { return Ok(None) };
        let (head, body) = match start {
            StreamStart::Body(head, body) => (head, Body::Helper(body)),
            StreamStart::Complete(head, body) => (head, Body::Buffered(Some(body))),
        };
        let total = head.header("content-length").and_then(|l| l.parse::<u64>().ok());
        if (300..400).contains(&head.status) {
            let next = head.header("location").and_then(|l| current.join(l).ok());
            match next {
                Some(next) if next.scheme() == "https" => {
                    if next.host_str() != Some(original_host.as_str()) && sensitive {
                        return Err(LoadError::Http(403));
                    }
                    current = next;
                    continue;
                }
                // Not followed (like the reqwest policy's `stop`): the 3xx is the answer.
                _ => return Ok(Some((head.status, total, body))),
            }
        }
        return Ok(Some((head.status, total, body)));
    }
    Err(LoadError::Network("too many redirects".into()))
}

/// Runs blocking work on a thread and awaits it from any executor.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("blocking task")
}

fn map_err(e: reqwest::Error) -> LoadError {
    if e.is_timeout() { LoadError::Timeout } else { LoadError::Network(e.to_string()) }
}

/// MediaDiagnostics classes.
pub fn class_of(e: &LoadError) -> String {
    match e {
        LoadError::Http(s) => format!("http-{s}"),
        LoadError::Timeout => "timeout".into(),
        LoadError::TooLarge => "size".into(),
        LoadError::Network(m) if m.contains("dns") || m.contains("resolve") => "dns".into(),
        LoadError::Network(m) if m.contains("certificate") || m.contains("tls") => "tls".into(),
        _ => "other".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_and_normalize() {
        assert_eq!(fnv1a64(""), "cbf29ce484222325");
        assert_eq!(fnv1a64("a"), "af63dc4c8601ec8c");
        assert_eq!(normalize("http://s120-ava-talk.zadn.vn/a.jpg").unwrap().as_str(), "https://s120-ava-talk.zadn.vn/a.jpg");
        assert!(normalize("ftp://x/y").is_none());
        assert!(normalize("javascript:alert(1)").is_none());
        assert!(is_zalo_cdn("s120-ava-talk.zadn.vn") && !is_zalo_cdn("evilzadn.vn"));
    }

    #[test]
    fn header_sanitizing() {
        let h = vec![
            ("cloud-viewer-key".to_string(), "k".to_string()),
            ("Host".to_string(), "evil".to_string()),
            ("x-ok".to_string(), "a\tb".to_string()),
            ("bad name".to_string(), "v".to_string()),
            ("x-crlf".to_string(), "a\r\nb".to_string()),
        ];
        let zalo = url::Url::parse("https://cloud.zalo.me/f").unwrap();
        let names = |v: Vec<(String, String)>| v.into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(sanitize_headers(&zalo, &h)), vec!["cloud-viewer-key", "x-ok"]);
        let other = url::Url::parse("https://example.com/f").unwrap();
        assert_eq!(names(sanitize_headers(&other, &h)), vec!["x-ok"]);
    }

    #[test]
    fn memory_lru_evicts() {
        let mut m = Memory { map: HashMap::new(), order: VecDeque::new(), bytes: 0 };
        for i in 0..8 {
            m.put(format!("k{i}"), Arc::new(vec![0; MEMORY_LIMIT / 5]));
        }
        assert!(m.bytes <= MEMORY_LIMIT);
        assert!(m.get("k0").is_none() && m.get("k7").is_some());
    }

    #[test]
    fn file_urls_only_inside_roots_and_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("ok");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::write(allowed.join("a.png"), b"png").unwrap();
        std::fs::write(dir.path().join("secret"), b"s").unwrap();
        let l = MediaLoader::new(&dir.path().join("cache"), vec![allowed.clone()], "ua");
        let ok = futures::executor::block_on(l.data(&format!("file://{}", allowed.join("a.png").display())));
        assert_eq!(ok.unwrap().as_slice(), b"png");
        let bad = futures::executor::block_on(l.data(&format!("file://{}", dir.path().join("secret").display())));
        assert_eq!(bad, Err(LoadError::BadUrl));
        // a failing host enters the 20 s backoff
        let r = futures::executor::block_on(l.data("https://127.0.0.1:9/x.png"));
        assert!(matches!(r, Err(LoadError::Network(_)) | Err(LoadError::Timeout)));
        assert_eq!(futures::executor::block_on(l.data("https://127.0.0.1:9/x.png")), Err(LoadError::Backoff));
        l.forget_failure("https://127.0.0.1:9/x.png");
        assert_ne!(futures::executor::block_on(l.data("https://127.0.0.1:9/x.png")), Err(LoadError::Backoff));
    }
}
