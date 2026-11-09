//! Polls: details, votes, voters, lock, share, create and pin, bridged to `window.__zaloPoll` (posts `{poll:{id, detail|error}}`).
//! Port of `ZaloEngine+Poll.swift`. Every write follows a click.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use olaz_core::exec::spawn;
use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::{ChatMessage, J, Kind, clamped_int};

fn date_ms(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|ms| *ms > 0.0 && *ms < 4_102_444_800_000.0)
}
fn flag(v: &Value, k: &str) -> bool {
    v.get_v(k).and_then(Value::as_bool).unwrap_or(false)
}
fn count(v: &Value, k: &str) -> i64 {
    v.get_v(k).and_then(Value::as_f64).map(clamped_int).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollOption {
    pub id: String,
    pub content: String,
    pub votes: i64,
    pub voted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollDetail {
    pub id: String,
    pub group_id: String,
    pub question: String,
    pub creator: String,
    pub creator_name: String,
    pub created: Option<f64>,
    pub expires: Option<f64>,
    pub closed: bool,
    pub joined: bool,
    pub multi: bool,
    pub allow_add: bool,
    pub anonymous: bool,
    pub hide_preview: bool,
    pub voter_count: i64,
    pub can_close: bool,
    pub options: Vec<PollOption>,
}

impl PollDetail {
    pub fn from_json(j: &Value) -> Option<Self> {
        let id = j.s("id");
        if id.is_empty() {
            return None;
        }
        Some(PollDetail {
            id,
            group_id: j.s("groupId"),
            question: j.s("question"),
            creator: j.s("creator"),
            creator_name: j.s("creatorName"),
            created: date_ms(j.get_v("created")),
            expires: date_ms(j.get_v("expired")),
            closed: flag(j, "closed"),
            joined: flag(j, "joined"),
            multi: flag(j, "multi"),
            allow_add: flag(j, "allowAdd"),
            anonymous: flag(j, "anonymous"),
            hide_preview: flag(j, "hidePreview"),
            voter_count: count(j, "voters"),
            can_close: flag(j, "canClose"),
            options: j
                .arr("options")
                .iter()
                .filter_map(|r| {
                    let id = r.s("id");
                    (!id.is_empty()).then(|| PollOption { id, content: r.s("content"), votes: count(r, "votes").max(0), voted: flag(r, "voted") })
                })
                .collect(),
        })
    }
    pub fn total_votes(&self) -> i64 {
        self.options.iter().map(|o| o.votes).sum()
    }
    pub fn is_expired(&self, now_ms: f64) -> bool {
        self.expires.is_some_and(|e| e < now_ms)
    }
    pub fn is_voting_locked(&self, now_ms: f64) -> bool {
        self.closed || self.is_expired(now_ms) || (self.hide_preview && self.joined)
    }
    pub fn results_hidden(&self) -> bool {
        self.hide_preview && !self.joined
    }
    pub fn voters_visible(&self) -> bool {
        !self.anonymous && !self.results_hidden() && self.total_votes() > 0
    }
    pub fn weight(&self, o: &PollOption) -> f64 {
        let t = self.total_votes();
        if t > 0 { o.votes as f64 / t as f64 } else { 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AddedOption {
    pub id: u64,
    pub text: String,
    pub voted: bool,
}

/// The user's pending vote on a poll (selected options + options being added).
#[derive(Debug, Clone, PartialEq)]
pub struct PollVoteDraft {
    pub selected: HashSet<String>,
    original: HashSet<String>,
    multi: bool,
    pub added: Vec<AddedOption>,
    next_id: u64,
}

impl PollVoteDraft {
    pub fn new(d: &PollDetail) -> Self {
        let voted: HashSet<String> = d.options.iter().filter(|o| o.voted).map(|o| o.id.clone()).collect();
        PollVoteDraft { selected: voted.clone(), original: voted, multi: d.multi, added: vec![], next_id: 1 }
    }
    pub fn toggle(&mut self, id: &str) {
        if self.selected.remove(id) {
            return;
        }
        if !self.multi {
            self.selected.clear();
            self.added.iter_mut().for_each(|a| a.voted = false);
        }
        self.selected.insert(id.into());
    }
    pub fn set_added_voted(&mut self, id: u64, voted: bool) {
        if !self.added.iter().any(|a| a.id == id) {
            return;
        }
        if voted && !self.multi {
            self.selected.clear();
            self.added.iter_mut().for_each(|a| a.voted = false);
        }
        if let Some(a) = self.added.iter_mut().find(|a| a.id == id) {
            a.voted = voted;
        }
    }
    pub fn add_row(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.added.push(AddedOption { id, ..Default::default() });
        id
    }
    pub fn remove_added(&mut self, id: u64) {
        self.added.retain(|a| a.id != id);
    }
    pub fn set_added_text(&mut self, id: u64, text: &str) {
        if let Some(a) = self.added.iter_mut().find(|a| a.id == id) {
            a.text = text.into();
        }
    }
    fn filled(&self) -> Vec<AddedOption> {
        self.added.iter().map(|a| AddedOption { id: a.id, text: a.text.trim().into(), voted: a.voted }).filter(|a| !a.text.is_empty()).collect()
    }
    pub fn has_changes(&self) -> bool {
        self.selected != self.original || !self.filled().is_empty()
    }
    pub fn votes_nothing(&self) -> bool {
        self.selected.is_empty() && !self.filled().iter().any(|a| a.voted)
    }
    pub fn has_duplicates(&self, d: &PollDetail) -> bool {
        let all: Vec<String> = d.options.iter().map(|o| o.content.trim().to_string()).chain(self.filled().into_iter().map(|a| a.text)).collect();
        all.iter().collect::<HashSet<_>>().len() != all.len()
    }
    pub fn is_duplicate(&self, id: u64, d: &PollDetail) -> bool {
        let filled = self.filled();
        let Some(row) = filled.iter().find(|a| a.id == id) else { return false };
        d.options.iter().any(|o| o.content.trim() == row.text) || filled.iter().any(|a| a.id != id && a.text == row.text)
    }
    pub fn request(&self, d: &PollDetail) -> (Vec<Value>, Vec<String>) {
        (
            self.filled().iter().map(|a| json!({"voted": a.voted, "content": a.text})).collect(),
            d.options.iter().map(|o| o.id.clone()).filter(|id| self.selected.contains(id)).collect(),
        )
    }
    /// The detail as it will look once the vote goes through (optimistic).
    pub fn applied(&self, d: &PollDetail) -> PollDetail {
        let mut next = d.clone();
        for (i, o) in next.options.iter_mut().enumerate() {
            let was = d.options[i].voted;
            let now = self.selected.contains(&o.id);
            o.voted = now;
            if was != now {
                o.votes = (o.votes + if now { 1 } else { -1 }).max(0);
            }
        }
        next
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollVoter {
    pub uid: String,
    pub name: String,
    pub avatar: Option<String>,
    pub is_me: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollVoterGroup {
    pub id: String,
    pub content: String,
    pub votes: i64,
    pub users: Vec<PollVoter>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollVoters {
    pub restricted: bool,
    pub groups: Vec<PollVoterGroup>,
}

impl PollVoters {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let o = v.filter(|v| v.is_object())?;
        Some(PollVoters {
            restricted: flag(o, "restricted"),
            groups: o
                .arr("groups")
                .iter()
                .filter_map(|r| {
                    let id = r.opt_s("optionId")?;
                    Some(PollVoterGroup {
                        id,
                        content: r.s("content"),
                        votes: count(r, "votes"),
                        users: r
                            .arr("users")
                            .iter()
                            .filter_map(|u| {
                                let uid = u.s("uid");
                                (!uid.is_empty()).then(|| PollVoter { uid, name: u.s("name"), avatar: u.opt_s("avatar").filter(|a| !a.is_empty()), is_me: flag(u, "isMe") })
                            })
                            .collect(),
                    })
                })
                .collect(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PollConfig {
    pub min_options: i64,
    pub max_options: i64,
    pub max_question: i64,
    pub max_option: i64,
    pub enable_anonymous: bool,
    pub enable_hide_preview: bool,
    pub display_options: i64,
}

impl Default for PollConfig {
    fn default() -> Self {
        PollConfig { min_options: 2, max_options: 30, max_question: 200, max_option: 120, enable_anonymous: true, enable_hide_preview: true, display_options: 3 }
    }
}

impl PollConfig {
    pub fn from_json(v: Option<&Value>) -> Self {
        let mut c = PollConfig::default();
        let Some(j) = v.filter(|v| v.is_object()) else { return c };
        let pos = |k: &str, f: i64| {
            let n = count(j, k);
            if n > 0 { n } else { f }
        };
        c.min_options = pos("minOptions", c.min_options);
        c.max_options = c.min_options.max(pos("maxOptions", c.max_options));
        c.max_question = pos("maxQuestion", c.max_question);
        c.max_option = pos("maxOption", c.max_option);
        c.display_options = pos("displayOptions", c.display_options);
        c.enable_anonymous = j.get_v("enableAnonymous").and_then(Value::as_bool).unwrap_or(true);
        c.enable_hide_preview = j.get_v("enableHidePreview").and_then(Value::as_bool).unwrap_or(true);
        c
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PollCapabilities {
    pub ready: bool,
    pub can_create: bool,
    pub can_pin: bool,
    pub config: PollConfig,
}

impl PollCapabilities {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(o) = v.filter(|v| v.is_object()) else { return Self::default() };
        PollCapabilities { ready: flag(o, "ready"), can_create: flag(o, "canCreate"), can_pin: flag(o, "canPin"), config: PollConfig::from_json(o.get_v("config")) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CreateIssue {
    EmptyQuestion,
    TooFewOptions(i64),
    Duplicate,
    PastDeadline,
}

impl CreateIssue {
    pub fn text(&self) -> String {
        match self {
            CreateIssue::EmptyQuestion => "Vui lòng nhập chủ đề bình chọn".into(),
            CreateIssue::TooFewOptions(n) => format!("Cần ít nhất {n} phương án"),
            CreateIssue::Duplicate => "Phương án bị trùng".into(),
            CreateIssue::PastDeadline => "Thời hạn phải ở tương lai".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollCreateDraft {
    pub question: String,
    pub options: Vec<String>,
    pub has_deadline: bool,
    /// ms since epoch.
    pub deadline: f64,
    pub pin: bool,
    pub multi: bool,
    pub allow_add: bool,
    pub hide_preview: bool,
    pub anonymous: bool,
}

impl PollCreateDraft {
    pub fn new(config: &PollConfig, now_ms: f64) -> Self {
        PollCreateDraft {
            question: String::new(),
            options: vec![String::new(); config.min_options.max(0) as usize],
            has_deadline: false,
            deadline: now_ms + 86_400_000.0,
            pin: false,
            multi: true,
            allow_add: true,
            hide_preview: false,
            anonymous: false,
        }
    }
    pub fn filled_options(&self) -> Vec<String> {
        self.options.iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect()
    }
    pub fn is_duplicate(&self, index: usize) -> bool {
        let Some(t) = self.options.get(index).map(|o| o.trim()) else { return false };
        !t.is_empty() && self.options.iter().enumerate().any(|(i, o)| i != index && o.trim() == t)
    }
    pub fn issue(&self, config: &PollConfig, now_ms: f64) -> Option<CreateIssue> {
        if self.question.trim().is_empty() {
            return Some(CreateIssue::EmptyQuestion);
        }
        let filled = self.filled_options();
        if filled.iter().collect::<HashSet<_>>().len() != filled.len() {
            return Some(CreateIssue::Duplicate);
        }
        if (filled.len() as i64) < config.min_options {
            return Some(CreateIssue::TooFewOptions(config.min_options));
        }
        if self.has_deadline && self.deadline <= now_ms {
            return Some(CreateIssue::PastDeadline);
        }
        None
    }
    pub fn effective_allow_add(&self) -> bool {
        self.allow_add && !self.hide_preview
    }
    pub fn spec(&self, config: &PollConfig, can_pin: bool) -> Value {
        let cut = |s: &str, n: i64| s.chars().take(n.max(0) as usize).collect::<String>();
        json!({
            "question": cut(self.question.trim(), config.max_question),
            "options": self.filled_options().iter().map(|o| cut(o, config.max_option)).collect::<Vec<_>>(),
            "expired": if self.has_deadline { self.deadline as i64 } else { 0 },
            "pin": self.pin && can_pin,
            "multi": self.multi,
            "allowAdd": self.effective_allow_add(),
            "hidePreview": self.hide_preview && config.enable_hide_preview,
            "anonymous": self.anonymous && config.enable_anonymous,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollAction {
    Vote,
    Lock,
    Share,
    Create,
    Pin,
    Unpin,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PollActionResult {
    pub ok: bool,
    pub error: Option<String>,
    pub pin_skipped: bool,
}

impl PollActionResult {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(j) = v.filter(|v| v.is_object()) else { return PollActionResult { ok: false, error: Some("unavailable".into()), pin_skipped: false } };
        PollActionResult { ok: flag(j, "ok"), error: j.opt_s("error"), pin_skipped: flag(j, "pinSkipped") }
    }
    pub fn message(&self, action: PollAction) -> &'static str {
        match self.error.as_deref() {
            Some("closed") => "Có lỗi xảy ra do bình chọn đã bị khoá",
            Some("expired") => "Bình chọn đã hết hạn",
            Some("network") => "Không có mạng, vui lòng kiểm tra lại kết nối và thử lại",
            Some("duplicate") => "Phương án được thêm đã tồn tại",
            Some("forbidden") => match action {
                PollAction::Create => "Bạn không đủ quyền tạo bình chọn",
                PollAction::Pin | PollAction::Unpin => "Bạn không có quyền thực hiện thao tác này.",
                _ => "Bạn không có quyền khóa bình chọn này",
            },
            Some("limit") => "Bảng ghim đã đầy. Hãy bỏ bớt ghim trong Zalo rồi thử lại.",
            Some("partial") => "Đã bỏ ghim cũ nhưng chưa ghim được bình chọn. Vui lòng thử ghim lại.",
            Some("unknown") => "Chưa xác nhận được kết quả. Vui lòng kiểm tra lại bình chọn sau.",
            _ => match action {
                PollAction::Vote => "Có lỗi xảy ra trong quá trình bình chọn.",
                PollAction::Lock => "Có lỗi xảy ra trong quá trình khóa bình chọn. Vui lòng thử lại.",
                PollAction::Share => "Có lỗi xảy ra trong quá trình chia sẻ bình chọn. Vui lòng thử lại.",
                PollAction::Create => "Có lỗi xảy ra trong quá trình tạo bình chọn. Vui lòng kiểm tra lại.",
                PollAction::Pin => "Ghim bình chọn không thành công. Vui lòng thử lại.",
                PollAction::Unpin => "Bỏ ghim không thành công. Vui lòng thử lại.",
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PollPinState {
    pub ready: bool,
    pub can_pin: bool,
    pub pinned: bool,
    pub limit: i64,
}

impl PollPinState {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(o) = v.filter(|v| v.is_object()) else { return Self::default() };
        PollPinState { ready: flag(o, "ready"), can_pin: flag(o, "canPin"), pinned: flag(o, "pinned"), limit: count(o, "limit").max(0) }
    }
    pub fn offers_pin(&self) -> bool {
        self.ready && self.can_pin
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PollPinConfirm {
    pub r#ref: String,
    pub limit: i64,
    pub kind: String,
    pub title: String,
}

impl PollPinConfirm {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let o = v?;
        let r = o.opt_s("ref").filter(|r| !r.is_empty())?;
        let replace = o.get_v("replace").cloned().unwrap_or(Value::Null);
        Some(PollPinConfirm { r#ref: r, limit: count(o, "limit").max(0), kind: replace.s("kind"), title: replace.s("title") })
    }
    pub fn message(&self) -> String {
        let label = match self.kind.as_str() {
            "poll" => "Bình chọn",
            "reminder" => "Nhắc hẹn",
            "message" => "Tin nhắn",
            _ => "Ghi chú",
        };
        let head = if self.limit > 0 { format!("Đã đạt giới hạn {} ghim. ", self.limit) } else { String::new() };
        format!("{head}Ghim cũ dưới đây sẽ được bỏ để cập nhật nội dung mới.\n{}", if self.title.is_empty() { label.to_string() } else { format!("{label}: {}", self.title) })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PollPinOutcome {
    Done,
    NeedsConfirmation(PollPinConfirm),
    Failed(PollActionResult),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PollStore {
    pub details: HashMap<String, PollDetail>,
    pub pins: HashMap<String, PollPinState>,
    pub failed: HashSet<String>,
    // Clock (watch / refresh bookkeeping).
    pub watched: HashMap<String, String>,
    pub last_attempt: HashMap<String, f64>,
    pub seen_messages: HashSet<String>,
    pub refresh_gen: HashMap<String, u64>,
}

impl ZaloEngine {
    fn call_poll(&self, method: &str, args: Vec<Value>) {
        self.call_chat(method, args, "__zaloPoll");
    }

    async fn await_poll(&self, secs: u64, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        let script = format!("const c = window.__zaloPoll; if (!c || !c.{method}) return null; return await c.{method}(...{json});");
        self.race(secs, self.evaluate(&script)).await.flatten()
    }

    pub fn watch_poll(&self, id: &str, conv: &str) {
        if id.is_empty() {
            return;
        }
        let now = crate::chat::now_ms();
        {
            let mut s = self.state.borrow_mut();
            let p = &mut s.polls;
            p.watched.insert(id.into(), conv.into());
            if p.watched.len() > 500 {
                if let Some(drop) = p.watched.keys().find(|k| k.as_str() != id).cloned() {
                    p.watched.remove(&drop);
                    p.last_attempt.remove(&drop);
                }
            }
            if p.details.contains_key(id) {
                return;
            }
            if p.last_attempt.get(id).is_some_and(|t| now - t < 5000.0) {
                return;
            }
            p.last_attempt.insert(id.into(), now);
            p.failed.remove(id);
        }
        self.call_poll("watch", vec![json!(id), json!(conv)]);
    }

    pub fn retry_poll(&self, id: &str, conv: &str) {
        {
            let mut s = self.state.borrow_mut();
            s.polls.last_attempt.insert(id.into(), crate::chat::now_ms());
            s.polls.failed.remove(id);
        }
        self.changed(Change::Other);
        self.call_poll("watch", vec![json!(id), json!(conv)]);
    }

    pub fn rewatch_polls(&self) {
        let watched: Vec<(String, String)> = self.state.borrow().polls.watched.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
        for (id, conv) in watched {
            self.call_poll("watch", vec![json!(id), json!(conv)]);
        }
    }

    pub fn refresh_poll(&self, id: &str) {
        self.call_poll("refresh", vec![json!(id), json!(false)]);
    }

    pub async fn poll_voters(&self, id: &str) -> Option<PollVoters> {
        PollVoters::from_json(self.await_poll(45, "voters", vec![json!(id)]).await.as_ref())
    }

    pub async fn refresh_poll_pin(&self, id: &str, conv: &str) -> PollPinState {
        let state = PollPinState::from_json(self.await_poll(30, "pinState", vec![json!(conv), json!(id)]).await.as_ref());
        let changed = {
            let mut s = self.state.borrow_mut();
            let known = s.polls.pins.get(id).copied();
            if (state.ready || known.is_none()) && known != Some(state) {
                s.polls.pins.insert(id.into(), state);
                true
            } else {
                false
            }
        };
        if changed {
            self.changed(Change::Other);
        }
        self.state.borrow().polls.pins.get(id).copied().unwrap_or(state)
    }

    pub async fn pin_poll(&self, id: &str, conv: &str, confirming: Option<&str>) -> PollPinOutcome {
        let Some(answer) = self.await_poll(100, "pin", vec![json!(conv), json!(id), confirming.map(|r| json!(r)).unwrap_or(Value::Null)]).await else {
            self.settle_poll_pin(id, conv, true, false);
            return PollPinOutcome::Failed(PollActionResult { ok: false, error: Some("unknown".into()), pin_skipped: false });
        };
        if answer.s("error") == "confirm" {
            if let Some(c) = PollPinConfirm::from_json(Some(&answer)) {
                return PollPinOutcome::NeedsConfirmation(c);
            }
        }
        let r = PollActionResult::from_json(Some(&answer));
        if r.ok {
            self.settle_poll_pin(id, conv, true, true);
            PollPinOutcome::Done
        } else {
            PollPinOutcome::Failed(r)
        }
    }

    pub async fn unpin_poll(&self, id: &str, conv: &str) -> PollActionResult {
        let Some(answer) = self.await_poll(100, "unpin", vec![json!(conv), json!(id)]).await else {
            self.settle_poll_pin(id, conv, false, false);
            return PollActionResult { ok: false, error: Some("unknown".into()), pin_skipped: false };
        };
        let r = PollActionResult::from_json(Some(&answer));
        if r.ok {
            self.settle_poll_pin(id, conv, false, true);
        }
        r
    }

    fn settle_poll_pin(&self, id: &str, conv: &str, expected: bool, optimistic: bool) {
        if optimistic {
            let mut s = self.state.borrow_mut();
            if let Some(st) = s.polls.pins.get_mut(id).filter(|st| st.ready) {
                st.pinned = expected;
            }
        }
        let (weak, exec, id, conv) = (self.weak(), self.exec.clone(), id.to_string(), conv.to_string());
        spawn(&self.exec, async move {
            for delay in [1000u64, 2500, 5000] {
                exec.timer(Duration::from_millis(delay)).await;
                let Some(me) = weak.upgrade() else { return };
                if me.refresh_poll_pin(&id, &conv).await.pinned == expected {
                    return;
                }
            }
        });
    }

    pub async fn poll_capabilities(&self, conv: &str) -> PollCapabilities {
        PollCapabilities::from_json(self.await_poll(30, "capabilities", vec![json!(conv)]).await.as_ref())
    }

    pub async fn vote_poll(&self, id: &str, conv: &str, draft: &PollVoteDraft, d: &PollDetail) -> PollActionResult {
        let (new_options, voted) = draft.request(d);
        PollActionResult::from_json(self.await_poll(60, "vote", vec![json!(conv), json!(id), json!(new_options), json!(voted)]).await.as_ref())
    }

    pub async fn lock_poll(&self, id: &str, conv: &str) -> PollActionResult {
        PollActionResult::from_json(self.await_poll(60, "lock", vec![json!(conv), json!(id)]).await.as_ref())
    }

    pub async fn share_poll(&self, id: &str, conv: &str) -> PollActionResult {
        PollActionResult::from_json(self.await_poll(60, "share", vec![json!(conv), json!(id)]).await.as_ref())
    }

    pub async fn create_poll(&self, conv: &str, draft: &PollCreateDraft, config: &PollConfig, can_pin: bool) -> PollActionResult {
        PollActionResult::from_json(self.await_poll(90, "create", vec![json!(conv), draft.spec(config, can_pin)]).await.as_ref())
    }

    /// A poll message that just arrived (vote / lock notices) refreshes a poll already on screen, coalesced over 1.5 s.
    pub(crate) fn note_poll_messages(&self, incoming: &[ChatMessage]) {
        let now = crate::chat::now_ms();
        for m in incoming {
            let Kind::Poll(info) = &m.kind else { continue };
            let id = info.poll_id.clone();
            {
                let mut s = self.state.borrow_mut();
                let p = &mut s.polls;
                if id.is_empty() || !p.details.contains_key(&id) || m.date <= now - 120_000.0 || !p.seen_messages.insert(m.id.clone()) {
                    continue;
                }
                if p.seen_messages.len() > 2000 {
                    p.seen_messages = HashSet::from([m.id.clone()]);
                }
            }
            let generation = {
                let mut s = self.state.borrow_mut();
                let g = s.polls.refresh_gen.entry(id.clone()).or_default();
                *g += 1;
                *g
            };
            let (weak, exec) = (self.weak(), self.exec.clone());
            spawn(&self.exec, async move {
                exec.timer(Duration::from_millis(1500)).await;
                let Some(me) = weak.upgrade() else { return };
                if me.state.borrow().polls.refresh_gen.get(&id) == Some(&generation) {
                    me.call_poll("refresh", vec![json!(id), json!(true)]);
                }
            });
        }
    }

    pub(crate) fn handle_poll(&self, state: &Map<String, Value>) {
        let Some(j) = state.get("poll").filter(|p| p.is_object()) else { return };
        let Some(id) = j.opt_s("id") else { return };
        let detail = j.get_v("detail").and_then(PollDetail::from_json);
        let changed = {
            let mut s = self.state.borrow_mut();
            match detail {
                Some(d) => {
                    s.polls.failed.remove(&id);
                    if s.polls.details.get(&id) != Some(&d) {
                        s.polls.details.insert(id, d);
                        true
                    } else {
                        false
                    }
                }
                None if j.get_v("error").is_some() && !s.polls.details.contains_key(&id) => s.polls.failed.insert(id),
                None => false,
            }
        };
        if changed {
            self.changed(Change::Other);
        }
    }

    pub fn reset_polls(&self) {
        self.state.borrow_mut().polls = PollStore::default();
        self.call_poll("reset", vec![]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(multi: bool) -> PollDetail {
        PollDetail::from_json(&json!({"id": "p", "multi": multi, "voters": 3, "options": [
            {"id": "a", "content": "Phở", "votes": 2, "voted": true}, {"id": "b", "content": "Bún", "votes": 1}, {"id": "", "content": "x"}
        ]}))
        .unwrap()
    }

    #[test]
    fn detail_rules() {
        let d = detail(false);
        assert_eq!((d.options.len(), d.total_votes()), (2, 3));
        assert!((d.weight(&d.options[0]) - 2.0 / 3.0).abs() < 1e-9);
        let mut h = d.clone();
        h.hide_preview = true;
        assert!(h.results_hidden() && !h.voters_visible());
        h.joined = true;
        assert!(h.is_voting_locked(0.0) && !h.results_hidden());
        let mut e = d.clone();
        e.expires = Some(10.0);
        assert!(e.is_voting_locked(11.0) && !e.is_voting_locked(9.0));
        assert!(PollDetail::from_json(&json!({"id": "x", "expired": 9e15})).unwrap().expires.is_none());
    }

    #[test]
    fn single_choice_draft() {
        let d = detail(false);
        let mut v = PollVoteDraft::new(&d);
        assert!(!v.has_changes());
        v.toggle("b");
        assert_eq!(v.selected, HashSet::from(["b".to_string()]));
        let row = v.add_row();
        v.set_added_text(row, " Cơm ");
        v.set_added_voted(row, true);
        assert!(v.selected.is_empty(), "single choice: voting the new option drops the others");
        let (new_options, voted) = v.request(&d);
        assert_eq!(new_options, vec![json!({"voted": true, "content": "Cơm"})]);
        assert!(voted.is_empty());
        v.set_added_text(row, "Phở");
        assert!(v.has_duplicates(&d) && v.is_duplicate(row, &d));
        let mut m = PollVoteDraft::new(&detail(true));
        m.toggle("b");
        let after = m.applied(&detail(true));
        assert_eq!((after.options[0].votes, after.options[1].votes, after.options[1].voted), (2, 2, true));
        m.toggle("a");
        assert_eq!(m.applied(&detail(true)).options[0].votes, 1);
    }

    #[test]
    fn create_draft_and_texts() {
        let cfg = PollConfig::from_json(Some(&json!({"minOptions": 3, "maxOptions": 1, "maxOption": 2})));
        assert_eq!((cfg.min_options, cfg.max_options), (3, 3));
        let mut c = PollCreateDraft::new(&cfg, 0.0);
        assert_eq!(c.options.len(), 3);
        assert_eq!(c.issue(&cfg, 0.0), Some(CreateIssue::EmptyQuestion));
        c.question = " Ăn gì? ".into();
        c.options = vec!["Phở".into(), "Bún".into(), " ".into()];
        assert_eq!(c.issue(&cfg, 0.0), Some(CreateIssue::TooFewOptions(3)));
        c.options[2] = "Phở".into();
        assert_eq!(c.issue(&cfg, 0.0), Some(CreateIssue::Duplicate));
        assert!(c.is_duplicate(2) && !c.is_duplicate(1));
        c.options[2] = "Cơm".into();
        c.has_deadline = true;
        c.deadline = 5.0;
        assert_eq!(c.issue(&cfg, 10.0), Some(CreateIssue::PastDeadline));
        c.has_deadline = false;
        c.hide_preview = true;
        c.pin = true;
        let spec = c.spec(&cfg, false);
        assert_eq!(spec["options"], json!(["Ph", "Bú", "Cơ"]));
        assert_eq!((spec["pin"].clone(), spec["allowAdd"].clone(), spec["question"].clone()), (json!(false), json!(false), json!("Ăn gì?")));
        let r = PollActionResult::from_json(Some(&json!({"ok": false, "error": "forbidden"})));
        assert_eq!(r.message(PollAction::Create), "Bạn không đủ quyền tạo bình chọn");
        assert_eq!(PollActionResult::from_json(None).message(PollAction::Vote), "Có lỗi xảy ra trong quá trình bình chọn.");
        let pc = PollPinConfirm::from_json(Some(&json!({"ref": "3:1", "limit": 3, "replace": {"kind": "message", "title": "Hi"}}))).unwrap();
        assert_eq!(pc.message(), "Đã đạt giới hạn 3 ghim. Ghim cũ dưới đây sẽ được bỏ để cập nhật nội dung mới.\nTin nhắn: Hi");
        let v = PollVoters::from_json(Some(&json!({"groups": [{"optionId": "a", "votes": 1, "users": [{"uid": "u", "name": "A", "isMe": true}, {"uid": ""}]}]}))).unwrap();
        assert!(v.groups[0].users.len() == 1 && v.groups[0].users[0].is_me);
    }
}
