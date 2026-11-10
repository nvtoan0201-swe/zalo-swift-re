//! `ZaloMainProcess` + `BridgeImplementation`: the Electron main process of Zalo PC.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;
use olaz_core::cookies::CookieJar;
use olaz_core::exec::{Exec, spawn};
use olaz_core::value::{Args, BV};
use olaz_host::bridge::{BridgeResult, ZaloBridge};
use olaz_host::{ClientHost, Page, WebHostProcess};
use serde_json::Value;

use crate::platform::ZaloPlatform;
use crate::secrets::{SecretStore, keychain_decrypt, keychain_encrypt, safe_storage_execute};
use crate::worker_router::SharedWorkerRouter;

#[derive(Debug, Clone, PartialEq)]
pub enum MainEvent {
    /// The renderer finished QR login; the main page is loading.
    LoginSucceeded,
    /// The renderer asked to go back to the login page (logout, session expiry).
    LoggedOut,
    Badge(i64),
}

/// A main-process service that answers its own IPC channels (downloads, notifications, calls…).
pub trait BridgeModule {
    fn install(&self, bridge: &Rc<ZaloBridge>, main: &Rc<MainProcess>);
    /// Logout: drop per-account state.
    fn reset(&self) {}
}

/// The 41 globals of the official preload (BridgeImplementation.swift lines 10–17).
pub const GLOBALS: [&str; 41] = [
    "$zcommon", "$znode", "$zwindow", "$zresource", "$zlogger", "$zupdater", "$zapp", "$zdownload", "$zdb", "$zsessionManager",
    "$zmulti", "$zcall", "$zsharedWorker", "$zelectron", "$zelectronNative", "$zuri", "$zscreencap", "$zlogin", "$zperf",
    "$zconfig", "$zMetric", "$zFileManager", "$zpc", "$zFeatures", "$zcloud", "$zMsgChannel", "$zscript", "$zInAppPayment",
    "$capp", "$zbackup", "$zpreload", "$zsqlitebw", "$zsafeStorage", "$zMigrateDrive", "$zTFModels", "$znetwork", "$zprotocol",
    "$znoti", "$zsqlite", "__SENTRY_IPC__", "MetriczGlobal",
];

/// Where the official client's data access layer keeps its databases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// The official "migration disabled" branch: IndexedDB in WebKit (Olaz before the SQLite host).
    IndexedDb,
    /// Native SQLite through `sqlite-host.js` + `SqliteHost`, as Zalo PC 26.10 runs after its migration.
    Sqlite,
}

impl StorageBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            StorageBackend::IndexedDb => "idb",
            StorageBackend::Sqlite => "sqlite",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "idb" => Some(StorageBackend::IndexedDb),
            "sqlite" => Some(StorageBackend::Sqlite),
            _ => None,
        }
    }
}

const STORAGE_STORE: &str = "olaz-storage";

/// Chosen once per install (`olaz-storage.json`): installs that already hold IndexedDB data stay on it (the official
/// IndexedDB → SQLite migration needs Electron's SQLite window), fresh installs start on SQLite, and
/// `OLAZ_STORAGE=idb|sqlite` overrides for a run.
pub fn choose_storage(platform: &ZaloPlatform) -> StorageBackend {
    if let Some(forced) = std::env::var("OLAZ_STORAGE").ok().as_deref().and_then(StorageBackend::parse) {
        return forced;
    }
    if let Some(saved) = platform.read_store(STORAGE_STORE).get("backend").and_then(Value::as_str).and_then(StorageBackend::parse) {
        return saved;
    }
    // An install that already ran the official client holds its data in IndexedDB; keep it readable.
    let chosen = if platform.store_path("database-config").exists() { StorageBackend::IndexedDb } else { StorageBackend::Sqlite };
    write_storage_choice(platform, chosen);
    chosen
}

fn write_storage_choice(platform: &ZaloPlatform, backend: StorageBackend) {
    let mut store = serde_json::Map::new();
    store.insert("backend".into(), Value::String(backend.as_str().into()));
    store.insert("chosenAt".into(), Value::from(olaz_core::clock::now_ms()));
    let _ = platform.write_store(STORAGE_STORE, &store);
}

type EventSink = Rc<dyn Fn(MainEvent)>;
type HostHook = Rc<dyn Fn(&Rc<ClientHost>)>;

pub struct MainProcess {
    pub platform: Rc<ZaloPlatform>,
    pub jar: Arc<CookieJar>,
    pub secrets: Arc<dyn SecretStore>,
    pub exec: Exec,
    pub webhost: Rc<WebHostProcess>,
    host: RefCell<Weak<ClientHost>>,
    pub on_event: RefCell<Option<EventSink>>,
    pub shared_worker: Rc<SharedWorkerRouter>,
    worker_host: RefCell<Option<Rc<ClientHost>>>,
    /// Configures logging of a freshly created worker window (the engine's policy).
    pub configure_worker: RefCell<Option<HostHook>>,
    db_clients: RefCell<Vec<Weak<ZaloBridge>>>,
    scripts: RefCell<Vec<Weak<ZaloBridge>>>,
    announced_session: RefCell<Option<BV>>,
    viewer_key: RefCell<Option<BV>>,
    session: RefCell<Option<BV>>,
    pending_close_dbs: RefCell<Vec<oneshot::Sender<()>>>,
    push_login_success_after_load: Cell<bool>,
    pub modules: RefCell<Vec<Rc<dyn BridgeModule>>>,
    /// Opens files/folders for `__shell__` (tests replace it).
    pub shell: RefCell<Rc<dyn Fn(&str, &str) -> bool>>,
    /// Dock/launcher badge.
    pub badge: RefCell<Option<Rc<dyn Fn(i64)>>>,
    pub trace_workers: bool,
    /// Where the databases live (chosen at start, `choose_storage`).
    pub storage: StorageBackend,
    /// The native SQLite executor behind `$znode.sqliteHost.*` (SQLite backend only).
    pub sqlite: Option<Arc<crate::sqlite_host::SqliteHost>>,
}

impl MainProcess {
    pub fn new(platform: Rc<ZaloPlatform>, jar: Arc<CookieJar>, secrets: Arc<dyn SecretStore>, webhost: Rc<WebHostProcess>, exec: Exec) -> Rc<Self> {
        let storage = choose_storage(&platform);
        let sqlite = (storage == StorageBackend::Sqlite).then(|| crate::sqlite_host::SqliteHost::new(platform.user_data.join("sqlite/_production")));
        olaz_core::log::write(format!("main-process storage backend: {}", storage.as_str()));
        Rc::new(Self {
            platform,
            jar,
            secrets,
            exec,
            webhost,
            host: RefCell::new(Weak::new()),
            on_event: RefCell::new(None),
            shared_worker: SharedWorkerRouter::new(),
            worker_host: RefCell::new(None),
            configure_worker: RefCell::new(None),
            db_clients: RefCell::new(Vec::new()),
            scripts: RefCell::new(Vec::new()),
            announced_session: RefCell::new(None),
            viewer_key: RefCell::new(None),
            session: RefCell::new(None),
            pending_close_dbs: RefCell::new(Vec::new()),
            push_login_success_after_load: Cell::new(false),
            modules: RefCell::new(Vec::new()),
            shell: RefCell::new(Rc::new(default_shell)),
            badge: RefCell::new(None),
            trace_workers: std::env::var_os("OLAZ_TRACE_WORKERS").is_some(),
            storage,
            sqlite,
        })
    }

    pub fn set_host(&self, host: &Rc<ClientHost>) {
        *self.host.borrow_mut() = Rc::downgrade(host);
    }

    pub fn host(&self) -> Option<Rc<ClientHost>> {
        self.host.borrow().upgrade()
    }

    pub fn worker_host(&self) -> Option<Rc<ClientHost>> {
        self.worker_host.borrow().clone()
    }

    fn event(&self, e: MainEvent) {
        let sink = self.on_event.borrow().clone();
        if let Some(s) = sink {
            s(e);
        }
    }

    pub fn has_session(&self) -> bool {
        self.jar.has_session()
    }

    /// Query string main appends to pages.
    pub fn page_query(&self, already_login: bool) -> BTreeMap<String, String> {
        page_query(&self.platform, already_login)
    }

    pub fn start(self: &Rc<Self>) {
        let Some(host) = self.host() else { return };
        self.normalize_database_adapter_state();
        if self.has_session() {
            self.start_shared_worker();
        }
        host.load(if self.has_session() { Page::Main } else { Page::Login }, &self.page_query(false));
    }

    /// Creates a window whose bridge is installed by this main process.
    pub fn create_window(self: &Rc<Self>) -> Rc<ClientHost> {
        let ua = self.platform.user_agent();
        let me = self.clone();
        ClientHost::new(&self.webhost, &ua, self.jar.clone(), self.trace_workers, move |bridge| install_bridge(bridge, &me))
    }

    /// Electron main creates the hidden shared-worker window together with the main window.
    pub fn start_shared_worker(self: &Rc<Self>) {
        if self.host().is_none() {
            return;
        }
        self.stop_shared_worker();
        if std::env::var_os("OLAZ_NO_SHARED_WORKER").is_some() {
            return;
        }
        let worker = self.create_window();
        let hook = self.configure_worker.borrow().clone();
        if let Some(h) = hook {
            h(&worker);
        }
        let (me, weak_worker) = (Rc::downgrade(self), Rc::downgrade(&worker));
        let previous = worker.on_page_event.borrow().clone();
        *worker.on_page_event.borrow_mut() = Some(Rc::new(move |event| {
            if let Some(p) = &previous {
                p(event.clone());
            }
            if let (olaz_host::PageEvent::FinishedLoading(_), Some(me), Some(worker)) = (&event, me.upgrade(), weak_worker.upgrade()) {
                me.shared_worker.worker_did_finish_load();
                let key = me.viewer_key.borrow().clone();
                if let Some(key) = key {
                    worker.bridge.send("receive-viewerkey", vec![key]);
                }
            }
        }));
        self.shared_worker.register_worker(&worker.bridge);
        worker.load(Page::SharedWorker, &self.page_query(true));
        *self.worker_host.borrow_mut() = Some(worker);
    }

    pub fn stop_shared_worker(&self) {
        let Some(worker) = self.worker_host.borrow_mut().take() else { return };
        let is_worker = |w: &Weak<ZaloBridge>| w.upgrade().is_none_or(|b| Rc::ptr_eq(&b, &worker.bridge));
        self.db_clients.borrow_mut().retain(|w| !is_worker(w));
        self.scripts.borrow_mut().retain(|w| !is_worker(w));
        self.shared_worker.reset_worker(&worker.bridge);
        worker.shutdown();
    }

    /// Keep every account on IndexedDB.
    pub fn normalize_database_adapter_state(&self) {
        // SQLite backend: the renderer moves every adapter state to SQLite itself (`isMigrateFullDone` is true).
        if self.storage != StorageBackend::IndexedDb {
            return;
        }
        normalize_database_adapter_state(&self.platform);
    }

    pub fn page_did_start_loading(&self) {
        if let Some(h) = self.host() {
            h.bridge.send("unload-zlog", vec![]);
        }
    }

    pub fn page_did_finish_loading(&self, page: Page) {
        let Some(h) = self.host() else { return };
        h.bridge.send("visibility-change", vec![BV::str("show")]);
        h.bridge.send("visibility-change", vec![BV::str("focus"), BV::str("1")]);
        if page == Page::Main && self.push_login_success_after_load.get() {
            self.push_login_success_after_load.set(false);
            h.bridge.send("login-success", vec![]);
        }
    }

    fn login_succeeded(self: &Rc<Self>) {
        olaz_core::log::write("main-process login-success → loading main page");
        self.push_login_success_after_load.set(true);
        self.start_shared_worker();
        if let Some(h) = self.host() {
            h.load(Page::Main, &self.page_query(true));
        }
        self.event(MainEvent::LoginSucceeded);
    }

    /// Drops everything tied to the signed-in account (logout).
    pub fn reset_account_state(&self) {
        *self.session.borrow_mut() = None;
        *self.announced_session.borrow_mut() = None;
        *self.viewer_key.borrow_mut() = None;
        for m in self.modules.borrow().iter() {
            m.reset();
        }
        self.stop_shared_worker();
        self.shared_worker.reset_session();
        if let Some(sqlite) = self.sqlite.clone() {
            spawn(&self.exec, async move { sqlite.close_all().await });
        }
    }

    /// Records `backend` for the next launch. The official client then meets empty databases (the account's data is
    /// not migrated between backends): the caller logs the account out so it starts over.
    pub fn switch_storage(&self, backend: StorageBackend) {
        write_storage_choice(&self.platform, backend);
        let mut store = self.platform.read_store("database-config");
        store.retain(|k, _| !k.ends_with("adp-st") && !k.to_lowercase().contains("upgraderequired"));
        let _ = self.platform.write_store("database-config", &store);
    }

    pub fn back_to_login(&self) {
        self.reset_account_state();
        if let Some(h) = self.host() {
            h.load(Page::Login, &self.page_query(false));
        }
        self.event(MainEvent::LoggedOut);
    }

    fn finish_close_dbs(&self) {
        for tx in self.pending_close_dbs.borrow_mut().drain(..) {
            let _ = tx.send(());
        }
    }

    // MARK: IPC handlers

    pub fn install(self: &Rc<Self>, bridge: &Rc<ZaloBridge>) {
        load_bridge_table(bridge);
        let p = self.platform.clone();
        let wb = Rc::downgrade(bridge);
        let me = Rc::downgrade(self);

        let pp = p.clone();
        bridge.handle_now("_electron_get-path", move |a| Ok(BV::String(pp.path(&a.string(0).unwrap_or_else(|| "userData".into())))));
        let pp = p.clone();
        bridge.handle_sync("_electron_get-path-sync", move |a| Ok(BV::String(pp.path(&a.string(0).unwrap_or_else(|| "userData".into())))));
        let pp = p.clone();
        bridge.handle_now("_electron_get-app-version", move |_| Ok(BV::String(pp.app_version.clone())));
        let pp = p.clone();
        bridge.handle_now("_electron_get-app-path", move |_| Ok(BV::String(pp.user_data.to_string_lossy().into_owned())));
        let pp = p.clone();
        bridge.handle_sync("_electron_get-app-path-sync", move |_| Ok(BV::String(pp.user_data.to_string_lossy().into_owned())));
        let pp = p.clone();
        bridge.handle_sync("electron-store-get-data", move |_| {
            Ok(BV::obj([("defaultCwd", BV::String(pp.user_data.to_string_lossy().into_owned())), ("appVersion", BV::String(pp.app_version.clone()))]))
        });
        bridge.handle_now("get-child-url", |_| Ok(BV::str("zalo-app://local/child.html")));
        bridge.handle_now("get-photo-viewer-url", |_| Ok(BV::str("zalo-app://local/popup-viewer.html")));
        bridge.handle_now("get-resource-dir", |_| Ok(BV::str("zalo-app://local/")));

        // Storage selection. SQLite backend: the official "migration fully done" state, so every database adapter is
        // SQLite and the DAL channel targets the utility process that sqlite-host.js plays. IndexedDB backend: the
        // official "migration disabled" branch. `fail-load-sqlite3` stays true in both: the legacy message search index
        // runs in its web worker (the native search binding is not provided).
        let sqlite_backend = self.storage == StorageBackend::Sqlite;
        bridge.handle_now("sqlite-available", move |_| Ok(BV::Bool(sqlite_backend)));
        bridge.handle_now("fail-load-sqlite3", |_| Ok(BV::Bool(true)));
        bridge.handle_now("is-migrate-full-done", move |_| Ok(BV::Bool(sqlite_backend)));
        bridge.handle_now("get-migrate-local-info", |_| Ok(BV::obj([("disable_migrate", BV::Bool(true))])));
        bridge.handle_now("migratedb:check_need_wait_bu", |_| Ok(BV::Bool(false)));
        bridge.handle_now("z-db-corruption-detector", |_| Ok(BV::int(0)));
        bridge.handle_now("get-name-of-target-script-for-dal-channel-from-host", move |_| {
            Ok(BV::str(if sqlite_backend { "utility-process-sqlite" } else { "preload-sqlite" }))
        });
        let m = me.clone();
        bridge.handle_now("migratedb:signal_restart", move |_| {
            let Some(me) = m.upgrade() else { return Ok(BV::Undefined) };
            olaz_core::log::write(format!("main-process renderer requested restart for DB migration — reloading on {}", me.storage.as_str()));
            me.normalize_database_adapter_state();
            if me.has_session() { me.start_shared_worker() } else { me.stop_shared_worker() }
            if let Some(h) = me.host() {
                h.load(if me.has_session() { Page::Main } else { Page::Login }, &me.page_query(false));
            }
            Ok(BV::Undefined)
        });

        // Cookies
        let jar = self.jar.clone();
        bridge.handle_now("_electron_set-app-cookie", move |a| {
            if let Some(v) = a.string(0) {
                jar.set_app_cookie(&v);
            }
            Ok(BV::Undefined)
        });
        let jar = self.jar.clone();
        bridge.handle_now("_electron_set-cookie", move |a| {
            let (Some(name), Some(value)) = (a.string(0), a.string(1)) else { return Ok(BV::Undefined) };
            let domain = a
                .string(3)
                .or_else(|| a.string(2).and_then(|u| url::Url::parse(&u).ok()).and_then(|u| u.host_str().map(str::to_string)))
                .unwrap_or_else(|| "zalo.me".into());
            jar.set_cookie(&name, &value, &domain, 365.0, false);
            Ok(BV::Undefined)
        });
        let jar = self.jar.clone();
        bridge.handle_now("_electron_clear-cookie", move |_| {
            jar.clear_zalo();
            Ok(BV::Undefined)
        });

        // Login / session hand-off
        let m = me.clone();
        bridge.handle_now("login-success", move |_| {
            if let Some(me) = m.upgrade() {
                me.login_succeeded();
            }
            Ok(BV::Undefined)
        });
        let m = me.clone();
        bridge.handle_now("login", move |_| {
            if let Some(me) = m.upgrade() {
                me.back_to_login();
            }
            Ok(BV::Undefined)
        });
        let (m, w) = (me.clone(), wb.clone());
        bridge.handle_now("script-connected", move |_| {
            let (Some(me), Some(bridge)) = (m.upgrade(), w.upgrade()) else { return Ok(BV::Undefined) };
            let mut scripts = me.scripts.borrow_mut();
            scripts.retain(|s| s.strong_count() > 0);
            if !scripts.iter().any(|s| s.upgrade().is_some_and(|b| Rc::ptr_eq(&b, &bridge))) {
                scripts.push(Rc::downgrade(&bridge));
            }
            drop(scripts);
            let announced = me.announced_session.borrow().clone();
            if let Some(a) = announced {
                bridge.send("new-session", vec![a]);
            }
            Ok(BV::Undefined)
        });
        let (m, w) = (me.clone(), wb.clone());
        bridge.handle_now("new-session", move |a| {
            let Some(me) = m.upgrade() else { return Ok(BV::Undefined) };
            let payload = a.first().cloned();
            *me.session.borrow_mut() = payload.clone();
            let empty = match &payload {
                None | Some(BV::Null) | Some(BV::Undefined) => true,
                Some(BV::Object(o)) => o.is_empty(),
                _ => false,
            };
            if empty {
                *me.announced_session.borrow_mut() = None;
            } else if let Some(session) = payload {
                *me.announced_session.borrow_mut() = Some(session.clone());
                let sender = w.upgrade();
                let targets: Vec<Rc<ZaloBridge>> = me.scripts.borrow().iter().filter_map(Weak::upgrade).collect();
                for s in targets {
                    if sender.as_ref().is_some_and(|b| Rc::ptr_eq(b, &s)) {
                        continue;
                    }
                    s.send("new-session", vec![session.clone()]);
                }
            }
            Ok(BV::Undefined)
        });
        let (m, w) = (me.clone(), wb.clone());
        bridge.handle_now("db-authenticated", move |a| {
            let Some(me) = m.upgrade() else { return Ok(BV::Undefined) };
            *me.session.borrow_mut() = a.first().cloned();
            let Some(session) = a.first().cloned() else { return Ok(BV::Undefined) };
            let mut targets: Vec<Rc<ZaloBridge>> = me.db_clients.borrow().iter().filter_map(Weak::upgrade).collect();
            if let Some(b) = w.upgrade() {
                if !targets.iter().any(|t| Rc::ptr_eq(t, &b)) {
                    targets.push(b);
                }
            }
            for t in targets {
                t.send("db-authenticated", vec![session.clone()]);
            }
            Ok(BV::Undefined)
        });
        let (m, w) = (me.clone(), wb.clone());
        bridge.handle_now("db-client-connected", move |_| {
            let (Some(me), Some(bridge)) = (m.upgrade(), w.upgrade()) else { return Ok(BV::Undefined) };
            let mut clients = me.db_clients.borrow_mut();
            clients.retain(|c| c.strong_count() > 0);
            if !clients.iter().any(|c| c.upgrade().is_some_and(|b| Rc::ptr_eq(&b, &bridge))) {
                clients.push(Rc::downgrade(&bridge));
            }
            drop(clients);
            let session = me.session.borrow().clone();
            if let Some(s) = session {
                bridge.send("db-authenticated", vec![s]);
            }
            Ok(BV::Undefined)
        });
        self.shared_worker.install(bridge);
        let modules: Vec<Rc<dyn BridgeModule>> = self.modules.borrow().clone();
        for module in modules {
            module.install(bridge, self);
        }
        let m = me.clone();
        bridge.handle_now("receive-viewerkey", move |a| {
            let (Some(me), Some(key)) = (m.upgrade(), a.first().cloned()) else { return Ok(BV::Undefined) };
            *me.viewer_key.borrow_mut() = Some(key.clone());
            let worker = me.worker_host.borrow().clone();
            if let Some(w) = worker {
                w.bridge.send("receive-viewerkey", vec![key]);
            }
            Ok(BV::Undefined)
        });
        let (m, w) = (me.clone(), wb.clone());
        bridge.handle("db-start-close-dbs", move |a| {
            let (m, w) = (m.clone(), w.clone());
            Box::pin(async move {
                let bridge = w.upgrade();
                if let Some(b) = &bridge {
                    b.send("db-start-close-dbs", a);
                }
                let Some(me) = m.upgrade() else { return Ok(BV::Undefined) };
                if !bridge.is_some_and(|b| b.is_subscribed("db-start-close-dbs")) {
                    return Ok(BV::Undefined);
                }
                let (tx, rx) = oneshot::channel();
                me.pending_close_dbs.borrow_mut().push(tx);
                let exec = me.exec.clone();
                let weak = Rc::downgrade(&me);
                drop(me);
                spawn(&exec.clone(), async move {
                    exec.timer(Duration::from_secs(5)).await;
                    if let Some(me) = weak.upgrade() {
                        me.finish_close_dbs();
                    }
                });
                let _ = rx.await;
                Ok(BV::Undefined)
            })
        });
        let m = me.clone();
        bridge.handle_now("db-finish-close-dbs", move |_| {
            if let Some(me) = m.upgrade() {
                me.finish_close_dbs();
            }
            Ok(BV::Undefined)
        });

        // Secure storage used by the trusted-device protocol
        let pp = p.clone();
        bridge.handle_now("zsafestorage-execute", move |a| safe_storage_execute(&pp, &a.string(0).unwrap_or_default(), &a[1.min(a.len())..]));
        let secrets = self.secrets.clone();
        bridge.handle("zproto-encrypt-with-keychain", move |a| {
            let secrets = secrets.clone();
            Box::pin(async move {
                let id = a.string(0).unwrap_or_default();
                let items = bytes_items(&a);
                let out = crate::fs::blocking(move || keychain_encrypt(secrets.as_ref(), &id, &items)).await?;
                Ok(BV::Array(out.into_iter().map(BV::Bytes).collect()))
            })
        });
        let secrets = self.secrets.clone();
        bridge.handle("zproto-decrypt-with-keychain", move |a| {
            let secrets = secrets.clone();
            Box::pin(async move {
                let id = a.string(0).unwrap_or_default();
                let items = bytes_items(&a);
                let out = crate::fs::blocking(move || keychain_decrypt(secrets.as_ref(), &id, &items)).await?;
                Ok(BV::Array(out.into_iter().map(BV::Bytes).collect()))
            })
        });

        // Native helpers without a native module here: "not supported", so the renderer falls back.
        let unsupported = |_: Vec<BV>| -> BridgeResult {
            Ok(BV::obj([(
                "error",
                BV::obj([("name", BV::str("NotSupported")), ("message", BV::str("Not supported")), ("code", BV::str("ENOTSUP"))]),
            )]))
        };
        bridge.handle_now("exec", unsupported);
        bridge.handle_now("exec_raw", unsupported);
        bridge.handle_now("get-cpu-infos", |_| {
            Ok(BV::Array(vec![BV::obj([
                ("pid", BV::int(std::process::id() as i64)),
                ("type", BV::str("Browser")),
                ("cpu", BV::obj([("percentCPUUsage", BV::int(0)), ("idleWakeupsPerSecond", BV::int(0))])),
                ("memory", BV::obj([("workingSetSize", BV::int(0)), ("peakWorkingSetSize", BV::int(0))])),
            ])]))
        });
        let m = me.clone();
        bridge.handle_now("badge-count", move |a| {
            let count = a.int(0).unwrap_or(0);
            if let Some(me) = m.upgrade() {
                let badge = me.badge.borrow().clone();
                if let Some(b) = badge {
                    b(count);
                }
                me.event(MainEvent::Badge(count));
            }
            Ok(BV::Undefined)
        });
        let m = me.clone();
        bridge.handle_now("__shell__", move |a| {
            let (op, path) = (a.string(0).unwrap_or_default(), a.string(1).unwrap_or_default());
            let Some(me) = m.upgrade() else { return Ok(BV::Undefined) };
            let shell = me.shell.borrow().clone();
            let ok = shell(&op, &path);
            Ok(BV::Bool(if op == "showItemInFolder" { true } else { ok }))
        });
        bridge.handle_now("__child_process__", |_| Err("EPERM: child_process is not available".into()));
        // Calls are out of v1 on Linux: accept and ignore so the renderer does not wait.
        bridge.handle_now("call-send-to-native", |_| Ok(BV::Undefined));
        bridge.handle_now("call-init", |_| Ok(BV::Undefined));
        bridge.handle_now("request-compressed-log-file", |_| {
            Ok(BV::obj([("status", BV::int(2)), ("data", BV::obj([("message", BV::str("Not supported")), ("errorData", BV::Null)]))]))
        });
    }
}

/// Query string main appends to pages.
pub fn page_query(platform: &ZaloPlatform, already_login: bool) -> BTreeMap<String, String> {
    let mut q = BTreeMap::new();
    q.insert("type".into(), platform.client_type.clone());
    q.insert("client_version".into(), platform.client_version.clone());
    q.insert("app_version".into(), platform.app_version.clone());
    q.insert("about_version".into(), String::new());
    q.insert("new_install".into(), "0".into());
    // No native sqlite3 *module* here (the legacy search index runs in its web worker); the DAL's databases are
    // served by sqlite-host.js on the SQLite backend.
    q.insert("fail_sqlite3".into(), "true".into());
    q.insert("mode_lang".into(), String::new());
    if already_login {
        q.insert("already_login".into(), "y".into());
    }
    q
}

/// No SQLite backend here: keep every account on IndexedDB (`*adp-st` → all 0) and drop `upgradeRequired`.
/// Returns whether the store was rewritten.
pub fn normalize_database_adapter_state(platform: &ZaloPlatform) -> bool {
    let mut store = platform.read_store("database-config");
    let mut changed = false;
    let keys: Vec<String> = store.keys().cloned().collect();
    for key in keys {
        let value = store.get(&key).cloned().unwrap_or(Value::Null);
        if key.ends_with("adp-st") {
            if let Some(Ok(Value::Object(mut states))) = value.as_str().map(serde_json::from_str::<Value>) {
                if states.values().any(|v| v.as_f64().map(|f| f as i64) != Some(0)) {
                    for v in states.values_mut() {
                        *v = Value::from(0);
                    }
                    store.insert(key, Value::String(Value::Object(states).to_string()));
                    changed = true;
                }
                continue;
            }
        }
        if key.to_lowercase().contains("upgraderequired") {
            store.remove(&key);
            changed = true;
        }
    }
    if changed {
        olaz_core::log::write("database-config: reset adapter state to IndexedDB");
        let _ = platform.write_store("database-config", &store);
    }
    changed
}

fn bytes_items(a: &[BV]) -> Vec<Vec<u8>> {
    a.get(1)
        .and_then(BV::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    BV::Bytes(b) | BV::ArrayBuffer(b) => Some(b.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `bridge-table.json` (entries without `channel` or `type` are skipped).
fn load_bridge_table(bridge: &ZaloBridge) {
    let Ok(Value::Object(json)) = serde_json::from_str::<Value>(olaz_core::scripts::BRIDGE_TABLE_JSON) else {
        olaz_core::log::write("main-process bridge-table.json missing — bridge will not match the official preload");
        return;
    };
    let Some(Value::Object(table)) = json.get("table") else { return };
    let entries = table.iter().filter_map(|(member, entry)| {
        let channel = entry.get("channel")?.as_str()?.to_string();
        let kind = entry.get("type")?.as_str()?.to_string();
        Some((member.clone(), channel, kind))
    });
    bridge.load_ipc_table(entries);
}

/// `__shell__`: `showItemInFolder` → FileManager1.ShowItems (fallback: open the folder); else open the file.
pub fn default_shell(op: &str, path: &str) -> bool {
    use std::process::Command;
    if op == "showItemInFolder" {
        let uri = url::Url::from_file_path(path).map(|u| u.to_string()).unwrap_or_default();
        let shown = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.FileManager1",
                "--object-path",
                "/org/freedesktop/FileManager1",
                "--method",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("['{uri}']"),
                "",
            ])
            .status()
            .is_ok_and(|s| s.success());
        if !shown {
            let dir = std::path::Path::new(path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let _ = Command::new("xdg-open").arg(dir).spawn();
        }
        true
    } else {
        Command::new("xdg-open").arg(path).spawn().is_ok()
    }
}

/// `BridgeImplementation.install` — the order matters.
pub fn install_bridge(bridge: &Rc<ZaloBridge>, main: &Rc<MainProcess>) {
    for g in GLOBALS {
        bridge.declare_global(g);
    }
    bridge.value("$zenv.isPreloaded", BV::Bool(true));
    bridge.value("$zenv.isRenderer", BV::Bool(true));

    main.install(bridge);
    // Native SQLite behind the official DAL (sqlite-host.js); absent on the IndexedDB backend.
    if let Some(sqlite) = &main.sqlite {
        sqlite.install(bridge);
    }

    let p = main.platform.clone();
    bridge.sync("$zelectron.storePath", move |a| Ok(BV::String(p.store_path(&a.string(0).unwrap_or_else(|| "config".into())).to_string_lossy().into_owned())));
    let p = main.platform.clone();
    bridge.sync("$zelectron.storeRead", move |a| Ok(BV::json(Value::Object(p.read_store(&a.string(0).unwrap_or_else(|| "config".into()))))));
    let p = main.platform.clone();
    bridge.sync("$zelectron.storeWrite", move |a| {
        let object = a.dict(1).map(|d| BV::Object(d.clone()).to_plain_json()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
        p.write_store(&a.string(0).unwrap_or_else(|| "config".into()), &object)?;
        Ok(BV::Undefined)
    });

    bridge.value("$znode.os.__info", os_info(&main.platform));
    let constants = BV::obj([("F_OK", BV::int(0)), ("R_OK", BV::int(4)), ("W_OK", BV::int(2)), ("X_OK", BV::int(1))]);
    bridge.value("$znode.fs.constants", constants.clone());
    bridge.value("$znode.fs.constanst", constants);

    crate::clipboard::install(bridge);
    crate::fs::install_file_system(bridge, &main.platform);
    for member in [
        "encrypt", "decrypt", "hash", "randomBytes", "__cipherCreate", "__cipherUpdate", "__cipherFinal", "__cipherOption", "__hashCreate",
        "__hashUpdate", "__hashDigest",
    ] {
        bridge.sync(&format!("$znode.crypto.{member}"), move |a| olaz_core::crypto::handle(member, &a).unwrap());
    }
    crate::fs::install_file_manager(bridge, &main.platform);
}

/// Every member/channel `install_bridge` answers, with its kind — `olaz --dump-bridge-registry`.
pub fn registry_without_host(bridge: &Rc<ZaloBridge>, paths: &olaz_core::paths::XdgPaths) -> Vec<(String, String)> {
    let platform = Rc::new(ZaloPlatform::new("0", paths.clone()));
    let config = olaz_host::WebHostConfig {
        binary: Default::default(),
        asar: Default::default(),
        data_dir: Default::default(),
        cache_dir: Default::default(),
        debug: false,
    };
    let webhost = WebHostProcess::detached(config, bridge.exec().clone());
    let main = MainProcess::new(platform, CookieJar::memory(), Arc::new(crate::secrets::MemoryStore::default()), webhost, bridge.exec().clone());
    install_bridge(bridge, &main);
    bridge.registry()
}

pub fn os_info(platform: &ZaloPlatform) -> BV {
    let uts = nix::sys::utsname::uname().ok();
    let field = |f: fn(&nix::sys::utsname::UtsName) -> &std::ffi::OsStr| uts.as_ref().map(|u| f(u).to_string_lossy().into_owned()).unwrap_or_default();
    let machine = field(|u| u.machine());
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<i64>().ok()))
        .unwrap_or(0)
        * 1024;
    let uptime = std::fs::read_to_string("/proc/uptime").ok().and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok()).unwrap_or(0.0);
    let user = std::env::var("USER").unwrap_or_default();
    BV::obj([
        ("release", BV::String(field(|u| u.release()))),
        ("version", BV::String(field(|u| u.version()))),
        ("arch", BV::str(if machine == "x86_64" { "x64" } else { "arm64" })),
        ("hostname", BV::String(field(|u| u.nodename()))),
        ("homedir", BV::String(platform.paths.home.to_string_lossy().into_owned())),
        ("tmpdir", BV::String(format!("{}/", std::env::temp_dir().to_string_lossy().trim_end_matches('/')))),
        ("totalmem", BV::int(mem)),
        ("cpus", BV::int(std::thread::available_parallelism().map(|n| n.get() as i64).unwrap_or(1))),
        ("uptime", BV::int(uptime as i64)),
        ("username", BV::String(user)),
        ("networkInterfaces", BV::String(network_interfaces_json())),
    ])
}

/// Node's `os.networkInterfaces()` as a JSON array of `[name, [addresses…]]` pairs (an array, not an object: getifaddrs
/// order must survive the bridge — the official QoS code takes the *first* non-internal IPv4 as the local IP, and Zalo
/// sends it). Address objects have Node's shape: address, netmask, family, mac, internal, cidr, scopeid (IPv6).
pub fn network_interfaces_json() -> String {
    use nix::net::if_::InterfaceFlags;
    use std::net::IpAddr;
    let Ok(addrs) = nix::ifaddrs::getifaddrs() else { return "[]".into() };
    let addrs: Vec<_> = addrs.collect();
    // Contiguous netmask bits, or None when the mask is not 1…10…0 (Node omits cidr then).
    fn prefix_len(bytes: &[u8]) -> Option<u32> {
        let bits: u128 = bytes.iter().fold(0u128, |acc, b| (acc << 8) | *b as u128);
        let total = bytes.len() as u32 * 8;
        let ones = bits.count_ones();
        let mask = if ones == 0 { 0 } else { (!0u128 << (128 - ones)) >> (128 - total) };
        (bits == mask).then_some(ones)
    }
    let mut macs: BTreeMap<String, String> = BTreeMap::new();
    for a in &addrs {
        if let Some(link) = a.address.as_ref().and_then(|s| s.as_link_addr()).and_then(|l| l.addr()) {
            macs.insert(a.interface_name.clone(), link.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"));
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut by_name: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for a in &addrs {
        let Some(sa) = a.address.as_ref() else { continue };
        let ip: IpAddr = if let Some(v4) = sa.as_sockaddr_in() {
            IpAddr::V4(v4.ip())
        } else if let Some(v6) = sa.as_sockaddr_in6() {
            IpAddr::V6(v6.ip())
        } else {
            continue;
        };
        let mask: Option<IpAddr> = a.netmask.as_ref().and_then(|m| {
            m.as_sockaddr_in().map(|v| IpAddr::V4(v.ip())).or_else(|| m.as_sockaddr_in6().map(|v| IpAddr::V6(v.ip())))
        });
        let mask_bytes = match mask {
            Some(IpAddr::V4(m)) => m.octets().to_vec(),
            Some(IpAddr::V6(m)) => m.octets().to_vec(),
            None => Vec::new(),
        };
        let address = ip.to_string();
        let family = if ip.is_ipv4() { "IPv4" } else { "IPv6" };
        let mut entry = serde_json::Map::new();
        entry.insert("address".into(), Value::String(address.clone()));
        entry.insert("netmask".into(), Value::String(mask.map(|m| m.to_string()).unwrap_or_default()));
        entry.insert("family".into(), Value::String(family.into()));
        entry.insert("mac".into(), Value::String(macs.get(&a.interface_name).cloned().unwrap_or_else(|| "00:00:00:00:00:00".into())));
        entry.insert("internal".into(), Value::Bool(a.flags.contains(InterfaceFlags::IFF_LOOPBACK)));
        // Node: IPv6 entries carry the scope as a number (`scopeid`), never inside `address`.
        if let Some(v6) = sa.as_sockaddr_in6() {
            entry.insert("scopeid".into(), Value::from(v6.scope_id()));
        }
        if let Some(p) = (!mask_bytes.is_empty()).then(|| prefix_len(&mask_bytes)).flatten() {
            entry.insert("cidr".into(), Value::String(format!("{address}/{p}")));
        }
        if !by_name.contains_key(&a.interface_name) {
            order.push(a.interface_name.clone());
        }
        by_name.entry(a.interface_name.clone()).or_default().push(Value::Object(entry));
    }
    let pairs: Vec<Value> = order.into_iter().map(|n| { let list = by_name.remove(&n).unwrap_or_default(); Value::Array(vec![Value::String(n), Value::Array(list)]) }).collect();
    Value::Array(pairs).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;


    fn platform() -> (tempfile::TempDir, ZaloPlatform) {
        let dir = tempfile::tempdir().unwrap();
        let p = ZaloPlatform::new("26.10.20", olaz_core::paths::XdgPaths::rooted(dir.path()));
        (dir, p)
    }

    #[test]
    fn page_query_both_variants() {
        let (_d, p) = platform();
        let url = olaz_host::host::page_url(Page::Login, &page_query(&p, false));
        assert_eq!(url, "zalo-app://local/login.html?about_version=&app_version=26.10.20&client_version=694&fail_sqlite3=true&mode_lang=&new_install=0&type=23");
        let url = olaz_host::host::page_url(Page::SharedWorker, &page_query(&p, true));
        assert_eq!(url, "zalo-app://local/shared-worker.html?about_version=&already_login=y&app_version=26.10.20&client_version=694&fail_sqlite3=true&mode_lang=&new_install=0&type=23");
    }

    #[test]
    fn database_adapter_state() {
        let (_d, p) = platform();
        let mut store = serde_json::Map::new();
        store.insert("u1adp-st".into(), Value::String("{\"a\":0,\"b\":0}".into()));
        p.write_store("database-config", &store).unwrap();
        assert!(!normalize_database_adapter_state(&p), "all zeros: no write");
        store.insert("u2adp-st".into(), Value::String("{\"a\":1,\"b\":0}".into()));
        store.insert("someUpgradeRequiredFlag".into(), Value::Bool(true));
        p.write_store("database-config", &store).unwrap();
        assert!(normalize_database_adapter_state(&p));
        let after = p.read_store("database-config");
        assert_eq!(after["u2adp-st"], "{\"a\":0,\"b\":0}");
        assert!(!after.contains_key("someUpgradeRequiredFlag"));
        assert_eq!(after["u1adp-st"], "{\"a\":0,\"b\":0}");
    }

    #[test]
    fn globals_are_41_and_unique() {
        let set: std::collections::HashSet<_> = GLOBALS.iter().collect();
        assert_eq!(set.len(), 41);
    }

    #[test]
    fn bridge_table_loads() {
        let t = olaz_core::exec::TestExecutor::new();
        let b = ZaloBridge::new(t.exec());
        load_bridge_table(&b);
        let spec = b.spec_script();
        assert!(spec.contains("\"ipc\":{"));
        assert!(b.globals().len() > 10);
    }

    #[test]
    fn os_info_shape() {
        let dir = tempfile::tempdir().unwrap();
        let p = ZaloPlatform::new("1", olaz_core::paths::XdgPaths::rooted(dir.path()));
        let info = os_info(&p);
        assert_eq!(info.get("arch"), Some(&BV::str("x64")));
        assert!(info.get("totalmem").and_then(BV::as_int).unwrap() > 0);
        assert!(info.get("release").and_then(BV::as_str).is_some_and(|s| !s.is_empty()));
    }
}
