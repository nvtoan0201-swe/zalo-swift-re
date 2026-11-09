//! App side of the renderer: the `olaz-webhost` process (IPC) and one `ClientHost` per view —
//! the port of `OfficialClientHost`. Every frame is dispatched on the executor, in order.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::{mpsc as fmpsc, oneshot};
use olaz_core::cookies::CookieJar;
use olaz_core::exec::{Exec, LocalBoxFuture, spawn};
use olaz_ipc::{AppToHost, HostToApp};
use serde_json::{Map, Value, json};

use crate::bridge::{PageSink, Replier, ZaloBridge};
use crate::net::NetworkProxy;

pub const SCHEME: &str = "zalo-app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Login,
    Main,
    SharedWorker,
}

impl Page {
    pub fn file(self) -> &'static str {
        match self {
            Page::Login => "login.html",
            Page::Main => "index.html",
            Page::SharedWorker => "shared-worker.html",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageEvent {
    StartedLoading,
    FinishedLoading(Page),
    Failed(String),
}

/// `zalo-app://local/<page>?<query sorted by key>`.
pub fn page_url(page: Page, query: &BTreeMap<String, String>) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    // URLComponents.queryItems: encodes what is not allowed in a query (plus `&`, `=`, `+` inside items).
    const QUERY: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'<').add(b'>').add(b'&').add(b'=').add(b'+').add(b'%');
    let q: Vec<String> =
        query.iter().map(|(k, v)| format!("{}={}", utf8_percent_encode(k, QUERY), utf8_percent_encode(v, QUERY))).collect();
    if q.is_empty() { format!("{SCHEME}://local/{}", page.file()) } else { format!("{SCHEME}://local/{}?{}", page.file(), q.join("&")) }
}

// MARK: - the webhost process

/// Where the webhost lives: `$OLAZ_WEBHOST_BIN`, else next to the current executable.
pub fn webhost_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("OLAZ_WEBHOST_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(Path::new("."));
    for candidate in [dir.join("olaz-webhost"), dir.join("../olaz-webhost")] {
        if candidate.is_file() {
            return candidate;
        }
    }
    dir.join("olaz-webhost")
}

#[derive(Clone)]
pub struct WebHostConfig {
    pub binary: PathBuf,
    pub asar: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub debug: bool,
}

struct ViewState {
    host: Weak<ClientHost>,
}

pub struct WebHostProcess {
    config: WebHostConfig,
    exec: Exec,
    writer: RefCell<std::sync::mpsc::Sender<AppToHost>>,
    child: RefCell<Option<Child>>,
    views: RefCell<HashMap<u32, ViewState>>,
    evals: RefCell<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    next_view: Cell<u32>,
    next_token: Cell<u64>,
    generation: Cell<u64>,
    pub webkit_version: RefCell<Option<String>>,
    /// http(s) links the page tried to open: the app opens them in the system browser.
    pub on_open_external: RefCell<Option<Rc<dyn Fn(String)>>>,
    /// Host diagnostics (`[404] <path>`, patch notes, crashes).
    pub diagnostics: RefCell<Option<Rc<dyn Fn(String)>>>,
    pub hello: RefCell<Option<oneshot::Sender<()>>>,
}

impl WebHostProcess {
    pub fn spawn(config: WebHostConfig, exec: Exec) -> std::io::Result<Rc<Self>> {
        let (tx, _rx) = std::sync::mpsc::channel();
        let process = Rc::new(Self {
            config,
            exec,
            writer: RefCell::new(tx),
            child: RefCell::new(None),
            views: RefCell::new(HashMap::new()),
            evals: RefCell::new(HashMap::new()),
            next_view: Cell::new(1),
            next_token: Cell::new(1),
            generation: Cell::new(0),
            webkit_version: RefCell::new(None),
            on_open_external: RefCell::new(None),
            diagnostics: RefCell::new(None),
            hello: RefCell::new(None),
        });
        process.start()?;
        Ok(process)
    }

    /// A process handle that never spawns (registry dumps, tests): frames go nowhere.
    pub fn detached(config: WebHostConfig, exec: Exec) -> Rc<Self> {
        let (tx, _rx) = std::sync::mpsc::channel();
        Rc::new(Self {
            config,
            exec,
            writer: RefCell::new(tx),
            child: RefCell::new(None),
            views: RefCell::new(HashMap::new()),
            evals: RefCell::new(HashMap::new()),
            next_view: Cell::new(1),
            next_token: Cell::new(1),
            generation: Cell::new(0),
            webkit_version: RefCell::new(None),
            on_open_external: RefCell::new(None),
            diagnostics: RefCell::new(None),
            hello: RefCell::new(None),
        })
    }

    fn start(self: &Rc<Self>) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        // The child end must survive exec.
        unsafe {
            let fd = theirs.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFD);
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("--fd")
            .arg(theirs.as_raw_fd().to_string())
            .arg("--asar")
            .arg(&self.config.asar)
            .arg("--data")
            .arg(&self.config.data_dir)
            .arg("--cache")
            .arg(&self.config.cache_dir);
        if self.config.debug {
            cmd.arg("--debug");
        }
        let child = cmd.spawn()?;
        drop(theirs);
        *self.child.borrow_mut() = Some(child);
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        // Writer thread (frames leave in call order).
        let (tx, rx) = std::sync::mpsc::channel::<AppToHost>();
        let mut writer = ours.try_clone()?;
        std::thread::Builder::new().name("olaz-ipc-w".into()).spawn(move || {
            for frame in rx {
                if olaz_ipc::write_frame(&mut writer, &frame).is_err() {
                    break;
                }
            }
        })?;
        *self.writer.borrow_mut() = tx;

        // Reader thread → executor, strictly in order.
        let (ftx, mut frx) = fmpsc::unbounded::<Option<HostToApp>>();
        let mut reader = ours;
        std::thread::Builder::new().name("olaz-ipc-r".into()).spawn(move || {
            loop {
                match olaz_ipc::read_frame::<_, HostToApp>(&mut reader) {
                    Ok(Some(f)) => {
                        if ftx.unbounded_send(Some(f)).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = ftx.unbounded_send(None);
                        break;
                    }
                }
            }
        })?;
        let weak = Rc::downgrade(self);
        spawn(&self.exec, async move {
            while let Some(frame) = frx.next().await {
                let Some(this) = weak.upgrade() else { break };
                if this.generation.get() != generation {
                    break;
                }
                match frame {
                    Some(f) => this.dispatch(f),
                    None => {
                        this.died();
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// The webhost exited: restart it and reload every page (`webContentProcessDidTerminate`).
    fn died(self: &Rc<Self>) {
        if let Some(mut c) = self.child.borrow_mut().take() {
            let _ = c.wait();
        }
        for (_, tx) in self.evals.borrow_mut().drain() {
            let _ = tx.send(Err("webhost exited".into()));
        }
        let views: Vec<Rc<ClientHost>> = self.views.borrow().values().filter_map(|v| v.host.upgrade()).collect();
        if views.is_empty() {
            return;
        }
        self.diag("[crashed] webhost exited — restarting".into());
        if self.start().is_ok() {
            for v in views {
                v.recreate();
            }
        }
    }

    fn diag(&self, line: String) {
        olaz_core::log::write(format!("host: {line}"));
        let d = self.diagnostics.borrow().clone();
        if let Some(d) = d {
            d(line);
        }
    }

    pub fn send(&self, frame: AppToHost) {
        let _ = self.writer.borrow().send(frame);
    }

    fn allocate_view(&self) -> u32 {
        let v = self.next_view.get();
        self.next_view.set(v + 1);
        v
    }

    fn register_view(&self, view: u32, host: &Rc<ClientHost>) {
        self.views.borrow_mut().insert(view, ViewState { host: Rc::downgrade(host) });
    }

    pub fn eval(&self, view: u32, body: &str, args: Option<Value>) -> LocalBoxFuture<Result<Value, String>> {
        let token = self.next_token.get();
        self.next_token.set(token + 1);
        let (tx, rx) = oneshot::channel();
        self.evals.borrow_mut().insert(token, tx);
        self.send(AppToHost::Eval { view, token, body: body.to_string(), args });
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err("cancelled".into())) })
    }

    fn view_host(&self, view: u32) -> Option<Rc<ClientHost>> {
        self.views.borrow().get(&view).and_then(|v| v.host.upgrade())
    }

    fn dispatch(self: &Rc<Self>, frame: HostToApp) {
        match frame {
            HostToApp::Hello { build, webkit, platform } => {
                if build != olaz_ipc::BUILD_ID {
                    self.diag(format!("webhost build mismatch: {build} != {}", olaz_ipc::BUILD_ID));
                }
                olaz_core::log::write(format!("host: webhost ready — WebKit{} {webkit}", if platform == "gtk" { "GTK" } else { " WPE" }));
                *self.webkit_version.borrow_mut() = Some(webkit);
                if let Some(tx) = self.hello.borrow_mut().take() {
                    let _ = tx.send(());
                }
            }
            HostToApp::Msg { view, handler, body } => {
                let Some(host) = self.view_host(view) else { return };
                match handler.as_str() {
                    "zlog" => host.console(match body {
                        Value::String(s) => s,
                        other => other.to_string(),
                    }),
                    "zstate" => {
                        if let Value::Object(map) = body {
                            host.state(map);
                        }
                    }
                    _ => {}
                }
            }
            HostToApp::Request { view, handler, id, body } => {
                let process = Rc::downgrade(self);
                let replier: Replier = Box::new(move |value, error| {
                    if let Some(p) = process.upgrade() {
                        p.send(AppToHost::Reply { view, id, value, error });
                    }
                });
                let Some(host) = self.view_host(view) else { return replier(None, Some("no host".into())) };
                match handler.as_str() {
                    "znet" => host.network.handle(body, replier),
                    "zbridge" => host.bridge.handle_message(body, replier),
                    _ => replier(None, Some("unknown handler".into())),
                }
            }
            HostToApp::Sync { view, id, body } => {
                let out = match self.view_host(view) {
                    Some(host) => host.bridge.handle_sync_request(body.as_bytes()),
                    None => b"{}".to_vec(),
                };
                self.send(AppToHost::SyncReply { view, id, body: String::from_utf8_lossy(&out).into_owned() });
            }
            HostToApp::EvalResult { token, value, error, .. } => {
                if let Some(tx) = self.evals.borrow_mut().remove(&token) {
                    let _ = tx.send(match error {
                        Some(e) => Err(e),
                        None => Ok(value.unwrap_or(Value::Null)),
                    });
                }
            }
            HostToApp::Page { view, event, error, url: _ } => {
                if let Some(host) = self.view_host(view) {
                    host.page_event(&event, error);
                }
            }
            HostToApp::Chooser { view, taken, .. } => {
                if let Some(host) = self.view_host(view) {
                    host.chooser_answered(taken);
                }
            }
            HostToApp::OpenExternal { url, .. } => {
                let h = self.on_open_external.borrow().clone();
                match h {
                    Some(h) => h(url),
                    None => {
                        let _ = Command::new("xdg-open").arg(url).spawn();
                    }
                }
            }
            HostToApp::NotFound { path, .. } => {
                olaz_core::log::write(format!("host: 404 {path}"));
                self.diag(format!("[404] {path}"));
            }
            HostToApp::Log { line } => {
                olaz_core::log::write(&line);
                self.diag(line);
            }
            HostToApp::Crashed { view, reason } => {
                self.diag(format!("[crashed] view {view}: {reason} — reloading"));
            }
        }
    }

    pub fn quit(&self) {
        self.send(AppToHost::Quit);
        if let Some(mut c) = self.child.borrow_mut().take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = c.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.child.borrow().as_ref().map(Child::id)
    }
}

impl Drop for WebHostProcess {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.get_mut().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// MARK: - one renderer window

type StateSink = Rc<dyn Fn(Map<String, Value>)>;
type ConsoleSink = Rc<dyn Fn(String)>;
type PageSinkFn = Rc<dyn Fn(PageEvent)>;

pub struct ClientHost {
    pub view: Cell<u32>,
    process: Rc<WebHostProcess>,
    pub bridge: Rc<ZaloBridge>,
    pub network: Rc<NetworkProxy>,
    ua: String,
    scripts: RefCell<Vec<String>>,
    current_page: Cell<Option<Page>>,
    current_url: RefCell<Option<String>>,
    pub on_state: RefCell<Option<StateSink>>,
    pub on_console: RefCell<Option<ConsoleSink>>,
    pub on_page_event: RefCell<Option<PageSinkFn>>,
    // file pick protocol
    pick_busy: Cell<bool>,
    pick_waiters: RefCell<VecDeque<oneshot::Sender<()>>>,
    pending_pick: Cell<Option<u64>>,
    finished_picks: RefCell<HashSet<u64>>,
    next_pick: Cell<u64>,
    shut: Cell<bool>,
}

impl ClientHost {
    /// `init(install:userAgent:configureBridge:)`: the bridge is configured **before** the spec script.
    pub fn new(
        process: &Rc<WebHostProcess>,
        ua: &str,
        jar: Arc<CookieJar>,
        trace_workers: bool,
        configure_bridge: impl FnOnce(&Rc<ZaloBridge>),
    ) -> Rc<Self> {
        let exec = process.exec.clone();
        let network = NetworkProxy::new(ua, jar, exec.clone());
        let bridge = ZaloBridge::new(exec);
        configure_bridge(&bridge);
        let scripts = olaz_core::scripts::injection_order(&bridge.spec_script(), trace_workers);
        let view = process.allocate_view();
        let host = Rc::new(Self {
            view: Cell::new(view),
            process: process.clone(),
            bridge,
            network,
            ua: ua.to_string(),
            scripts: RefCell::new(scripts),
            current_page: Cell::new(None),
            current_url: RefCell::new(None),
            on_state: RefCell::new(None),
            on_console: RefCell::new(None),
            on_page_event: RefCell::new(None),
            pick_busy: Cell::new(false),
            pick_waiters: RefCell::new(VecDeque::new()),
            pending_pick: Cell::new(None),
            finished_picks: RefCell::new(HashSet::new()),
            next_pick: Cell::new(1),
            shut: Cell::new(false),
        });
        let sink: Rc<dyn PageSink> = host.clone();
        host.bridge.set_page(Rc::downgrade(&sink));
        host.network.set_page(Rc::downgrade(&sink));
        process.register_view(view, &host);
        process.send(AppToHost::Create { view, ua: ua.to_string(), scripts: host.scripts.borrow().clone(), debug: false });
        host
    }

    fn recreate(self: &Rc<Self>) {
        if self.shut.get() {
            return;
        }
        let view = self.process.allocate_view();
        self.view.set(view);
        self.process.register_view(view, self);
        self.process.send(AppToHost::Create { view, ua: self.ua.clone(), scripts: self.scripts.borrow().clone(), debug: false });
        if let Some(url) = self.current_url.borrow().clone() {
            self.process.send(AppToHost::Load { view, url });
        }
    }

    pub fn process(&self) -> &Rc<WebHostProcess> {
        &self.process
    }

    pub fn current_page(&self) -> Option<Page> {
        self.current_page.get()
    }

    pub fn load(&self, page: Page, query: &BTreeMap<String, String>) {
        self.current_page.set(Some(page));
        let url = page_url(page, query);
        olaz_core::log::write(format!("host: loading {}", page.file()));
        *self.current_url.borrow_mut() = Some(url.clone());
        self.process.send(AppToHost::Load { view: self.view.get(), url });
    }

    /// `callAsyncJavaScript` in the page world; `None` on failure (like `try?`).
    pub fn evaluate(&self, script: &str) -> LocalBoxFuture<Option<Value>> {
        let fut = self.process.eval(self.view.get(), script, None);
        Box::pin(async move { fut.await.ok() })
    }

    pub fn evaluate_result(&self, script: &str, args: Option<Value>) -> LocalBoxFuture<Result<Value, String>> {
        self.process.eval(self.view.get(), script, args)
    }

    /// Runs `script`, answering the file chooser it opens with `files`.
    pub fn evaluate_providing_files(self: &Rc<Self>, script: &str, files: Vec<PathBuf>) -> LocalBoxFuture<Option<Value>> {
        let this = self.clone();
        let script = script.to_string();
        Box::pin(async move {
            while this.pick_busy.get() {
                let (tx, rx) = oneshot::channel();
                this.pick_waiters.borrow_mut().push_back(tx);
                let _ = rx.await;
            }
            this.pick_busy.set(true);
            let id = this.next_pick.get();
            this.next_pick.set(id + 1);
            this.pending_pick.set(Some(id));
            this.process.send(AppToHost::Pick {
                view: this.view.get(),
                files: Some(files.iter().map(|p| p.to_string_lossy().into_owned()).collect()),
            });
            let run = {
                let this2 = this.clone();
                olaz_core::exec::spawn_with_result(&this.process.exec, async move {
                    let value = this2.evaluate(&script).await;
                    this2.finished_picks.borrow_mut().insert(id);
                    value
                })
            };
            let deadline = Instant::now() + Duration::from_secs(15);
            while this.pending_pick.get() == Some(id) && !this.finished_picks.borrow().contains(&id) && Instant::now() < deadline {
                this.process.exec.timer(Duration::from_millis(20)).await;
            }
            if this.pending_pick.get() == Some(id) {
                this.pending_pick.set(None);
                this.process.send(AppToHost::Pick { view: this.view.get(), files: None });
            }
            this.pick_busy.set(false);
            if let Some(w) = this.pick_waiters.borrow_mut().pop_front() {
                let _ = w.send(());
            }
            let result = run.await.ok().flatten();
            this.finished_picks.borrow_mut().remove(&id);
            result
        })
    }

    fn chooser_answered(&self, _taken: usize) {
        // The webhost took (or cancelled) the pending pick: it is single use.
        self.pending_pick.set(None);
    }

    /// `navigator.connection` polyfill update.
    pub fn connection_changed(&self, info: Value) {
        self.call("window.__zaloNet && window.__zaloNet.connectionChanged(info)", json!({"info": info}));
    }

    pub fn shutdown(&self) {
        self.shut.set(true);
        self.network.close_all_sockets();
        self.bridge.clear_page();
        self.process.send(AppToHost::Shutdown { view: self.view.get() });
        self.process.views.borrow_mut().remove(&self.view.get());
    }

    pub fn show_debug_window(&self) {
        self.process.send(AppToHost::Debug { view: self.view.get() });
    }

    fn console(&self, line: String) {
        let sink = self.on_console.borrow().clone();
        if let Some(s) = sink {
            s(line);
        }
    }

    fn state(&self, map: Map<String, Value>) {
        let sink = self.on_state.borrow().clone();
        if let Some(s) = sink {
            s(map);
        }
    }

    fn page_event(&self, event: &str, error: Option<String>) {
        let ev = match event {
            "started" => PageEvent::StartedLoading,
            "finished" => match self.current_page.get() {
                Some(p) => PageEvent::FinishedLoading(p),
                None => return,
            },
            "failed" => PageEvent::Failed(error.unwrap_or_default()),
            _ => return,
        };
        let sink = self.on_page_event.borrow().clone();
        if let Some(s) = sink {
            s(ev);
        }
    }
}

impl PageSink for ClientHost {
    fn call(&self, body: &str, args: Value) {
        if self.shut.get() {
            return;
        }
        self.process.send(AppToHost::Call { view: self.view.get(), body: body.to_string(), args });
    }
    fn eval(&self, body: &str, args: Value) -> LocalBoxFuture<Result<Value, String>> {
        self.process.eval(self.view.get(), body, Some(args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls_sorted() {
        let mut q = BTreeMap::new();
        q.insert("type".to_string(), "23".to_string());
        q.insert("app_version".to_string(), "26.10.20".to_string());
        q.insert("about_version".to_string(), String::new());
        assert_eq!(page_url(Page::Login, &q), "zalo-app://local/login.html?about_version=&app_version=26.10.20&type=23");
        assert_eq!(page_url(Page::Main, &BTreeMap::new()), "zalo-app://local/index.html");
    }
}
