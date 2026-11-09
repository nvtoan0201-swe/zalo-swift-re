//! `ZaloEngine` core: boots the runtime (installer → webhost → main process), turns renderer snapshots
//! into view state, exposes actions. All protocol work happens in the official renderer.
//! Port of `ZaloEngine.swift`; feature modules live in sibling files (`chat.rs`, `conversations.rs`…).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use olaz_core::exec::{Exec, spawn};
use olaz_core::paths::XdgPaths;
use olaz_host::{ClientHost, Page, PageEvent, WebHostConfig, WebHostProcess};
use olaz_main::secrets::SecretStore;
use olaz_main::{MainEvent, MainProcess, ZaloPlatform};
use olaz_runtime::{Installer, Layout, Step};
use serde_json::{Map, Value};

use crate::conversations::ConversationExtras;
use crate::models::{ChatMessage, FailedDraftStore, J};

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSetup {
    pub step: Step,
    /// Why the last attempt failed; the window offers to try again.
    pub failure: Option<String>,
}

impl Default for RuntimeSetup {
    fn default() -> Self {
        Self { step: Step::Looking, failure: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoginStep {
    Generating,
    WaitingScan,
    /// Phone scanned; waiting for "Đăng nhập" on the phone.
    WaitingConfirm { name: String, avatar: Option<String> },
    Expired,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoginState {
    pub step: LoginStep,
    pub qr_png: Option<Vec<u8>>,
    /// The server asked for a second scan (`error_code -20`): the QR on screen is QR2.
    pub is_second_qr: bool,
}

impl Default for LoginState {
    fn default() -> Self {
        Self { step: LoginStep::Generating, qr_png: None, is_second_qr: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Starting,
    Preparing(RuntimeSetup),
    Login(LoginState),
    SigningIn,
    Ready,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub user_id: String,
    pub display_name: String,
    pub avatar: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub text: String,
    /// ms since epoch.
    pub date: Option<f64>,
    pub from_me: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSummary {
    pub id: String,
    pub is_group: bool,
    pub name: String,
    pub avatar: Option<String>,
    pub pinned: bool,
    pub members: i64,
    pub unread: i64,
    pub preview: Option<Preview>,
}

impl ConversationSummary {
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if self.is_group {
            "Nhóm".into()
        } else {
            "Người dùng Zalo".into()
        }
    }

    pub fn from_json(row: &Value) -> Option<ConversationSummary> {
        let id = row.get_v("id").and_then(Value::as_str)?.to_string();
        let preview = row.get_v("preview").filter(|p| p.is_object()).map(|p| {
            let ts = p.n("ts");
            Preview { text: p.s("text"), date: (ts > 0.0).then(|| if ts > 1e12 { ts } else { ts * 1000.0 }), from_me: p.b("fromMe") }
        });
        Some(ConversationSummary {
            id,
            is_group: row.b("isGroup"),
            name: row.s("name"),
            avatar: row.get_v("avatar").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            pinned: row.b("pinned"),
            members: row.n("members") as i64,
            unread: row.n("unread") as i64,
            preview,
        })
    }
}

/// Pinned first, then most recent activity, then name (Vietnamese collation).
pub fn list_order(a: &ConversationSummary, b: &ConversationSummary) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a.pinned != b.pinned {
        return if a.pinned { Ordering::Less } else { Ordering::Greater };
    }
    let (da, db) = (a.preview.as_ref().and_then(|p| p.date).unwrap_or(f64::MIN), b.preview.as_ref().and_then(|p| p.date).unwrap_or(f64::MIN));
    if da != db {
        return db.partial_cmp(&da).unwrap_or(Ordering::Equal);
    }
    crate::collate::compare(&a.display_name(), &b.display_name())
}

/// What changed (views re-render on any change; the kind lets them skip unrelated work).
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Phase,
    Conversations,
    Profile,
    Messages(String),
    Typing(String),
    Extras,
    Log,
    Notice,
    Contacts,
    Other,
}

pub const KICKOUT_NOTICE: &str = "Tài khoản của bạn vừa đăng nhập trên một Zalo PC khác, nên phiên này đã bị đăng xuất. Zalo chỉ cho phép một phiên máy tính cho mỗi tài khoản.";
/// `qr-id` poll result meaning "scanned once; show the second QR".
pub const SECOND_QR_REQUIRED_CODE: i64 = -20;

/// Observable engine state (read by the UI, mutated only on the executor thread).
#[derive(Default)]
pub struct EngineState {
    pub phase: Option<Phase>,
    pub conversations: Vec<ConversationSummary>,
    pub profile: Option<Profile>,
    pub sign_out_notice: Option<String>,
    pub client_version: Option<String>,
    pub recent_log: Vec<String>,
    pub open_conversation_id: Option<String>,
    pub failed_drafts: FailedDraftStore,
    pub messages: HashMap<String, Vec<ChatMessage>>,
    pub has_more_history: HashMap<String, bool>,
    pub history_ticks: HashMap<String, u64>,
    pub typing: HashMap<String, String>,
    pub self_conversation_id: Option<String>,
    pub navigation_request: Option<String>,
    pub conversation_extras: ConversationExtras,
    pub badge: i64,
    pub toast: Option<crate::composer::Toast>,
    pub composer_confirm: Option<crate::composer::ComposerConfirm>,
    pub queued_confirms: Vec<crate::composer::ComposerConfirm>,
    pub forward_request: Option<crate::message_actions::ForwardRequest>,
    pub message_confirm: Option<crate::message_actions::MessageActionConfirm>,
    pub confirm_seq: u64,
    pub delete_undo: Option<crate::message_actions::DeleteUndo>,
    pub pins: HashMap<String, crate::message_actions::PinnedList>,
    pub message_actions_busy: std::collections::HashSet<String>,
    pub conversation_actions: crate::conversation_actions::ConversationActionsState,
    pub conversation_prompt: Option<crate::conversation_actions::ConversationPrompt>,
    pub conversation_ui: crate::conversation_actions::ConversationActionUI,
    pub contacts: crate::contacts::ContactsState,
    pub sticker_suggest: Option<crate::stickers::StickerSuggestSetting>,
    pub sticker_suggest_attempt: Option<f64>,
    pub polls: crate::polls::PollStore,
    pub cloud: crate::cloud::CloudState,
    pub transfer: Option<crate::transfer::MessageTransferState>,
    pub can_start_transfer: bool,
    pub transfer_blocked_reason: Option<String>,
    pub connection_info: Option<serde_json::Value>,
}

/// Desktop services the engine needs (secrets, opening links…), injected by the app or tests.
pub struct Services {
    pub secrets: Arc<dyn SecretStore>,
    /// Shared CDN loader (native views and the main process's download manager).
    pub media: Option<Arc<olaz_media::MediaLoader>>,
    pub open_url: Rc<dyn Fn(&str)>,
    pub badge: Option<Rc<dyn Fn(i64)>>,
    pub webhost_binary: PathBuf,
    pub debug: bool,
}

pub struct Runtime {
    pub platform: Rc<ZaloPlatform>,
    pub main: Rc<MainProcess>,
    pub host: Rc<ClientHost>,
    pub webhost: Rc<WebHostProcess>,
}

type Listener = Rc<dyn Fn(&Change)>;

pub struct ZaloEngine {
    pub exec: Exec,
    pub paths: XdgPaths,
    pub state: RefCell<EngineState>,
    pub(crate) runtime: RefCell<Option<Runtime>>,
    services: Services,
    listeners: RefCell<Vec<Listener>>,
    installing: Cell<bool>,
    login_timeout_gen: Cell<u64>,
    pub(crate) typing_gen: RefCell<HashMap<String, u64>>,
    pub(crate) media_resolver: Rc<crate::media::MediaResolver>,
    pub(crate) notification_settings: RefCell<crate::notifications::NotificationSettingsState>,
    pub notifier: Rc<crate::notifications::Notifier>,
    /// (app active, main window visible, last user input)
    pub(crate) window_state: RefCell<(bool, bool, std::time::Instant)>,
    /// Brings the main window to front (set by the UI).
    pub on_activate: RefCell<Option<Rc<dyn Fn()>>>,
    me: RefCell<Weak<ZaloEngine>>,
    /// Visual tests: renderer calls answered from fixtures instead of a webview.
    pub fake_renderer: RefCell<Option<Rc<crate::fixtures::FakeRenderer>>>,
}

impl ZaloEngine {
    pub fn new(exec: Exec, paths: XdgPaths, services: Services) -> Rc<Self> {
        let engine = Rc::new(Self {
            exec: exec.clone(),
            paths: paths.clone(),
            state: RefCell::new(EngineState { phase: Some(Phase::Starting), ..Default::default() }),
            runtime: RefCell::new(None),
            services,
            listeners: RefCell::new(Vec::new()),
            installing: Cell::new(false),
            login_timeout_gen: Cell::new(0),
            typing_gen: RefCell::new(HashMap::new()),
            media_resolver: crate::media::MediaResolver::new(3),
            notification_settings: RefCell::new(crate::notifications::NotificationSettingsState {
                shows_preview: olaz_main::Settings::load(&paths).get_bool("olaz.notifications.showsPreview").unwrap_or(true),
                ..Default::default()
            }),
            notifier: crate::notifications::Notifier::new(),
            window_state: RefCell::new((true, true, std::time::Instant::now())),
            on_activate: RefCell::new(None),
            me: RefCell::new(Weak::new()),
            fake_renderer: RefCell::new(None),
        });
        *engine.me.borrow_mut() = Rc::downgrade(&engine);
        engine.notifier.connect(&exec);
        engine
    }

    pub(crate) fn weak(&self) -> Weak<ZaloEngine> {
        self.me.borrow().clone()
    }

    pub fn subscribe(&self, f: impl Fn(&Change) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    pub(crate) fn changed(&self, change: Change) {
        let listeners: Vec<Listener> = self.listeners.borrow().clone();
        for l in listeners {
            l(&change);
        }
    }

    pub fn phase(&self) -> Phase {
        self.state.borrow().phase.clone().unwrap_or(Phase::Starting)
    }

    pub(crate) fn set_phase(&self, phase: Phase) {
        if self.state.borrow().phase.as_ref() == Some(&phase) {
            return;
        }
        self.state.borrow_mut().phase = Some(phase);
        self.changed(Change::Phase);
    }

    pub fn host(&self) -> Option<Rc<ClientHost>> {
        self.runtime.borrow().as_ref().map(|r| r.host.clone())
    }

    pub fn main_process(&self) -> Option<Rc<MainProcess>> {
        self.runtime.borrow().as_ref().map(|r| r.main.clone())
    }

    pub fn media_loader(&self) -> Option<Arc<olaz_media::MediaLoader>> {
        self.services.media.clone()
    }

    pub fn open_url(&self, url: &str) {
        (self.services.open_url)(url);
    }

    pub(crate) fn log(&self, line: &str) {
        let mut s = self.state.borrow_mut();
        s.recent_log.push(line.to_string());
        let n = s.recent_log.len();
        if n > 200 {
            s.recent_log.drain(..n - 200);
        }
    }

    pub fn runtime_layout(&self) -> Layout {
        Layout::new(&self.paths.runtime_root())
    }

    // MARK: lifecycle

    pub fn start(self: &Rc<Self>) {
        if self.runtime.borrow().is_some() || self.installing.get() {
            return;
        }
        let layout = self.runtime_layout();
        olaz_runtime::prepare_for_launch(&layout);
        let current = layout.current();
        if !olaz_runtime::is_complete(&current) {
            self.install_runtime();
            return;
        }
        let version = olaz_runtime::version_of(&current).unwrap_or_else(|| "?".into());
        self.boot(&current, &version);
        self.upgrade_runtime_if_outdated(&version);
    }

    pub fn retry_runtime_setup(self: &Rc<Self>) {
        if let Phase::Preparing(setup) = self.phase() {
            if setup.failure.is_some() {
                self.install_runtime();
            }
        }
    }

    fn install_runtime(self: &Rc<Self>) {
        self.set_phase(Phase::Preparing(RuntimeSetup::default()));
        self.installing.set(true);
        // Progress comes from I/O threads: hop to the executor in order, never over a later failure.
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Step>();
        let weak = Rc::downgrade(self);
        spawn(&self.exec, async move {
            use futures::StreamExt;
            while let Some(step) = rx.next().await {
                let Some(me) = weak.upgrade() else { break };
                if let Phase::Preparing(mut setup) = me.phase() {
                    if setup.failure.is_none() {
                        setup.step = step;
                        me.set_phase(Phase::Preparing(setup));
                    }
                }
            }
        });
        let layout = self.runtime_layout();
        let installer = Installer::new(layout.work(), Arc::new(move |step| {
            let _ = tx.unbounded_send(step);
        }));
        let weak = Rc::downgrade(self);
        spawn(&self.exec, async move {
            let current = layout.current();
            let result = installer.install(&current).await;
            let Some(me) = weak.upgrade() else { return };
            me.installing.set(false);
            match result {
                Ok(version) => {
                    olaz_core::log::write(format!("runtime: installed Zalo {version}"));
                    me.boot(&current, &version);
                }
                Err(e) => {
                    olaz_core::log::write(format!("runtime: install failed — {e}"));
                    if let Phase::Preparing(mut setup) = me.phase() {
                        setup.failure = Some(e.to_string());
                        me.set_phase(Phase::Preparing(setup));
                    }
                }
            }
        });
    }

    /// A runtime older than the pinned version is upgraded quietly into `pending` (used from the next launch).
    fn upgrade_runtime_if_outdated(&self, installed: &str) {
        let pinned = olaz_runtime::pinned().version;
        if !olaz_runtime::is_newer(&pinned, installed) {
            return;
        }
        let layout = self.runtime_layout();
        let mut installer = Installer::new(layout.work().join("upgrade"), Arc::new(|_| {}));
        installer.allows_latest = false;
        let installed = installed.to_string();
        spawn(&self.exec, async move {
            match installer.install(&layout.pending()).await {
                Ok(v) => olaz_core::log::write(format!("runtime: Zalo {v} ready, used from the next launch (now {installed})")),
                Err(e) => olaz_core::log::write(format!("runtime: upgrade to Zalo {pinned} failed — {e}")),
            }
        });
    }

    fn boot(self: &Rc<Self>, app_dir: &std::path::Path, version: &str) {
        self.state.borrow_mut().client_version = Some(version.to_string());
        let config = WebHostConfig {
            binary: self.services.webhost_binary.clone(),
            asar: app_dir.join("Contents/Resources/app.asar"),
            data_dir: self.paths.webkit_data(),
            cache_dir: self.paths.cache().join("webkit"),
            debug: self.services.debug,
        };
        let webhost = match WebHostProcess::spawn(config, self.exec.clone()) {
            Ok(w) => w,
            Err(e) => {
                olaz_core::log::write(format!("host: cannot start olaz-webhost: {e}"));
                self.set_phase(Phase::Preparing(RuntimeSetup { step: Step::Installing, failure: Some(format!("Không chạy được olaz-webhost: {e}")) }));
                return;
            }
        };
        let open = self.services.open_url.clone();
        *webhost.on_open_external.borrow_mut() = Some(Rc::new(move |u| open(&u)));
        let weak = self.weak();
        *webhost.diagnostics.borrow_mut() = Some(Rc::new(move |line| {
            if let Some(me) = weak.upgrade() {
                me.log(&line);
            }
        }));

        let platform = Rc::new(ZaloPlatform::new(version, self.paths.clone()));
        let jar = olaz_core::cookies::CookieJar::persistent(&self.paths.cookies_file());
        let main = MainProcess::new(platform.clone(), jar, self.services.secrets.clone(), webhost.clone(), self.exec.clone());
        if let Some(badge) = &self.services.badge {
            *main.badge.borrow_mut() = Some(badge.clone());
        }
        crate::modules::install_modules(&main, self);
        *main.configure_worker.borrow_mut() = Some(Rc::new(configure_worker_logging));
        let host = main.create_window();
        main.set_host(&host);

        let weak = self.weak();
        *main.on_event.borrow_mut() = Some(Rc::new(move |e| {
            if let Some(me) = weak.upgrade() {
                me.handle_main_event(e);
            }
        }));
        let weak = self.weak();
        *host.on_page_event.borrow_mut() = Some(Rc::new(move |e| {
            if let Some(me) = weak.upgrade() {
                me.handle_page_event(e);
            }
        }));
        let weak = self.weak();
        *host.on_state.borrow_mut() = Some(Rc::new(move |s| {
            if let Some(me) = weak.upgrade() {
                me.handle_state(s);
            }
        }));
        let weak = self.weak();
        *host.on_console.borrow_mut() = Some(Rc::new(move |line: String| {
            if let Some(me) = weak.upgrade() {
                me.log(&line);
            }
            // Official log/info lines can carry payloads: only warnings/errors and Olaz's own diagnostics reach disk.
            let official = line.starts_with("[log]") || line.starts_with("[info]");
            if !official || line.contains("[adapter]") || line.contains("[chat]") || line.contains("[transfer]") {
                olaz_core::log::write(format!("console {}", line.chars().take(300).collect::<String>()));
            }
        }));
        *host.bridge.trace.borrow_mut() = Some(Rc::new(|key: &str, args: &[olaz_core::BV]| {
            if !key.contains("ZLog") && !key.starts_with("$zelectron.store") {
                olaz_core::log::write(format!("bridge {key}{}", olaz_core::log::shape(args)));
            }
        }));
        *host.bridge.on_handler_error.borrow_mut() = Some(Rc::new(|key: &str, e: &str| olaz_core::log::write(format!("bridge-error {key}: {e}"))));
        *host.network.on_request_completed.borrow_mut() = Some(Rc::new(|m: &str, u: &url::Url, s: i64| {
            olaz_core::log::write(format!("net {s} {m} {}{}", u.host_str().unwrap_or(""), u.path()))
        }));
        *host.network.socket_trace.borrow_mut() = Some(Rc::new(|l| olaz_core::log::write(l)));
        let weak = self.weak();
        *host.network.response_observer.borrow_mut() = Some(Rc::new(move |u: &url::Url, status: u16, body: &[u8]| {
            if let Some(me) = weak.upgrade() {
                me.observe(u, status, body);
            }
        }));

        olaz_core::log::write(format!("engine start — Zalo {version}, session cookie: {}", main.has_session()));
        let has_session = main.has_session();
        *self.runtime.borrow_mut() = Some(Runtime { platform, main: main.clone(), host, webhost });
        crate::network_monitor::start(self);
        crate::dev::start_if_enabled(self);
        self.set_phase(if has_session { Phase::SigningIn } else { Phase::Login(LoginState::default()) });
        main.start();
    }

    /// Fresh QR (the official page regenerates on load).
    pub fn refresh_qr(&self) {
        let Some(rt) = self.runtime.borrow().as_ref().map(|r| (r.host.clone(), r.main.clone())) else { return };
        self.set_phase(Phase::Login(LoginState::default()));
        rt.0.load(Page::Login, &rt.1.page_query(false));
    }

    /// Clears the session cookies and local client data, then shows a fresh QR.
    pub fn log_out(&self) {
        if let Some(main) = self.main_process() {
            main.jar.clear_zalo();
            main.reset_account_state();
        }
        self.reset_account();
        self.refresh_qr();
    }

    fn reset_account(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.profile = None;
            // Unsent text of the previous account must not show up in the next one.
            s.failed_drafts = FailedDraftStore::default();
            s.messages.clear();
            s.conversations.clear();
            s.open_conversation_id = None;
            s.conversation_extras = ConversationExtras::default();
        }
        self.reset_message_actions();
        self.reset_conversation_actions();
        if std::mem::take(&mut self.state.borrow_mut().badge) != 0 {
            crate::badge::publish(0);
        }
        self.state.borrow_mut().contacts = Default::default();
        self.reset_stickers();
        self.reset_polls();
        self.reset_card_state();
        self.reset_cloud();
        self.changed(Change::Profile);
        self.changed(Change::Conversations);
    }

    pub fn show_debug_renderer(&self) {
        if let Some(h) = self.host() {
            h.show_debug_window();
        }
    }

    // MARK: events from the runtime

    fn handle_main_event(&self, event: MainEvent) {
        olaz_core::log::write(format!("main-process {event:?}"));
        match event {
            MainEvent::LoginSucceeded => {
                self.login_timeout_gen.set(self.login_timeout_gen.get() + 1);
                self.state.borrow_mut().sign_out_notice = None;
                self.set_phase(Phase::SigningIn);
            }
            MainEvent::LoggedOut => {
                self.reset_account();
                self.set_phase(Phase::Login(LoginState::default()));
            }
            MainEvent::Badge(n) => {
                if self.state.borrow().badge != n {
                    crate::badge::publish(n);
                }
                self.state.borrow_mut().badge = n;
                self.changed(Change::Other);
            }
        }
    }

    fn handle_page_event(&self, event: PageEvent) {
        olaz_core::log::write(format!("page {event:?}"));
        let main = self.main_process();
        match event {
            PageEvent::StartedLoading => {
                if let Some(m) = main {
                    m.page_did_start_loading();
                }
                // The page that asked is gone: its confirms can no longer be answered.
                self.drop_composer_confirms();
            }
            PageEvent::FinishedLoading(page) => {
                if let Some(m) = main {
                    m.page_did_finish_loading(page);
                }
                self.republish_connection();
                if page == Page::Main {
                    self.republish_conversation_actions();
                    self.rewatch_polls();
                    self.rewatch_pins();
                }
                if page == Page::Main && self.phase() == Phase::SigningIn {
                    // The main client is up; conversations stream in as it syncs.
                    self.set_phase(Phase::Ready);
                }
            }
            PageEvent::Failed(e) => {
                self.log(&format!("[navigation] {e}"));
                self.changed(Change::Log);
            }
        }
    }

    /// Snapshot routing — first match wins.
    pub fn handle_state(&self, state: Map<String, Value>) {
        let mut keys: Vec<&String> = state.keys().collect();
        keys.sort();
        olaz_core::log::write(format!(
            "state page={} event={} keys={:?}",
            state.get("page").and_then(Value::as_str).unwrap_or("-"),
            state.get("event").and_then(Value::as_str).unwrap_or("-"),
            keys
        ));
        let page = state.get("page").and_then(Value::as_str).unwrap_or("");
        match page {
            "login" => self.handle_login_snapshot(&state),
            "main" => {
                // `state[key] != nil` in Swift: a present key routes even when its value is null (e.g. `transfer: null`).
                for key in crate::modules::FEATURE_KEYS {
                    if state.contains_key(*key) {
                        if *key == "conversationExtras" {
                            self.handle_conversation_extras(&state);
                        } else {
                            crate::modules::route_feature_snapshot(self, key, &state);
                        }
                        return;
                    }
                }
                if state.contains_key("chat") || state.contains_key("typing") {
                    self.handle_chat(&state);
                } else if state.contains_key("conversations") {
                    let mut rows: Vec<ConversationSummary> = state
                        .get("conversations")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(ConversationSummary::from_json).collect())
                        .unwrap_or_default();
                    rows.sort_by(list_order);
                    if self.state.borrow().conversations != rows {
                        self.state.borrow_mut().conversations = rows;
                        self.changed(Change::Conversations);
                    }
                } else if let Some(user) = state.get("user").filter(|u| u.is_object()) {
                    let id = user.s("userId");
                    if !id.is_empty() {
                        let display = user.opt_s("displayName").or_else(|| user.opt_s("zaloName")).unwrap_or_default();
                        let profile = Profile {
                            user_id: id,
                            display_name: display,
                            avatar: user.opt_s("avatar").filter(|a| url::Url::parse(a).is_ok()),
                            phone: user.opt_s("phoneNumber"),
                        };
                        if self.state.borrow().profile.as_ref() != Some(&profile) {
                            self.state.borrow_mut().profile = Some(profile);
                            self.changed(Change::Profile);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_login_snapshot(&self, state: &Map<String, Value>) {
        let Phase::Login(mut login) = self.phase() else { return };
        // The official page gave up on this QR: a scan would do nothing. Only from waiting-scan.
        if state.get("invalid").and_then(Value::as_bool) == Some(true) && login.step == LoginStep::WaitingScan {
            self.login_timeout_gen.set(self.login_timeout_gen.get() + 1);
            login.step = LoginStep::Expired;
            self.set_phase(Phase::Login(login));
            return;
        }
        if let Some(png) = state.get("qr").and_then(Value::as_str).and_then(decode_data_url) {
            login.qr_png = Some(png);
            match state.get("event").and_then(Value::as_str) {
                Some("qr") => {
                    login.step = LoginStep::WaitingScan;
                    login.is_second_qr = false;
                }
                Some("qr2") => {
                    login.step = LoginStep::WaitingScan;
                    login.is_second_qr = true;
                }
                _ => {}
            }
            self.set_phase(Phase::Login(login));
            self.schedule_expiry();
        }
    }

    /// Fallback only: 100 s after a QR, a still-waiting scan is expired.
    fn schedule_expiry(&self) {
        let generation = self.login_timeout_gen.get() + 1;
        self.login_timeout_gen.set(generation);
        let weak = self.weak();
        let exec = self.exec.clone();
        spawn(&self.exec, async move {
            exec.timer(Duration::from_secs(100)).await;
            let Some(me) = weak.upgrade() else { return };
            if me.login_timeout_gen.get() != generation {
                return;
            }
            if let Phase::Login(mut login) = me.phase() {
                if login.step == LoginStep::WaitingScan {
                    login.step = LoginStep::Expired;
                    me.set_phase(Phase::Login(login));
                }
            }
        });
    }

    /// Every proxied response: QR long-polls and the kick-out notice.
    pub fn observe(&self, url: &url::Url, status: u16, body: &[u8]) {
        if url.path().ends_with("/api/session/kickout-info") {
            olaz_core::log::write("session kicked by server");
            self.state.borrow_mut().sign_out_notice = Some(KICKOUT_NOTICE.into());
            self.changed(Change::Notice);
            return;
        }
        if !url.host_str().is_some_and(|h| h.starts_with("qr-id.")) {
            return;
        }
        let Phase::Login(mut login) = self.phase() else { return };
        if status != 200 {
            return;
        }
        let Ok(Value::Object(json)) = serde_json::from_slice::<Value>(body) else { return };
        let data = json.get("data").filter(|d| d.is_object()).cloned().unwrap_or_else(|| Value::Object(json.clone()));
        let code = json.get("error_code").and_then(Value::as_f64).map(|f| f as i64).unwrap_or(0);
        let mut keys: Vec<String> = data.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        keys.sort();
        olaz_core::log::write(format!("qr-poll {} error_code={code} data.keys={keys:?}", url.path()));
        if code == SECOND_QR_REQUIRED_CODE {
            return;
        }
        let name = data.s("display_name");
        if !name.is_empty() {
            login.step = LoginStep::WaitingConfirm { name, avatar: data.opt_s("avatar").filter(|a| url::Url::parse(a).is_ok()) };
            self.set_phase(Phase::Login(login));
        } else if code != 0 {
            let message = json.get("error_message").and_then(Value::as_str).unwrap_or("");
            if !message.is_empty() {
                login.step = LoginStep::Failed(message.to_string());
                self.set_phase(Phase::Login(login));
            }
        }
    }

    /// Runs an async function body in the main page; `None` on failure or no page.
    pub fn evaluate(&self, script: &str) -> olaz_core::exec::LocalBoxFuture<Option<Value>> {
        if let Some(fake) = self.fake_renderer.borrow().clone() {
            return match fake.answer(script) {
                Some(v) => Box::pin(async move { Some(v) }),
                None => Box::pin(futures::future::pending()),
            };
        }
        match self.host() {
            Some(h) => h.evaluate(script),
            None => Box::pin(async { None }),
        }
    }

    /// `window.<object>[method](...args)`, fire-and-forget; arguments JSON-encoded.
    pub fn call_chat(&self, method: &str, args: Vec<Value>, object: &str) {
        let json = Value::Array(args).to_string();
        let script = format!(
            "return (() => {{ const c = window.{object}; if (!c || !c.{method}) return 'missing'; c.{method}(...{json}); return 'ok'; }})()"
        );
        let fut = self.evaluate(&script);
        let method = method.to_string();
        spawn(&self.exec, async move {
            let result = fut.await;
            if result.as_ref().and_then(Value::as_str) != Some("ok") {
                olaz_core::log::write(format!("chat {method} → {}", result.map(|v| v.to_string()).unwrap_or_else(|| "nil".into())));
            }
        });
    }

    /// Awaited call: `return await c.<method>(...args)` with a fallback when the adapter is missing.
    pub fn await_call(&self, object: &str, method: &str, args: Vec<Value>, fallback: &str) -> olaz_core::exec::LocalBoxFuture<Option<Value>> {
        let json = Value::Array(args).to_string();
        let script = format!("const c = window.{object}; if (!c || !c.{method}) return {fallback}; return await c.{method}(...{json});");
        self.evaluate(&script)
    }

    pub fn shutdown(&self) {
        if let Some(rt) = self.runtime.borrow_mut().take() {
            rt.main.stop_shared_worker();
            rt.host.shutdown();
            rt.main.jar.flush_now();
            rt.webhost.quit();
        }
    }
}

fn configure_worker_logging(worker: &Rc<ClientHost>) {
    *worker.on_console.borrow_mut() = Some(Rc::new(|line: String| {
        let official = line.starts_with("[log]") || line.starts_with("[info]");
        if line.contains("do open indexeddb") {
            return;
        }
        if !official || line.contains("[adapter]") || line.contains("[chat]") || line.contains("[transfer]") {
            olaz_core::log::write(format!("sw-console {}", line.chars().take(300).collect::<String>()));
        }
    }));
    *worker.bridge.trace.borrow_mut() = Some(Rc::new(|key: &str, args: &[olaz_core::BV]| {
        if !key.contains("ZLog") && !key.starts_with("$zelectron.store") {
            olaz_core::log::write(format!("sw-bridge {key}{}", olaz_core::log::shape(args)));
        }
    }));
    *worker.bridge.on_handler_error.borrow_mut() = Some(Rc::new(|k: &str, e: &str| olaz_core::log::write(format!("sw-bridge-error {k}: {e}"))));
    *worker.network.on_request_completed.borrow_mut() = Some(Rc::new(|m: &str, u: &url::Url, s: i64| {
        olaz_core::log::write(format!("sw-net {s} {m} {}{}", u.host_str().unwrap_or(""), u.path()))
    }));
    *worker.network.socket_trace.borrow_mut() = Some(Rc::new(|l| olaz_core::log::write(format!("sw-{l}"))));
    let previous = worker.on_page_event.borrow().clone();
    *worker.on_page_event.borrow_mut() = Some(Rc::new(move |e| {
        olaz_core::log::write(format!("sw-page {e:?}"));
        if let Some(p) = &previous {
            p(e);
        }
    }));
}

/// `data:<mime>;base64,<payload>` → bytes.
pub fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    if !url.starts_with("data:") {
        return None;
    }
    let comma = url.find(',')?;
    base64::engine::general_purpose::STANDARD.decode(&url[comma + 1..]).ok()
}
