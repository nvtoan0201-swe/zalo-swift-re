//! Stickers: the composer picker and the suggestions while typing. Port of `ZaloEngine+Stickers.swift`, bridged to
//! `window.__zaloSticker`. Sending runs only from a click on a sticker. Typed text goes to the JS side only when "Gợi ý Sticker"
//! is on, never for end-to-end encrypted chats. No ids, names or content go to the log.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::engine::ZaloEngine;
use crate::models::{CardActionOutcome, ComposerDraft, J, NO_ANSWER_TEXT, clamped_int, sanitized_link};

/// Where a sticker animates from: a sprite sheet (`frames` pictures side by side, `frame_ms` each) or an animated image.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StickerAnimation {
    Sprite { url: String, frames: i64, frame_ms: i64 },
    Animated { url: String },
}

impl StickerAnimation {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let row = v?;
        let url = sanitized_link(row.opt_s("url"))?;
        match row.s("kind").as_str() {
            "sprite" => {
                let frames = row.get_v("frames").and_then(Value::as_f64).map(clamped_int).unwrap_or(0);
                let ms = row.get_v("ms").and_then(Value::as_f64).map(clamped_int).unwrap_or(0);
                if !(2..=200).contains(&frames) {
                    return None;
                }
                Some(StickerAnimation::Sprite { url, frames, frame_ms: ms.clamp(30, 1000) })
            }
            "gif" | "webp" => Some(StickerAnimation::Animated { url }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StickerEntry {
    pub cate_id: i64,
    pub sticker_id: i64,
    /// The still picture (http(s) CDN link).
    pub url: String,
    pub animation: Option<StickerAnimation>,
}

impl StickerEntry {
    pub fn id(&self) -> String {
        format!("{}_{}", self.cate_id, self.sticker_id)
    }

    /// Rows that are not a sticker are dropped; at most `limit` are kept, each sticker once.
    pub fn list(json: Option<&Value>, limit: usize) -> Vec<StickerEntry> {
        let mut seen = HashSet::new();
        let mut out = vec![];
        for row in json.and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[]) {
            let Some(cate) = row.get_v("cateId").and_then(Value::as_f64).map(clamped_int).filter(|c| *c >= 0) else { continue };
            let Some(id) = row.get_v("id").and_then(Value::as_f64).map(clamped_int).filter(|i| *i > 0) else { continue };
            let Some(url) = sanitized_link(row.opt_s("url")) else { continue };
            let e = StickerEntry { cate_id: cate, sticker_id: id, url, animation: StickerAnimation::from_json(row.get_v("anim")) };
            if !seen.insert(e.id()) {
                continue;
            }
            out.push(e);
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

/// One tab of the picker: a downloaded set, or the recent stickers (`RECENT_ID`, first when present).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StickerTab {
    pub id: i64,
    pub name: String,
    pub icon: Option<String>,
    pub count: i64,
}

impl StickerTab {
    pub const RECENT_ID: i64 = -2;
    pub fn is_recent(&self) -> bool {
        self.id == Self::RECENT_ID
    }
    /// None = the list could not be read (not the same as "no sets").
    pub fn list(json: Option<&Value>) -> Option<Vec<StickerTab>> {
        let rows = json?.as_array()?;
        let mut seen = HashSet::new();
        let mut out = vec![];
        for row in rows {
            let Some(id) = row.get_v("id").and_then(Value::as_f64).map(clamped_int).filter(|i| *i >= Self::RECENT_ID) else { continue };
            if !seen.insert(id) {
                continue;
            }
            out.push(StickerTab {
                id,
                name: row.s("name").chars().take(200).collect(),
                icon: sanitized_link(row.opt_s("icon")),
                count: row.get_v("count").and_then(Value::as_f64).map(clamped_int).unwrap_or(0).clamp(0, 10_000),
            });
            if out.len() >= 200 {
                break;
            }
        }
        Some(out)
    }
}

/// Decides the send source the official client records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickerSource {
    Category,
    Recent,
    Search,
    Suggestion,
}

impl StickerSource {
    pub fn raw(self) -> &'static str {
        match self {
            StickerSource::Category => "category",
            StickerSource::Recent => "recent",
            StickerSource::Search => "search",
            StickerSource::Suggestion => "suggestion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickerSuggestSetting {
    pub enabled: bool,
    /// Official typing debounce, ms.
    pub debounce: i64,
}

impl StickerSuggestSetting {
    pub const MINIMUM_DELAY: i64 = 120;
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let root = v?;
        if root.get_v("ok").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let enabled = root.get_v("enabled").and_then(Value::as_bool)?;
        Some(StickerSuggestSetting { enabled, debounce: root.get_v("debounce").and_then(Value::as_f64).map(clamped_int).unwrap_or(0).clamp(0, 2000) })
    }
    /// Milliseconds to wait after the last keystroke before asking.
    pub fn delay(s: Option<&Self>) -> i64 {
        Self::MINIMUM_DELAY.max(s.map(|s| s.debounce).unwrap_or(0))
    }
}

/// The composer's sticker suggestions: what was offered, for which text, and whether the user hid it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypingStickerState {
    pub stickers: Vec<StickerEntry>,
    pub suggested_for: Option<String>,
    pub dismissed: bool,
    held: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Taken {
    pub draft: ComposerDraft,
    pub typed: String,
}

impl TypingStickerState {
    pub fn offer(&mut self, found: Vec<StickerEntry>, text: &str) {
        if self.held.as_deref() == Some(text) {
            self.stickers.clear();
            self.suggested_for = None;
            return;
        }
        self.held = None;
        self.suggested_for = (!found.is_empty()).then(|| text.to_string());
        self.stickers = found;
    }
    pub fn hold(&mut self, text: &str) {
        self.held = Some(text.into());
    }
    pub fn is_held(&self, text: &str) -> bool {
        self.held.as_deref() == Some(text)
    }
    pub fn clear(&mut self) {
        self.stickers.clear();
        self.suggested_for = None;
    }
    /// `setting_enabled`: None = not known yet (not a "no").
    pub fn is_visible(&self, draft: &str, mention_list_open: bool, setting_enabled: Option<bool>) -> bool {
        !self.stickers.is_empty()
            && self.suggested_for.as_deref() == Some(draft)
            && !draft.trim().is_empty()
            && !self.dismissed
            && !mention_list_open
            && setting_enabled != Some(false)
    }
    pub fn take(&mut self, draft: ComposerDraft) -> Taken {
        let typed = self.suggested_for.clone().unwrap_or_else(|| draft.text.clone());
        self.clear();
        Taken { draft, typed }
    }
}

impl ZaloEngine {
    async fn call_sticker(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        self.evaluate(&format!("const c = window.__zaloSticker; if (!c || !c.{method}) return null; return await c.{method}(...{json});")).await
    }

    /// Downloaded sticker sets, "Gần đây" first when there are recent ones. None = unavailable.
    pub async fn sticker_tabs(&self) -> Option<Vec<StickerTab>> {
        StickerTab::list(self.race(60, self.call_sticker("categories", vec![])).await.flatten().as_ref())
    }

    /// The stickers of one tab. None = unavailable.
    pub async fn stickers_in_tab(&self, id: i64) -> Option<Vec<StickerEntry>> {
        let root = self.race(60, self.call_sticker("category", vec![json!(id)])).await.flatten()?;
        if root.get_v("id").and_then(Value::as_i64) != Some(id) {
            return None;
        }
        Some(StickerEntry::list(root.get_v("stickers"), 500))
    }

    /// Official dock search. None = could not be asked; [] = nothing matches.
    pub async fn search_stickers(&self, text: &str) -> Option<Vec<StickerEntry>> {
        let rows = self.race(40, self.call_sticker("search", vec![json!(text)])).await.flatten()?;
        rows.is_array().then(|| StickerEntry::list(Some(&rows), 40))
    }

    pub async fn typing_sticker_suggestions(&self, conv: &str, text: &str) -> Vec<StickerEntry> {
        StickerEntry::list(self.race(25, self.call_sticker("suggest", vec![json!(conv), json!(text)])).await.flatten().as_ref(), 30)
    }

    /// Sends one sticker through the official chat-box path (the user clicked it).
    pub async fn send_sticker(&self, s: &StickerEntry, conv: &str, source: StickerSource, typed: Option<&str>, index: i64) -> CardActionOutcome {
        let target = json!({"cateId": s.cate_id, "id": s.sticker_id});
        let context = if source == StickerSource::Suggestion { json!({"text": typed.unwrap_or(""), "index": index}) } else { json!({}) };
        let Some(answer) = self.race(90, self.call_sticker("send", vec![json!(conv), target, json!(source.raw()), context])).await else {
            return CardActionOutcome::Pending(NO_ANSWER_TEXT.into());
        };
        let outcome = CardActionOutcome::from_json(&answer.unwrap_or(Value::Null), |_| false);
        if outcome != CardActionOutcome::Done {
            olaz_core::log::write(format!("sticker send {} → not done", source.raw()));
        }
        outcome
    }

    pub async fn sticker_suggest_setting(&self) -> Option<StickerSuggestSetting> {
        StickerSuggestSetting::from_json(self.race(15, self.call_sticker("suggestSetting", vec![])).await.flatten().as_ref())
    }

    /// Turns "Gợi ý Sticker" on or off; returns the value now in force (None = could not be changed).
    pub async fn set_sticker_suggest(&self, enabled: bool) -> Option<StickerSuggestSetting> {
        let root = self.race(15, self.call_sticker("setSuggestSetting", vec![json!(enabled)])).await.flatten()?;
        if root.get_v("ok").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let now = self.sticker_suggest_setting().await;
        self.state.borrow_mut().sticker_suggest = now;
        now
    }

    /// Reads the suggestion setting when it is not known yet (at most once per 5 s).
    pub async fn load_sticker_suggest_if_needed(&self) {
        let now = crate::chat::now_ms();
        {
            let s = self.state.borrow();
            if s.sticker_suggest.is_some() || s.sticker_suggest_attempt.is_some_and(|t| now - t < 5000.0) {
                return;
            }
        }
        self.state.borrow_mut().sticker_suggest_attempt = Some(now);
        if let Some(v) = self.sticker_suggest_setting().await {
            self.state.borrow_mut().sticker_suggest = Some(v);
        }
    }

    pub fn reset_stickers(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.sticker_suggest = None;
            s.sticker_suggest_attempt = None;
        }
        self.call_chat("reset", vec![], "__zaloSticker");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stickers_and_tabs() {
        let rows = json!([
            {"cateId": 1, "id": 2, "url": "https://cdn.example.test/a.png", "anim": {"kind": "sprite", "url": "https://cdn.example.test/s.png", "frames": 8, "ms": 5}},
            {"cateId": 1, "id": 2, "url": "https://cdn.example.test/a.png"},
            {"cateId": 1, "id": 0, "url": "https://cdn.example.test/b.png"},
            {"cateId": 3, "id": 4, "url": "javascript:alert(1)"},
            {"cateId": 3, "id": 5, "url": "https://cdn.example.test/c.webp", "anim": {"kind": "sprite", "url": "https://x.test/", "frames": 1}}
        ]);
        let list = StickerEntry::list(Some(&rows), 500);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].animation, Some(StickerAnimation::Sprite { url: "https://cdn.example.test/s.png".into(), frames: 8, frame_ms: 30 }));
        assert_eq!(list[1].animation, None);
        assert_eq!(list[0].id(), "1_2");
        assert!(StickerTab::list(Some(&json!({}))).is_none());
        let tabs = StickerTab::list(Some(&json!([{"id": -2, "name": "Gần đây"}, {"id": -2}, {"id": -3}, {"id": 7, "count": 99999}]))).unwrap();
        assert_eq!(tabs.len(), 2);
        assert!(tabs[0].is_recent() && tabs[1].count == 10_000);
        assert_eq!(StickerSuggestSetting::from_json(Some(&json!({"ok": true, "enabled": true, "debounce": 0}))).map(|s| StickerSuggestSetting::delay(Some(&s))), Some(120));
        assert!(StickerSuggestSetting::from_json(Some(&json!({"ok": false, "enabled": true}))).is_none());
    }

    #[test]
    fn typing_strip_rules() {
        let s = StickerEntry { cate_id: 1, sticker_id: 1, url: "https://x.test/a".into(), animation: None };
        let mut t = TypingStickerState::default();
        t.offer(vec![s.clone()], "haha");
        assert!(t.is_visible("haha", false, None));
        assert!(!t.is_visible("hahaa", false, Some(true)), "stale suggestions never show");
        assert!(!t.is_visible("haha", true, Some(true)) && !t.is_visible("haha", false, Some(false)));
        let taken = t.take(ComposerDraft { text: "haha".into(), mentions: vec![], reply_to: None });
        assert_eq!(taken.typed, "haha");
        assert!(t.stickers.is_empty());
        t.hold("haha");
        t.offer(vec![s.clone()], "haha");
        assert!(t.stickers.is_empty(), "held text gets no strip");
        t.offer(vec![s], "hihi");
        assert!(t.is_visible("hihi", false, None) && !t.is_held("haha"));
    }
}
