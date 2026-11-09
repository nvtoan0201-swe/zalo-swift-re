//! Conversation labels, filters, stranger folder, My Documents id. Port of `ZaloEngine+Conversations.swift`.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::J;

/// Official `FilterType` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationTypeFilter {
    All = 0,
    Unread = 1,
    Focused = 3,
    Other = 4,
}

pub const INBOX_TITLE: &str = "Tin nhắn";
pub const STRANGER_BOX_ID: &str = "stranger_box";

impl ConversationTypeFilter {
    pub fn title(self) -> &'static str {
        match self {
            ConversationTypeFilter::All | ConversationTypeFilter::Focused => INBOX_TITLE,
            ConversationTypeFilter::Unread => "Chưa đọc",
            ConversationTypeFilter::Other => "Khác",
        }
    }
    pub fn from_raw(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::All),
            1 => Some(Self::Unread),
            3 => Some(Self::Focused),
            4 => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub id: String,
    pub name: String,
    pub color_hex: String,
    pub conv_ids: Vec<String>,
}

pub const ZALO_BLUE: u32 = 0x0068FF;

/// "#RRGGBB" / "RRGGBB" → 0xRRGGBB.
pub fn parse_hex_color(hex: &str) -> Option<u32> {
    let t = hex.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if t.len() != 6 {
        return None;
    }
    u32::from_str_radix(t, 16).ok()
}

/// Label order after dragging the label at `from` onto the row at `to` (the dragged label takes that row's place).
pub fn reordered_labels(ids: &[String], from: usize, to: usize) -> Vec<String> {
    let mut ids = ids.to_vec();
    if from < ids.len() && to < ids.len() && from != to {
        let id = ids.remove(from);
        ids.insert(to, id);
    }
    ids
}

/// A label name capped at `max` characters (the label editor field).
pub fn cap_label_name(name: &str, max: i64) -> String {
    name.chars().take(max.max(0) as usize).collect()
}

impl Label {
    pub fn color(&self) -> u32 {
        parse_hex_color(&self.color_hex).unwrap_or(ZALO_BLUE)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub max_labels: i64,
    pub max_name_length: i64,
    pub palette: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_labels: 15,
            max_name_length: 30,
            palette: ["#D91B1B", "#F31BC8", "#FF6905", "#FAC000", "#4BC377", "#2AC5BB", "#0068FF", "#6F3FCF"].iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StrangerBox {
    pub chats: i64,
    pub unread_chats: i64,
    pub unread_messages: i64,
    pub newest_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub seq: i64,
    pub ok: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConversationExtras {
    pub loaded: bool,
    pub labels: Vec<Label>,
    pub limits: Limits,
    pub archived_enabled: bool,
    pub type_filter: i64,
    pub label_filter_ids: Vec<String>,
    pub list_ids: Vec<String>,
    pub stranger_ids: HashSet<String>,
    pub stranger_folder_ids: Vec<String>,
    pub stranger_box: StrangerBox,
    pub unbox_stranger: bool,
    pub notice: Option<Notice>,
    pub label_by_conversation: HashMap<String, String>,
}

impl ConversationExtras {
    pub fn label_of(&self, conversation: &str) -> Option<&Label> {
        let id = self.label_by_conversation.get(conversation)?;
        self.labels.iter().find(|l| &l.id == id)
    }
    pub fn tabs(&self) -> Vec<ConversationTypeFilter> {
        use ConversationTypeFilter::*;
        if self.archived_enabled { vec![Focused, Other, Unread] } else { vec![All, Unread] }
    }
    pub fn default_tab(&self) -> ConversationTypeFilter {
        if self.archived_enabled { ConversationTypeFilter::Focused } else { ConversationTypeFilter::All }
    }

    /// Snapshot merge: only keys present are applied.
    pub fn merged(&self, json: &Value) -> ConversationExtras {
        let mut next = self.clone();
        next.loaded = true;
        if let Some(rows) = json.get_v("labels").and_then(Value::as_array) {
            next.labels = rows
                .iter()
                .filter_map(|row| {
                    let id = row.get_v("id").and_then(Value::as_str)?.to_string();
                    Some(Label {
                        id,
                        name: row.s("name"),
                        color_hex: row.s("color"),
                        conv_ids: row.arr("convIds").iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                    })
                })
                .collect();
            let mut map = HashMap::new();
            for label in &next.labels {
                for conv in &label.conv_ids {
                    map.insert(conv.clone(), label.id.clone());
                }
            }
            next.label_by_conversation = map;
        }
        if let Some(limits) = json.get_v("limits").filter(|l| l.is_object()) {
            if let Some(n) = limits.opt_n("maxLabels").map(|f| f as i64).filter(|n| *n > 0) {
                next.limits.max_labels = n;
            }
            if let Some(n) = limits.opt_n("maxNameLength").map(|f| f as i64).filter(|n| *n > 0) {
                next.limits.max_name_length = n;
            }
            if let Some(p) = limits.get_v("palette").and_then(Value::as_array) {
                let palette: Vec<String> = p.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                if palette.len() == p.len() && !palette.is_empty() {
                    next.limits.palette = palette;
                }
            }
        }
        if let Some(flag) = json.get_v("archivedEnabled").and_then(Value::as_bool) {
            next.archived_enabled = flag;
        }
        if let Some(filter) = json.get_v("filter").filter(|f| f.is_object()) {
            if let Some(t) = filter.opt_n("type") {
                next.type_filter = t as i64;
            }
            next.label_filter_ids = filter.arr("labelIds").iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        let strings = |key: &str| -> Option<Vec<String>> {
            let a = json.get_v(key)?.as_array()?;
            let v: Vec<String> = a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
            (v.len() == a.len()).then_some(v)
        };
        if let Some(ids) = strings("listIds") {
            next.list_ids = ids;
        }
        if let Some(ids) = strings("strangerIds") {
            next.stranger_ids = ids.into_iter().collect();
        }
        if let Some(ids) = strings("strangerFolderIds") {
            next.stranger_folder_ids = ids;
        }
        if let Some(b) = json.get_v("strangerBox").filter(|b| b.is_object()) {
            next.stranger_box = StrangerBox {
                chats: b.n("chats") as i64,
                unread_chats: b.n("unreadChats") as i64,
                unread_messages: b.n("unreadMessages") as i64,
                newest_id: b.opt_s("newestId").filter(|s| !s.is_empty()),
            };
        }
        if let Some(flag) = json.get_v("unboxStranger").and_then(Value::as_bool) {
            next.unbox_stranger = flag;
        }
        if let Some(action) = json.get_v("action").filter(|a| a.is_object()) {
            if let Some(seq) = action.opt_n("seq") {
                next.notice = Some(Notice { seq: seq as i64, ok: action.get_v("ok").and_then(Value::as_bool).unwrap_or(false), text: action.s("text") });
            }
        }
        next
    }
}

impl ZaloEngine {
    pub fn set_conversation_filter(&self, kind: ConversationTypeFilter, label_ids: Vec<String>) {
        {
            let mut s = self.state.borrow_mut();
            s.conversation_extras.type_filter = kind as i64;
            s.conversation_extras.label_filter_ids = label_ids.clone();
        }
        self.changed(Change::Extras);
        self.call_chat("setFilter", vec![json!(kind as i64), json!(label_ids)], "__zaloConversations");
    }

    /// Optimistic: the next snapshot confirms.
    pub fn set_conversation_label(&self, conversation: &str, label_id: Option<&str>) {
        let changed = {
            let mut s = self.state.borrow_mut();
            let map = &mut s.conversation_extras.label_by_conversation;
            let previous = map.get(conversation).cloned();
            match label_id {
                Some(l) => {
                    map.insert(conversation.to_string(), l.to_string());
                }
                None => {
                    map.remove(conversation);
                }
            }
            previous.as_deref() != label_id
        };
        if changed {
            self.changed(Change::Extras);
        }
        self.call_chat("setConvLabel", vec![json!(conversation), label_id.map(|l| json!(l)).unwrap_or(Value::Null)], "__zaloConversations");
    }

    pub fn create_conversation_label(&self, name: &str, color_hex: &str, conversation_ids: Vec<String>) {
        self.call_chat("createLabel", vec![json!(name), json!(color_hex), json!(conversation_ids)], "__zaloConversations");
    }
    pub fn update_conversation_label(&self, id: &str, name: &str, color_hex: &str) {
        self.call_chat("updateLabel", vec![json!(id), json!(name), json!(color_hex)], "__zaloConversations");
    }
    pub fn delete_conversation_label(&self, id: &str) {
        self.call_chat("deleteLabel", vec![json!(id)], "__zaloConversations");
    }
    pub fn reorder_conversation_labels(&self, ids: Vec<String>) {
        self.call_chat("reorderLabels", vec![json!(ids)], "__zaloConversations");
    }

    pub(crate) fn handle_conversation_extras(&self, state: &Map<String, Value>) {
        let Some(json) = state.get("conversationExtras").filter(|j| j.is_object()) else { return };
        if let Some(id) = json.get_v("selfConversationId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            if self.state.borrow().self_conversation_id.as_deref() != Some(id) {
                self.state.borrow_mut().self_conversation_id = Some(id.to_string());
                self.changed(Change::Other);
            }
        }
        let next = self.state.borrow().conversation_extras.merged(json);
        if next != self.state.borrow().conversation_extras {
            self.state.borrow_mut().conversation_extras = next;
            self.changed(Change::Extras);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_reorder_and_name_cap() {
        let ids: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::reordered_labels(&ids, 0, 2), ["b", "c", "a", "d"]);
        assert_eq!(super::reordered_labels(&ids, 3, 0), ["d", "a", "b", "c"]);
        assert_eq!(super::reordered_labels(&ids, 1, 1), ids);
        assert_eq!(super::reordered_labels(&ids, 9, 1), ids);
        assert_eq!(super::cap_label_name("Gia đình thân yêu", 8), "Gia đình");
        assert_eq!(super::cap_label_name("abc", 30), "abc");
    }

    use super::*;

    #[test]
    fn partial_snapshots_keep_untouched_fields() {
        let a = ConversationExtras::default().merged(&json!({"listIds": ["1", "2"], "archivedEnabled": true}));
        assert!(a.loaded && a.archived_enabled);
        let b = a.merged(&json!({"strangerIds": ["9"]}));
        assert_eq!(b.list_ids, vec!["1", "2"]);
        assert!(b.archived_enabled);
        assert!(b.stranger_ids.contains("9"));
        let c = b.merged(&json!({"strangerBox": {"chats": 3, "newestId": ""}, "action": {"seq": 4, "ok": true, "text": "Đã tạo"}}));
        assert_eq!(c.stranger_box, StrangerBox { chats: 3, unread_chats: 0, unread_messages: 0, newest_id: None });
        assert_eq!(c.notice, Some(Notice { seq: 4, ok: true, text: "Đã tạo".into() }));
    }

    #[test]
    fn one_label_per_conversation_last_wins() {
        let e = ConversationExtras::default().merged(&json!({"labels": [
            {"id": "a", "name": "Công việc", "color": "#FF6905", "convIds": ["c1", "c2"]},
            {"id": "b", "name": "Gia đình", "color": "zz", "convIds": ["c2"]},
            {"name": "no id"}
        ]}));
        assert_eq!(e.labels.len(), 2);
        assert_eq!(e.label_of("c1").unwrap().id, "a");
        assert_eq!(e.label_of("c2").unwrap().id, "b");
        assert_eq!(e.labels[0].color(), 0xFF6905);
        assert_eq!(e.labels[1].color(), ZALO_BLUE);
    }

    #[test]
    fn tabs_and_titles() {
        let mut e = ConversationExtras::default();
        assert_eq!(e.tabs(), vec![ConversationTypeFilter::All, ConversationTypeFilter::Unread]);
        assert_eq!(e.default_tab(), ConversationTypeFilter::All);
        e.archived_enabled = true;
        assert_eq!(e.tabs(), vec![ConversationTypeFilter::Focused, ConversationTypeFilter::Other, ConversationTypeFilter::Unread]);
        assert_eq!(e.default_tab().title(), "Tin nhắn");
        assert_eq!(ConversationTypeFilter::Other.title(), "Khác");
        assert_eq!(parse_hex_color("0068ff"), Some(0x0068FF));
        assert_eq!(parse_hex_color("#12345"), None);
    }

    #[test]
    fn limits_override_only_when_valid() {
        let e = ConversationExtras::default().merged(&json!({"limits": {"maxLabels": 0, "maxNameLength": 20, "palette": []}}));
        assert_eq!(e.limits.max_labels, 15);
        assert_eq!(e.limits.max_name_length, 20);
        assert_eq!(e.limits.palette.len(), 8);
    }
}
