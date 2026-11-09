//! Chat: open/close, history, send with mention rebasing, reactions, typing. Port of `ZaloEngine+Chat.swift`.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use olaz_core::exec::spawn;
use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::{ChatMessage, J, Mention, MentionCandidate, ReactionDetail};

/// Outcome of a merge (for logging and tests).
#[derive(Debug, PartialEq)]
pub struct MergeResult {
    pub list: Vec<ChatMessage>,
    pub incoming: usize,
    pub removed: usize,
}

/// `reset` replaces the list; otherwise changed messages merge by id and `removed` ids drop out. Oldest first,
/// stable on equal timestamps.
pub fn merge_chat(current: &[ChatMessage], chat: &Value, conv_id: &str) -> MergeResult {
    let incoming: Vec<ChatMessage> = chat.arr("messages").iter().filter_map(|m| ChatMessage::from_json(m, conv_id)).collect();
    let removed: HashSet<String> = chat.arr("removed").iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let reset = chat.get_v("reset").and_then(Value::as_bool).unwrap_or(false);
    let mut list: Vec<ChatMessage> = if reset { Vec::new() } else { current.to_vec() };
    if !removed.is_empty() {
        list.retain(|m| !removed.contains(&m.id));
    }
    let mut index: HashMap<String, usize> = list.iter().enumerate().map(|(i, m)| (m.id.clone(), i)).collect();
    let mut needs_sort = reset;
    for message in &incoming {
        if let Some(&at) = index.get(&message.id) {
            if list[at].date != message.date {
                needs_sort = true;
            }
            list[at] = message.clone();
        } else {
            if list.last().is_some_and(|last| message.date < last.date) {
                needs_sort = true;
            }
            index.insert(message.id.clone(), list.len());
            list.push(message.clone());
        }
    }
    if needs_sort {
        let mut indexed: Vec<(usize, ChatMessage)> = list.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| a.1.date.partial_cmp(&b.1.date).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
        list = indexed.into_iter().map(|(_, m)| m).collect();
    }
    MergeResult { list, incoming: incoming.len(), removed: removed.len() }
}

fn is_ws16(unit: u16) -> bool {
    char::from_u32(unit as u32).is_some_and(|c| c.is_whitespace())
}

/// Mention rebasing for `sendText`: returns the trimmed text and the wire mentions, or None if empty.
pub fn prepare_send(text: &str, mentions: &[Mention]) -> Option<(String, Vec<Value>)> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let leading = text.encode_utf16().take_while(|u| is_ws16(*u)).count() as i64;
    let body: Vec<u16> = trimmed.encode_utf16().collect();
    let mut wire: Vec<(i64, Value)> = mentions
        .iter()
        .filter_map(|m| {
            let pos = m.pos - leading;
            if pos < 0 || m.len <= 1 || pos + m.len > body.len() as i64 || body[pos as usize] != 0x40 {
                return None;
            }
            Some((pos, json!({"pos": pos, "len": m.len, "uid": m.uid, "type": if m.is_all { 1 } else { 0 }})))
        })
        .collect();
    wire.sort_by_key(|(p, _)| *p);
    Some((trimmed, wire.into_iter().map(|(_, v)| v).collect()))
}

impl ZaloEngine {
    pub fn open_conversation(&self, id: &str) {
        let switching = self.state.borrow().open_conversation_id.as_deref() != Some(id);
        if switching {
            crate::modules::drop_message_prompts(self);
        }
        self.state.borrow_mut().open_conversation_id = Some(id.to_string());
        self.notifier.clear_delivered(id);
        self.call_chat("open", vec![json!(id)], "__zaloChat");
        self.call_chat("watchPins", vec![json!(id)], "__zaloMsgActions");
        self.changed(Change::Other);
    }

    pub fn close_conversation(&self) {
        let Some(id) = self.state.borrow_mut().open_conversation_id.take() else { return };
        crate::modules::drop_message_prompts(self);
        self.call_chat("close", vec![json!(id)], "__zaloChat");
        self.call_chat("watchPins", vec![Value::Null], "__zaloMsgActions");
        self.changed(Change::Other);
    }

    pub fn load_older_messages(&self, id: &str) {
        self.call_chat("loadMore", vec![json!(id)], "__zaloChat");
    }

    pub fn mark_read(&self, id: &str) {
        self.call_chat("markRead", vec![json!(id)], "__zaloChat");
    }

    pub fn react(&self, emoji: &str, message_id: &str, conversation: &str) {
        self.call_chat("react", vec![json!(conversation), json!(message_id), json!(emoji)], "__zaloChat");
    }

    /// Returns false when the official client could not take the message (nothing was sent).
    pub async fn send_text(&self, text: &str, to: &str, quoting: Option<&str>, mentions: &[Mention]) -> bool {
        let Some((trimmed, wire)) = prepare_send(text, mentions) else { return true };
        let args = Value::Array(vec![json!(to), json!(trimmed), quoting.map(|q| json!(q)).unwrap_or(Value::Null), Value::Array(wire)]);
        let script = format!("const c = window.__zaloChat; if (!c || !c.sendText) return 'missing'; return await c.sendText(...{args});");
        let result = self.evaluate(&script).await;
        let sent = result.as_ref().and_then(Value::as_str) == Some("sent");
        if !sent {
            olaz_core::log::write(format!("chat sendText → {}", result.map(|v| v.to_string()).unwrap_or_else(|| "nil".into())));
        }
        sent
    }

    pub async fn mention_candidates(&self, id: &str, query: &str, excluding: &[String]) -> Vec<MentionCandidate> {
        let r = self.await_call("__zaloChat", "mentionCandidates", vec![json!(id), json!(query), json!(excluding)], "[]").await;
        r.map(|v| MentionCandidate::list(&v)).unwrap_or_default()
    }

    pub async fn reactors(&self, message_id: &str, conversation: &str) -> Option<ReactionDetail> {
        let r = self.await_call("__zaloChat", "reactors", vec![json!(conversation), json!(message_id)], "null").await?;
        ReactionDetail::from_json(&r)
    }

    /// PNG bytes of an official reaction icon (`pc-dist/<path>` in the asar).
    pub fn reaction_image(&self, path: &str) -> Option<Vec<u8>> {
        if path.is_empty() {
            return None;
        }
        let app = self.runtime_layout().current();
        thread_local! {
            static CACHE: std::cell::RefCell<HashMap<String, Option<Vec<u8>>>> = std::cell::RefCell::new(HashMap::new());
            static ASAR: std::cell::RefCell<Option<olaz_core::asar::AsarArchive>> = const { std::cell::RefCell::new(None) };
        }
        if let Some(hit) = CACHE.with(|c| c.borrow().get(path).cloned()) {
            return hit;
        }
        let data = ASAR.with(|a| {
            let mut a = a.borrow_mut();
            if a.is_none() {
                *a = olaz_core::asar::AsarArchive::open(&app.join("Contents/Resources/app.asar")).ok();
            }
            a.as_ref().and_then(|asar| asar.read(&format!("pc-dist/{path}")).map(|c| c.into_owned()))
        });
        CACHE.with(|c| c.borrow_mut().insert(path.to_string(), data.clone()));
        data
    }

    pub(crate) fn handle_chat(&self, state: &Map<String, Value>) {
        if let Some(chat) = state.get("chat").filter(|c| c.is_object()) {
            if let Some(conv) = chat.get_v("convId").and_then(Value::as_str) {
                self.merge_chat_snapshot(chat, conv);
            }
        }
        if let Some(t) = state.get("typing").filter(|t| t.is_object()) {
            if let Some(conv) = t.get_v("convId").and_then(Value::as_str) {
                self.show_typing(&t.s("name"), conv);
            }
        }
    }

    fn merge_chat_snapshot(&self, chat: &Value, conv: &str) {
        let current = self.state.borrow().messages.get(conv).cloned().unwrap_or_default();
        let result = merge_chat(&current, chat, conv);
        let reset = chat.get_v("reset").and_then(Value::as_bool).unwrap_or(false);
        olaz_core::log::write(format!(
            "chat merge conv={}… reset={reset} in={} removed={} total={} hasMore={}",
            conv.chars().take(4).collect::<String>(),
            result.incoming,
            result.removed,
            result.list.len(),
            chat.get_v("hasMore").map(|v| v.to_string()).unwrap_or_else(|| "-".into())
        ));
        let newest_incoming =
            chat.arr("messages").iter().filter_map(|m| ChatMessage::from_json(m, conv)).last();
        let mut changed = false;
        if current != result.list {
            self.state.borrow_mut().messages.insert(conv.to_string(), result.list);
            changed = true;
        }
        crate::modules::note_poll_messages(self, chat);
        if let Some(has_more) = chat.get_v("hasMore").and_then(Value::as_bool) {
            if self.state.borrow().has_more_history.get(conv) != Some(&has_more) {
                self.state.borrow_mut().has_more_history.insert(conv.to_string(), has_more);
                changed = true;
            }
        }
        if chat.get_v("grew").and_then(Value::as_bool) == Some(true) {
            *self.state.borrow_mut().history_ticks.entry(conv.to_string()).or_default() += 1;
            changed = true;
        }
        if changed {
            self.changed(Change::Messages(conv.to_string()));
        }
        let typing = self.state.borrow().typing.contains_key(conv);
        if typing {
            if let Some(newest) = newest_incoming {
                if !newest.is_mine && newest.date > now_ms() - 5000.0 {
                    self.clear_typing(conv);
                }
            }
        }
    }

    fn show_typing(&self, name: &str, conv: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            self.clear_typing(conv);
            return;
        }
        self.state.borrow_mut().typing.insert(conv.to_string(), trimmed.to_string());
        let generation = {
            let mut gens = self.typing_gen.borrow_mut();
            let g = gens.entry(conv.to_string()).or_default();
            *g += 1;
            *g
        };
        self.changed(Change::Typing(conv.to_string()));
        let weak = self.weak();
        let exec = self.exec.clone();
        let conv = conv.to_string();
        spawn(&self.exec, async move {
            exec.timer(Duration::from_secs(5)).await;
            let Some(me) = weak.upgrade() else { return };
            if me.typing_gen.borrow().get(&conv) == Some(&generation) {
                me.clear_typing(&conv);
            }
        });
    }

    fn clear_typing(&self, conv: &str) {
        *self.typing_gen.borrow_mut().entry(conv.to_string()).or_default() += 1;
        if self.state.borrow_mut().typing.remove(conv).is_some() {
            self.changed(Change::Typing(conv.to_string()));
        }
    }
}

pub fn now_ms() -> f64 {
    olaz_core::clock::now_ms() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, date: f64) -> Value {
        json!({"id": id, "date": date, "text": id})
    }

    fn ids(list: &[ChatMessage]) -> Vec<&str> {
        list.iter().map(|m| m.id.as_str()).collect()
    }

    #[test]
    fn merge_reset_replace_insert_remove() {
        let r = merge_chat(&[], &json!({"messages": [msg("a", 1.0), msg("b", 2.0)], "reset": true}), "c");
        assert_eq!(ids(&r.list), vec!["a", "b"]);
        // in-place replace keeps position
        let r2 = merge_chat(&r.list, &json!({"messages": [json!({"id": "a", "date": 1.0, "text": "edited"})]}), "c");
        assert_eq!(ids(&r2.list), vec!["a", "b"]);
        assert_eq!(r2.list[0].text, "edited");
        // out-of-order insert → sorted
        let r3 = merge_chat(&r2.list, &json!({"messages": [msg("z", 0.5)]}), "c");
        assert_eq!(ids(&r3.list), vec!["z", "a", "b"]);
        // date change of an existing message → resorted
        let r4 = merge_chat(&r3.list, &json!({"messages": [msg("z", 3.0)]}), "c");
        assert_eq!(ids(&r4.list), vec!["a", "b", "z"]);
        // removed
        let r5 = merge_chat(&r4.list, &json!({"removed": ["a"], "messages": []}), "c");
        assert_eq!(ids(&r5.list), vec!["b", "z"]);
        // reset drops everything not in the snapshot
        let r6 = merge_chat(&r5.list, &json!({"messages": [msg("q", 9.0)], "reset": true}), "c");
        assert_eq!(ids(&r6.list), vec!["q"]);
    }

    #[test]
    fn merge_is_stable_on_equal_timestamps() {
        let r = merge_chat(&[], &json!({"messages": [msg("x", 5.0), msg("y", 5.0), msg("w", 4.0)], "reset": true}), "c");
        assert_eq!(ids(&r.list), vec!["w", "x", "y"]);
        let r2 = merge_chat(&r.list, &json!({"messages": [msg("v", 5.0), msg("u", 1.0)]}), "c");
        assert_eq!(ids(&r2.list), vec!["u", "w", "x", "y", "v"]);
        // unparsable messages are dropped
        let r3 = merge_chat(&r2.list, &json!({"messages": [{"id": ""}, 5]}), "c");
        assert_eq!(r3.list.len(), 5);
    }

    #[test]
    fn mention_rebasing() {
        let m = |pos, len| Mention { uid: "u".into(), pos, len, is_all: false };
        // leading spaces/newline shift the offsets
        let (t, wire) = prepare_send("  \n@Lan hi", &[m(3, 4)]).unwrap();
        assert_eq!(t, "@Lan hi");
        assert_eq!(wire, vec![json!({"pos": 0, "len": 4, "uid": "u", "type": 0})]);
        // emoji before the mention: UTF-16 offsets (😀 = 2 units)
        let (_, wire) = prepare_send("😀 @An", &[m(3, 3)]).unwrap();
        assert_eq!(wire[0]["pos"], 3);
        // invalid: not pointing at '@', too short, out of range
        let (_, wire) = prepare_send("hello @An", &[m(0, 3), m(6, 1), m(6, 30)]).unwrap();
        assert!(wire.is_empty());
        // sorted by pos, isAll → type 1
        let (_, wire) = prepare_send("@A @B", &[m(3, 2), Mention { uid: "-1".into(), pos: 0, len: 2, is_all: true }]).unwrap();
        assert_eq!(wire[0]["type"], 1);
        assert_eq!(wire[1]["pos"], 3);
        assert!(prepare_send(" \n\t ", &[]).is_none());
    }
}
