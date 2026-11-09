//! DownloadManager — the main process's `new-download-channel`. Port of `DownloadManager.swift`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::{AbortHandle, Abortable};
use olaz_core::exec::spawn;
use olaz_core::value::{BV, interpolate};
use olaz_host::ZaloBridge;
use olaz_main::{BridgeModule, MainProcess, ZaloPlatform};

use crate::secure::{self, Parsed};
use crate::{LoadError, MediaLoader};

pub const CHANNEL: &str = "new-download-channel";
pub const RESPONSE: &str = "new-download-file-resp";
pub const PHOTO_BYTE_CAP: usize = 64 << 20;
const NOT_FOUND_CAPACITY: usize = 500;
const NOT_FOUND_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub name: String,
    pub code: i64,
    pub message: String,
}

impl Failure {
    fn new(name: &str, code: i64) -> Self {
        Self { name: name.into(), code, message: String::new() }
    }
    pub fn aborted() -> Self {
        Self::new("GENERAL_MAIN_CONTROLLER_OBJ_DOWNLOAD_ABORT", 1009)
    }
    pub fn invalid_url() -> Self {
        Self::new("GENERAL_UI_OBJ_DOWNLOAD_INVALID_URL", 9)
    }
    pub fn save_dir_invalid() -> Self {
        Self::new("GENERAL_MAIN_CONTROLLER_OBJ_DOWNLOAD_SAVE_DIR_INVALID", 1012)
    }
    pub fn empty_body() -> Self {
        Self::new("DOWNLOAD_SERVICE_SINGLE_INVALID_CONTENT_LENGTH", 80002)
    }
    pub fn ttfb_timeout() -> Self {
        Self::new("DOWNLOAD_SERVICE_SINGLE_MAX_TTFB_TIMEOUT", 80027)
    }
    pub fn decrypt_failed() -> Self {
        Self { name: "INS_OF_ERROR_ERR".into(), code: 14998, message: "File secure decrypt failed".into() }
    }
    pub fn too_large() -> Self {
        Self::new("DOWNLOAD_SERVICE_SINGLE_FILE_SIZE_LIMIT_EXCEED", 80026)
    }
    pub fn unknown() -> Self {
        Self::new("DOWNLOAD_SERVICE_SINGLE_DOWNLOAD_ON_EMPTY_ERR", 80025)
    }
    pub fn http(status: u16) -> Self {
        Self::new(&format!("DOWNLOAD_SERVICE_NET_{status}"), 75000 + status as i64)
    }
    pub fn net(name: &str, n: i64) -> Self {
        Self::new(name, 30000 + n)
    }

    /// MediaLoader errors → what the official main process reports.
    pub fn from(e: &LoadError) -> Self {
        match e {
            LoadError::Http(s) => Self::http(*s),
            LoadError::Timeout => Self::ttfb_timeout(),
            LoadError::BadUrl => Self::invalid_url(),
            LoadError::TooLarge => Self::too_large(),
            LoadError::Backoff => Self::unknown(),
            LoadError::Network(m) => {
                let m = m.to_lowercase();
                if m.contains("dns") || m.contains("resolve") || m.contains("lookup") {
                    Self::net("ERR_NAME_NOT_RESOLVED", 34)
                } else if m.contains("refused") {
                    Self::net("ERR_CONNECTION_REFUSED", 31)
                } else if m.contains("certificate") || m.contains("tls") || m.contains("ssl") {
                    Self::net("ERR_SSL_PROTOCOL_ERROR", 36)
                } else if m.contains("unreachable") || m.contains("network is down") {
                    Self::net("ERR_INTERNET_DISCONNECTED", 35)
                } else if m.contains("timed out") {
                    Self::net("ERR_CONNECTION_TIMED_OUT", 47)
                } else {
                    Self::net("ERR_CONNECTION_FAILED", 33)
                }
            }
        }
    }

    pub fn to_bv(&self) -> BV {
        let mut v = vec![("name", BV::String(self.name.clone())), ("code", BV::int(self.code)), ("normalize", BV::Bool(true))];
        if !self.message.is_empty() {
            v.push(("message", BV::String(self.message.clone())));
        }
        BV::obj(v)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub save_path: Option<String>,
    pub failure: Option<Failure>,
    pub cached: bool,
}

/// `saveDir/fileName`, the name reduced to one path component (NULs removed; empty/./.. → `no_name`).
pub fn destination(save_dir: Option<&str>, file_name: Option<&str>) -> Option<PathBuf> {
    let dir = save_dir.filter(|d| d.starts_with('/'))?;
    let mut name = Path::new(file_name.unwrap_or("")).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name = name.replace('\0', "");
    if name.is_empty() || name == "." || name == ".." {
        name = "no_name".into();
    }
    Some(Path::new(dir).join(name))
}

/// `options.headers` [{name, value}] + `extraHeader` (names only ever logged).
pub fn request_headers(request: &BV) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut set = |k: String, v: String| {
        out.retain(|(n, _)| n != &k);
        out.push((k, v));
    };
    if let Some(list) = request.get("options").and_then(|o| o.get("headers")).and_then(BV::as_array) {
        for e in list {
            if let (Some(n), Some(v)) = (e.get("name").and_then(BV::as_str), e.get("value").and_then(BV::as_str)) {
                if !n.is_empty() {
                    set(n.to_string(), v.to_string());
                }
            }
        }
    }
    if let Some(extra) = request.get("extraHeader").and_then(BV::as_object) {
        for (n, v) in extra {
            if let (false, Some(v)) = (n.is_empty(), v.as_str()) {
                set(n.clone(), v.to_string());
            }
        }
    }
    out
}

fn is_usable_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

pub struct DownloadManager {
    loader: Arc<MediaLoader>,
    platform: RefCell<Option<Rc<ZaloPlatform>>>,
    temp_dir: PathBuf,
    tasks: RefCell<HashMap<String, Vec<(u64, AbortHandle)>>>,
    not_found: RefCell<HashMap<String, Instant>>,
    next: std::cell::Cell<u64>,
}

impl DownloadManager {
    pub fn new(loader: Arc<MediaLoader>, temp_dir: PathBuf) -> Rc<Self> {
        remove_stale_temps(&temp_dir);
        Rc::new(Self {
            loader,
            platform: RefCell::new(None),
            temp_dir,
            tasks: RefCell::new(HashMap::new()),
            not_found: RefCell::new(HashMap::new()),
            next: std::cell::Cell::new(1),
        })
    }

    fn handle(self: &Rc<Self>, channel: &str, data: BV, bridge: &Rc<ZaloBridge>) {
        match channel {
            "new-download-file" => self.start(data, bridge),
            "new-download-file-abort" => {
                let Some(key) = data.get("key").map(interpolate) else { return };
                let from = data.get("downloadFrom").and_then(BV::as_str).unwrap_or("").to_string();
                if let Some(list) = self.tasks.borrow().get(&format!("{from}|{key}")) {
                    for (_, h) in list {
                        h.abort();
                    }
                }
            }
            _ => {}
        }
    }

    fn start(self: &Rc<Self>, request: BV, bridge: &Rc<ZaloBridge>) {
        let Some(key) = request.get("key").map(interpolate) else { return };
        let from = request.get("downloadFrom").and_then(BV::as_str).unwrap_or("").to_string();
        let kind = request.get("type").and_then(BV::as_str).unwrap_or("").to_string();
        let id = format!("{from}|{key}");
        let token = self.next.get();
        self.next.set(token + 1);
        let (handle, reg) = AbortHandle::new_pair();
        self.tasks.borrow_mut().entry(id.clone()).or_default().push((token, handle));
        let this = self.clone();
        let weak_bridge: Weak<ZaloBridge> = Rc::downgrade(bridge);
        let original_key = request.get("key").cloned().unwrap_or(BV::String(key.clone()));
        let fut = Abortable::new(this.clone().perform(request), reg);
        spawn(bridge.exec(), async move {
            let outcome = fut.await.unwrap_or(Outcome { failure: Some(Failure::aborted()), ..Default::default() });
            {
                let mut tasks = this.tasks.borrow_mut();
                if let Some(list) = tasks.get_mut(&id) {
                    list.retain(|(t, _)| *t != token);
                    if list.is_empty() {
                        tasks.remove(&id);
                    }
                }
            }
            let Some(bridge) = weak_bridge.upgrade() else { return };
            bridge.send(
                CHANNEL,
                vec![BV::obj([
                    ("channel", BV::str(RESPONSE)),
                    (
                        "data",
                        BV::obj([
                            ("downloadError", outcome.failure.as_ref().map(Failure::to_bv).unwrap_or(BV::Null)),
                            ("savePath", outcome.save_path.clone().map(BV::String).unwrap_or(BV::Null)),
                            ("downloadFrom", BV::String(from)),
                            ("key", original_key),
                            ("type", BV::String(kind)),
                            ("isCached", BV::Bool(outcome.cached)),
                        ]),
                    ),
                ])],
            );
        });
    }

    pub async fn perform(self: Rc<Self>, request: BV) -> Outcome {
        let fail = |f: Failure| Outcome { failure: Some(f), ..Default::default() };
        let data = request.get("data").cloned().unwrap_or_default();
        let options = request.get("options").cloned().unwrap_or_default();
        let kind = request.get("type").and_then(BV::as_str).unwrap_or("").to_string();
        let Some(url) = data.get("url").and_then(BV::as_str).map(str::to_string) else { return fail(Failure::invalid_url()) };
        let parsed = url::Url::parse(&url).ok().filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
        if parsed.is_none() {
            return fail(Failure::invalid_url());
        }
        let Some(dest) = destination(options.get("saveDir").and_then(BV::as_str), data.get("fileName").and_then(BV::as_str)) else {
            return fail(Failure::save_dir_invalid());
        };
        let writable = self.platform.borrow().as_ref().is_some_and(|p| p.is_writable(&dest.parent().unwrap_or(Path::new("/")).to_string_lossy()));
        if !writable {
            return fail(Failure::save_dir_invalid());
        }
        let manual = options.get("srcAction").and_then(BV::as_str) == Some("Manual");
        let headers = request_headers(&request);
        let secure = secure::parse(Some(&request.get("secure").map(BV::to_plain_json).unwrap_or(serde_json::Value::Null)));
        let host = parsed.as_ref().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        olaz_core::log::write(format!(
            "download type={kind} host={host} src={} headers={} secure={}",
            options.get("srcAction").and_then(BV::as_str).unwrap_or("-"),
            headers.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(","),
            match secure {
                Parsed::None => "no",
                Parsed::Stream(_) => "yes",
                Parsed::Invalid => "invalid",
            }
        ));
        if is_usable_file(&dest) {
            return Outcome { save_path: Some(dest.to_string_lossy().into_owned()), cached: true, failure: None };
        }
        if secure == Parsed::Invalid {
            return fail(Failure::decrypt_failed());
        }
        if !manual {
            let until = self.not_found.borrow().get(&url).copied();
            if let Some(until) = until {
                if until > Instant::now() {
                    return fail(Failure::http(404));
                }
                self.not_found.borrow_mut().remove(&url);
            }
        }
        let ttfb = options.get("maxTTFB").and_then(BV::as_f64).unwrap_or(0.0);
        let is_photo = kind.starts_with("Photo");
        let fetched: Result<Fetched, LoadError> = if headers.is_empty() && is_photo {
            self.loader.data(&url).await.map(|b| Fetched::Bytes(b.to_vec()))
        } else if headers.is_empty() {
            self.loader.local_file(&url, |_| {}).await.map(|p| Fetched::File(p, false))
        } else {
            let max = if is_photo { PHOTO_BYTE_CAP as u64 } else { crate::MAX_FILE_BYTES };
            let ttfb = (ttfb > 0.0).then(|| Duration::from_secs_f64(ttfb / 1000.0));
            self.loader.download_with_headers(&url, headers, max, ttfb, &self.temp_dir).await.map(|p| Fetched::File(p, true))
        };
        let outcome = match fetched {
            Ok(f) => {
                let temp = self.temp_dir.clone();
                let dest2 = dest.clone();
                crate::blocking(move || finalize(&dest2, &temp, f, secure)).await
            }
            Err(e) => fail(Failure::from(&e)),
        };
        if outcome.failure.as_ref().is_some_and(|f| f.code == 75404) {
            self.remember_not_found(&url);
        }
        outcome
    }

    fn remember_not_found(&self, url: &str) {
        let mut nf = self.not_found.borrow_mut();
        let now = Instant::now();
        if nf.len() >= NOT_FOUND_CAPACITY {
            nf.retain(|_, t| *t > now);
            if nf.len() >= NOT_FOUND_CAPACITY {
                let mut by_age: Vec<(String, Instant)> = nf.iter().map(|(k, v)| (k.clone(), *v)).collect();
                by_age.sort_by_key(|(_, t)| *t);
                for (k, _) in by_age.into_iter().take(NOT_FOUND_CAPACITY / 4) {
                    nf.remove(&k);
                }
            }
        }
        nf.insert(url.to_string(), now + NOT_FOUND_TTL);
    }

    pub fn not_found_count(&self) -> usize {
        self.not_found.borrow().len()
    }

    pub fn set_platform(&self, platform: Rc<ZaloPlatform>) {
        *self.platform.borrow_mut() = Some(platform);
    }
}

pub enum Fetched {
    Bytes(Vec<u8>),
    /// `ours`: a temporary file this download owns (deleted afterwards); else a shared cache file.
    File(PathBuf, bool),
}

/// Decrypt (when asked), stage in the temp dir, then publish atomically at `dest`. Temporaries always removed.
pub fn finalize(dest: &Path, temp_dir: &Path, fetched: Fetched, secure: Parsed) -> Outcome {
    let fail = |f: Failure| Outcome { failure: Some(f), ..Default::default() };
    let _ = std::fs::create_dir_all(temp_dir);
    if let Some(parent) = dest.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return fail(Failure::save_dir_invalid());
        }
    }
    let staged = temp_dir.join(format!("{}.part", uuid::Uuid::new_v4()));
    let mut cleanup = vec![staged.clone()];
    let stream = match secure {
        Parsed::Stream(s) => Some(s),
        Parsed::Invalid => {
            if let Fetched::File(p, true) = &fetched {
                let _ = std::fs::remove_file(p);
            }
            return fail(Failure::decrypt_failed());
        }
        Parsed::None => None,
    };
    let result = (|| -> Result<(), Failure> {
        match fetched {
            Fetched::Bytes(bytes) => {
                if bytes.is_empty() {
                    return Err(Failure::empty_body());
                }
                if bytes.len() > PHOTO_BYTE_CAP {
                    return Err(Failure::too_large());
                }
                let out = match &stream {
                    Some(s) => s.decrypt(&bytes).map_err(|_| Failure::decrypt_failed())?,
                    None => bytes,
                };
                std::fs::write(&staged, out).map_err(|_| Failure::unknown())?;
            }
            Fetched::File(raw, ours) => {
                if ours {
                    cleanup.push(raw.clone());
                }
                match &stream {
                    Some(s) => s.decrypt_file(&raw, &staged).map_err(|_| Failure::decrypt_failed())?,
                    None if ours => std::fs::rename(&raw, &staged).map_err(|_| Failure::unknown())?,
                    None => {
                        std::fs::copy(&raw, &staged).map_err(|_| Failure::unknown())?;
                    }
                }
            }
        }
        if std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err(Failure::empty_body());
        }
        std::fs::rename(&staged, dest).or_else(|_| std::fs::copy(&staged, dest).map(|_| ())).map_err(|_| Failure::unknown())
    })();
    for p in cleanup {
        let _ = std::fs::remove_file(p);
    }
    match result {
        Ok(()) => Outcome { save_path: Some(dest.to_string_lossy().into_owned()), ..Default::default() },
        Err(f) => fail(f),
    }
}

fn remove_stale_temps(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let stale = e.metadata().and_then(|m| m.modified()).map(|t| t.elapsed().map(|d| d > Duration::from_secs(600)).unwrap_or(false)).unwrap_or(true);
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The download manager as a main-process module.
pub struct DownloadModule(pub Rc<DownloadManager>);

impl BridgeModule for DownloadModule {
    fn install(&self, bridge: &Rc<ZaloBridge>, main: &Rc<MainProcess>) {
        self.0.set_platform(main.platform.clone());
        let me = Rc::downgrade(&self.0);
        let wb = Rc::downgrade(bridge);
        bridge.handle_now(CHANNEL, move |args| {
            let (Some(me), Some(bridge)) = (me.upgrade(), wb.upgrade()) else { return Ok(BV::Undefined) };
            let Some(req) = args.first().filter(|r| r.as_object().is_some()) else { return Ok(BV::Undefined) };
            let Some(channel) = req.get("channel").and_then(BV::as_str).map(str::to_string) else { return Ok(BV::Undefined) };
            let data = req.get("data").cloned().unwrap_or(BV::Object(Default::default()));
            me.handle(&channel, data, &bridge);
            Ok(BV::Undefined)
        });
    }

    /// Logout: cancel everything, forget verdicts.
    fn reset(&self) {
        for list in self.0.tasks.borrow_mut().drain().map(|(_, l)| l) {
            for (_, h) in list {
                h.abort();
            }
        }
        self.0.not_found.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(destination(Some("/d"), Some("../../etc/passwd")), Some(PathBuf::from("/d/passwd")));
        assert_eq!(destination(Some("/d"), Some("")), Some(PathBuf::from("/d/no_name")));
        assert_eq!(destination(Some("/d"), Some("..")), Some(PathBuf::from("/d/no_name")));
        assert_eq!(destination(Some("rel"), Some("a")), None);
        assert_eq!(destination(None, Some("a")), None);
    }

    #[test]
    fn error_table() {
        assert_eq!(Failure::http(404).code, 75404);
        assert_eq!(Failure::http(404).name, "DOWNLOAD_SERVICE_NET_404");
        assert_eq!(Failure::from(&LoadError::Timeout).code, 80027);
        assert_eq!(Failure::from(&LoadError::BadUrl).code, 9);
        assert_eq!(Failure::from(&LoadError::TooLarge).code, 80026);
        assert_eq!(Failure::from(&LoadError::Network("dns error: failed to lookup".into())).name, "ERR_NAME_NOT_RESOLVED");
        assert_eq!(Failure::from(&LoadError::Network("Connection refused".into())).code, 30031);
        assert_eq!(Failure::from(&LoadError::Network("invalid peer certificate".into())).code, 30036);
        assert_eq!(Failure::from(&LoadError::Network("weird".into())).code, 30033);
        assert_eq!(Failure::decrypt_failed().to_bv().get("message"), Some(&BV::str("File secure decrypt failed")));
        assert_eq!(Failure::aborted().code, 1009);
        assert_eq!(Failure::save_dir_invalid().code, 1012);
        assert_eq!(Failure::empty_body().code, 80002);
        assert_eq!(Failure::unknown().code, 80025);
    }

    #[test]
    fn headers_merge() {
        let req = BV::from_js(serde_json::json!({"options": {"headers": [{"name": "cloud-viewer-key", "value": "k"}, {"name": "", "value": "x"}]}, "extraHeader": {"x-a": "1", "x-b": 2}}));
        let h = request_headers(&req);
        assert_eq!(h, vec![("cloud-viewer-key".to_string(), "k".to_string()), ("x-a".to_string(), "1".to_string())]);
    }

    #[test]
    fn finalize_publishes_atomically_and_never_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out/a.jpg");
        let tmp = dir.path().join("tmp");
        let o = finalize(&dest, &tmp, Fetched::Bytes(b"img".to_vec()), Parsed::None);
        assert_eq!(o.save_path.as_deref(), Some(dest.to_str().unwrap()));
        assert_eq!(std::fs::read(&dest).unwrap(), b"img");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
        let o = finalize(&dir.path().join("e.jpg"), &tmp, Fetched::Bytes(vec![]), Parsed::None);
        assert_eq!(o.failure, Some(Failure::empty_body()));
        let raw = dir.path().join("raw.bin");
        std::fs::write(&raw, b"cipher").unwrap();
        let o = finalize(&dir.path().join("c.bin"), &tmp, Fetched::File(raw.clone(), true), Parsed::Invalid);
        assert_eq!(o.failure, Some(Failure::decrypt_failed()));
        assert!(!raw.exists() && !dir.path().join("c.bin").exists());
        // shared cache file: copied, never deleted
        let shared = dir.path().join("shared.bin");
        std::fs::write(&shared, b"s").unwrap();
        let o = finalize(&dir.path().join("s.bin"), &tmp, Fetched::File(shared.clone(), false), Parsed::None);
        assert!(o.failure.is_none() && shared.exists());
    }
}
