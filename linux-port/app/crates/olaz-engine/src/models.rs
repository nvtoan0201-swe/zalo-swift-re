//! Chat data models. Port of `ChatModels.swift`. INVARIANT: parsers never fail on bad input —
//! unknown types become `Unsupported`, missing or mistyped fields become empty/zero/None.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

/// Lenient accessors over a JSON object (`as? String`, `as? NSNumber` semantics).
pub trait J {
    fn obj(&self) -> Option<&Map<String, Value>>;
    fn s(&self, key: &str) -> String {
        self.obj().and_then(|o| o.get(key)).and_then(Value::as_str).unwrap_or("").to_string()
    }
    fn opt_s(&self, key: &str) -> Option<String> {
        self.obj().and_then(|o| o.get(key)).and_then(Value::as_str).map(str::to_string)
    }
    /// `NSNumber.doubleValue` (bools are 0/1); missing → 0.
    fn n(&self, key: &str) -> f64 {
        match self.obj().and_then(|o| o.get(key)) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::Bool(b)) => *b as i64 as f64,
            _ => 0.0,
        }
    }
    fn opt_n(&self, key: &str) -> Option<f64> {
        match self.obj().and_then(|o| o.get(key)) {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::Bool(b)) => Some(*b as i64 as f64),
            _ => None,
        }
    }
    /// `NSNumber.boolValue` / `as? Bool`.
    fn b(&self, key: &str) -> bool {
        match self.obj().and_then(|o| o.get(key)) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
            _ => false,
        }
    }
    fn opt_b(&self, key: &str) -> Option<bool> {
        match self.obj().and_then(|o| o.get(key)) {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0),
            _ => None,
        }
    }
    fn arr(&self, key: &str) -> Vec<Value> {
        self.obj().and_then(|o| o.get(key)).and_then(Value::as_array).cloned().unwrap_or_default()
    }
    fn get_v(&self, key: &str) -> Option<&Value> {
        self.obj().and_then(|o| o.get(key))
    }
    /// Non-empty (trimmed) string as a URL string.
    fn url(&self, key: &str) -> Option<String> {
        url_value(self.get_v(key))
    }
}

impl J for Value {
    fn obj(&self) -> Option<&Map<String, Value>> {
        self.as_object()
    }
}
impl J for Map<String, Value> {
    fn obj(&self) -> Option<&Map<String, Value>> {
        Some(self)
    }
}

/// A URL from a JSON string: empty/whitespace → None; must parse.
pub fn url_value(v: Option<&Value>) -> Option<String> {
    let text = v?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    url::Url::parse(text).ok().map(|_| text.to_string())
}

/// Sender-supplied numbers can be NaN or astronomically large.
pub fn clamped_int(v: f64) -> i64 {
    if !v.is_finite() {
        return 0;
    }
    v.clamp(-9e15, 9e15) as i64
}

/// The only links a card may open: http/https with a host.
pub fn sanitized_link(u: Option<String>) -> Option<String> {
    let text = u?;
    let parsed = url::Url::parse(&text).ok()?;
    let scheme = parsed.scheme().to_lowercase();
    if (scheme == "http" || scheme == "https") && parsed.host_str().is_some_and(|h| !h.is_empty()) {
        return Some(text);
    }
    // Visual tests only: fixture media are `file://` URLs under the fixtures directory.
    let root = FIXTURE_ROOT.get()?;
    (scheme == "file" && parsed.to_file_path().is_ok_and(|p| p.starts_with(root) && !p.components().any(|c| c == std::path::Component::ParentDir))).then_some(text)
}

static FIXTURE_ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// `--ui-render` (debug builds): lets [`sanitized_link`] accept `file://` URLs under the fixtures directory.
pub fn allow_fixture_links(root: &std::path::Path) {
    if cfg!(debug_assertions) {
        let _ = FIXTURE_ROOT.set(root.to_path_buf());
    }
}

/// UTF-16 length of a string (official offsets are UTF-16).
pub fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Milliseconds since epoch as used by the models (`Date`).
pub type Millis = f64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mention {
    pub uid: String,
    pub pos: i64,
    pub len: i64,
    pub is_all: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposerDraft {
    pub text: String,
    pub mentions: Vec<Mention>,
    pub reply_to: Option<Box<ChatMessage>>,
}

impl ComposerDraft {
    pub fn text(text: &str) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    /// `other` after `self`, one line below; its mention offsets move with it; first reply target kept.
    pub fn appending(&self, other: &ComposerDraft) -> ComposerDraft {
        if self.text.is_empty() {
            return ComposerDraft { text: other.text.clone(), mentions: other.mentions.clone(), reply_to: self.reply_to.clone().or(other.reply_to.clone()) };
        }
        if other.text.is_empty() {
            return self.clone();
        }
        let shift = len16(&self.text) as i64 + 1;
        let mut merged = self.mentions.clone();
        merged.extend(other.mentions.iter().map(|m| Mention { pos: m.pos + shift, ..m.clone() }));
        ComposerDraft { text: format!("{}\n{}", self.text, other.text), mentions: merged, reply_to: self.reply_to.clone().or(other.reply_to.clone()) }
    }
}

/// Drafts the official client did not take, per conversation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FailedDraftStore {
    drafts: HashMap<String, ComposerDraft>,
    silent: HashSet<String>,
    pub revision: u64,
}

impl FailedDraftStore {
    pub fn has(&self, id: &str) -> bool {
        self.drafts.contains_key(id)
    }

    pub fn stash(&mut self, draft: ComposerDraft, id: &str, silent: bool) {
        if draft.text.is_empty() {
            return;
        }
        if !self.drafts.contains_key(id) {
            if silent {
                self.silent.insert(id.to_string());
            }
        } else if !silent {
            self.silent.remove(id);
        }
        let merged = match self.drafts.get(id) {
            Some(existing) => existing.appending(&draft),
            None => draft,
        };
        self.drafts.insert(id.to_string(), merged);
        self.revision += 1;
    }

    pub fn take(&mut self, id: &str) -> Option<ComposerDraft> {
        self.take_with_notice(id).map(|(d, _)| d)
    }

    pub fn take_with_notice(&mut self, id: &str) -> Option<(ComposerDraft, bool)> {
        let was_silent = self.silent.remove(id);
        self.drafts.remove(id).map(|d| (d, !was_silent))
    }

    pub fn drain(&mut self) -> Vec<(String, ComposerDraft)> {
        self.silent.clear();
        self.drafts.drain().collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MentionCandidate {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub is_all: bool,
}

impl MentionCandidate {
    /// Rows need a non-empty id; non-"all" rows a non-empty trimmed name; all → "All".
    pub fn list(json: &Value) -> Vec<MentionCandidate> {
        json.as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        let id = r.s("id");
                        if id.is_empty() {
                            return None;
                        }
                        let is_all = r.b("isAll");
                        let name = r.s("name").trim().to_string();
                        if !is_all && name.is_empty() {
                            return None;
                        }
                        Some(MentionCandidate { id, name: if is_all { "All".into() } else { name }, avatar: r.url("avatar"), is_all })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReactionIcon {
    pub kind: String,
    pub key: String,
    pub emoji: String,
    pub image: String,
}

impl ReactionIcon {
    pub fn from_json(json: &Value) -> Option<ReactionIcon> {
        let emoji = json.s("emoji");
        let key = json.s("key");
        if emoji.is_empty() && key.is_empty() {
            return None;
        }
        let kind = json.opt_s("type").filter(|t| !t.is_empty()).unwrap_or_else(|| if key.is_empty() { emoji.clone() } else { key.clone() });
        Some(ReactionIcon { kind, key: key.clone(), emoji: if emoji.is_empty() { key } else { emoji }, image: json.s("image") })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    pub icon: ReactionIcon,
    pub count: i64,
    pub includes_me: bool,
    pub names: Vec<String>,
    pub people: i64,
}

impl Reaction {
    /// Chip tooltip: "A", "A và B", "A, B và C", "A, B, C và 2 người khác"; None when nobody is known.
    pub fn tooltip(&self) -> Option<String> {
        let others = (self.people - self.names.len() as i64).max(0);
        match (self.names.len(), others) {
            (0, 0) => None,
            (0, n) => Some(format!("{n} người")),
            (1, 0) => Some(self.names[0].clone()),
            (k, 0) => Some(format!("{} và {}", self.names[..k - 1].join(", "), self.names[k - 1])),
            (_, n) => Some(format!("{} và {n} người khác", self.names.join(", "))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactCard {
    pub name: String,
    pub uid: String,
    pub avatar: Option<String>,
    pub phone: String,
    pub qr: Option<String>,
    pub oa_link: String,
    pub is_official_account: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocationInfo {
    pub title: String,
    pub address: String,
    pub coordinate: Option<(f64, f64)>,
    pub thumbnail: Option<String>,
}

impl LocationInfo {
    pub fn map_url(&self) -> Option<String> {
        self.coordinate.map(|(lat, lon)| format!("https://maps.google.com/maps?q={},{}&z=14&t=m", fmt_num(lat), fmt_num(lon)))
    }
}

/// Swift's `"\(double)"`: shortest round-trip representation, always with a fractional part.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e16 { format!("{v:.1}") } else { format!("{v}") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ECardStyle {
    #[default]
    Banner,
    Friend,
    Birthday,
    Reminder,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ECard {
    pub style: ECardStyle,
    pub image: Option<String>,
    pub thumb: Option<String>,
    pub title: String,
    pub description: String,
    pub actions: Vec<String>,
    pub avatars: Vec<String>,
    pub reminder_title: String,
    pub reminder_start: Option<Millis>,
    pub reminder_repeat: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OAPart {
    pub is_header: bool,
    pub title: String,
    pub description: String,
    pub thumb: Option<String>,
    pub url: Option<String>,
    pub help: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PollInfo {
    pub question: String,
    pub header: String,
    pub anonymous: bool,
    pub poll_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text,
    Photo { url: Option<String>, width: i64, height: i64 },
    Sticker { url: Option<String> },
    Gif { url: Option<String> },
    Video { thumbnail: Option<String>, duration: i64, url: Option<String> },
    Voice { url: Option<String>, duration: i64 },
    File { name: String, size: i64, url: Option<String> },
    Link { title: String, url: Option<String>, thumbnail: Option<String>, description: String, source: String },
    ContactCard(ContactCard),
    Location(LocationInfo),
    Call { video: bool, missed: bool, duration: i64, title: String, detail: String },
    ECard(ECard),
    OAMessage(Vec<OAPart>),
    Poll(PollInfo),
    StickerSet { title: String, thumbnail: Option<String> },
    ZInstant { title: String, text: String, thumbnail: Option<String> },
    Notice { symbol: String, text: String },
    Placeholder { symbol: String, text: String },
    System,
    Recalled,
    Unsupported { raw_type: String },
}

/// Plausible timestamps only (1970 … 2100).
fn plausible_ms(ms: f64) -> Option<Millis> {
    (ms > 0.0 && ms < 4_102_444_800_000.0).then_some(ms)
}

impl Kind {
    /// Kinds laid out centered, outside the bubbles.
    pub fn is_standalone(&self) -> bool {
        matches!(self, Kind::System | Kind::Notice { .. } | Kind::ECard(_))
    }

    pub fn from_json(json: &Value) -> Kind {
        let t = |k: &str| json.s(k);
        let n = |k: &str| json.n(k);
        let u = |k: &str| json.url(k);
        let kind = json.get_v("type").and_then(Value::as_str).unwrap_or("text");
        match kind {
            "text" => Kind::Text,
            "photo" => Kind::Photo { url: u("url"), width: clamped_int(n("width")), height: clamped_int(n("height")) },
            "sticker" => Kind::Sticker { url: u("url") },
            "gif" => Kind::Gif { url: u("url") },
            "video" => Kind::Video { thumbnail: u("thumbnail"), duration: clamped_int(n("duration")), url: u("url") },
            "voice" => Kind::Voice { url: u("url"), duration: clamped_int(n("duration")) },
            "file" => Kind::File { name: t("name"), size: clamped_int(n("size")), url: u("url") },
            "link" => Kind::Link {
                title: t("title"),
                url: sanitized_link(u("url")),
                thumbnail: u("thumbnail"),
                description: t("description"),
                source: t("source"),
            },
            "contactCard" => Kind::ContactCard(ContactCard {
                name: t("name"),
                uid: t("uid"),
                avatar: u("avatar"),
                phone: t("phone"),
                qr: u("qr"),
                oa_link: t("oaLink"),
                is_official_account: json.b("isOA"),
            }),
            "location" => {
                let mut info = LocationInfo { title: t("title"), address: t("address"), coordinate: None, thumbnail: u("thumbnail") };
                if json.b("hasCoordinates") {
                    let (lat, lon) = (n("latitude"), n("longitude"));
                    if lat.abs() <= 90.0 && lon.abs() <= 180.0 {
                        info.coordinate = Some((lat, lon));
                    }
                }
                Kind::Location(info)
            }
            "call" => Kind::Call {
                video: json.b("video"),
                missed: json.b("missed"),
                duration: clamped_int(n("duration")),
                title: t("title"),
                detail: t("detail"),
            },
            "ecard" => Kind::ECard(ECard {
                style: match t("style").as_str() {
                    "friend" => ECardStyle::Friend,
                    "birthday" => ECardStyle::Birthday,
                    "reminder" => ECardStyle::Reminder,
                    _ => ECardStyle::Banner,
                },
                image: u("image"),
                thumb: u("thumb"),
                title: t("title"),
                description: t("description"),
                actions: json.arr("actions").iter().filter_map(|a| a.as_str().filter(|s| !s.is_empty()).map(str::to_string)).collect(),
                avatars: json.arr("avatars").iter().filter_map(|a| url_value(Some(a))).collect(),
                reminder_title: t("reminderTitle"),
                reminder_start: plausible_ms(n("reminderStart")),
                reminder_repeat: t("reminderRepeat"),
            }),
            "oaMessage" => Kind::OAMessage(
                json.arr("parts")
                    .iter()
                    .filter_map(|row| {
                        let title = row.s("title");
                        let thumb = row.url("thumb");
                        if title.is_empty() && thumb.is_none() {
                            return None;
                        }
                        Some(OAPart {
                            is_header: row.b("header"),
                            title,
                            description: row.s("description"),
                            thumb,
                            url: sanitized_link(row.url("url")),
                            help: row.s("help"),
                        })
                    })
                    .collect(),
            ),
            "poll" => Kind::Poll(PollInfo {
                question: t("question"),
                header: t("header"),
                anonymous: json.b("anonymous"),
                poll_id: t("pollId"),
                conversation_id: t("conversationId"),
            }),
            "stickerSet" => Kind::StickerSet { title: t("title"), thumbnail: u("thumb") },
            "zinstant" => Kind::ZInstant { title: t("title"), text: t("text"), thumbnail: u("thumb") },
            "notice" => Kind::Notice { symbol: non_empty_or(t("symbol"), "info.circle"), text: t("text") },
            "placeholder" => Kind::Placeholder { symbol: non_empty_or(t("symbol"), "lock"), text: t("text") },
            "system" => Kind::System,
            "recalled" => Kind::Recalled,
            _ => Kind::Unsupported { raw_type: t("rawType") },
        }
    }
}

fn non_empty_or(s: String, d: &str) -> String {
    if s.is_empty() { d.into() } else { s }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Sending,
    Sent,
    Delivered,
    Seen,
    Failed,
}

impl Status {
    pub fn parse(s: &str) -> Status {
        match s {
            "sending" => Status::Sending,
            "delivered" => Status::Delivered,
            "seen" => Status::Seen,
            "failed" => Status::Failed,
            _ => Status::Sent,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub sender_name: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Availability {
    #[default]
    Hidden,
    Available,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuDelete {
    Plain,
    ForMe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuStar {
    Star,
    Unstar,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MessageMenu {
    pub forward: Availability,
    pub recall: bool,
    pub delete: Option<MenuDelete>,
    pub pin: bool,
    pub star: Option<MenuStar>,
    pub starred: bool,
    pub pinned: bool,
    pub is_protected: bool,
}

impl MessageMenu {
    pub fn from_json(json: Option<&Value>) -> MessageMenu {
        let Some(row) = json.filter(|v| v.is_object()) else { return MessageMenu::default() };
        let strict_bool = |k: &str| row.get_v(k).and_then(Value::as_bool).unwrap_or(false);
        MessageMenu {
            forward: match row.s("forward").as_str() {
                "ok" => Availability::Available,
                "blocked" => Availability::Blocked,
                _ => Availability::Hidden,
            },
            recall: strict_bool("recall"),
            delete: match row.s("delete").as_str() {
                "plain" => Some(MenuDelete::Plain),
                "forMe" => Some(MenuDelete::ForMe),
                _ => None,
            },
            pin: strict_bool("pin"),
            star: match row.s("star").as_str() {
                "star" => Some(MenuStar::Star),
                "unstar" => Some(MenuStar::Unstar),
                "blocked" => Some(MenuStar::Blocked),
                _ => None,
            },
            starred: strict_bool("starred"),
            pinned: strict_bool("pinned"),
            is_protected: strict_bool("protected"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CardButton {
    pub r#ref: String,
    pub label: String,
    pub enabled: bool,
    pub help: String,
}

impl CardButton {
    pub fn list(json: Option<&Value>) -> Vec<CardButton> {
        json.and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let r = row.s("ref");
                        let label = row.s("label").trim().to_string();
                        if r.is_empty() || label.is_empty() {
                            return None;
                        }
                        Some(CardButton { r#ref: r, label, enabled: row.b("enabled"), help: row.s("help") })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub sender_avatar: Option<String>,
    pub is_mine: bool,
    pub kind: Kind,
    pub text: String,
    /// ms since epoch.
    pub date: Millis,
    pub status: Status,
    pub quote: Option<Quote>,
    pub reactions: Vec<Reaction>,
    pub is_e2ee: bool,
    pub mentions: Vec<Mention>,
    pub card_buttons: Vec<CardButton>,
    pub suggests_stickers: bool,
    pub menu: MessageMenu,
}

impl ChatMessage {
    /// One `ChatMessageJSON`; None without an id.
    pub fn from_json(json: &Value, fallback_conversation: &str) -> Option<ChatMessage> {
        let id = json.s("id");
        if id.is_empty() {
            return None;
        }
        let strict_bool = |v: &Value, k: &str| v.get_v(k).and_then(Value::as_bool).unwrap_or(false);
        let quote = json.get_v("quote").filter(|q| q.is_object()).map(|q| Quote { sender_name: q.s("senderName"), text: q.s("text") });
        let reactions = json
            .arr("reactions")
            .iter()
            .filter_map(|r| {
                let icon = ReactionIcon::from_json(r)?;
                Some(Reaction {
                    icon,
                    count: clamped_int(r.n("count")),
                    includes_me: strict_bool(r, "includesMe"),
                    names: r.arr("names").iter().filter_map(|n| n.as_str().map(str::to_string)).collect(),
                    people: clamped_int(r.n("people")),
                })
            })
            .collect();
        let mentions = json
            .arr("mentions")
            .iter()
            .filter_map(|m| {
                let uid = m.s("uid");
                if uid.is_empty() {
                    return None;
                }
                Some(Mention { uid, pos: clamped_int(m.n("pos")), len: clamped_int(m.n("len")), is_all: strict_bool(m, "isAll") })
            })
            .collect();
        let empty = Value::Object(Map::new());
        Some(ChatMessage {
            id,
            conversation_id: json.opt_s("conversationId").unwrap_or_else(|| fallback_conversation.to_string()),
            sender_id: json.s("senderId"),
            sender_name: json.s("senderName"),
            sender_avatar: json.get_v("senderAvatar").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            is_mine: strict_bool(json, "isMine"),
            kind: Kind::from_json(json.get_v("kind").filter(|k| k.is_object()).unwrap_or(&empty)),
            text: json.s("text"),
            date: json.n("date"),
            status: Status::parse(&json.s("status")),
            quote,
            reactions,
            is_e2ee: strict_bool(json, "isE2EE"),
            mentions,
            card_buttons: CardButton::list(json.get_v("buttons")),
            suggests_stickers: strict_bool(json, "stickers"),
            menu: MessageMenu::from_json(json.get_v("menu")),
        })
    }
}

/// What `performAction` made of a click.
#[derive(Debug, Clone, PartialEq)]
pub enum CardActionOutcome {
    Done,
    OpenChat(String),
    OpenProfile(String),
    OpenURL(String),
    Copy(String),
    Present(Value),
    Unsupported(String),
    Locked(String),
    Failed(String),
    Pending(String),
}

pub const DEFAULT_UNSUPPORTED: &str = "Chưa hỗ trợ trên ứng dụng này";
pub const NO_ANSWER_TEXT: &str = "Thao tác vẫn đang được xử lý. Hãy kiểm tra lại cuộc trò chuyện trước khi thử lại.";
pub const DEFAULT_PENDING: &str = "Thao tác đang được xử lý, chưa nhận được phản hồi. Hãy kiểm tra lại cuộc trò chuyện trước khi thử lại.";

impl CardActionOutcome {
    /// `is_sheet` decides whether an effect is a native sheet (`CardSheet(effect:)`).
    pub fn from_json(json: &Value, is_sheet: impl Fn(&Value) -> bool) -> CardActionOutcome {
        let Some(status) = json.get_v("status").and_then(Value::as_str) else {
            return CardActionOutcome::Failed("Không thực hiện được thao tác.".into());
        };
        let text = json.opt_s("text").filter(|t| !t.is_empty());
        match status {
            "done" => CardActionOutcome::Done,
            "effect" => {
                let effect = json.get_v("effect").cloned().unwrap_or(Value::Object(Map::new()));
                let id = effect.opt_s("id").or_else(|| effect.opt_s("uid")).unwrap_or_default();
                match effect.s("type").as_str() {
                    "openChat" if !id.is_empty() => CardActionOutcome::OpenChat(id),
                    "openProfile" if !id.is_empty() => CardActionOutcome::OpenProfile(id),
                    "copy" if !effect.s("text").is_empty() => CardActionOutcome::Copy(effect.s("text")),
                    "openURL" => match sanitized_link(effect.opt_s("url")) {
                        Some(u) => CardActionOutcome::OpenURL(u),
                        None => CardActionOutcome::Unsupported(DEFAULT_UNSUPPORTED.into()),
                    },
                    _ => {
                        if is_sheet(&effect) {
                            CardActionOutcome::Present(effect)
                        } else {
                            CardActionOutcome::Unsupported(DEFAULT_UNSUPPORTED.into())
                        }
                    }
                }
            }
            "unsupported" => CardActionOutcome::Unsupported(text.unwrap_or_else(|| DEFAULT_UNSUPPORTED.into())),
            "locked" => CardActionOutcome::Locked(text.unwrap_or_else(|| "Bạn không thể gửi tin nhắn vào cuộc trò chuyện này.".into())),
            "not-open" => CardActionOutcome::Failed("Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát.".into()),
            "busy" => CardActionOutcome::Failed(text.unwrap_or_else(|| "Thao tác đang được xử lý, hãy chờ giây lát.".into())),
            "pending" => CardActionOutcome::Pending(text.unwrap_or_else(|| DEFAULT_PENDING.into())),
            _ => CardActionOutcome::Failed(text.unwrap_or_else(|| "Không thực hiện được thao tác.".into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuggestedSticker {
    pub r#ref: String,
    pub url: String,
}

impl SuggestedSticker {
    pub fn list(json: &Value) -> Vec<SuggestedSticker> {
        json.as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let r = row.s("ref");
                        let url = sanitized_link(row.opt_s("url"))?;
                        (!r.is_empty()).then_some(SuggestedSticker { r#ref: r, url })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactProfile {
    pub uid: String,
    pub name: String,
    pub zalo_name: String,
    pub avatar: Option<String>,
    pub bio: String,
    pub is_self: bool,
    pub is_friend: bool,
    pub is_official_account: bool,
}

impl ContactProfile {
    pub fn from_json(json: &Value) -> Option<ContactProfile> {
        let uid = json.s("uid");
        if uid.is_empty() {
            return None;
        }
        Some(ContactProfile {
            uid,
            name: json.s("name"),
            zalo_name: json.s("zaloName"),
            avatar: json.get_v("avatar").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            bio: json.s("bio"),
            is_self: json.b("isSelf"),
            is_friend: json.b("isFriend"),
            is_official_account: json.b("isOA"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReminderVariant {
    #[default]
    Hidden,
    Pending,
    Invite,
    Alert,
    DemotedCreate,
    DemotedAlert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Reject,
}

fn answer(s: &str) -> Option<Answer> {
    match s {
        "accept" => Some(Answer::Accept),
        "reject" => Some(Answer::Reject),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReminderSnapshot {
    pub variant: ReminderVariant,
    pub id: String,
    pub title: String,
    pub start: Option<Millis>,
    pub repeat_label: String,
    pub creator_name: String,
    pub only_me: bool,
    pub is_group: bool,
    pub mine: Option<Answer>,
    pub pending: Option<Answer>,
    pub accepted_count: i64,
    pub rejected_count: i64,
    pub can_rsvp: bool,
    pub can_view: bool,
    pub deleted: bool,
    pub sig: String,
}

impl ReminderSnapshot {
    pub fn from_json(json: &Value) -> Option<ReminderSnapshot> {
        json.as_object()?;
        Some(ReminderSnapshot {
            variant: match json.s("variant").as_str() {
                "pending" => ReminderVariant::Pending,
                "invite" => ReminderVariant::Invite,
                "alert" => ReminderVariant::Alert,
                "demotedCreate" => ReminderVariant::DemotedCreate,
                "demotedAlert" => ReminderVariant::DemotedAlert,
                _ => ReminderVariant::Hidden,
            },
            id: json.s("rid"),
            title: json.s("title"),
            start: plausible_ms(json.n("start")),
            repeat_label: json.s("repeat"),
            creator_name: json.s("creatorName"),
            only_me: json.b("onlyMe"),
            is_group: json.b("isGroup"),
            mine: answer(&json.s("mine")),
            pending: answer(&json.s("pending")),
            accepted_count: clamped_int(json.n("accepted")).max(0),
            rejected_count: clamped_int(json.n("rejected")).max(0),
            can_rsvp: json.b("canRsvp"),
            can_view: json.b("canView"),
            deleted: json.b("deleted"),
            sig: json.s("sig"),
        })
    }
    pub fn is_drawn(&self) -> bool {
        !matches!(self.variant, ReminderVariant::Hidden | ReminderVariant::Pending)
    }
    pub fn shows_rsvp_buttons(&self) -> bool {
        self.is_group && !self.only_me && self.can_rsvp && !self.deleted && self.mine.is_none()
    }
    pub fn shows_rsvp_status(&self) -> bool {
        self.is_group && !self.only_me && self.mine.is_some()
    }
    pub fn has_responders(&self) -> bool {
        self.is_group && !self.only_me
    }
    pub fn responder_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.accepted_count > 0 {
            parts.push(format!("{} người tham gia", self.accepted_count));
        }
        if self.rejected_count > 0 {
            parts.push(format!("{} người từ chối", self.rejected_count));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub uid: String,
    pub name: String,
    pub avatar: Option<String>,
    pub is_me: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReminderResponders {
    pub accept: Vec<Person>,
    pub reject: Vec<Person>,
    pub accept_total: i64,
    pub reject_total: i64,
    pub accept_more: i64,
    pub reject_more: i64,
}

impl ReminderResponders {
    pub fn from_json(json: &Value) -> Option<ReminderResponders> {
        if !json.b("ok") {
            return None;
        }
        let people = |key: &str| {
            let mut seen = HashSet::new();
            json.arr(key)
                .iter()
                .filter_map(|row| {
                    let uid = row.s("uid");
                    if uid.is_empty() || !seen.insert(uid.clone()) {
                        return None;
                    }
                    Some(Person {
                        uid,
                        name: row.s("name"),
                        avatar: row.get_v("avatar").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                        is_me: row.b("isMe"),
                    })
                })
                .collect::<Vec<_>>()
        };
        let (accept, reject) = (people("accept"), people("reject"));
        Some(ReminderResponders {
            accept_total: (accept.len() as i64).max(clamped_int(json.n("acceptTotal"))),
            reject_total: (reject.len() as i64).max(clamped_int(json.n("rejectTotal"))),
            accept_more: clamped_int(json.n("acceptMore")).max(0),
            reject_more: clamped_int(json.n("rejectMore")).max(0),
            accept,
            reject,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendRequestState {
    Ready,
    Requested,
    Requesting,
    Friend,
    Me,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FriendRequestPrep {
    pub state: FriendRequestState,
    pub default_message: String,
    pub max_message: i64,
}

impl FriendRequestPrep {
    pub fn from_json(json: &Value) -> FriendRequestPrep {
        let limit = clamped_int(json.opt_n("maxMessage").unwrap_or(150.0));
        FriendRequestPrep {
            state: match json.s("state").as_str() {
                "ready" => FriendRequestState::Ready,
                "requested" => FriendRequestState::Requested,
                "requesting" => FriendRequestState::Requesting,
                "friend" => FriendRequestState::Friend,
                "self" => FriendRequestState::Me,
                _ => FriendRequestState::Error,
            },
            default_message: json.s("defaultMessage"),
            max_message: if limit > 0 { limit.min(500) } else { 150 },
        }
    }
}

// MARK: reaction detail

#[derive(Debug, Clone, PartialEq)]
pub struct ReactionTab {
    pub icon: ReactionIcon,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReactionUserIcon {
    pub icon: ReactionIcon,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReactionUser {
    pub uid: String,
    pub name: String,
    pub avatar: Option<String>,
    pub is_me: bool,
    pub total: i64,
    pub icons: Vec<ReactionUserIcon>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReactionDetail {
    pub tabs: Vec<ReactionTab>,
    pub users: Vec<ReactionUser>,
}

pub const UNKNOWN_NAME: &str = "Người dùng Zalo";

impl ReactionDetail {
    pub fn from_json(json: &Value) -> Option<ReactionDetail> {
        json.as_object()?;
        let int = |v: &Value, k: &str| v.get_v(k).and_then(Value::as_f64).map(|f| f as i64).unwrap_or(0);
        let tabs = json.arr("tabs").iter().filter_map(|row| ReactionIcon::from_json(row).map(|icon| ReactionTab { icon, total: int(row, "total") })).collect();
        let users = json
            .arr("users")
            .iter()
            .filter_map(|row| {
                let uid = row.s("uid");
                if uid.is_empty() {
                    return None;
                }
                let name = row.s("name").trim().to_string();
                let icons = row
                    .arr("icons")
                    .iter()
                    .filter_map(|e| ReactionIcon::from_json(e).map(|icon| ReactionUserIcon { icon, count: int(e, "count") }))
                    .collect();
                Some(ReactionUser {
                    uid,
                    name: if name.is_empty() { UNKNOWN_NAME.into() } else { name },
                    avatar: row.get_v("avatar").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                    is_me: row.get_v("isMe").and_then(Value::as_bool).unwrap_or(false),
                    total: int(row, "total"),
                    icons,
                })
            })
            .collect();
        Some(ReactionDetail { tabs, users })
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }
    pub fn total(&self) -> i64 {
        self.users.iter().map(|u| u.total).sum()
    }

    /// `None` = "Tất cả" (≤ 6 icons each); a type = people who used it, by count desc → me first → order.
    pub fn rows(&self, kind: Option<&str>) -> Vec<ReactionUser> {
        let Some(kind) = kind else {
            return self
                .users
                .iter()
                .map(|u| {
                    let mut u = u.clone();
                    u.icons.truncate(6);
                    u
                })
                .collect();
        };
        let mut rows: Vec<(usize, ReactionUser)> = self
            .users
            .iter()
            .enumerate()
            .filter_map(|(i, u)| {
                let used = u.icons.iter().find(|ic| ic.icon.kind == kind)?.clone();
                let mut u = u.clone();
                u.total = used.count;
                u.icons = vec![used];
                Some((i, u))
            })
            .collect();
        rows.sort_by(|a, b| b.1.total.cmp(&a.1.total).then(b.1.is_me.cmp(&a.1.is_me)).then(a.0.cmp(&b.0)));
        rows.into_iter().map(|(_, u)| u).collect()
    }

    pub fn short_count(n: i64) -> String {
        if n <= 0 {
            return "0".into();
        }
        if n <= 999 {
            return n.to_string();
        }
        let thousands = n / 1000;
        let k = thousands.min(9);
        if n % 1000 > 0 || thousands > 9 { format!("{k}k+") } else { format!("{k}k") }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds_never_fail() {
        assert_eq!(Kind::from_json(&json!({})), Kind::Text);
        assert_eq!(Kind::from_json(&json!({"type": "weird", "rawType": "x"})), Kind::Unsupported { raw_type: "x".into() });
        assert_eq!(
            Kind::from_json(&json!({"type": "photo", "url": " https://a.zadn.vn/x.jpg ", "width": "big", "height": 1e300})),
            Kind::Photo { url: Some("https://a.zadn.vn/x.jpg".into()), width: 0, height: 9_000_000_000_000_000 }
        );
        assert_eq!(Kind::from_json(&json!({"type": "photo", "url": "   "})), Kind::Photo { url: None, width: 0, height: 0 });
        assert_eq!(Kind::from_json(&json!({"type": "link", "url": "file:///etc/passwd"})), Kind::Link {
            title: "".into(),
            url: None,
            thumbnail: None,
            description: "".into(),
            source: "".into()
        });
        assert_eq!(Kind::from_json(&json!({"type": "notice"})), Kind::Notice { symbol: "info.circle".into(), text: "".into() });
        assert_eq!(Kind::from_json(&json!({"type": "placeholder"})), Kind::Placeholder { symbol: "lock".into(), text: "".into() });
        let Kind::Location(l) = Kind::from_json(&json!({"type": "location", "hasCoordinates": true, "latitude": 10.5, "longitude": 106})) else { panic!() };
        assert_eq!(l.map_url().as_deref(), Some("https://maps.google.com/maps?q=10.5,106.0&z=14&t=m"));
        let Kind::Location(l) = Kind::from_json(&json!({"type": "location", "hasCoordinates": true, "latitude": 91, "longitude": 0})) else { panic!() };
        assert!(l.coordinate.is_none());
        let Kind::ECard(c) = Kind::from_json(&json!({"type": "ecard", "style": "friend", "actions": ["a", "", 3], "reminderStart": 5e12})) else { panic!() };
        assert_eq!(c.style, ECardStyle::Friend);
        assert_eq!(c.actions, vec!["a"]);
        assert_eq!(c.reminder_start, None);
        let Kind::OAMessage(parts) = Kind::from_json(&json!({"type": "oaMessage", "parts": [{"title": ""}, {"title": "T", "url": "javascript:x"}]})) else { panic!() };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].url, None);
        assert!(Kind::System.is_standalone() && !Kind::Text.is_standalone());
    }

    #[test]
    fn message_parse_defaults() {
        assert!(ChatMessage::from_json(&json!({"id": ""}), "c").is_none());
        let m = ChatMessage::from_json(&json!({"id": "1", "status": "?", "date": 1500, "menu": {"forward": "ok", "delete": "forMe", "star": "unstar"}}), "conv").unwrap();
        assert_eq!(m.conversation_id, "conv");
        assert_eq!(m.status, Status::Sent);
        assert_eq!(m.kind, Kind::Text);
        assert_eq!(m.date, 1500.0);
        assert_eq!(m.menu.forward, Availability::Available);
        assert_eq!(m.menu.delete, Some(MenuDelete::ForMe));
        assert_eq!(m.menu.star, Some(MenuStar::Unstar));
        let m = ChatMessage::from_json(
            &json!({"id": "2", "mentions": [{"uid": "", "pos": 1}, {"uid": "u", "pos": 0, "len": 4, "isAll": true}], "buttons": [{"ref": "a", "label": "  OK "}, {"ref": "", "label": "x"}], "reactions": [{"emoji": "", "key": ""}, {"key": "/-heart", "count": 2, "names": ["Bạn"], "people": 3}]}),
            "c",
        )
        .unwrap();
        assert_eq!(m.mentions, vec![Mention { uid: "u".into(), pos: 0, len: 4, is_all: true }]);
        assert_eq!(m.card_buttons, vec![CardButton { r#ref: "a".into(), label: "OK".into(), enabled: false, help: "".into() }]);
        assert_eq!(m.reactions.len(), 1);
        assert_eq!(m.reactions[0].icon.kind, "/-heart");
        assert_eq!(m.reactions[0].icon.emoji, "/-heart");
        assert_eq!(m.reactions[0].tooltip().as_deref(), Some("Bạn và 2 người khác"));
    }

    #[test]
    fn reaction_tooltips() {
        let r = |names: &[&str], people| Reaction {
            icon: ReactionIcon { kind: "t".into(), key: "".into(), emoji: "x".into(), image: "".into() },
            count: 1,
            includes_me: false,
            names: names.iter().map(|s| s.to_string()).collect(),
            people,
        };
        assert_eq!(r(&[], 0).tooltip(), None);
        assert_eq!(r(&[], 4).tooltip().as_deref(), Some("4 người"));
        assert_eq!(r(&["A"], 1).tooltip().as_deref(), Some("A"));
        assert_eq!(r(&["A", "B"], 2).tooltip().as_deref(), Some("A và B"));
        assert_eq!(r(&["A", "B", "C"], 3).tooltip().as_deref(), Some("A, B và C"));
        assert_eq!(r(&["A", "B", "C"], 5).tooltip().as_deref(), Some("A, B, C và 2 người khác"));
    }

    #[test]
    fn reaction_detail_rows_and_short_count() {
        let d = ReactionDetail::from_json(&json!({
            "tabs": [{"type": "1", "emoji": "❤", "total": 3}],
            "users": [
                {"uid": "a", "name": " ", "total": 2, "icons": [{"type": "1", "emoji": "❤", "count": 1}, {"type": "2", "emoji": "👍", "count": 1}]},
                {"uid": "b", "name": "B", "isMe": true, "total": 1, "icons": [{"type": "1", "emoji": "❤", "count": 1}]},
                {"uid": "", "name": "x"}
            ]
        }))
        .unwrap();
        assert_eq!(d.users.len(), 2);
        assert_eq!(d.users[0].name, UNKNOWN_NAME);
        assert_eq!(d.total(), 3);
        let rows = d.rows(Some("1"));
        assert_eq!(rows.iter().map(|u| u.uid.as_str()).collect::<Vec<_>>(), vec!["b", "a"], "equal counts: me first");
        assert!(ReactionDetail::from_json(&Value::Null).is_none());
        assert_eq!(ReactionDetail::short_count(0), "0");
        assert_eq!(ReactionDetail::short_count(999), "999");
        assert_eq!(ReactionDetail::short_count(1000), "1k");
        assert_eq!(ReactionDetail::short_count(1001), "1k+");
        assert_eq!(ReactionDetail::short_count(9000), "9k");
        assert_eq!(ReactionDetail::short_count(10000), "9k+");
    }

    #[test]
    fn card_outcomes() {
        let o = |v: Value| CardActionOutcome::from_json(&v, |e| e.s("type") == "sheet");
        assert_eq!(o(json!(null)), CardActionOutcome::Failed("Không thực hiện được thao tác.".into()));
        assert_eq!(o(json!({"status": "done"})), CardActionOutcome::Done);
        assert_eq!(o(json!({"status": "effect", "effect": {"type": "openChat", "uid": "9"}})), CardActionOutcome::OpenChat("9".into()));
        assert_eq!(o(json!({"status": "effect", "effect": {"type": "openURL", "url": "ftp://x"}})), CardActionOutcome::Unsupported(DEFAULT_UNSUPPORTED.into()));
        assert!(matches!(o(json!({"status": "effect", "effect": {"type": "sheet"}})), CardActionOutcome::Present(_)));
        assert_eq!(o(json!({"status": "pending"})), CardActionOutcome::Pending(DEFAULT_PENDING.into()));
        assert_eq!(o(json!({"status": "busy", "text": ""})), CardActionOutcome::Failed("Thao tác đang được xử lý, hãy chờ giây lát.".into()));
        assert_eq!(o(json!({"status": "not-open", "text": "x"})), CardActionOutcome::Failed("Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát.".into()));
    }

    #[test]
    fn failed_drafts() {
        let mut s = FailedDraftStore::default();
        s.stash(ComposerDraft::text(""), "c", false);
        assert!(!s.has("c"));
        s.stash(ComposerDraft { text: "ab".into(), mentions: vec![], reply_to: None }, "c", true);
        s.stash(ComposerDraft { text: "@x".into(), mentions: vec![Mention { uid: "u".into(), pos: 0, len: 2, is_all: false }], reply_to: None }, "c", true);
        let (d, notice) = s.take_with_notice("c").unwrap();
        assert_eq!(d.text, "ab\n@x");
        assert_eq!(d.mentions[0].pos, 3);
        assert!(!notice, "silent stash");
        s.stash(ComposerDraft::text("a"), "d", true);
        s.stash(ComposerDraft::text("b"), "d", false);
        assert!(s.take_with_notice("d").unwrap().1, "silent + normal = normal");
        assert_eq!(s.revision, 4);
        s.stash(ComposerDraft::text("z"), "e", false);
        assert_eq!(s.drain().len(), 1);
        assert!(!s.has("e"));
    }

    #[test]
    fn appending_counts_utf16() {
        let a = ComposerDraft { text: "😀".into(), mentions: vec![], reply_to: None };
        let b = ComposerDraft { text: "@A".into(), mentions: vec![Mention { uid: "1".into(), pos: 0, len: 2, is_all: false }], reply_to: None };
        assert_eq!(a.appending(&b).mentions[0].pos, 3);
    }

    #[test]
    fn misc_models() {
        assert_eq!(FriendRequestPrep::from_json(&json!({"maxMessage": 0})).max_message, 150);
        assert_eq!(FriendRequestPrep::from_json(&json!({"maxMessage": 900, "state": "self"})), FriendRequestPrep {
            state: FriendRequestState::Me,
            default_message: "".into(),
            max_message: 500
        });
        let r = ReminderSnapshot::from_json(&json!({"variant": "invite", "isGroup": true, "canRsvp": true, "accepted": 2, "rejected": -1})).unwrap();
        assert!(r.shows_rsvp_buttons() && r.is_drawn());
        assert_eq!(r.responder_summary().as_deref(), Some("2 người tham gia"));
        let rr = ReminderResponders::from_json(&json!({"ok": true, "accept": [{"uid": "a"}, {"uid": "a"}], "acceptTotal": 0})).unwrap();
        assert_eq!((rr.accept.len(), rr.accept_total), (1, 1));
        assert!(ReminderResponders::from_json(&json!({"ok": false})).is_none());
        assert_eq!(MentionCandidate::list(&json!([{"id": "-1", "isAll": true, "name": ""}, {"id": "2", "name": " "}, {"id": "3", "name": "Lan"}])).len(), 2);
        assert_eq!(SuggestedSticker::list(&json!([{"ref": "r", "url": "https://x/y.png"}, {"ref": "s", "url": "data:x"}])).len(), 1);
        assert!(ContactProfile::from_json(&json!({"uid": ""})).is_none());
    }
}
