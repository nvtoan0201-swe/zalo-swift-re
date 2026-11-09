//! Card actions: the buttons of official cards, contact cards, group invite links, sticker sets, reminders and
//! aliases. Port of `ZaloEngine+CardActions.swift` and `CardSheetModels.swift`. Every call follows a click.

use serde_json::{Value, json};

use crate::engine::ZaloEngine;
use crate::models::{
    CardActionOutcome, ContactProfile, FriendRequestPrep, J, NO_ANSWER_TEXT, ReminderResponders, ReminderSnapshot, SuggestedSticker, clamped_int,
    sanitized_link,
};

pub fn is_group_invite(url: &str) -> bool {
    let Some(u) = sanitized_link(Some(url.to_string())).and_then(|u| url::Url::parse(&u).ok()) else { return false };
    let host = u.host_str().unwrap_or("").to_lowercase();
    (host == "zalo.me" || host == "zaloapp.com") && u.path().starts_with("/g/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTarget {
    Chats,
    Contacts,
    Groups,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AliasDraft {
    pub uid: String,
    pub name: String,
    pub suggest: String,
    pub placeholder: String,
    pub max: i64,
    pub has_alias: Option<bool>,
}

pub const ALIAS_FALLBACK_MAX: i64 = 50;

/// Control characters (below U+0008) are dropped from alias text.
pub fn alias_sanitized(text: &str) -> String {
    text.chars().filter(|c| (*c as u32) > 7).collect()
}

/// A native sheet an official card effect asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum CardSheet {
    Alias(AliasDraft),
    GroupInvite(String),
    Stickers(i64),
    Poll(String),
    Tab(SidebarTarget),
}

fn digits(v: Option<&Value>) -> Option<String> {
    let text = match v? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_i64().map(|i| i.to_string())?,
        _ => return None,
    };
    (!text.is_empty() && text.len() <= 24 && text.chars().all(|c| c.is_ascii_digit())).then_some(text)
}

fn clip(v: Option<&Value>, n: usize) -> String {
    v.and_then(Value::as_str).unwrap_or("").chars().take(n).collect()
}

impl CardSheet {
    pub fn from_effect(e: &Value) -> Option<Self> {
        match e.s("type").as_str() {
            "alias" => {
                let uid = digits(e.get_v("uid"))?;
                let limit = e.get_v("max").and_then(Value::as_f64).map(clamped_int).unwrap_or(ALIAS_FALLBACK_MAX);
                Some(CardSheet::Alias(AliasDraft {
                    uid,
                    name: clip(e.get_v("name"), 200),
                    suggest: alias_sanitized(&clip(e.get_v("suggest"), 400)),
                    placeholder: clip(e.get_v("placeholder"), 200),
                    max: if limit > 0 { limit.min(200) } else { ALIAS_FALLBACK_MAX },
                    has_alias: e.get_v("hasAlias").and_then(Value::as_bool),
                }))
            }
            "groupInvite" => {
                let url = e.opt_s("url")?;
                is_group_invite(&url).then_some(CardSheet::GroupInvite(url))
            }
            "stickers" => {
                let id: i64 = digits(e.get_v("cateId"))?.parse().ok()?;
                (id > 0).then_some(CardSheet::Stickers(id))
            }
            "poll" => Some(CardSheet::Poll(digits(e.get_v("pollId"))?)),
            "tab" => match e.s("tab").as_str() {
                "chats" => Some(CardSheet::Tab(SidebarTarget::Chats)),
                "contacts" => Some(CardSheet::Tab(SidebarTarget::Contacts)),
                "groups" => Some(CardSheet::Tab(SidebarTarget::Groups)),
                _ => None,
            },
            _ => None,
        }
    }
}

fn valid_group(g: &str) -> bool {
    g.len() > 1 && g.len() <= 25 && g.starts_with('g') && g[1..].chars().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InviteState {
    Preview,
    Member,
    #[default]
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InviteMember {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub admin: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupInvitePreview {
    pub state: InviteState,
    pub group_id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub total_member: i64,
    pub desc: String,
    pub community: bool,
    pub creator_name: String,
    pub members: Vec<InviteMember>,
    pub text: String,
}

impl GroupInvitePreview {
    pub fn from_json(v: Option<&Value>) -> Self {
        let root = v.cloned().unwrap_or(Value::Null);
        let mut p = GroupInvitePreview { text: clip(root.get_v("text"), 300), ..Default::default() };
        let group = root.s("groupId");
        match root.s("state").as_str() {
            "member" if valid_group(&group) => {
                p.state = InviteState::Member;
                p.group_id = group;
            }
            "preview" if valid_group(&group) => {
                p.state = InviteState::Preview;
                p.group_id = group;
                p.name = clip(root.get_v("name"), 200);
                p.avatar = sanitized_link(root.opt_s("avatar"));
                p.total_member = root.get_v("totalMember").and_then(Value::as_f64).map(clamped_int).unwrap_or(0);
                p.desc = clip(root.get_v("desc"), 2000);
                p.community = root.b("community");
                p.creator_name = clip(root.get_v("creatorName"), 200);
                p.members = root
                    .arr("members")
                    .iter()
                    .take(12)
                    .filter_map(|r| {
                        let id = r.s("id");
                        (!id.is_empty()).then(|| InviteMember { id, name: clip(r.get_v("name"), 200), avatar: sanitized_link(r.opt_s("avatar")), admin: r.b("admin") })
                    })
                    .collect();
            }
            _ => {
                if p.text.is_empty() {
                    p.text = "Đã có lỗi xảy ra. Vui lòng thử lại".into();
                }
            }
        }
        p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinState {
    Joined,
    Pending,
    Already,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GroupJoinResult {
    pub state: JoinState,
    pub group_id: String,
    pub text: String,
}

impl GroupJoinResult {
    pub fn from_json(v: Option<&Value>) -> Self {
        let root = v.cloned().unwrap_or(Value::Null);
        let group = root.s("groupId");
        let text = clip(root.get_v("text"), 300);
        match root.s("state").as_str() {
            "joined" if valid_group(&group) => GroupJoinResult { state: JoinState::Joined, group_id: group, text },
            "already" if valid_group(&group) => GroupJoinResult { state: JoinState::Already, group_id: group, text },
            "pending" => GroupJoinResult { state: JoinState::Pending, group_id: String::new(), text },
            _ => GroupJoinResult { state: JoinState::Error, group_id: String::new(), text: if text.is_empty() { "Vào nhóm không thành công.".into() } else { text } },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CategorySticker {
    pub r#ref: String,
    pub url: String,
}

impl CategorySticker {
    pub fn sticker_id(&self) -> i64 {
        self.r#ref.rsplit('_').next().and_then(|s| s.parse().ok()).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StickerCategory {
    pub id: i64,
    pub name: String,
    pub icon: Option<String>,
    pub stickers: Vec<CategorySticker>,
}

impl StickerCategory {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let root = v?;
        let id = root.get_v("id").and_then(Value::as_f64).map(clamped_int).filter(|i| *i > 0)?;
        let prefix = format!("{id}_");
        let stickers: Vec<CategorySticker> = root
            .arr("stickers")
            .iter()
            .take(300)
            .filter_map(|r| {
                let rf = r.s("ref");
                let rest = rf.strip_prefix(&prefix)?;
                rest.parse::<i64>().ok()?;
                Some(CategorySticker { url: sanitized_link(r.opt_s("url"))?, r#ref: rf })
            })
            .collect();
        if stickers.is_empty() {
            return None;
        }
        Some(StickerCategory { id, name: clip(root.get_v("name"), 200), icon: sanitized_link(root.opt_s("icon")), stickers })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendRequestOrigin {
    Add,
    Card,
    None,
}

impl ZaloEngine {
    async fn call_card(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        self.evaluate(&format!("const c = window.__zaloChat; if (!c || !c.{method}) return null; return await c.{method}(...{json});")).await
    }

    /// Runs a card button through the official handler and applies its plain effects (open chat / URL, copy).
    pub async fn perform_card_action(&self, conv: &str, message: &str, r: &str) -> CardActionOutcome {
        let Some(answer) = self.race(90, self.call_card("performAction", vec![json!(conv), json!(message), json!(r)])).await else {
            return CardActionOutcome::Pending(NO_ANSWER_TEXT.into());
        };
        let outcome = CardActionOutcome::from_json(&answer.unwrap_or(Value::Null), |e| CardSheet::from_effect(e).is_some());
        let tag = r.split(':').next().unwrap_or("-");
        if self.state.borrow().open_conversation_id.as_deref() != Some(conv) {
            let drop = match &outcome {
                CardActionOutcome::OpenChat(_) | CardActionOutcome::OpenURL(_) | CardActionOutcome::Copy(_) => true,
                CardActionOutcome::Present(e) => matches!(CardSheet::from_effect(e), Some(CardSheet::Tab(_))),
                _ => false,
            };
            if drop {
                olaz_core::log::write(format!("card action {tag} → dropped (conversation changed)"));
                return CardActionOutcome::Done;
            }
        }
        match &outcome {
            CardActionOutcome::OpenChat(id) => self.open_contact_conversation(id),
            CardActionOutcome::OpenURL(u) => self.open_url(u),
            CardActionOutcome::Copy(t) => {
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(t.clone()));
            }
            CardActionOutcome::Failed(_) | CardActionOutcome::Locked(_) | CardActionOutcome::Unsupported(_) | CardActionOutcome::Pending(_) => {
                olaz_core::log::write(format!("card action {tag} → not done"));
            }
            _ => {}
        }
        outcome
    }

    pub fn reset_card_state(&self) {
        self.call_chat("cardReset", vec![], "__zaloChat");
    }

    pub async fn suggested_stickers(&self, conv: &str, message: &str) -> Vec<SuggestedSticker> {
        SuggestedSticker::list(&self.call_card("cardStickers", vec![json!(conv), json!(message)]).await.unwrap_or(Value::Null))
    }

    pub async fn contact_card_state(&self, uid: &str) -> (bool, bool) {
        let root = self.call_card("contactState", vec![json!(uid)]).await.unwrap_or(Value::Null);
        (root.b("isSelf"), root.b("isFriend"))
    }

    pub async fn contact_profile(&self, uid: &str) -> Option<ContactProfile> {
        ContactProfile::from_json(&self.call_card("contactProfile", vec![json!(uid)]).await?)
    }

    pub async fn prepare_friend_request(&self, uid: &str, conv: &str, via: FriendRequestOrigin) -> FriendRequestPrep {
        let via = match via {
            FriendRequestOrigin::Add => "add",
            FriendRequestOrigin::Card => "card",
            FriendRequestOrigin::None => "none",
        };
        let answer = self.race(20, self.call_card("prepareFriendRequest", vec![json!(uid), json!(conv), json!(via)])).await.flatten();
        FriendRequestPrep::from_json(&answer.unwrap_or(Value::Null))
    }

    /// (ok, text, pending)
    pub async fn submit_friend_request(&self, uid: &str, message: &str) -> (bool, String, bool) {
        let args = json!([uid, message]).to_string();
        let script = format!("const c = window.__zaloContacts; if (!c || !c.sendRequest) return 'missing'; return await c.sendRequest(...{args});");
        let Some(answer) = self.race(90, self.evaluate(&script)).await else {
            return (false, "Chưa nhận được phản hồi. Hãy kiểm tra mục “Đã gửi” trong Lời mời kết bạn trước khi thử lại.".into(), true);
        };
        let root = answer.unwrap_or(Value::Null);
        let Some(ok) = root.get_v("ok").and_then(Value::as_bool) else {
            return (false, "Chưa sẵn sàng gửi lời mời kết bạn, hãy thử lại sau giây lát.".into(), false);
        };
        let text = root.opt_s("text").unwrap_or_else(|| if ok { "Đã gửi lời mời kết bạn.".into() } else { "Không thể gửi lời mời kết bạn.".into() });
        (ok, text, root.b("pending"))
    }

    pub async fn save_alias(&self, uid: &str, name: &str) -> (bool, String) {
        let answer = self.race(90, self.call_card("aliasSave", vec![json!(uid), json!(name)])).await.flatten();
        match answer.as_ref().and_then(|a| a.get_v("ok").and_then(Value::as_bool).map(|ok| (ok, a))) {
            Some((ok, a)) => (ok, clip(a.get_v("text"), 300)),
            None => (false, "Chưa nhận được phản hồi. Hãy kiểm tra lại tên gợi nhớ trước khi thử lại.".into()),
        }
    }

    pub async fn group_invite_preview(&self, url: &str) -> GroupInvitePreview {
        GroupInvitePreview::from_json(self.race(60, self.call_card("groupPreview", vec![json!(url)])).await.flatten().as_ref())
    }

    pub async fn join_group(&self, url: &str) -> GroupJoinResult {
        match self.race(90, self.call_card("groupJoin", vec![json!(url)])).await {
            None => GroupJoinResult { state: JoinState::Error, group_id: String::new(), text: "Chưa nhận được phản hồi. Hãy kiểm tra danh sách nhóm trước khi thử lại.".into() },
            Some(v) => GroupJoinResult::from_json(v.as_ref()),
        }
    }

    pub async fn sticker_category(&self, id: i64) -> Option<StickerCategory> {
        StickerCategory::from_json(self.race(60, self.call_card("stickerCategory", vec![json!(id)])).await.flatten().as_ref())
    }

    pub async fn send_category_sticker(&self, conv: &str, category: i64, sticker: i64) -> CardActionOutcome {
        match self.race(90, self.call_card("stickerCategorySend", vec![json!(conv), json!(category), json!(sticker)])).await {
            None => CardActionOutcome::Pending(NO_ANSWER_TEXT.into()),
            Some(v) => CardActionOutcome::from_json(&v.unwrap_or(Value::Null), |_| false),
        }
    }

    pub async fn reminder_watch(&self, conv: &str, message: &str, signature: &str) -> Option<ReminderSnapshot> {
        ReminderSnapshot::from_json(&self.call_card("reminderWatch", vec![json!(conv), json!(message), json!(signature)]).await?)
    }

    pub async fn respond_to_reminder(&self, conv: &str, message: &str, accept: bool) -> (bool, String, Option<ReminderSnapshot>) {
        let answer = self.race(40, self.call_card("reminderRespond", vec![json!(conv), json!(message), json!(if accept { "accept" } else { "reject" })])).await.flatten();
        let Some(root) = answer.filter(|a| a.is_object()) else { return (false, "Đang xử lý, hãy kiểm tra lại sau.".into(), None) };
        (root.b("ok"), root.s("text"), root.get_v("snapshot").and_then(ReminderSnapshot::from_json))
    }

    pub async fn reminder_responders(&self, conv: &str, message: &str) -> Option<ReminderResponders> {
        ReminderResponders::from_json(&self.race(40, self.call_card("reminderResponders", vec![json!(conv), json!(message)])).await.flatten()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheets_from_effects() {
        assert!(matches!(CardSheet::from_effect(&json!({"type": "alias", "uid": "123", "max": 999, "suggest": "a\u{1}b"})), Some(CardSheet::Alias(a)) if a.max == 200 && a.suggest == "ab"));
        assert!(CardSheet::from_effect(&json!({"type": "alias", "uid": "12a"})).is_none());
        assert_eq!(CardSheet::from_effect(&json!({"type": "groupInvite", "url": "https://zalo.me/g/abc"})), Some(CardSheet::GroupInvite("https://zalo.me/g/abc".into())));
        assert!(CardSheet::from_effect(&json!({"type": "groupInvite", "url": "https://evil.test/g/abc"})).is_none());
        assert_eq!(CardSheet::from_effect(&json!({"type": "stickers", "cateId": 42})), Some(CardSheet::Stickers(42)));
        assert_eq!(CardSheet::from_effect(&json!({"type": "tab", "tab": "groups"})), Some(CardSheet::Tab(SidebarTarget::Groups)));
        let o = CardActionOutcome::from_json(&json!({"status": "effect", "effect": {"type": "poll", "pollId": "77"}}), |e| CardSheet::from_effect(e).is_some());
        assert!(matches!(o, CardActionOutcome::Present(_)));
    }

    #[test]
    fn invite_and_join() {
        let p = GroupInvitePreview::from_json(Some(&json!({"state": "preview", "groupId": "g123", "name": "N", "totalMember": 5, "members": [{"id": "1", "name": "A", "admin": true}, {"id": ""}]})));
        assert_eq!((p.state, p.members.len(), p.total_member), (InviteState::Preview, 1, 5));
        assert_eq!(GroupInvitePreview::from_json(Some(&json!({"state": "preview", "groupId": "x1"}))).state, InviteState::Error);
        assert_eq!(GroupJoinResult::from_json(None).text, "Vào nhóm không thành công.");
        assert_eq!(GroupJoinResult::from_json(Some(&json!({"state": "joined", "groupId": "g9"}))).state, JoinState::Joined);
        let c = StickerCategory::from_json(Some(&json!({"id": 7, "stickers": [{"ref": "7_12", "url": "https://x.test/a"}, {"ref": "8_1", "url": "https://x.test/b"}]}))).unwrap();
        assert_eq!((c.stickers.len(), c.stickers[0].sticker_id()), (1, 12));
        assert!(StickerCategory::from_json(Some(&json!({"id": 7, "stickers": []}))).is_none());
    }
}
