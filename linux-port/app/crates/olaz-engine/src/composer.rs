//! Composer "+" actions: media, files, contact card, reminders, official toasts and confirms.
//! Port of `ZaloEngine+Composer.swift`.

use std::path::PathBuf;
use std::time::Duration;

use olaz_core::exec::spawn;
use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::{DEFAULT_PENDING, J, NO_ANSWER_TEXT};

pub const GENERIC_FAILURE: &str = "Không gửi được. Hãy thử lại.";

#[derive(Debug, Clone, PartialEq)]
pub enum ComposerOutcome {
    Done,
    Cancelled,
    Failed(String),
    Pending(String),
}

impl ComposerOutcome {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(status) = v.and_then(|v| v.get_v("status")).and_then(Value::as_str) else { return Self::Failed(GENERIC_FAILURE.into()) };
        let text = v.and_then(|v| v.opt_s("text")).filter(|t| !t.is_empty());
        match status {
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            "invalid" => text.map(Self::Failed).unwrap_or(Self::Cancelled),
            "pending" => Self::Pending(text.unwrap_or_else(|| DEFAULT_PENDING.into())),
            "not-open" => Self::Failed("Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát.".into()),
            "not-ready" => Self::Failed("Zalo chưa sẵn sàng, hãy thử lại sau giây lát.".into()),
            _ => Self::Failed(text.unwrap_or_else(|| GENERIC_FAILURE.into())),
        }
    }
    pub fn log_name(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Failed(_) => "failed",
            Self::Pending(_) => "pending",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerConfirm {
    pub id: String,
    pub conversation_id: String,
    pub title: String,
    pub message: String,
    pub ok: String,
    pub cancel: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MyCardInfo {
    pub uid: String,
    pub name: String,
    pub avatar: Option<String>,
    pub phone: String,
    pub with_phone: bool,
}

impl MyCardInfo {
    pub fn from_json(v: &Value) -> Option<Self> {
        let uid = v.s("uid");
        if uid.is_empty() {
            return None;
        }
        Some(Self {
            uid,
            name: v.s("name"),
            avatar: v.opt_s("avatar").filter(|a| !a.is_empty()),
            phone: v.s("phone"),
            with_phone: v.get_v("withPhone").and_then(Value::as_bool).unwrap_or(true),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReminderComposeInfo {
    pub allowed: bool,
    pub hidden: bool,
    pub text: String,
    pub is_group: bool,
    pub can_pin: bool,
    pub pin_toggle: bool,
    pub title_max_chars: usize,
    pub title_max_bytes: usize,
}

impl ReminderComposeInfo {
    pub fn from_json(v: &Value) -> Option<Self> {
        v.as_object()?;
        let b = |k: &str| v.get_v(k).and_then(Value::as_bool).unwrap_or(false);
        Some(Self {
            allowed: b("allowed"),
            hidden: b("hidden"),
            text: v.s("text"),
            is_group: b("isGroup"),
            can_pin: b("canPin"),
            pin_toggle: b("pinToggle"),
            title_max_chars: v.opt_n("titleMaxChars").map(|n| n as usize).unwrap_or(3000),
            title_max_bytes: v.opt_n("titleMaxBytes").map(|n| n as usize).unwrap_or(3700),
        })
    }
    pub fn title_too_long(&self, raw: &str) -> bool {
        raw.encode_utf16().count() >= self.title_max_chars || raw.trim().len() > self.title_max_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReminderRepeat {
    None = 0,
    Daily = 1,
    Weekly = 2,
    Monthly = 3,
}

impl ReminderRepeat {
    pub fn title(self) -> &'static str {
        match self {
            Self::None => "Không lặp lại",
            Self::Daily => "Hàng ngày",
            Self::Weekly => "Hàng tuần",
            Self::Monthly => "Hàng tháng",
        }
    }
}

/// A toast the UI shows (conversation-scoped or global) — `CardActionCenter.show`.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub seq: u64,
    pub text: String,
    pub conversation: Option<String>,
}

impl ZaloEngine {
    pub fn show_toast(&self, text: &str, conversation: Option<&str>) {
        let seq = {
            let mut s = self.state.borrow_mut();
            let seq = s.toast.as_ref().map(|t| t.seq + 1).unwrap_or(1);
            s.toast = Some(Toast { seq, text: text.to_string(), conversation: conversation.filter(|c| !c.is_empty()).map(str::to_string) });
            seq
        };
        self.changed(Change::Notice);
        let weak = self.weak();
        let exec = self.exec.clone();
        spawn(&self.exec, async move {
            exec.timer(Duration::from_millis(3200)).await;
            if let Some(me) = weak.upgrade() {
                let clear = me.state.borrow().toast.as_ref().is_some_and(|t| t.seq == seq);
                if clear {
                    me.state.borrow_mut().toast = None;
                    me.changed(Change::Notice);
                }
            }
        });
    }

    pub fn report(&self, outcome: &ComposerOutcome, conversation: &str) {
        if let ComposerOutcome::Failed(t) | ComposerOutcome::Pending(t) = outcome {
            self.show_toast(t, Some(conversation));
        }
    }

    async fn call_compose(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        self.await_call("__zaloCompose", method, args, "null").await
    }

    async fn compose(&self, method: &str, args: Vec<Value>, timeout: Duration) -> ComposerOutcome {
        let fut = self.call_compose(method, args);
        let timer = self.exec.timer(timeout);
        futures::pin_mut!(fut);
        let outcome = match futures::future::select(fut, timer).await {
            futures::future::Either::Left((v, _)) => ComposerOutcome::from_json(v.as_ref()),
            futures::future::Either::Right(_) => ComposerOutcome::Pending(NO_ANSWER_TEXT.into()),
        };
        olaz_core::log::write(format!("compose {method} → {}", outcome.log_name()));
        outcome
    }

    async fn compose_with_files(&self, method: &str, args: Vec<Value>, files: Vec<PathBuf>) -> ComposerOutcome {
        if files.is_empty() {
            return ComposerOutcome::Cancelled;
        }
        let Some(host) = self.host() else { return ComposerOutcome::Failed(GENERIC_FAILURE.into()) };
        let n = files.len();
        let script = format!(
            "const c = window.__zaloCompose; if (!c || !c.{method}) return null; return await c.{method}(...{});",
            Value::Array(args)
        );
        let fut = host.evaluate_providing_files(&script, files);
        let timer = self.exec.timer(Duration::from_secs(180));
        futures::pin_mut!(fut);
        let mut outcome = match futures::future::select(fut, timer).await {
            futures::future::Either::Left((v, _)) => ComposerOutcome::from_json(v.as_ref()),
            futures::future::Either::Right(_) => ComposerOutcome::Pending(NO_ANSWER_TEXT.into()),
        };
        // `cancelled` while files were picked = the page's input received none.
        if outcome == ComposerOutcome::Cancelled {
            outcome = ComposerOutcome::Failed("Không đọc được tệp đã chọn. Hãy thử lại.".into());
        }
        olaz_core::log::write(format!("compose {method} files={n} → {}", outcome.log_name()));
        outcome
    }

    pub async fn send_media(&self, files: Vec<PathBuf>, to: &str) -> ComposerOutcome {
        self.compose_with_files("sendMedia", vec![json!(to)], files).await
    }

    pub async fn send_files(&self, files: Vec<PathBuf>, to: &str) -> ComposerOutcome {
        self.compose_with_files("sendFiles", vec![json!(to)], files).await
    }

    pub async fn send_shot(&self, png: PathBuf, to: &str, to_me: bool) -> ComposerOutcome {
        self.compose_with_files("sendShot", vec![json!(to), json!(to_me)], vec![png]).await
    }

    pub async fn share_shot(&self, png: PathBuf, to: Vec<String>) -> ComposerOutcome {
        self.compose_with_files("shareShot", vec![json!(to)], vec![png]).await
    }

    pub async fn my_card(&self) -> Option<MyCardInfo> {
        let fut = self.call_compose("myCard", vec![]);
        let timer = self.exec.timer(Duration::from_secs(15));
        futures::pin_mut!(fut);
        match futures::future::select(fut, timer).await {
            futures::future::Either::Left((Some(v), _)) => MyCardInfo::from_json(&v),
            _ => None,
        }
    }

    pub async fn send_my_card(&self, to: &str, with_phone: bool) -> ComposerOutcome {
        self.compose("sendMyCard", vec![json!(to), json!(with_phone)], Duration::from_secs(45)).await
    }

    pub async fn reminder_compose_info(&self, conversation: &str) -> Option<ReminderComposeInfo> {
        let fut = self.call_compose("reminderInfo", vec![json!(conversation)]);
        let timer = self.exec.timer(Duration::from_secs(15));
        futures::pin_mut!(fut);
        match futures::future::select(fut, timer).await {
            futures::future::Either::Left((Some(v), _)) => ReminderComposeInfo::from_json(&v),
            _ => None,
        }
    }

    pub async fn create_reminder(&self, conversation: &str, title: &str, start_ms: f64, repeat: ReminderRepeat, pin: bool) -> ComposerOutcome {
        let options = json!({"title": title, "startTime": start_ms.round(), "repeat": repeat as i64, "needPin": pin});
        self.compose("createReminder", vec![json!(conversation), options], Duration::from_secs(40)).await
    }

    pub fn composer_confirm(&self) -> Option<ComposerConfirm> {
        self.state.borrow().composer_confirm.clone()
    }

    pub fn answer_composer_confirm(&self, confirm: &ComposerConfirm, ok: bool) {
        if self.state.borrow().composer_confirm.as_ref().map(|c| &c.id) != Some(&confirm.id) {
            return;
        }
        self.state.borrow_mut().composer_confirm = None;
        self.changed(Change::Notice);
        self.call_chat("answer", vec![json!(confirm.id), json!(ok)], "__zaloCompose");
        if self.state.borrow().queued_confirms.is_empty() {
            return;
        }
        let weak = self.weak();
        let exec = self.exec.clone();
        spawn(&self.exec, async move {
            exec.timer(Duration::from_millis(300)).await;
            let Some(me) = weak.upgrade() else { return };
            let next = {
                let mut s = me.state.borrow_mut();
                if s.composer_confirm.is_none() && !s.queued_confirms.is_empty() { Some(s.queued_confirms.remove(0)) } else { None }
            };
            if let Some(n) = next {
                me.state.borrow_mut().composer_confirm = Some(n);
                me.changed(Change::Notice);
            }
        });
    }

    pub fn drop_composer_confirms(&self) {
        let mut s = self.state.borrow_mut();
        s.composer_confirm = None;
        s.queued_confirms.clear();
    }

    pub(crate) fn handle_compose(&self, state: &Map<String, Value>) {
        let Some(ev) = state.get("compose").filter(|e| e.is_object()) else { return };
        let conv = ev.opt_s("convId").filter(|c| !c.is_empty());
        match ev.s("kind").as_str() {
            "toast" => {
                let t = ev.s("text");
                if !t.is_empty() {
                    self.show_toast(&t, conv.as_deref());
                }
            }
            "confirm" => {
                let Some(token) = ev.opt_s("token") else { return };
                let c = ComposerConfirm {
                    id: token,
                    conversation_id: conv.unwrap_or_default(),
                    title: ev.s("title"),
                    message: ev.s("message"),
                    ok: ev.opt_s("ok").unwrap_or_else(|| "OK".into()),
                    cancel: ev.opt_s("cancel").unwrap_or_else(|| "Hủy".into()),
                };
                let mut s = self.state.borrow_mut();
                if s.composer_confirm.is_none() {
                    s.composer_confirm = Some(c);
                } else {
                    s.queued_confirms.push(c);
                }
                drop(s);
                self.changed(Change::Notice);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        let o = |v: Value| ComposerOutcome::from_json(Some(&v));
        assert_eq!(ComposerOutcome::from_json(None), ComposerOutcome::Failed(GENERIC_FAILURE.into()));
        assert_eq!(o(json!({"status": "done"})), ComposerOutcome::Done);
        assert_eq!(o(json!({"status": "invalid"})), ComposerOutcome::Cancelled);
        assert_eq!(o(json!({"status": "invalid", "text": "Quá lớn"})), ComposerOutcome::Failed("Quá lớn".into()));
        assert_eq!(o(json!({"status": "pending"})), ComposerOutcome::Pending(DEFAULT_PENDING.into()));
        assert_eq!(o(json!({"status": "locked", "text": ""})), ComposerOutcome::Failed(GENERIC_FAILURE.into()));
        assert_eq!(o(json!({"status": "not-ready"})), ComposerOutcome::Failed("Zalo chưa sẵn sàng, hãy thử lại sau giây lát.".into()));
    }

    #[test]
    fn card_and_reminder_info() {
        assert!(MyCardInfo::from_json(&json!({"uid": ""})).is_none());
        assert!(MyCardInfo::from_json(&json!({"uid": "1"})).unwrap().with_phone);
        let r = ReminderComposeInfo::from_json(&json!({"allowed": true, "titleMaxChars": 5, "titleMaxBytes": 6})).unwrap();
        assert!(r.title_too_long("12345"));
        assert!(!r.title_too_long("1234"));
        assert!(r.title_too_long(" ệệệ "), "utf8 bytes of the trimmed title");
        assert_eq!(ReminderRepeat::Weekly.title(), "Hàng tuần");
    }
}
