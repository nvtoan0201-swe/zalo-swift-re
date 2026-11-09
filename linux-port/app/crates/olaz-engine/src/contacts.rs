//! Friends, groups, friend requests, add friend. Port of `ZaloEngine+Contacts.swift`. Bridged to
//! `window.__zaloContacts` (posts `{page:'main', contacts:{…}}`; each key replaces that section). Every action runs through
//! the official client's own services.

use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::J;

#[derive(Debug, Clone, PartialEq)]
pub struct Friend {
    pub id: String,
    pub name: String,
    pub zalo_name: String,
    pub avatar: Option<String>,
    /// Last activity (ms since epoch), None when unknown.
    pub last_active: Option<f64>,
    pub online: bool,
    /// Upper-case Latin initial for the alphabetical sections ("#" when not a letter).
    pub section: String,
    /// Diacritic-folded lowercase name for sorting and searching.
    pub sort_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub members: i64,
    pub sort_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FriendRequest {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub message: String,
    /// ms since epoch.
    pub date: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundUser {
    pub id: String,
    pub name: String,
    pub zalo_name: String,
    pub avatar: Option<String>,
    pub bio: String,
    pub is_friend: bool,
    pub is_self: bool,
    /// I already sent this person a request.
    pub requested: bool,
    /// This person already sent me a request.
    pub requesting: bool,
    /// Official default greeting for "find by phone".
    pub default_message: String,
    pub max_message: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchStatus {
    #[default]
    Idle,
    Searching,
    Found,
    Error,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Search {
    pub status: SearchStatus,
    pub error: Option<String>,
    pub user: Option<FoundUser>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub seq: i64,
    pub ok: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactsState {
    pub friends: Vec<Friend>,
    pub groups: Vec<Group>,
    pub received: Vec<FriendRequest>,
    pub sent: Vec<FriendRequest>,
    pub pending_request_count: i64,
    pub loading_requests: bool,
    pub search: Search,
    pub notice: Option<Notice>,
    pub friends_loaded: bool,
    pub groups_loaded: bool,
}

/// Lower-cased, diacritic-folded text (Vietnamese đ/Đ included) for searching and sorting.
pub fn fold(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.replace('đ', "d")
        .replace('Đ', "D")
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

fn url(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

fn ms(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|t| *t > 0.0)
}

pub fn parse_friend(j: &Value) -> Option<Friend> {
    let id = j.s("id");
    if id.is_empty() {
        return None;
    }
    let name = j.opt_s("name").filter(|n| !n.is_empty()).unwrap_or_else(|| j.s("zaloName"));
    let key = fold(&name).trim().to_string();
    let first = key.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "#".into());
    let section = if first.chars().all(|c| c.is_ascii_uppercase()) { first } else { "#".into() };
    Some(Friend {
        id,
        name,
        zalo_name: j.s("zaloName"),
        avatar: url(j.get_v("avatar")),
        last_active: ms(j.get_v("lastActive")),
        online: j.get_v("online").and_then(Value::as_bool).unwrap_or(false),
        section,
        sort_key: key,
    })
}

pub fn parse_group(j: &Value) -> Option<Group> {
    let id = j.s("id");
    if id.is_empty() {
        return None;
    }
    let name = j.s("name");
    Some(Group { id, sort_key: fold(&name), name, avatar: url(j.get_v("avatar")), members: j.get_v("members").and_then(Value::as_i64).unwrap_or(0) })
}

pub fn parse_request(j: &Value) -> Option<FriendRequest> {
    let id = j.s("id");
    if id.is_empty() {
        return None;
    }
    Some(FriendRequest { id, name: j.s("name"), avatar: url(j.get_v("avatar")), message: j.s("message"), date: ms(j.get_v("time")) })
}

pub fn parse_search(j: &Value) -> Search {
    let status = match j.s("status").as_str() {
        "searching" => SearchStatus::Searching,
        "found" => SearchStatus::Found,
        "error" => SearchStatus::Error,
        _ => SearchStatus::Idle,
    };
    let b = |u: &Value, k: &str| u.get_v(k).and_then(Value::as_bool).unwrap_or(false);
    let user = j.get_v("user").filter(|u| !u.s("id").is_empty()).map(|u| FoundUser {
        id: u.s("id"),
        name: u.s("name"),
        zalo_name: u.s("zaloName"),
        avatar: url(u.get_v("avatar")),
        bio: u.s("bio"),
        is_friend: b(u, "isFriend"),
        is_self: b(u, "isSelf"),
        requested: b(u, "requested"),
        requesting: b(u, "requesting"),
        default_message: u.s("defaultMessage"),
        max_message: u.get_v("maxMessage").and_then(Value::as_i64).unwrap_or(150),
    });
    Search { status, error: j.opt_s("error"), user }
}

impl ContactsState {
    pub fn apply(&mut self, j: &Value) {
        if let Some(rows) = j.get_v("friends").and_then(Value::as_array) {
            self.friends = rows.iter().filter_map(parse_friend).collect();
            self.friends_loaded = true;
        }
        if let Some(rows) = j.get_v("groups").and_then(Value::as_array) {
            self.groups = rows.iter().filter_map(parse_group).collect();
            self.groups_loaded = true;
        }
        if let Some(rows) = j.get_v("received").and_then(Value::as_array) {
            self.received = rows.iter().filter_map(parse_request).collect();
        }
        if let Some(rows) = j.get_v("sent").and_then(Value::as_array) {
            self.sent = rows.iter().filter_map(parse_request).collect();
        }
        if let Some(n) = j.get_v("pendingRequestCount").and_then(Value::as_i64) {
            self.pending_request_count = n.max(0);
        }
        if let Some(flag) = j.get_v("loading").and_then(|l| l.get_v("requests")).and_then(Value::as_bool) {
            self.loading_requests = flag;
        }
        if let Some(s) = j.get_v("search").filter(|s| s.is_object()) {
            self.search = parse_search(s);
        }
        if let Some(n) = j.get_v("notice").filter(|n| n.is_object()) {
            if let Some(seq) = n.get_v("seq").and_then(Value::as_i64) {
                self.notice = Some(Notice { seq, ok: n.get_v("ok").and_then(Value::as_bool).unwrap_or(false), text: n.s("text") });
            }
        }
    }
}

impl ZaloEngine {
    /// Re-reads friend requests from the server (as opening the official invitation page does).
    pub fn refresh_contacts(&self) {
        self.call_chat("refresh", vec![], "__zaloContacts");
    }
    pub fn accept_friend_request(&self, id: &str) {
        self.call_chat("accept", vec![json!(id)], "__zaloContacts");
    }
    pub fn decline_friend_request(&self, id: &str) {
        self.call_chat("decline", vec![json!(id)], "__zaloContacts");
    }
    pub fn cancel_friend_request(&self, id: &str) {
        self.call_chat("cancel", vec![json!(id)], "__zaloContacts");
    }

    /// Looks a user up by phone number through the official find-user API.
    pub fn find_friend_by_phone(&self, phone: &str) {
        let trimmed = phone.trim();
        if trimmed.is_empty() {
            return;
        }
        self.state.borrow_mut().contacts.search = Search { status: SearchStatus::Searching, ..Default::default() };
        self.changed(Change::Contacts);
        self.call_chat("search", vec![json!(trimmed)], "__zaloContacts");
    }

    pub fn reset_friend_search(&self) {
        let changed = self.state.borrow().contacts.search != Search::default();
        if changed {
            self.state.borrow_mut().contacts.search = Search::default();
            self.changed(Change::Contacts);
        }
        self.call_chat("resetSearch", vec![], "__zaloContacts");
    }

    pub fn send_friend_request(&self, id: &str, message: &str) {
        self.call_chat("sendRequest", vec![json!(id), json!(message)], "__zaloContacts");
    }

    /// Opens the 1-1 or group conversation with `id`.
    pub fn open_contact_conversation(&self, id: &str) {
        self.state.borrow_mut().navigation_request = Some(id.to_string());
        self.changed(Change::Other);
    }

    /// Friends without the hidden chats; empty until the hidden state is known.
    pub fn visible_friends(&self) -> Vec<Friend> {
        let s = self.state.borrow();
        if !s.conversation_actions.hidden_known {
            return vec![];
        }
        s.contacts.friends.iter().filter(|f| !s.conversation_actions.is_hidden(&f.id)).cloned().collect()
    }

    pub fn visible_groups(&self) -> Vec<Group> {
        let s = self.state.borrow();
        if !s.conversation_actions.hidden_known {
            return vec![];
        }
        s.contacts.groups.iter().filter(|g| !s.conversation_actions.is_hidden(&g.id)).cloned().collect()
    }

    pub(crate) fn handle_contacts(&self, state: &Map<String, Value>) {
        let Some(j) = state.get("contacts").filter(|c| c.is_object()) else { return };
        let mut next = self.state.borrow().contacts.clone();
        next.apply(j);
        if next != self.state.borrow().contacts {
            self.state.borrow_mut().contacts = next;
            self.changed(Change::Contacts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_vietnamese() {
        assert_eq!(fold("Đặng Thị Ánh"), "dang thi anh");
        assert_eq!(fold("Nguyễn"), "nguyen");
    }

    #[test]
    fn applies_sections() {
        let mut s = ContactsState::default();
        s.apply(&json!({"friends": [{"id": "1", "name": "Ánh", "lastActive": 5}, {"id": "", "name": "x"}, {"id": "2", "zaloName": "9x"}]}));
        assert!(s.friends_loaded && !s.groups_loaded);
        assert_eq!(s.friends.len(), 2);
        assert_eq!((s.friends[0].section.as_str(), s.friends[0].sort_key.as_str()), ("A", "anh"));
        assert_eq!((s.friends[1].name.as_str(), s.friends[1].section.as_str()), ("9x", "#"));
        assert_eq!(s.friends[0].last_active, Some(5.0));
        s.apply(&json!({"groups": [{"id": "g1", "name": "Nhóm Đi", "members": 3}], "pendingRequestCount": -2, "loading": {"requests": true}}));
        assert_eq!(s.friends.len(), 2, "sections not in the payload stay");
        assert_eq!((s.groups[0].sort_key.as_str(), s.pending_request_count, s.loading_requests), ("nhom di", 0, true));
        s.apply(&json!({"search": {"status": "found", "user": {"id": "u", "name": "A", "requested": true}}, "notice": {"seq": 3, "ok": true, "text": "Đã gửi"}}));
        let u = s.search.user.as_ref().unwrap();
        assert!(s.search.status == SearchStatus::Found && u.requested && u.max_message == 150);
        assert_eq!(s.notice, Some(Notice { seq: 3, ok: true, text: "Đã gửi".into() }));
        s.apply(&json!({"received": [{"id": "r", "time": 0, "message": "hi"}]}));
        assert_eq!(s.received[0].date, None);
    }
}
