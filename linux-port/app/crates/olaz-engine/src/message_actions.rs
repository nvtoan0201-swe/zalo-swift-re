//! Message actions: forward, copy, recall, delete + undo, pin, star; the pinned list.
//! Port of `ZaloEngine+MessageActions.swift`. Write actions run only from an explicit user click.

use std::time::Duration;

use olaz_core::exec::spawn;
use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::media::{MediaKind, MediaResolution};
use crate::models::{Availability, ChatMessage, DEFAULT_PENDING, J, Kind, MenuStar, NO_ANSWER_TEXT};

pub const PROTECTED: &str = "Trò chuyện này đã bật Chặn chia sẻ";

#[derive(Debug, Clone, PartialEq)]
pub enum MessageActionOutcome {
    Done(Option<String>),
    Confirm { title: String, message: String, ok: String, cancel: String, r#ref: Option<String> },
    Failed(String),
    Pending(String),
}

impl MessageActionOutcome {
    pub fn from_json(v: Option<&Value>, failure: &str) -> Self {
        let Some(status) = v.and_then(|v| v.get_v("status")).and_then(Value::as_str) else { return Self::Failed(failure.into()) };
        let v = v.unwrap();
        let text = v.opt_s("text").filter(|t| !t.is_empty());
        match status {
            "done" => Self::Done(text),
            "confirm" => Self::Confirm {
                title: v.s("title"),
                message: v.s("message"),
                ok: v.opt_s("ok").unwrap_or_else(|| "OK".into()),
                cancel: v.opt_s("cancel").unwrap_or_else(|| "Hủy".into()),
                r#ref: v.opt_s("ref").filter(|r| !r.is_empty()),
            },
            "pending" => Self::Pending(text.unwrap_or_else(|| DEFAULT_PENDING.into())),
            "not-ready" => Self::Failed(text.unwrap_or_else(|| "Zalo chưa sẵn sàng, hãy thử lại sau giây lát.".into())),
            "not-open" => Self::Failed(text.unwrap_or_else(|| "Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát.".into())),
            _ => Self::Failed(text.unwrap_or_else(|| failure.into())),
        }
    }
    pub fn log_name(&self) -> &'static str {
        match self {
            Self::Done(_) => "done",
            Self::Confirm { .. } => "confirm",
            Self::Failed(_) => "failed",
            Self::Pending(_) => "pending",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    DeleteForMe(Box<ChatMessage>),
    Pin(Box<ChatMessage>, String),
    Unpin(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageActionConfirm {
    pub id: u64,
    pub conversation_id: String,
    pub action: ConfirmAction,
    pub title: String,
    pub message: String,
    pub ok: String,
    pub cancel: String,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteUndo {
    pub id: String,
    pub conversation_id: String,
    pub count: i64,
    /// Instant as ms since epoch.
    pub deadline_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForwardRequest {
    pub conversation_id: String,
    pub message: ChatMessage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinKind {
    Message,
    Note,
    Link,
    Poll,
    Reminder,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PinnedItem {
    pub r#ref: String,
    pub kind: PinKind,
    pub title: String,
    pub sender: String,
    pub thumbnail: Option<String>,
    pub msg_id: String,
    pub cli_msg_id: String,
}

impl PinnedItem {
    pub fn label(&self) -> &'static str {
        match self.kind {
            PinKind::Message => "Tin nhắn",
            PinKind::Note | PinKind::Link => "Ghi chú",
            PinKind::Poll => "Bình chọn",
            PinKind::Reminder => "Nhắc hẹn",
        }
    }
    /// The last loaded message whose id equals `msgId` or `cliMsgId` (message kind only).
    pub fn message_in<'a>(&self, list: &'a [ChatMessage]) -> Option<&'a ChatMessage> {
        if self.kind != PinKind::Message {
            return None;
        }
        list.iter().rev().find(|m| (!self.msg_id.is_empty() && m.id == self.msg_id) || (!self.cli_msg_id.is_empty() && m.id == self.cli_msg_id))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PinnedList {
    pub ready: bool,
    pub can_pin: bool,
    pub items: Vec<PinnedItem>,
}

impl PinnedList {
    pub fn from_json(v: &Value) -> Self {
        let b = |k: &str| v.get_v(k).and_then(Value::as_bool).unwrap_or(false);
        PinnedList {
            ready: b("ready"),
            can_pin: b("canPin"),
            items: v
                .arr("items")
                .iter()
                .filter_map(|row| {
                    let r = row.s("ref");
                    if r.is_empty() {
                        return None;
                    }
                    Some(PinnedItem {
                        r#ref: r,
                        kind: match row.s("kind").as_str() {
                            "message" => PinKind::Message,
                            "link" => PinKind::Link,
                            "poll" => PinKind::Poll,
                            "reminder" => PinKind::Reminder,
                            _ => PinKind::Note,
                        },
                        title: row.s("title"),
                        sender: row.s("sender"),
                        thumbnail: row.opt_s("thumb").filter(|t| !t.is_empty()),
                        msg_id: row.s("msgId"),
                        cli_msg_id: row.s("cliMsgId"),
                    })
                })
                .collect(),
        }
    }
}

impl ChatMessage {
    pub fn copyable_text(&self) -> Option<String> {
        match &self.kind {
            Kind::Text => (!self.text.is_empty()).then(|| self.text.clone()),
            Kind::Link { title, url, .. } => {
                if !self.text.is_empty() && !self.text.starts_with("[Link]") {
                    return Some(self.text.clone());
                }
                url.clone().or_else(|| (!title.is_empty()).then(|| title.clone()))
            }
            _ => None,
        }
    }
    pub fn caption(&self) -> Option<String> {
        match self.kind {
            Kind::Photo { .. } | Kind::Video { .. } => (!self.text.is_empty() && !self.text.starts_with('[')).then(|| self.text.clone()),
            _ => None,
        }
    }
}

impl ZaloEngine {
    async fn eval_actions(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        self.await_call("__zaloMsgActions", method, args, "{status: 'not-ready'}").await
    }

    pub(crate) async fn race<T>(&self, secs: u64, fut: impl std::future::Future<Output = T>) -> Option<T> {
        let timer = self.exec.timer(Duration::from_secs(secs));
        futures::pin_mut!(fut);
        match futures::future::select(fut, timer).await {
            futures::future::Either::Left((v, _)) => Some(v),
            futures::future::Either::Right(_) => None,
        }
    }

    async fn call_actions(&self, method: &str, args: Vec<Value>, secs: u64, failure: &str) -> MessageActionOutcome {
        let answer = self.race(secs, self.eval_actions(method, args)).await;
        let Some(answer) = answer else { return MessageActionOutcome::Pending(NO_ANSWER_TEXT.into()) };
        let outcome = MessageActionOutcome::from_json(answer.as_ref(), failure);
        olaz_core::log::write(format!("message {method} → {}", outcome.log_name()));
        outcome
    }

    fn busy_insert(&self, key: &str) -> bool {
        self.state.borrow_mut().message_actions_busy.insert(key.to_string())
    }
    fn busy_remove(&self, key: &str) {
        self.state.borrow_mut().message_actions_busy.remove(key);
    }

    async fn copy_allowed(&self, conv: &str) -> bool {
        let answer = self.race(5, self.eval_actions("isProtected", vec![json!(conv)])).await.flatten();
        if answer.and_then(|v| v.as_bool()) != Some(true) {
            return true;
        }
        self.show_toast(PROTECTED, Some(conv));
        false
    }

    pub fn copy_text(&self, text: &str, conv: &str) {
        let Some(me) = self.weak().upgrade() else { return };
        let (text, conv) = (text.to_string(), conv.to_string());
        spawn(&self.exec, async move {
            if !me.copy_allowed(&conv).await {
                return;
            }
            let ok = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok();
            me.show_toast(if ok { "Đã sao chép" } else { "Có lỗi xảy ra" }, Some(&conv));
        });
    }

    pub fn copy_photo(&self, m: &ChatMessage) {
        let Kind::Photo { url, .. } = &m.kind else { return };
        let Some(me) = self.weak().upgrade() else { return };
        let (m, url) = (m.clone(), url.clone());
        spawn(&self.exec, async move {
            let conv = m.conversation_id.clone();
            if !me.copy_allowed(&conv).await {
                return;
            }
            let mut data: Option<Vec<u8>> = None;
            if let MediaResolution::File(path) = me.resolve_media(&conv, &m.id, MediaKind::Photo, false).await {
                data = std::fs::read(path).ok();
            }
            if data.is_none() && !m.is_e2ee {
                if let (Some(u), Some(loader)) = (url.filter(|u| u.starts_with("https://")), me.media_loader()) {
                    data = loader.data(&u).await.ok().map(|b| b.to_vec());
                }
            }
            let image = data.and_then(|d| olaz_media::decode::decode_rgba(&d, 4096));
            let ok = image.is_some_and(|img| {
                let (w, h) = img.dimensions();
                arboard::Clipboard::new()
                    .and_then(|mut c| c.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: img.into_raw().into() }))
                    .is_ok()
            });
            me.show_toast(if ok { "Đã sao chép" } else { "Không sao chép được hình ảnh. Hãy thử lại." }, Some(&conv));
        });
    }

    pub fn request_forward(&self, m: &ChatMessage) {
        if m.menu.forward != Availability::Available {
            if m.menu.forward == Availability::Blocked {
                self.show_toast(PROTECTED, Some(&m.conversation_id));
            }
            return;
        }
        self.state.borrow_mut().forward_request = Some(ForwardRequest { conversation_id: m.conversation_id.clone(), message: m.clone() });
        self.changed(Change::Notice);
    }

    pub fn dismiss_forward(&self) {
        self.state.borrow_mut().forward_request = None;
        self.changed(Change::Notice);
    }

    pub async fn forward(&self, m: &ChatMessage, targets: Vec<String>) -> MessageActionOutcome {
        self.call_actions("forward", vec![json!(m.conversation_id), json!(m.id), json!(targets)], 75, "Có lỗi khi chia sẻ tin nhắn").await
    }

    fn run_action(&self, m: &ChatMessage, name: &str, args: Vec<Value>, method: &str, failure: &'static str) {
        let conv = m.conversation_id.clone();
        let key = format!("{conv}|{}|{name}", m.id);
        if !self.busy_insert(&key) {
            return;
        }
        let Some(me) = self.weak().upgrade() else { return };
        let method = method.to_string();
        spawn(&self.exec, async move {
            let outcome = me.call_actions(&method, args, 45, failure).await;
            me.busy_remove(&key);
            match outcome {
                MessageActionOutcome::Done(Some(t)) | MessageActionOutcome::Failed(t) | MessageActionOutcome::Pending(t) => me.show_toast(&t, Some(&conv)),
                _ => {}
            }
        });
    }

    pub fn recall(&self, m: &ChatMessage) {
        self.run_action(m, "recall", vec![json!(m.conversation_id), json!(m.id)], "recall", "Thu hồi tin nhắn không thành công");
    }

    pub fn set_starred(&self, m: &ChatMessage, on: bool) {
        if on && m.menu.star == Some(MenuStar::Blocked) {
            self.show_toast(PROTECTED, Some(&m.conversation_id));
            return;
        }
        let name = if on { "star" } else { "unstar" };
        self.run_action(m, name, vec![json!(m.conversation_id), json!(m.id), json!(on)], "star", "Có lỗi xảy ra");
    }

    pub fn delete_for_me(&self, m: &ChatMessage, confirmed: bool) {
        let conv = m.conversation_id.clone();
        let key = format!("{conv}|{}|delete", m.id);
        if !self.busy_insert(&key) {
            return;
        }
        let Some(me) = self.weak().upgrade() else { return };
        let m = m.clone();
        spawn(&self.exec, async move {
            let answer = me.race(45, me.eval_actions("deleteForMe", vec![json!(conv), json!(m.id), json!(confirmed)])).await;
            me.busy_remove(&key);
            let Some(answer) = answer else {
                me.show_toast(NO_ANSWER_TEXT, Some(&conv));
                return;
            };
            let outcome = MessageActionOutcome::from_json(answer.as_ref(), "Không thể xóa tin nhắn");
            olaz_core::log::write(format!("message deleteForMe → {}", outcome.log_name()));
            match outcome {
                MessageActionOutcome::Done(_) => {
                    let Some(undo) = answer.as_ref().and_then(|a| a.get_v("undo")).filter(|u| u.is_object()) else { return };
                    let Some(id) = undo.opt_s("id") else { return };
                    let ms = undo.n("ms");
                    me.show_delete_undo(DeleteUndo {
                        id,
                        conversation_id: conv,
                        count: (undo.opt_n("count").unwrap_or(1.0) as i64).max(1),
                        deadline_ms: crate::chat::now_ms() + (ms / 1000.0).max(1.0) * 1000.0,
                    });
                }
                MessageActionOutcome::Confirm { title, message, ok, cancel, .. } => {
                    me.ask(MessageActionConfirm { id: 0, conversation_id: conv, action: ConfirmAction::DeleteForMe(Box::new(m)), title, message, ok, cancel, destructive: true });
                }
                MessageActionOutcome::Failed(t) | MessageActionOutcome::Pending(t) => me.show_toast(&t, Some(&conv)),
            }
        });
    }

    fn show_delete_undo(&self, undo: DeleteUndo) {
        let previous = self.state.borrow_mut().delete_undo.replace(undo.clone());
        if let Some(p) = previous {
            self.call_chat("dropDelete", vec![json!(p.id)], "__zaloMsgActions");
        }
        self.changed(Change::Notice);
        let weak = self.weak();
        let exec = self.exec.clone();
        let wait = ((undo.deadline_ms - crate::chat::now_ms()) / 1000.0).max(0.5);
        spawn(&self.exec, async move {
            exec.timer(Duration::from_secs_f64(wait)).await;
            let Some(me) = weak.upgrade() else { return };
            let same = me.state.borrow().delete_undo.as_ref() == Some(&undo);
            if same {
                me.state.borrow_mut().delete_undo = None;
                me.changed(Change::Notice);
                me.call_chat("dropDelete", vec![json!(undo.id)], "__zaloMsgActions");
            }
        });
    }

    pub fn undo_delete(&self) {
        let Some(undo) = self.state.borrow_mut().delete_undo.take() else { return };
        self.changed(Change::Notice);
        let Some(me) = self.weak().upgrade() else { return };
        spawn(&self.exec, async move {
            match me.call_actions("undoDelete", vec![json!(undo.id)], 30, "Không thể khôi phục").await {
                MessageActionOutcome::Done(_) => me.show_toast("Khôi phục thành công", Some(&undo.conversation_id)),
                MessageActionOutcome::Failed(t) | MessageActionOutcome::Pending(t) => me.show_toast(&t, Some(&undo.conversation_id)),
                _ => {}
            }
        });
    }

    pub fn dismiss_delete_undo(&self) {
        let Some(undo) = self.state.borrow_mut().delete_undo.take() else { return };
        self.changed(Change::Notice);
        self.call_chat("commitDelete", vec![json!(undo.id)], "__zaloMsgActions");
    }

    pub fn pin_message(&self, m: &ChatMessage, replacing: Option<String>) {
        let conv = m.conversation_id.clone();
        let key = format!("{conv}|{}|pin", m.id);
        if !self.busy_insert(&key) {
            return;
        }
        let Some(me) = self.weak().upgrade() else { return };
        let m = m.clone();
        spawn(&self.exec, async move {
            let outcome = me
                .call_actions("pin", vec![json!(conv), json!(m.id), replacing.map(Value::String).unwrap_or(Value::Null)], 60, "Ghim tin nhắn không thành công.")
                .await;
            me.busy_remove(&key);
            match outcome {
                MessageActionOutcome::Done(_) => {}
                MessageActionOutcome::Confirm { title, message, ok, cancel, r#ref } => match r#ref {
                    Some(r) => me.ask(MessageActionConfirm { id: 0, conversation_id: conv, action: ConfirmAction::Pin(Box::new(m), r), title, message, ok, cancel, destructive: false }),
                    None => me.show_toast("Ghim tin nhắn không thành công.", Some(&conv)),
                },
                MessageActionOutcome::Failed(t) | MessageActionOutcome::Pending(t) => me.show_toast(&t, Some(&conv)),
            }
        });
    }

    pub fn request_unpin_item(&self, item: &PinnedItem, conv: &str) {
        let is_msg = item.kind == PinKind::Message;
        self.ask(MessageActionConfirm {
            id: 0,
            conversation_id: conv.to_string(),
            action: ConfirmAction::Unpin(item.r#ref.clone()),
            title: if is_msg { "Bỏ ghim tin nhắn".into() } else { "Bỏ ghim".into() },
            message: if is_msg { "Bạn muốn bỏ ghim tin nhắn?".into() } else { "Bạn muốn bỏ ghim nội dung này?".into() },
            ok: "Bỏ ghim".into(),
            cancel: "Hủy".into(),
            destructive: true,
        });
    }

    pub fn request_unpin_message(&self, m: &ChatMessage) {
        let item = self.state.borrow().pins.get(&m.conversation_id).and_then(|l| l.items.iter().find(|i| i.message_in(std::slice::from_ref(m)).is_some()).cloned());
        if let Some(item) = item {
            self.request_unpin_item(&item, &m.conversation_id);
        }
    }

    fn ask(&self, mut confirm: MessageActionConfirm) {
        let open = self.state.borrow().open_conversation_id.clone();
        if open.as_deref() != Some(confirm.conversation_id.as_str()) || self.state.borrow().message_confirm.is_some() {
            olaz_core::log::write("message confirm dropped (conversation left or another confirm open)");
            return;
        }
        let seq = self.state.borrow().confirm_seq + 1;
        self.state.borrow_mut().confirm_seq = seq;
        confirm.id = seq;
        self.state.borrow_mut().message_confirm = Some(confirm);
        self.changed(Change::Notice);
    }

    fn unpin(&self, r: String, conv: String) {
        let key = format!("{conv}|{r}|unpin");
        if !self.busy_insert(&key) {
            return;
        }
        let Some(me) = self.weak().upgrade() else { return };
        spawn(&self.exec, async move {
            let outcome = me.call_actions("unpin", vec![json!(conv), json!(r)], 40, "Yêu cầu thực hiện không thành công").await;
            me.busy_remove(&key);
            if let MessageActionOutcome::Failed(t) | MessageActionOutcome::Pending(t) = outcome {
                me.show_toast(&t, Some(&conv));
            }
        });
    }

    pub fn answer_message_confirm(&self, id: u64, ok: bool) {
        let confirm = {
            let mut s = self.state.borrow_mut();
            if s.message_confirm.as_ref().map(|c| c.id) != Some(id) {
                return;
            }
            s.message_confirm.take().unwrap()
        };
        self.changed(Change::Notice);
        if !ok {
            return;
        }
        match confirm.action {
            ConfirmAction::DeleteForMe(m) => self.delete_for_me(&m, true),
            ConfirmAction::Pin(m, r) => self.pin_message(&m, Some(r)),
            ConfirmAction::Unpin(r) => self.unpin(r, confirm.conversation_id),
        }
    }

    pub(crate) fn handle_pins(&self, state: &Map<String, Value>) {
        let Some(json) = state.get("pins").filter(|p| p.is_object()) else { return };
        let conv = json.s("convId");
        if conv.is_empty() {
            return;
        }
        let list = PinnedList::from_json(json);
        if self.state.borrow().pins.get(&conv) != Some(&list) {
            self.state.borrow_mut().pins.insert(conv.clone(), list);
            self.changed(Change::Messages(conv));
        }
    }

    pub fn rewatch_pins(&self) {
        let id = self.state.borrow().open_conversation_id.clone();
        if let Some(id) = id {
            self.call_chat("watchPins", vec![json!(id)], "__zaloMsgActions");
        }
    }

    pub fn drop_message_prompts(&self) {
        let mut s = self.state.borrow_mut();
        s.forward_request = None;
        s.message_confirm = None;
    }

    pub fn reset_message_actions(&self) {
        let mut s = self.state.borrow_mut();
        s.forward_request = None;
        s.message_confirm = None;
        s.delete_undo = None;
        s.pins.clear();
        s.message_actions_busy.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        let o = |v: Value| MessageActionOutcome::from_json(Some(&v), "F");
        assert_eq!(MessageActionOutcome::from_json(None, "F"), MessageActionOutcome::Failed("F".into()));
        assert_eq!(o(json!({"status": "done", "text": ""})), MessageActionOutcome::Done(None));
        assert_eq!(
            o(json!({"status": "confirm", "title": "T", "ref": ""})),
            MessageActionOutcome::Confirm { title: "T".into(), message: "".into(), ok: "OK".into(), cancel: "Hủy".into(), r#ref: None }
        );
        assert_eq!(o(json!({"status": "not-ready"})), MessageActionOutcome::Failed("Zalo chưa sẵn sàng, hãy thử lại sau giây lát.".into()));
        assert_eq!(o(json!({"status": "weird"})), MessageActionOutcome::Failed("F".into()));
    }

    #[test]
    fn pins_and_copyable() {
        let list = PinnedList::from_json(&json!({"ready": true, "items": [{"ref": ""}, {"ref": "m:1", "kind": "message", "msgId": "9"}, {"ref": "x", "kind": "??"}]}));
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[1].kind, PinKind::Note);
        assert_eq!(list.items[1].label(), "Ghi chú");
        let msgs: Vec<ChatMessage> = ["8", "9", "9"]
            .iter()
            .enumerate()
            .map(|(i, id)| ChatMessage::from_json(&json!({"id": id, "text": format!("t{i}")}), "c").unwrap())
            .collect();
        assert_eq!(list.items[0].message_in(&msgs).unwrap().text, "t2", "the last match");
        let link = ChatMessage::from_json(&json!({"id": "1", "text": "[Link] x", "kind": {"type": "link", "url": "https://zalo.me", "title": "Z"}}), "c").unwrap();
        assert_eq!(link.copyable_text().as_deref(), Some("https://zalo.me"));
        let photo = ChatMessage::from_json(&json!({"id": "2", "text": "[Hình ảnh]", "kind": {"type": "photo"}}), "c").unwrap();
        assert_eq!(photo.caption(), None);
        assert_eq!(photo.copyable_text(), None);
    }
}
