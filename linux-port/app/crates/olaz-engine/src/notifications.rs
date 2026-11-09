//! Notifications (delivery over D-Bus, settings and mutes). Port of `NativeNotifier.swift` and
//! `ZaloEngine+Notifications.swift`. Delivery: `org.freedesktop.Notifications` over D-Bus (zbus).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use chrono::{DateTime, Local, TimeZone, Timelike};
use olaz_core::exec::spawn;
use olaz_core::value::{Args, BV};
use olaz_host::ZaloBridge;
use olaz_main::{BridgeModule, MainProcess};
use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::{ComposerDraft, J};

// MARK: mute

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteDuration {
    OneHour = 1,
    FourHours = 2,
    Until8AM = 3,
    UntilReopened = 4,
}

pub const MUTE_DURATIONS: [MuteDuration; 4] = [MuteDuration::OneHour, MuteDuration::FourHours, MuteDuration::Until8AM, MuteDuration::UntilReopened];

impl MuteDuration {
    pub fn title(self) -> &'static str {
        match self {
            MuteDuration::OneHour => "Trong 1 giờ",
            MuteDuration::FourHours => "Trong 4 giờ",
            MuteDuration::Until8AM => "Cho đến 8:00 AM",
            MuteDuration::UntilReopened => "Cho đến khi được mở lại",
        }
    }

    /// When the mute ends (seconds since epoch); None = until reopened.
    pub fn expiry(self, now: DateTime<Local>) -> Option<f64> {
        let t = now.timestamp_millis() as f64 / 1000.0;
        match self {
            MuteDuration::OneHour => Some(t + 3600.0),
            MuteDuration::FourHours => Some(t + 4.0 * 3600.0),
            MuteDuration::Until8AM => {
                let today8 = now.with_hour(8).and_then(|d| d.with_minute(0)).and_then(|d| d.with_second(0)).and_then(|d| d.with_nanosecond(0))?;
                let next = if today8 > now { today8 } else { today8 + chrono::Duration::days(1) };
                Some(next.timestamp() as f64)
            }
            MuteDuration::UntilReopened => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MuteInfo {
    /// seconds
    pub start_time: f64,
    /// seconds; −1 = until reopened
    pub duration: f64,
}

impl MuteInfo {
    pub fn is_indefinite(&self) -> bool {
        self.duration < 0.0
    }
    pub fn until(&self) -> Option<f64> {
        (!self.is_indefinite()).then_some(self.start_time + self.duration)
    }
    pub fn is_active(&self, now_s: f64) -> bool {
        self.until().is_none_or(|u| u > now_s)
    }

    /// "Cho đến 14:30" / "Cho đến 14:30 ngày 27/10" / "Cho đến khi được mở lại".
    pub fn description(&self, now: DateTime<Local>) -> String {
        let Some(until) = self.until() else { return "Cho đến khi được mở lại".into() };
        let u = Local.timestamp_opt(until as i64, 0).single().unwrap_or(now);
        let time = format!("{:02}:{:02}", u.hour(), u.minute());
        if u.date_naive() == now.date_naive() {
            format!("Cho đến {time}")
        } else {
            format!("Cho đến {time} ngày {}", u.format("%d/%m"))
        }
    }

    pub fn status_text(&self, now: DateTime<Local>) -> String {
        if self.until().is_none() {
            return "Đang tắt cho đến khi được mở lại".into();
        }
        let d = self.description(now);
        format!("Đang tắt đến{}", d.trim_start_matches("Cho đến"))
    }

    pub fn parse(json: &Value) -> Option<(String, MuteInfo)> {
        let id = json.s("id");
        if id.is_empty() {
            return None;
        }
        Some((id, MuteInfo { start_time: json.opt_n("startTime").unwrap_or(0.0), duration: json.opt_n("duration").unwrap_or(-1.0) }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Authorization {
    Unknown,
    Unavailable,
    NotDetermined,
    Denied,
    Authorized,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationSettingsState {
    pub is_loaded: bool,
    pub enabled: bool,
    pub sound: bool,
    pub has_sound_setting: bool,
    pub shows_preview: bool,
    pub authorization: Authorization,
    pub mutes: HashMap<String, MuteInfo>,
}

impl Default for NotificationSettingsState {
    fn default() -> Self {
        Self { is_loaded: false, enabled: true, sound: false, has_sound_setting: false, shows_preview: true, authorization: Authorization::Unknown, mutes: HashMap::new() }
    }
}

pub fn now_s() -> f64 {
    crate::chat::now_ms() / 1000.0
}

impl NotificationSettingsState {
    pub fn mute_for(&self, id: &str, now: f64) -> Option<MuteInfo> {
        self.mutes.get(id).copied().filter(|m| m.is_active(now))
    }
    pub fn is_muted(&self, id: &str) -> bool {
        self.mute_for(id, now_s()).is_some()
    }
    pub fn active_mutes(&self, now: f64) -> Vec<(String, MuteInfo)> {
        let mut v: Vec<(String, MuteInfo)> = self.mutes.iter().filter(|(_, m)| m.is_active(now)).map(|(k, m)| (k.clone(), *m)).collect();
        v.sort_by(|a, b| a.1.start_time.partial_cmp(&b.1.start_time).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
        v
    }
    pub fn apply(&mut self, json: &Value) {
        self.is_loaded = true;
        if let Some(v) = json.opt_b("enabled") {
            self.enabled = v;
        }
        if let Some(v) = json.opt_b("sound") {
            self.sound = v;
        }
        if let Some(v) = json.opt_b("hasSoundSetting") {
            self.has_sound_setting = v;
        }
        self.mutes = json.arr("mutes").iter().filter_map(MuteInfo::parse).collect();
    }
}

// MARK: request & policy

#[derive(Debug, Clone, PartialEq)]
pub enum Icon {
    Remote(String),
    Inline(Vec<u8>),
    Bundled(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    pub tag: String,
    pub silent: bool,
    pub icon: Option<Icon>,
}

impl NotificationRequest {
    pub fn parse(args: &[BV]) -> Option<Self> {
        let title = args.string(0).unwrap_or_default();
        let empty = Default::default();
        let options = args.dict(1).unwrap_or(&empty);
        let body = options.get("body").and_then(BV::as_str).unwrap_or("").to_string();
        if title.is_empty() && body.is_empty() {
            return None;
        }
        let tag = match options.get("tag") {
            Some(BV::String(s)) => s.clone(),
            Some(BV::Number(n)) => olaz_core::value::js_number_string(n),
            _ => String::new(),
        };
        Some(Self {
            title,
            body,
            tag,
            silent: options.get("silent").and_then(BV::as_bool).unwrap_or(false),
            icon: options.get("icon").and_then(BV::as_str).and_then(parse_icon),
        })
    }
}

pub fn parse_icon(raw: &str) -> Option<Icon> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with("data:") {
        let comma = text.find(',')?;
        if !text[..comma].contains(";base64") {
            return None;
        }
        let data = base64::engine::general_purpose::STANDARD.decode(&text[comma + 1..]).ok().filter(|d| !d.is_empty())?;
        return Some(Icon::Inline(data));
    }
    if let Ok(u) = url::Url::parse(text) {
        if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() {
            return Some(Icon::Remote(text.to_string()));
        }
    }
    if text.contains("://") {
        return None;
    }
    let path = text.split('?').next().unwrap_or(text);
    (!path.is_empty()).then(|| Icon::Bundled(path.to_string()))
}

pub fn should_suppress(app_is_active: bool, window_visible: bool, open_conversation: Option<&str>, tag: &str, idle_seconds: f64) -> bool {
    app_is_active && window_visible && !tag.is_empty() && open_conversation == Some(tag) && idle_seconds < 180.0
}

pub const HIDDEN_PREVIEW_BODY: &str = "Bạn có tin nhắn mới";

pub fn body(body: &str, shows_preview: bool) -> String {
    if shows_preview { body.to_string() } else { HIDDEN_PREVIEW_BODY.to_string() }
}

pub fn attachment_ext(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if data.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
        Some("gif")
    } else {
        None
    }
}

pub fn is_sound_url_allowed(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(olaz_media::is_zalo_cdn) && u.path().ends_with("/sound/noti.mp3"))
}

// MARK: D-Bus delivery

#[derive(Debug)]
pub enum BusEvent {
    Action { id: u32, action: String },
    Replied { id: u32, text: String },
    Closed { id: u32 },
}

struct Live {
    bridge: Weak<ZaloBridge>,
    tag: String,
    dbus_id: Option<u32>,
}

pub struct Notifier {
    engine: RefCell<Weak<ZaloEngine>>,
    live: RefCell<HashMap<String, Live>>,
    /// conversation → delivered D-Bus ids (to withdraw).
    delivered: RefCell<HashMap<String, Vec<u32>>>,
    by_dbus: RefCell<HashMap<u32, (String, String)>>,
    connection: RefCell<Option<zbus::Connection>>,
    pub available: std::cell::Cell<bool>,
    inline_reply: std::cell::Cell<bool>,
    last_activity_push: std::cell::Cell<Option<Instant>>,
    orphan_drafts: RefCell<crate::models::FailedDraftStore>,
}

impl Notifier {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            engine: RefCell::new(Weak::new()),
            live: RefCell::default(),
            delivered: RefCell::default(),
            by_dbus: RefCell::default(),
            connection: RefCell::new(None),
            available: std::cell::Cell::new(false),
            inline_reply: std::cell::Cell::new(false),
            last_activity_push: std::cell::Cell::new(None),
            orphan_drafts: RefCell::default(),
        })
    }

    /// Connects to the session bus, reads capabilities and starts listening to signals.
    pub fn connect(self: &Rc<Self>, exec: &olaz_core::exec::Exec) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Result<(zbus::Connection, bool), String>>();
        let (etx, mut erx) = futures::channel::mpsc::unbounded::<BusEvent>();
        olaz_host::net::runtime().spawn(async move {
            let result = async {
                let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;
                let proxy = notifications_proxy(&conn).await?;
                let caps: Vec<String> = proxy.call("GetCapabilities", &()).await.map_err(|e| e.to_string())?;
                Ok::<_, String>((conn, caps.iter().any(|c| c == "inline-reply")))
            }
            .await;
            let conn = result.as_ref().ok().map(|(c, _)| c.clone());
            let _ = tx.unbounded_send(result);
            if let Some(conn) = conn {
                listen(conn, etx).await;
            }
        });
        let weak = Rc::downgrade(self);
        spawn(exec, async move {
            use futures::StreamExt;
            if let Some(result) = rx.next().await {
                let Some(me) = weak.upgrade() else { return };
                match result {
                    Ok((conn, inline)) => {
                        *me.connection.borrow_mut() = Some(conn);
                        me.available.set(true);
                        me.inline_reply.set(inline);
                    }
                    Err(e) => olaz_core::log::write(format!("notifications: unavailable ({e})")),
                }
            }
        });
        let weak = Rc::downgrade(self);
        spawn(exec, async move {
            use futures::StreamExt;
            while let Some(ev) = erx.next().await {
                let Some(me) = weak.upgrade() else { return };
                me.handle_bus_event(ev);
            }
        });
    }

    pub fn attach(&self, engine: &Rc<ZaloEngine>) {
        *self.engine.borrow_mut() = Rc::downgrade(engine);
        for (id, draft) in self.orphan_drafts.borrow_mut().drain() {
            engine.state.borrow_mut().failed_drafts.stash(draft, &id, false);
        }
    }

    fn engine(&self) -> Option<Rc<ZaloEngine>> {
        self.engine.borrow().upgrade()
    }

    fn push(&self, channel: &str, id: &str, bridge: Option<Rc<ZaloBridge>>, tag: &str) {
        let bridge = bridge.or_else(|| self.engine().and_then(|e| e.host()).map(|h| h.bridge.clone()));
        if let Some(b) = bridge {
            b.send(channel, vec![BV::obj([("id", BV::str(id)), ("tag", BV::str(tag))])]);
        }
    }

    fn create(self: &Rc<Self>, args: &[BV], bridge: &Rc<ZaloBridge>) -> BV {
        if !self.available.get() {
            return BV::Undefined;
        }
        let Some(req) = NotificationRequest::parse(args) else { return BV::Undefined };
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        self.live.borrow_mut().insert(id.clone(), Live { bridge: Rc::downgrade(bridge), tag: req.tag.clone(), dbus_id: None });
        let me = self.clone();
        let id2 = id.clone();
        spawn(bridge.exec(), async move { me.deliver(req, id2).await });
        BV::String(id)
    }

    fn close(&self, id: &str) {
        let entry = self.live.borrow_mut().remove(id);
        if let Some(e) = entry {
            self.push("_noti_on-close", id, e.bridge.upgrade(), &e.tag);
        }
    }

    async fn deliver(self: Rc<Self>, req: NotificationRequest, id: String) {
        let Some(engine) = self.engine() else {
            self.live.borrow_mut().remove(&id);
            return;
        };
        let (active, visible, idle) = engine.window_state();
        let open = engine.state.borrow().open_conversation_id.clone();
        if should_suppress(active, visible, open.as_deref(), &req.tag, idle) {
            olaz_core::log::write("notification suppressed: conversation is open and the app is in use");
            return;
        }
        let shows_preview = engine.notification_settings().shows_preview;
        let image_path = self.icon_file(&engine, req.icon.clone()).await;
        let Some(conn) = self.connection.borrow().clone() else { return };
        let mut actions = vec!["default".to_string(), "Mở".to_string()];
        if self.inline_reply.get() {
            actions.push("inline-reply".into());
            actions.push("Trả lời".into());
        }
        let title = req.title.clone();
        let body_text = body(&req.body, shows_preview);
        let silent = req.silent;
        let (tx, rx) = futures::channel::oneshot::channel();
        olaz_host::net::runtime().spawn(async move {
            let r = notify(&conn, &title, &body_text, image_path.as_deref(), silent, actions).await;
            let _ = tx.send(r);
        });
        match rx.await {
            Ok(Ok(dbus_id)) => {
                let tag = req.tag.clone();
                if let Some(l) = self.live.borrow_mut().get_mut(&id) {
                    l.dbus_id = Some(dbus_id);
                }
                self.by_dbus.borrow_mut().insert(dbus_id, (id.clone(), tag.clone()));
                self.delivered.borrow_mut().entry(if tag.is_empty() { "zalo".into() } else { tag.clone() }).or_default().push(dbus_id);
                engine.exec.timer(Duration::from_millis(60)).await;
                let entry = self.live.borrow().get(&id).map(|l| (l.bridge.upgrade(), l.tag.clone()));
                if let Some((b, t)) = entry {
                    self.push("_noti_on-show", &id, b, &t);
                }
            }
            other => {
                olaz_core::log::write(format!("notification add failed: {other:?}"));
                self.live.borrow_mut().remove(&id);
            }
        }
    }

    async fn icon_file(&self, engine: &Rc<ZaloEngine>, icon: Option<Icon>) -> Option<String> {
        let data = match icon? {
            Icon::Inline(b) => Some(b),
            Icon::Bundled(p) => engine.reaction_image(&p),
            Icon::Remote(u) => {
                let loader = engine.media_loader()?;
                let fut = loader.data(&u);
                let timer = engine.exec.timer(Duration::from_secs(3));
                futures::pin_mut!(timer);
                match futures::future::select(fut, timer).await {
                    futures::future::Either::Left((Ok(b), _)) => Some(b.to_vec()),
                    _ => None,
                }
            }
        }?;
        let ext = attachment_ext(&data)?;
        let dir = engine.paths.temp().join("notif");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4()));
        std::fs::write(&file, data).ok()?;
        Some(file.to_string_lossy().into_owned())
    }

    /// Withdraws a conversation's delivered notifications (opening it, hiding it).
    pub fn clear_delivered(&self, conversation: &str) {
        let ids = self.delivered.borrow_mut().remove(conversation).unwrap_or_default();
        let Some(conn) = self.connection.borrow().clone() else { return };
        if ids.is_empty() {
            return;
        }
        olaz_host::net::runtime().spawn(async move {
            if let Ok(p) = notifications_proxy(&conn).await {
                for id in ids {
                    let _: Result<(), _> = p.call("CloseNotification", &(id,)).await;
                }
            }
        });
    }

    fn handle_bus_event(&self, ev: BusEvent) {
        let (dbus_id, action, text) = match ev {
            BusEvent::Action { id, action } => (id, action, None),
            BusEvent::Replied { id, text } => (id, "inline-reply".to_string(), Some(text)),
            BusEvent::Closed { id } => {
                self.by_dbus.borrow_mut().remove(&id);
                return;
            }
        };
        let Some((id, conv)) = self.by_dbus.borrow().get(&dbus_id).cloned() else { return };
        let entry = self.live.borrow_mut().remove(&id);
        let engine = self.engine();
        match (action.as_str(), text) {
            ("inline-reply", Some(text)) if !conv.is_empty() => {
                if let Some(engine) = engine.clone() {
                    let me_conv = conv.clone();
                    let e2 = engine.clone();
                    spawn(&engine.exec, async move {
                        let refused = e2.conversation_locked(&me_conv);
                        if !refused && e2.send_text(&text, &me_conv, None, &[]).await {
                            return;
                        }
                        e2.state.borrow_mut().failed_drafts.stash(ComposerDraft::text(&text), &me_conv, false);
                        e2.changed(Change::Other);
                    });
                } else {
                    self.orphan_drafts.borrow_mut().stash(ComposerDraft::text(&text), &conv, false);
                }
            }
            _ => {
                if let Some(engine) = &engine {
                    engine.activate_app();
                    if !conv.is_empty() {
                        engine.state.borrow_mut().navigation_request = Some(conv.clone());
                        engine.changed(Change::Other);
                    }
                }
                let b = entry.as_ref().and_then(|e| e.bridge.upgrade());
                self.push("_noti_on-click", &id, b, &conv);
            }
        }
        if let Some(e) = entry {
            self.push("_noti_on-close", &id, e.bridge.upgrade(), &e.tag);
        }
        self.clear_delivered(&conv);
    }

    /// User input (mouse/keys/scroll) → at most every 2 s `__zaloNotification.activity()`.
    pub fn note_user_activity(&self) {
        let now = Instant::now();
        if self.last_activity_push.get().is_some_and(|t| now.duration_since(t) < Duration::from_secs(2)) {
            return;
        }
        self.last_activity_push.set(Some(now));
        if let Some(e) = self.engine() {
            e.call_chat("activity", vec![], "__zaloNotification");
        }
    }
}

async fn notifications_proxy(conn: &zbus::Connection) -> Result<zbus::Proxy<'static>, String> {
    zbus::Proxy::new(conn, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")
        .await
        .map_err(|e| e.to_string())
}

async fn notify(conn: &zbus::Connection, title: &str, body: &str, image: Option<&str>, silent: bool, actions: Vec<String>) -> Result<u32, String> {
    use zbus::zvariant::Value as V;
    let proxy = notifications_proxy(conn).await?;
    let mut hints: HashMap<&str, V<'_>> = HashMap::new();
    hints.insert("category", V::from("im.received"));
    hints.insert("desktop-entry", V::from(olaz_core::APP_ID));
    if silent {
        hints.insert("suppress-sound", V::from(true));
    } else {
        hints.insert("sound-name", V::from("message-new-instant"));
    }
    if let Some(p) = image {
        hints.insert("image-path", V::from(format!("file://{p}")));
    }
    let actions_ref: Vec<&str> = actions.iter().map(String::as_str).collect();
    let id: u32 = proxy
        .call("Notify", &("Olaz", 0u32, image.unwrap_or(olaz_core::APP_ID), title, body, actions_ref, hints, -1i32))
        .await
        .map_err(|e| e.to_string())?;
    Ok(id)
}

async fn listen(conn: zbus::Connection, tx: futures::channel::mpsc::UnboundedSender<BusEvent>) {
    use futures::StreamExt;
    let Ok(proxy) = notifications_proxy(&conn).await else { return };
    let (Ok(mut actions), Ok(mut closed)) = (proxy.receive_signal("ActionInvoked").await, proxy.receive_signal("NotificationClosed").await) else {
        return;
    };
    let mut replied = proxy.receive_signal("NotificationReplied").await.ok();
    loop {
        tokio::select! {
            Some(m) = actions.next() => {
                if let Ok((id, action)) = m.body().deserialize::<(u32, String)>() {
                    let _ = tx.unbounded_send(BusEvent::Action { id, action });
                }
            }
            Some(m) = closed.next() => {
                if let Ok((id, _reason)) = m.body().deserialize::<(u32, u32)>() {
                    let _ = tx.unbounded_send(BusEvent::Closed { id });
                }
            }
            Some(m) = async { match replied.as_mut() { Some(r) => r.next().await, None => futures::future::pending().await } } => {
                if let Ok((id, text)) = m.body().deserialize::<(u32, String)>() {
                    let _ = tx.unbounded_send(BusEvent::Replied { id, text });
                }
            }
            else => break,
        }
    }
}

/// `_noti_*` IPC channels on every window's bridge.
pub struct NotifierModule(pub Rc<Notifier>);

impl BridgeModule for NotifierModule {
    fn install(&self, bridge: &Rc<ZaloBridge>, _main: &Rc<MainProcess>) {
        let n = self.0.clone();
        bridge.handle_now("_noti_is-supported", move |_| Ok(BV::Bool(n.available.get())));
        let (n, wb) = (self.0.clone(), Rc::downgrade(bridge));
        bridge.handle_now("_noti_create", move |args| Ok(wb.upgrade().map(|b| n.create(&args, &b)).unwrap_or(BV::Undefined)));
        let n = self.0.clone();
        bridge.handle_now("_noti_close", move |args| {
            if let Some(id) = args.string(0) {
                n.close(&id);
            }
            Ok(BV::Undefined)
        });
    }
}

// MARK: engine side

impl ZaloEngine {
    pub fn notification_settings(&self) -> NotificationSettingsState {
        self.notification_settings.borrow().clone()
    }

    pub fn set_notifications_enabled(&self, on: bool) {
        {
            let mut s = self.notification_settings.borrow_mut();
            s.enabled = on;
            s.sound = on;
        }
        self.changed(Change::Other);
        self.call_chat("setEnabled", vec![json!(on)], "__zaloNotification");
    }

    pub fn set_notification_sound(&self, on: bool) {
        {
            let mut s = self.notification_settings.borrow_mut();
            s.sound = on;
            s.has_sound_setting = true;
        }
        self.changed(Change::Other);
        self.call_chat("setSound", vec![json!(on)], "__zaloNotification");
    }

    pub fn set_message_preview(&self, shown: bool) {
        self.notification_settings.borrow_mut().shows_preview = shown;
        let mut st = olaz_main::Settings::load(&self.paths);
        st.set("olaz.notifications.showsPreview", Value::Bool(shown));
        st.save();
        self.changed(Change::Other);
    }

    pub fn notification_settings_did_appear(&self) {
        self.call_chat("reconcileDefaults", vec![], "__zaloNotification");
        self.call_chat("refresh", vec![], "__zaloNotification");
    }

    pub fn is_conversation_muted(&self, id: &str) -> bool {
        self.notification_settings.borrow().is_muted(id)
    }

    pub fn mute_conversation(&self, id: &str, d: MuteDuration) {
        let now = Local::now();
        let t = now.timestamp_millis() as f64 / 1000.0;
        let duration = d.expiry(now).map(|u| u - t).unwrap_or(-1.0);
        self.notification_settings.borrow_mut().mutes.insert(id.to_string(), MuteInfo { start_time: t, duration });
        self.changed(Change::Other);
        self.call_chat("mute", vec![json!(id), json!(d as i64)], "__zaloNotification");
    }

    pub fn unmute_conversation(&self, id: &str) {
        self.notification_settings.borrow_mut().mutes.remove(id);
        self.changed(Change::Other);
        self.call_chat("unmute", vec![json!(id)], "__zaloNotification");
    }

    fn apply_authorization(&self, auth: Authorization) {
        self.notification_settings.borrow_mut().authorization = auth;
        let permission = match auth {
            Authorization::Authorized => "granted",
            Authorization::Denied => "denied",
            _ => "default",
        };
        self.call_chat("setPermission", vec![json!(permission)], "__zaloNotification");
    }

    fn current_authorization(&self) -> Authorization {
        if self.notifier.available.get() { Authorization::Authorized } else { Authorization::Unavailable }
    }

    pub(crate) fn handle_notification(self: &ZaloEngine, state: &Map<String, Value>) {
        let Some(p) = state.get("notification").filter(|p| p.is_object()) else { return };
        match p.s("kind").as_str() {
            "ready" => {
                if let Some(me) = self.weak().upgrade() {
                    self.notifier.attach(&me);
                }
                self.apply_authorization(self.current_authorization());
                self.call_chat("refresh", vec![], "__zaloNotification");
                let weak = self.weak();
                let exec = self.exec.clone();
                spawn(&self.exec, async move {
                    exec.timer(Duration::from_secs(4)).await;
                    if let Some(me) = weak.upgrade() {
                        me.sync_focus_state();
                    }
                });
            }
            "requestPermission" => {
                let auth = self.current_authorization();
                self.apply_authorization(auth);
                let permission = if auth == Authorization::Authorized { "granted" } else { "default" };
                self.call_chat("permissionResult", vec![json!(permission)], "__zaloNotification");
            }
            "settings" => {
                let next = {
                    let mut n = self.notification_settings.borrow().clone();
                    n.apply(p);
                    n
                };
                if next != *self.notification_settings.borrow() {
                    *self.notification_settings.borrow_mut() = next;
                    self.changed(Change::Other);
                }
            }
            "sound" => {
                let url = p.s("url");
                if is_sound_url_allowed(&url) {
                    if let Some(loader) = self.media_loader() {
                        let fut = loader.local_file(&url, |_| {});
                        spawn(&self.exec, async move {
                            if let Ok(path) = fut.await {
                                let _ = std::process::Command::new("paplay").arg(&path).spawn().or_else(|_| std::process::Command::new("pw-play").arg(&path).spawn());
                            }
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// Window focus changes (UI) → the official FocusDetector.
    pub fn set_window_state(&self, active: bool, visible: bool) {
        let changed = { self.window_state.borrow().0 != active };
        {
            let mut w = self.window_state.borrow_mut();
            w.0 = active;
            w.1 = visible;
        }
        if changed {
            self.sync_focus_state();
        }
    }

    pub fn sync_focus_state(&self) {
        let active = self.window_state.borrow().0;
        if let Some(h) = self.host() {
            h.bridge.send("visibility-change", vec![BV::str(if active { "focus" } else { "blur" }), BV::str("1")]);
        }
    }

    /// Called by the UI on mouse/key input.
    pub fn note_user_activity(&self) {
        self.window_state.borrow_mut().2 = Instant::now();
        self.notifier.note_user_activity();
    }

    /// (app active, main window visible, idle seconds)
    pub fn window_state(&self) -> (bool, bool, f64) {
        let w = self.window_state.borrow();
        (w.0, w.1, w.2.elapsed().as_secs_f64())
    }

    pub fn activate_app(&self) {
        let h = self.on_activate.borrow().clone();
        if let Some(h) = h {
            h();
        }
    }

    /// A hidden conversation that is locked must not receive inline replies.
    pub fn conversation_locked(&self, id: &str) -> bool {
        self.is_conversation_locked(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn mute_expiry() {
        let now = at(2026, 11, 9, 7, 30);
        let t = now.timestamp() as f64;
        assert_eq!(MuteDuration::OneHour.expiry(now), Some(t + 3600.0));
        assert_eq!(MuteDuration::Until8AM.expiry(now), Some(at(2026, 11, 9, 8, 0).timestamp() as f64));
        assert_eq!(MuteDuration::Until8AM.expiry(at(2026, 11, 9, 9, 0)), Some(at(2026, 11, 10, 8, 0).timestamp() as f64));
        assert_eq!(MuteDuration::UntilReopened.expiry(now), None);
    }

    #[test]
    fn mute_texts() {
        let now = at(2026, 11, 9, 10, 0);
        let today = MuteInfo { start_time: now.timestamp() as f64, duration: 3600.0 * 4.5 };
        assert_eq!(today.description(now), "Cho đến 14:30");
        assert_eq!(today.status_text(now), "Đang tắt đến 14:30");
        let tomorrow = MuteInfo { start_time: now.timestamp() as f64, duration: 86400.0 };
        assert_eq!(tomorrow.description(now), "Cho đến 10:00 ngày 10/11");
        let forever = MuteInfo { start_time: 0.0, duration: -1.0 };
        assert_eq!(forever.status_text(now), "Đang tắt cho đến khi được mở lại");
        assert!(forever.is_active(1e12));
        assert_eq!(MuteInfo::parse(&json!({"id": "x"})), Some(("x".into(), MuteInfo { start_time: 0.0, duration: -1.0 })));
        assert!(MuteInfo::parse(&json!({"id": ""})).is_none());
    }

    #[test]
    fn settings_apply_replaces_mutes() {
        let mut s = NotificationSettingsState::default();
        s.mutes.insert("old".into(), MuteInfo { start_time: 0.0, duration: -1.0 });
        s.apply(&json!({"enabled": false, "sound": 1, "mutes": [{"id": "a", "startTime": 5, "duration": 10}, {"id": "b", "startTime": 1}]}));
        assert!(s.is_loaded && !s.enabled && s.sound);
        assert!(!s.mutes.contains_key("old"));
        let active = s.active_mutes(10.0);
        assert_eq!(active.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
        assert!(s.active_mutes(100.0).iter().all(|(k, _)| k == "b"));
    }

    #[test]
    fn request_parsing() {
        let args = vec![BV::str("Lan"), BV::obj([("body", BV::str("hi")), ("tag", BV::int(123)), ("silent", BV::Bool(true)), ("icon", BV::str("assets/a.png?v=1"))])];
        let r = NotificationRequest::parse(&args).unwrap();
        assert_eq!((r.tag.as_str(), r.silent), ("123", true));
        assert_eq!(r.icon, Some(Icon::Bundled("assets/a.png".into())));
        assert!(NotificationRequest::parse(&[BV::str(""), BV::obj([("body", BV::str(""))])]).is_none());
        assert_eq!(parse_icon("data:image/png;base64,iVBO"), Some(Icon::Inline(vec![0x89, 0x50, 0x4e])));
        assert_eq!(parse_icon("data:image/png,raw"), None);
        assert_eq!(parse_icon("https://s120.zadn.vn/a"), Some(Icon::Remote("https://s120.zadn.vn/a".into())));
        assert_eq!(parse_icon("file:///etc/passwd"), None);
        assert_eq!(parse_icon("  "), None);
    }

    #[test]
    fn policies() {
        assert!(should_suppress(true, true, Some("c"), "c", 10.0));
        assert!(!should_suppress(true, true, Some("c"), "c", 200.0));
        assert!(!should_suppress(false, true, Some("c"), "c", 0.0));
        assert!(!should_suppress(true, false, Some("c"), "c", 0.0));
        assert!(!should_suppress(true, true, Some("d"), "c", 0.0));
        assert!(!should_suppress(true, true, Some(""), "", 0.0));
        assert_eq!(body("x", false), HIDDEN_PREVIEW_BODY);
        assert_eq!(attachment_ext(&[0xff, 0xd8, 0xff, 0]), Some("jpg"));
        assert_eq!(attachment_ext(b"RIFF"), None);
        assert!(is_sound_url_allowed("https://res-zalo.zadn.vn/sound/noti.mp3"));
        assert!(!is_sound_url_allowed("http://res-zalo.zadn.vn/sound/noti.mp3"));
        assert!(!is_sound_url_allowed("https://evil.com/sound/noti.mp3"));
    }
}
