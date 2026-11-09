//! Shared media of a conversation ("Kho lưu trữ"): photos / videos, files and links, bridged to `window.__zaloMediaStore`.
//! Port of `ZaloEngine+SharedMedia.swift` and the pure parts of `SharedMediaModel.swift`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::engine::ZaloEngine;
use crate::models::{ChatMessage, J, Kind, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharedMediaKind {
    Images,
    Files,
    Links,
}

pub const ALL_KINDS: [SharedMediaKind; 3] = [SharedMediaKind::Images, SharedMediaKind::Files, SharedMediaKind::Links];

impl SharedMediaKind {
    pub fn raw(self) -> &'static str {
        match self {
            SharedMediaKind::Images => "images",
            SharedMediaKind::Files => "files",
            SharedMediaKind::Links => "links",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            SharedMediaKind::Images => "Ảnh/Video",
            SharedMediaKind::Files => "Tệp",
            SharedMediaKind::Links => "Liên kết",
        }
    }
    /// The same split the official panel makes by message type.
    pub fn accepts(self, kind: &Kind) -> bool {
        match (self, kind) {
            (SharedMediaKind::Images, Kind::Photo { .. } | Kind::Video { .. }) => true,
            (SharedMediaKind::Files, Kind::File { .. }) => true,
            (SharedMediaKind::Links, Kind::Link { url, .. }) => url.is_some(),
            _ => false,
        }
    }
    pub fn preview_limit(self) -> usize {
        match self {
            SharedMediaKind::Images => 9,
            _ => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedMediaPage {
    pub items: Vec<ChatMessage>,
    /// Pass back to read the next (older) page.
    pub cursor: Option<String>,
    pub more: bool,
}

impl SharedMediaPage {
    pub fn from_json(v: Option<&Value>, conv: &str) -> Option<Self> {
        let root = v?;
        let rows = root.get_v("items")?.as_array()?;
        Some(SharedMediaPage {
            items: rows.iter().filter_map(|r| ChatMessage::from_json(r, conv)).collect(),
            cursor: root.opt_s("cursor").filter(|c| !c.is_empty()),
            more: root.get_v("more").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

pub fn qualifies(m: &ChatMessage, kind: SharedMediaKind) -> bool {
    kind.accepts(&m.kind) && m.status != Status::Failed
}

/// Newest first; ties broken by id (longer = newer server id).
pub fn precedes(a: &ChatMessage, b: &ChatMessage) -> bool {
    if a.date != b.date {
        return a.date > b.date;
    }
    if a.id.len() != b.id.len() { a.id.len() > b.id.len() } else { a.id > b.id }
}

pub fn newest_first(mut items: Vec<ChatMessage>) -> Vec<ChatMessage> {
    items.sort_by(|a, b| {
        if precedes(a, b) {
            std::cmp::Ordering::Less
        } else if precedes(b, a) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    items
}

/// Stored pages + the messages loaded in the chat: loaded copies win, and a loaded message that no longer qualifies (recalled)
/// drops the stored copy. `loaded_since`: loaded messages older than the stored range are left to later pages.
pub fn merged(stored: &[ChatMessage], loaded: &[ChatMessage], kind: SharedMediaKind, loaded_since: Option<f64>) -> Vec<ChatMessage> {
    let mut by_id: HashMap<String, ChatMessage> = HashMap::new();
    for m in stored.iter().filter(|m| kind.accepts(&m.kind)) {
        by_id.insert(m.id.clone(), m.clone());
    }
    for m in loaded {
        if qualifies(m, kind) {
            if loaded_since.is_some_and(|since| m.date < since) {
                continue;
            }
            by_id.insert(m.id.clone(), m.clone());
        } else {
            by_id.remove(&m.id);
        }
    }
    newest_first(by_id.into_values().collect())
}

/// The newest few for the inspector.
pub fn preview(kind: SharedMediaKind, stored: &[ChatMessage], loaded: &[ChatMessage]) -> Vec<ChatMessage> {
    let limit = kind.preview_limit();
    let loaded_ids: HashSet<&str> = loaded.iter().map(|m| m.id.as_str()).collect();
    let candidates: Vec<ChatMessage> = loaded
        .iter()
        .filter(|m| qualifies(m, kind))
        .chain(stored.iter().filter(|m| kind.accepts(&m.kind) && !loaded_ids.contains(m.id.as_str())))
        .cloned()
        .collect();
    newest_first(candidates).into_iter().take(limit).collect()
}

fn local(ms: f64) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms as i64).single().unwrap_or_default()
}

pub fn month_title(ms: f64) -> String {
    use chrono::Datelike;
    let d = local(ms);
    format!("Tháng {}, {}", d.month(), d.year())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Month {
    pub id: String,
    pub title: String,
    pub items: Vec<ChatMessage>,
}

pub fn months(items: &[ChatMessage]) -> Vec<Month> {
    use chrono::Datelike;
    let mut out: Vec<Month> = vec![];
    for m in items {
        let d = local(m.date);
        let id = format!("{}-{}", d.year(), d.month());
        match out.last_mut() {
            Some(last) if last.id == id => last.items.push(m.clone()),
            _ => out.push(Month { id, title: month_title(m.date), items: vec![m.clone()] }),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedPhase {
    #[default]
    Idle,
    Loading,
    Failed,
}

/// One list's stored pages.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedMediaFeed {
    pub items: Vec<ChatMessage>,
    pub cursor: Option<String>,
    pub more: bool,
    pub phase: FeedPhase,
    pub loaded: bool,
    pub revision: u64,
}

impl Default for SharedMediaFeed {
    fn default() -> Self {
        SharedMediaFeed { items: vec![], cursor: None, more: true, phase: FeedPhase::Idle, loaded: false, revision: 0 }
    }
}

impl SharedMediaFeed {
    pub fn is_pending(&self) -> bool {
        !self.loaded && self.phase != FeedPhase::Failed
    }
    pub fn did_fail(&self) -> bool {
        !self.loaded && self.phase == FeedPhase::Failed
    }
    pub fn loaded_since(&self) -> Option<f64> {
        if self.loaded && self.more { self.items.last().map(|m| m.date) } else { None }
    }

    pub fn append(&mut self, page: SharedMediaPage) {
        let before = self.items.len();
        self.union(page.items);
        self.more = page.more && page.cursor.is_some() && self.items.len() > before;
        self.cursor = page.cursor;
        self.phase = FeedPhase::Idle;
        self.loaded = true;
        self.revision += 1;
    }

    /// A refreshed first page: drop stored items newer than its oldest one that it no longer lists (deleted / recalled), add new ones.
    pub fn merge_first_page(&mut self, page: SharedMediaPage) {
        if !self.loaded {
            self.append(page);
            return;
        }
        let page_ids: HashSet<String> = page.items.iter().map(|m| m.id.clone()).collect();
        let connected = self.items.iter().any(|m| page_ids.contains(&m.id));
        let held = self.items.len();
        if let Some(boundary) = newest_first(page.items.clone()).last().cloned() {
            self.items.retain(|m| page_ids.contains(&m.id) || !precedes(m, &boundary));
        } else if !page.more && page.cursor.is_none() {
            self.items.clear();
        }
        let removed = self.items.len() != held;
        if self.items.is_empty() || !connected {
            self.append(page);
        } else if self.union(page.items) || removed {
            self.revision += 1;
        }
    }

    fn union(&mut self, incoming: Vec<ChatMessage>) -> bool {
        let mut seen: HashSet<String> = self.items.iter().map(|m| m.id.clone()).collect();
        let fresh: Vec<ChatMessage> = incoming.into_iter().filter(|m| seen.insert(m.id.clone())).collect();
        if fresh.is_empty() {
            return false;
        }
        let mut all = std::mem::take(&mut self.items);
        all.extend(fresh);
        self.items = newest_first(all);
        true
    }
}

impl ZaloEngine {
    /// A page of what was ever shared in `conv`, older than `cursor` (None = newest). None = the service could not answer.
    pub async fn shared_media(&self, conv: &str, kind: SharedMediaKind, cursor: Option<&str>, limit: usize) -> Option<SharedMediaPage> {
        let args = json!([conv, kind.raw(), cursor, limit]).to_string();
        let script = format!("const s = window.__zaloMediaStore; if (!s || !s.page) return null; return await s.page(...{args});");
        let v = self.race(40, self.evaluate(&script)).await.flatten();
        SharedMediaPage::from_json(v.as_ref(), conv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, date: f64, kind: Value) -> ChatMessage {
        ChatMessage::from_json(&json!({"id": id, "date": date, "kind": kind}), "c").unwrap()
    }
    fn photo(id: &str, date: f64) -> ChatMessage {
        msg(id, date, json!({"type": "photo", "url": "https://x.test/a.jpg"}))
    }

    #[test]
    fn kinds_and_pages() {
        assert!(SharedMediaKind::Images.accepts(&photo("1", 1.0).kind));
        assert!(!SharedMediaKind::Links.accepts(&msg("2", 1.0, json!({"type": "link", "title": "x"})).kind), "links need a URL");
        assert!(SharedMediaPage::from_json(Some(&json!({"cursor": "x"})), "c").is_none());
        let p = SharedMediaPage::from_json(Some(&json!({"items": [{"id": "1", "kind": {"type": "file", "name": "a"}}], "cursor": "", "more": true})), "c").unwrap();
        assert_eq!((p.items.len(), p.cursor.clone(), p.more), (1, None, true));
    }

    #[test]
    fn preview_merges_loaded_and_stored() {
        let stored: Vec<_> = (1..=12).map(|i| photo(&format!("s{i}"), 1000.0 * i as f64)).collect();
        let mut recalled = photo("s12", 12000.0);
        recalled.kind = Kind::Recalled;
        let loaded = vec![photo("new", 20000.0), recalled];
        let p = preview(SharedMediaKind::Images, &stored, &loaded);
        assert_eq!(p.len(), 9);
        assert_eq!(p[0].id, "new");
        assert_eq!(p[1].id, "s11", "a loaded copy that no longer qualifies (recalled) hides the stored one");
        let m = merged(&stored, &loaded, SharedMediaKind::Images, None);
        assert!(!m.iter().any(|x| x.id == "s12"));
        assert_eq!(m.len(), 12);
    }

    #[test]
    fn feed_paging() {
        let mut f = SharedMediaFeed::default();
        assert!(f.is_pending());
        f.append(SharedMediaPage { items: vec![photo("3", 3.0), photo("2", 2.0)], cursor: Some("c1".into()), more: true });
        assert!(f.loaded && f.more && f.loaded_since() == Some(2.0));
        f.append(SharedMediaPage { items: vec![photo("2", 2.0)], cursor: Some("c2".into()), more: true });
        assert!(!f.more, "a page with nothing new ends the list");
        // Refresh: "3" was deleted, "4" is new.
        f.merge_first_page(SharedMediaPage { items: vec![photo("4", 4.0), photo("2", 2.0)], cursor: Some("c".into()), more: true });
        assert_eq!(f.items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["4", "2"]);
        let mut empty = f.clone();
        empty.merge_first_page(SharedMediaPage { items: vec![], cursor: None, more: false });
        assert!(empty.items.is_empty());
        let ms = months(&[photo("a", 1_700_000_000_000.0), photo("b", 1_700_000_100_000.0), photo("c", 1_600_000_000_000.0)]);
        assert_eq!(ms.len(), 2);
        assert!(ms[0].title.starts_with("Tháng "));
    }
}
