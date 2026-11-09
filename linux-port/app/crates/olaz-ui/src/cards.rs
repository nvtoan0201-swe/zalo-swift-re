//! Card actions UI (CardActions.swift, CardSheets.swift, MessageCards.swift contact card): the button bar of official
//! cards, the contact card footer, sticker suggestions, and the sheets they open (profile + friend request, alias, group
//! invite, sticker set). Sheets are requested through [`request`] and presented by the open chat.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use olaz_engine::card_actions::{AliasDraft, CardSheet, FriendRequestOrigin, GroupInvitePreview, InviteState, JoinState, StickerCategory, alias_sanitized, is_group_invite};
use olaz_engine::models::{CardActionOutcome, CardButton, ChatMessage, ContactCard, ContactProfile, FriendRequestPrep, FriendRequestState, SuggestedSticker};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, avatar, button, remote_picture, spinner};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileRequest {
    pub uid: String,
    pub conversation: String,
    pub name: String,
    pub avatar: Option<String>,
    pub phone: String,
    pub is_official_account: bool,
    pub origin: FriendRequestOrigin,
    pub start_with_friend_request: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CardRequest {
    Profile(ProfileRequest),
    Alias { conversation: String, draft: AliasDraft },
    GroupInvite { conversation: String, url: String },
    Stickers { conversation: String, cate_id: i64 },
    Reminder { conversation: String, message: String, open_list: bool },
}

#[derive(Default)]
struct Center {
    busy: HashSet<String>,
    request: Option<CardRequest>,
    contact_state: HashMap<String, Option<(bool, bool)>>,
    suggestions: HashMap<String, Option<Vec<SuggestedSticker>>>,
}

thread_local! {
    static CENTER: RefCell<Center> = RefCell::new(Center::default());
}

pub fn request(r: CardRequest, cx: &mut App) {
    CENTER.with(|c| c.borrow_mut().request = Some(r));
    cx.refresh_windows();
}

pub fn take_request() -> Option<CardRequest> {
    CENTER.with(|c| c.borrow_mut().request.take())
}

fn key(conv: &str, msg: &str, r: &str) -> String {
    format!("{conv}|{msg}|{r}")
}

fn is_busy(conv: &str, msg: &str, r: Option<&str>) -> bool {
    CENTER.with(|c| {
        let c = c.borrow();
        match r {
            Some(r) => c.busy.contains(&key(conv, msg, r)),
            None => {
                let prefix = key(conv, msg, "");
                c.busy.iter().any(|k| k.starts_with(&prefix))
            }
        }
    })
}

/// Runs a card button (one action per message at a time) and presents what it asks for.
pub fn run(conv: &str, msg: &str, r: &str, cx: &mut App) {
    if is_busy(conv, msg, None) {
        return;
    }
    let k = key(conv, msg, r);
    CENTER.with(|c| c.borrow_mut().busy.insert(k.clone()));
    cx.refresh_windows();
    let (e, conv, msg, r) = (engine(cx), conv.to_string(), msg.to_string(), r.to_string());
    cx.spawn(async move |cx| {
        let outcome = e.perform_card_action(&conv, &msg, &r).await;
        CENTER.with(|c| c.borrow_mut().busy.remove(&k));
        let open = e.state.borrow().open_conversation_id.as_deref() == Some(conv.as_str());
        let _ = cx.update(|cx| {
            match outcome {
                CardActionOutcome::OpenProfile(uid) if open => request(
                    CardRequest::Profile(ProfileRequest {
                        uid,
                        conversation: conv.clone(),
                        name: String::new(),
                        avatar: None,
                        phone: String::new(),
                        is_official_account: false,
                        origin: FriendRequestOrigin::None,
                        start_with_friend_request: false,
                    }),
                    cx,
                ),
                CardActionOutcome::Copy(_) => e.show_toast("Đã sao chép", Some(&conv)),
                CardActionOutcome::Unsupported(t) | CardActionOutcome::Locked(t) | CardActionOutcome::Failed(t) | CardActionOutcome::Pending(t) => e.show_toast(&t, Some(&conv)),
                CardActionOutcome::Present(effect) if open => match CardSheet::from_effect(&effect) {
                    Some(CardSheet::Alias(draft)) => request(CardRequest::Alias { conversation: conv.clone(), draft }, cx),
                    Some(CardSheet::GroupInvite(url)) => request(CardRequest::GroupInvite { conversation: conv.clone(), url }, cx),
                    Some(CardSheet::Stickers(id)) => request(CardRequest::Stickers { conversation: conv.clone(), cate_id: id }, cx),
                    Some(CardSheet::Poll(id)) => crate::polls::request(crate::polls::PollRequest::Detail { poll_id: id, conversation: conv.clone(), question: String::new(), voters: false }, cx),
                    Some(CardSheet::Tab(target)) => crate::shell::open_section(target, cx),
                    None => {}
                },
                _ => {}
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Opens a link from a card: group invite links get the native sheet.
pub fn open_link(conv: &str, url: &str, cx: &mut App) {
    if is_group_invite(url) {
        request(CardRequest::GroupInvite { conversation: conv.to_string(), url: url.to_string() }, cx);
    } else {
        engine(cx).open_url(url);
    }
}

/// The row of buttons under an official card.
pub fn button_bar(m: &ChatMessage, width: f32, theme: &Theme) -> Option<AnyElement> {
    if m.card_buttons.is_empty() {
        return None;
    }
    let any_busy = is_busy(&m.conversation_id, &m.id, None);
    let mut row = div().flex().items_center();
    for (i, b) in m.card_buttons.iter().enumerate() {
        let busy = is_busy(&m.conversation_id, &m.id, Some(&b.r#ref));
        let enabled = b.enabled && !any_busy;
        let (conv, msg, r) = (m.conversation_id.clone(), m.id.clone(), b.r#ref.clone());
        if i > 0 {
            row = row.child(div().w(px(1.0)).h(px(22.0)).bg(theme.separator()));
        }
        row = row.child(
            div()
                .id(SharedString::from(format!("cb-{}-{i}", m.id)))
                .flex_1()
                .py(px(10.0))
                .flex()
                .justify_center()
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if b.enabled { theme.accent } else { theme.secondary() })
                .when(enabled, |el| el.hover(|s| s.bg(theme.hover())))
                .child(if busy { spinner(12.0, theme.secondary()).into_any_element() } else { div().truncate().child(b.label.clone()).into_any_element() })
                .on_click(move |_, _, cx| {
                    if enabled {
                        run(&conv, &msg, &r, cx)
                    }
                }),
        );
    }
    Some(div().w(px(width)).border_t_1().border_color(theme.separator()).child(row).into_any_element())
}

#[allow(dead_code)]
fn unused_button(_: &CardButton) {}

/// The official contact card ("danh thiếp"): opens the profile; KẾT BẠN (stranger) / NHẮN TIN.
pub fn contact_card(card: &ContactCard, m: &ChatMessage, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let state = if card.uid.is_empty() {
        None
    } else {
        let known = CENTER.with(|c| c.borrow().contact_state.get(&card.uid).cloned());
        match known {
            Some(s) => s,
            None => {
                CENTER.with(|c| c.borrow_mut().contact_state.insert(card.uid.clone(), None));
                let (e, uid) = (engine(cx), card.uid.clone());
                cx.spawn(async move |cx| {
                    let s = e.contact_card_state(&uid).await;
                    CENTER.with(|c| c.borrow_mut().contact_state.insert(uid, Some(s)));
                    let _ = cx.update(|cx| cx.refresh_windows());
                })
                .detach();
                None
            }
        }
    };
    let extra = if card.phone.is_empty() { card.oa_link.clone() } else { card.phone.clone() };
    let profile = |add: bool| ProfileRequest {
        uid: card.uid.clone(),
        conversation: m.conversation_id.clone(),
        name: card.name.clone(),
        avatar: card.avatar.clone(),
        phone: card.phone.clone(),
        is_official_account: card.is_official_account,
        origin: if add { FriendRequestOrigin::Add } else { FriendRequestOrigin::Card },
        start_with_friend_request: add,
    };
    let (p_open, p_add) = (profile(false), profile(true));
    let has_uid = !card.uid.is_empty();
    let header = div()
        .id(SharedString::from(format!("cc-{}", m.id)))
        .p(px(12.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(avatar(card.avatar.as_deref(), &card.name, 52.0, cx))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(2).child(if card.name.is_empty() { "Danh thiếp".to_string() } else { card.name.clone() }))
                .when(!extra.is_empty(), |el| el.child(div().truncate().text_size(px(12.0)).text_color(theme.secondary()).child(extra.clone())))
                .child(
                    div()
                        .pt(px(2.0))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if card.is_official_account { theme.orange() } else { theme.accent })
                        .child(if card.is_official_account { "Official Account" } else { "Danh thiếp Zalo" }),
                ),
        )
        .when_some(card.qr.clone(), |el, qr| el.child(div().p(px(2.0)).rounded(px(6.0)).bg(gpui::white()).child(remote_picture(Some(&qr), 50.0, 50.0, 0.0, ObjectFit::Contain, cx))))
        .when(has_uid, |el| el.cursor_pointer().on_click(move |_, _, cx| request(CardRequest::Profile(p_open.clone()), cx)));
    let mut col = div().w(px(280.0)).rounded(px(16.0)).overflow_hidden().bg(theme.incoming_bubble()).flex().flex_col().child(header);
    let is_self = state.is_some_and(|s| s.0);
    if has_uid && !is_self {
        let footer_btn = |id: String, title: &'static str, f: Box<dyn Fn(&mut App)>| {
            div()
                .id(SharedString::from(id))
                .flex_1()
                .py(px(9.0))
                .flex()
                .justify_center()
                .text_size(px(12.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.accent)
                .hover(|s| s.bg(theme.hover()))
                .child(title)
                .on_click(move |_, _, cx| f(cx))
        };
        let mut row = div().flex().items_center().border_t_1().border_color(theme.separator());
        if let Some((_, is_friend)) = state {
            if !card.is_official_account && !is_friend {
                row = row.child(footer_btn(format!("cc-add-{}", m.id), "KẾT BẠN", Box::new(move |cx| request(CardRequest::Profile(p_add.clone()), cx))));
                row = row.child(div().w(px(1.0)).h(px(22.0)).bg(theme.separator()));
            }
        }
        let uid = card.uid.clone();
        row = row.child(footer_btn(format!("cc-msg-{}", m.id), "NHẮN TIN", Box::new(move |cx| engine(cx).open_contact_conversation(&uid))));
        col = col.child(row);
    }
    col.into_any_element()
}

/// Official sticker suggestions on a "say hi" card (`message.suggestsStickers`).
pub fn sticker_suggestions(m: &ChatMessage, cx: &mut App) -> Option<AnyElement> {
    if !m.suggests_stickers {
        return None;
    }
    let theme = Theme::get(cx).clone();
    let k = key(&m.conversation_id, &m.id, "");
    let known = CENTER.with(|c| c.borrow().suggestions.get(&k).cloned());
    let list = match known {
        Some(Some(l)) => l,
        Some(None) => return None,
        None => {
            CENTER.with(|c| c.borrow_mut().suggestions.insert(k.clone(), None));
            let (e, conv, msg) = (engine(cx), m.conversation_id.clone(), m.id.clone());
            cx.spawn(async move |cx| {
                for attempt in 0..4 {
                    let list = e.suggested_stickers(&conv, &msg).await;
                    if !list.is_empty() {
                        CENTER.with(|c| c.borrow_mut().suggestions.insert(k.clone(), Some(list)));
                        let _ = cx.update(|cx| cx.refresh_windows());
                        return;
                    }
                    if attempt < 3 {
                        cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                    }
                }
            })
            .detach();
            return None;
        }
    };
    let any_busy = is_busy(&m.conversation_id, &m.id, None);
    let mut row = div().flex().gap(px(8.0)).px(px(12.0));
    for s in list {
        let (conv, msg, r) = (m.conversation_id.clone(), m.id.clone(), s.r#ref.clone());
        let busy = is_busy(&conv, &msg, Some(&r));
        row = row.child(
            div()
                .id(SharedString::from(format!("ss-{}-{}", m.id, s.r#ref)))
                .size(px(58.0))
                .when(busy, |el| el.opacity(0.4))
                .child(remote_picture(Some(&s.url), 58.0, 58.0, 0.0, ObjectFit::Contain, cx))
                .on_click(move |_, _, cx| {
                    if !any_busy {
                        run(&conv, &msg, &r, cx)
                    }
                }),
        );
    }
    let _ = theme;
    Some(div().id(SharedString::from(format!("ss-row-{}", m.id))).h(px(64.0)).overflow_x_scroll().child(row).into_any_element())
}

pub enum CardSheetEvent {
    Close,
}

fn sheet(theme: &Theme, w: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id("card-sheet")
        .relative()
            .child(crate::widgets::occluder())
        .relative()
        .w(px(w))
        .p(px(24.0))
        .rounded(px(14.0))
        .bg(theme.content_bg())
        .border_1()
        .border_color(theme.separator())
        .shadow_lg()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.0))
}

// MARK: Profile

pub struct ProfileSheet {
    req: ProfileRequest,
    profile: Option<ContactProfile>,
    prep: Option<FriendRequestPrep>,
    preparing: bool,
    message: Entity<TextareaState>,
    sending: bool,
    pending: bool,
    result: Option<(bool, String)>,
    prep_applied: bool,
}

impl EventEmitter<CardSheetEvent> for ProfileSheet {}

impl ProfileSheet {
    pub fn new(req: ProfileRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 3));
        cx.subscribe(&message, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        let (e, uid, start) = (engine(cx), req.uid.clone(), req.start_with_friend_request);
        cx.spawn(async move |this, cx| {
            let p = e.contact_profile(&uid).await;
            let _ = this.update(cx, |this, cx| {
                this.profile = p;
                if start {
                    this.begin_request(cx);
                }
                cx.notify();
            });
        })
        .detach();
        ProfileSheet { req, profile: None, prep: None, preparing: false, message, sending: false, pending: false, result: None, prep_applied: false }
    }

    fn begin_request(&mut self, cx: &mut Context<Self>) {
        if self.prep.is_some() || self.preparing {
            return;
        }
        self.preparing = true;
        let origin = if self.req.start_with_friend_request { FriendRequestOrigin::Add } else { self.req.origin };
        let (e, uid, conv) = (engine(cx), self.req.uid.clone(), self.req.conversation.clone());
        cx.spawn(async move |this, cx| {
            let prep = e.prepare_friend_request(&uid, &conv, origin).await;
            let _ = this.update(cx, |this, cx| {
                this.preparing = false;
                this.prep = Some(prep);
                cx.notify();
            });
        })
        .detach();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.sending = true;
        self.result = None;
        let (e, uid, text) = (engine(cx), self.req.uid.clone(), self.message.read(cx).value().to_string());
        cx.spawn(async move |this, cx| {
            let (ok, text, pending) = e.submit_friend_request(&uid, &text).await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                this.result = Some((ok, text));
                if ok {
                    if let Some(p) = this.prep.as_mut() {
                        p.state = FriendRequestState::Requested;
                    }
                } else if pending {
                    this.pending = true;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for ProfileSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        if let (Some(p), false) = (&self.prep, self.prep_applied) {
            self.prep_applied = true;
            let text = crate::cards::prefix_utf16(&p.default_message, p.max_message.max(0) as usize);
            self.message.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let name = self
            .profile
            .as_ref()
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| if self.req.name.is_empty() { "Người dùng Zalo".into() } else { self.req.name.clone() });
        let url = self.profile.as_ref().and_then(|p| p.avatar.clone()).or(self.req.avatar.clone());
        let is_oa = self.profile.as_ref().map(|p| p.is_official_account).unwrap_or(self.req.is_official_account);
        let me = cx.entity().downgrade();
        let act = move |f: fn(&mut ProfileSheet, &mut Context<ProfileSheet>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    f(this, cx);
                    cx.notify();
                });
            }
        };
        let mut s = sheet(&theme, 340.0).child(avatar(url.as_deref(), &name, 84.0, cx)).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(div().text_center().text_size(px(17.0)).font_weight(FontWeight::BOLD).child(name.clone()))
                .when_some(self.profile.as_ref().map(|p| p.zalo_name.clone()).filter(|z| !z.is_empty() && *z != name), |el, z| el.child(div().text_size(px(12.5)).text_color(theme.secondary()).child(z)))
                .when_some(self.profile.as_ref().map(|p| p.bio.clone()).filter(|b| !b.is_empty()), |el, b| el.child(div().text_center().text_size(px(12.5)).text_color(theme.secondary()).child(b)))
                .when(!self.req.phone.is_empty(), |el| el.child(div().text_size(px(12.5)).text_color(theme.secondary()).child(self.req.phone.clone()))),
        );
        if self.profile.as_ref().is_none_or(|p| !p.is_self) {
            let uid = self.req.uid.clone();
            let mut row = div().flex().gap(px(8.0)).child(button("pf-message", "Nhắn tin", ButtonStyle::Prominent, false, &theme, {
                let me = cx.entity().downgrade();
                move |_, _, cx| {
                    engine(cx).open_contact_conversation(&uid);
                    let _ = me.update(cx, |_, cx| cx.emit(CardSheetEvent::Close));
                }
            }));
            if self.profile.as_ref().is_some_and(|p| !p.is_friend) && !is_oa && self.prep.is_none() {
                row = row.child(button("pf-add", if self.preparing { "Đang kiểm tra…" } else { "Kết bạn" }, ButtonStyle::Glass, self.preparing, &theme, act(|t, cx| t.begin_request(cx))));
            }
            s = s.child(row);
        }
        if let Some(p) = self.prep.clone() {
            let note = |t: &str| div().text_center().text_size(px(12.5)).text_color(theme.secondary()).child(t.to_string());
            s = match p.state {
                FriendRequestState::Ready => {
                    let text = self.message.read(cx).value().to_string();
                    let count = text.encode_utf16().count() as i64;
                    if count > p.max_message {
                        let clipped = prefix_utf16(&text, p.max_message.max(0) as usize);
                        self.message.update(cx, |st, cx| st.set_value(clipped, window, cx));
                    }
                    let disabled = self.sending || self.pending || text.trim().is_empty();
                    s.child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Lời nhắn"))
                            .child(div().p(px(8.0)).rounded(px(12.0)).bg(with_alpha(theme.label(), 0.06)).child(Textarea::new(&self.message).appearance(false).text_size(px(13.0))))
                            .child(div().flex().justify_end().text_size(px(11.0)).text_color(theme.tertiary()).child(format!("{}/{}", count.min(p.max_message), p.max_message)))
                            .child(button("pf-send", if self.sending { "Đang gửi…" } else { "Gửi lời mời kết bạn" }, ButtonStyle::Prominent, disabled, &theme, act(|t, cx| t.submit(cx))))
                            .when_some(self.result.clone().filter(|r| !r.0), |el, (_, t)| el.child(div().text_size(px(12.0)).text_color(theme.orange()).child(t))),
                    )
                }
                FriendRequestState::Requested => s.child(note(if self.result.as_ref().is_some_and(|r| r.0) { "Đã gửi lời mời kết bạn." } else { "Bạn đã gửi yêu cầu kết bạn và đang chờ người này đồng ý" })),
                FriendRequestState::Requesting => s.child(note("Người này đã gửi lời mời kết bạn cho bạn. Hãy chấp nhận ở mục “Lời mời kết bạn”.")),
                FriendRequestState::Friend => s.child(note("Đã là bạn bè.")),
                FriendRequestState::Me => s,
                FriendRequestState::Error => s.child(note("Không thể kiểm tra trạng thái kết bạn lúc này. Hãy thử lại sau.")),
            };
        }
        s.child(button("pf-close", "Đóng", ButtonStyle::Glass, false, &theme, act(|_, cx| cx.emit(CardSheetEvent::Close))))
    }
}

/// The longest prefix of `s` within `limit` UTF-16 units (official limits count UTF-16).
pub fn prefix_utf16(s: &str, limit: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let n = ch.len_utf16();
        if used + n > limit {
            break;
        }
        used += n;
        out.push(ch);
    }
    out
}

// MARK: Alias

pub struct AliasSheet {
    conversation: String,
    draft: AliasDraft,
    input: Entity<InputState>,
    avatar: Option<String>,
    saving: bool,
    error: Option<String>,
}

impl EventEmitter<CardSheetEvent> for AliasSheet {}

impl AliasSheet {
    pub fn new(conversation: String, draft: AliasDraft, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let max = draft.max.max(0) as usize;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(draft.placeholder.clone()));
        input.update(cx, |s, cx| {
            s.set_value(prefix_utf16(&draft.suggest, max), window, cx);
            s.focus(window, cx);
        });
        cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                let v = input.read(cx).value().to_string();
                let clean = prefix_utf16(&alias_sanitized(&v), max);
                if clean != v {
                    input.update(cx, |s, cx| s.set_value(clean, window, cx));
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.save(cx),
            _ => {}
        })
        .detach();
        let (e, uid) = (engine(cx), draft.uid.clone());
        cx.spawn(async move |this, cx| {
            let p = e.contact_profile(&uid).await;
            let _ = this.update(cx, |this, cx| {
                this.avatar = p.and_then(|p| p.avatar);
                cx.notify();
            });
        })
        .detach();
        AliasSheet { conversation, draft, input, avatar: None, saving: false, error: None }
    }

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let empty = self.text(cx).trim().is_empty();
        if self.saving || (empty && self.draft.has_alias == Some(false)) {
            return;
        }
        self.saving = true;
        self.error = None;
        let (e, uid, name, conv) = (engine(cx), self.draft.uid.clone(), self.text(cx), self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let (ok, text) = e.save_alias(&uid, &name).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if ok {
                    e.show_toast(&text, Some(&conv));
                    cx.emit(CardSheetEvent::Close);
                } else {
                    this.error = Some(if text.is_empty() { "Có lỗi xảy ra, vui lòng thử lại sau".into() } else { text });
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for AliasSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let text = self.text(cx);
        let empty = text.trim().is_empty();
        let removes = empty && self.draft.has_alias == Some(true);
        let nothing = empty && self.draft.has_alias == Some(false);
        let shown = if self.draft.name.is_empty() { "người này".to_string() } else { self.draft.name.clone() };
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        sheet(&theme, 360.0)
            .child(avatar(self.avatar.as_deref(), &self.draft.name, 84.0, cx))
            .child(div().text_size(px(17.0)).font_weight(FontWeight::BOLD).child("Đặt tên gợi nhớ"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(3.0))
                    .text_size(px(12.5))
                    .child(div().text_center().child(format!("Hãy đặt cho {shown} một cái tên dễ nhớ.")))
                    .child(div().text_color(theme.secondary()).child("Lưu ý: Tên gợi nhớ sẽ chỉ hiển thị riêng với bạn.")),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(4.0))
                    .child(div().w_full().px(px(12.0)).py(px(8.0)).rounded(px(12.0)).bg(with_alpha(theme.label(), 0.06)).child(Input::new(&self.input).appearance(false).text_size(px(13.5))))
                    .child(div().text_size(px(11.0)).text_color(theme.tertiary()).child(format!("{}/{}", text.encode_utf16().count(), self.draft.max))),
            )
            .when_some(self.error.clone(), |el, e| el.child(div().text_center().text_size(px(12.0)).text_color(theme.orange()).child(e)))
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(button("al-cancel", "Hủy", ButtonStyle::Glass, self.saving, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |_, cx| cx.emit(CardSheetEvent::Close));
                    }))
                    .child(button("al-save", if self.saving { "Đang lưu…" } else if removes { "Xoá tên gợi nhớ" } else { "Xác nhận" }, ButtonStyle::Prominent, self.saving || nothing, &theme, move |_, _, cx| {
                        let _ = me2.update(cx, |this, cx| this.save(cx));
                    })),
            )
    }
}

// MARK: Group invite

pub struct GroupInviteSheet {
    conversation: String,
    url: String,
    preview: Option<GroupInvitePreview>,
    joining: bool,
    error: Option<String>,
}

impl EventEmitter<CardSheetEvent> for GroupInviteSheet {}

impl GroupInviteSheet {
    pub fn new(conversation: String, url: String, cx: &mut Context<Self>) -> Self {
        let (e, u, conv) = (engine(cx), url.clone(), conversation.clone());
        cx.spawn(async move |this, cx| {
            let p = e.group_invite_preview(&u).await;
            let still = e.state.borrow().open_conversation_id.as_deref() == Some(conv.as_str());
            let _ = this.update(cx, |this, cx| {
                if !still {
                    return;
                }
                if p.state == InviteState::Member {
                    e.open_contact_conversation(&p.group_id);
                    cx.emit(CardSheetEvent::Close);
                    return;
                }
                this.preview = Some(p);
                cx.notify();
            });
        })
        .detach();
        GroupInviteSheet { conversation, url, preview: None, joining: false, error: None }
    }

    fn join(&mut self, cx: &mut Context<Self>) {
        if self.joining {
            return;
        }
        self.joining = true;
        self.error = None;
        let (e, u, conv) = (engine(cx), self.url.clone(), self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let r = e.join_group(&u).await;
            let _ = this.update(cx, |this, cx| {
                this.joining = false;
                match r.state {
                    JoinState::Joined | JoinState::Already => {
                        if e.state.borrow().open_conversation_id.as_deref() == Some(conv.as_str()) {
                            e.open_contact_conversation(&r.group_id);
                        }
                        cx.emit(CardSheetEvent::Close);
                    }
                    JoinState::Pending => {
                        e.show_toast(&r.text, Some(&conv));
                        cx.emit(CardSheetEvent::Close);
                    }
                    JoinState::Error => this.error = Some(r.text),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for GroupInviteSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let community = self.preview.as_ref().is_some_and(|p| p.community);
        let mut s = sheet(&theme, 360.0).child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(if community { "Thông tin cộng đồng" } else { "Thông tin nhóm" }));
        match self.preview.clone() {
            None => s = s.child(div().py(px(40.0)).child(spinner(13.0, theme.secondary()))),
            Some(p) if p.state == InviteState::Preview => {
                let mut sub = format!("{} thành viên", p.total_member);
                if !p.creator_name.is_empty() {
                    sub += &format!(" • Tạo bởi {}", p.creator_name);
                }
                s = s
                    .child(avatar(p.avatar.as_deref(), &p.name, 84.0, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(4.0))
                            .child(div().text_center().text_size(px(17.0)).font_weight(FontWeight::BOLD).line_clamp(2).child(p.name.clone()))
                            .child(div().text_center().text_size(px(12.5)).text_color(theme.secondary()).child(sub)),
                    )
                    .child(div().text_center().text_size(px(12.5)).text_color(if p.desc.is_empty() { theme.secondary() } else { theme.label() }).line_clamp(4).child(if !p.desc.is_empty() {
                        p.desc.clone()
                    } else if community {
                        "Bạn đang ở phòng chờ. Hãy tham gia chat để cùng trò chuyện với mọi người trong cộng đồng.".into()
                    } else {
                        "Bạn đang ở phòng chờ. Hãy tham gia chat để cùng trò chuyện với mọi người trong nhóm.".into()
                    }));
                if !p.members.is_empty() {
                    let mut list = div().id("invite-members").w_full().max_h(px(150.0)).overflow_y_scroll().flex().flex_col().gap(px(8.0));
                    for m in &p.members {
                        list = list.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(avatar(m.avatar.as_deref(), &m.name, 28.0, cx))
                                .child(div().text_size(px(13.0)).child(if m.name.is_empty() { "Người dùng Zalo".to_string() } else { m.name.clone() }))
                                .when(m.admin, |el| el.child(icon("key.fill", 10.0, theme.secondary()))),
                        );
                    }
                    s = s.child(div().w_full().h(px(1.0)).bg(theme.separator())).child(list);
                }
            }
            Some(p) => s = s.child(div().text_center().text_size(px(12.5)).text_color(theme.secondary()).child(p.text.clone())),
        }
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        let can_join = self.preview.as_ref().is_some_and(|p| p.state == InviteState::Preview);
        s.when_some(self.error.clone(), |el, e| el.child(div().text_center().text_size(px(12.0)).text_color(theme.orange()).child(e))).child(
            div()
                .flex()
                .gap(px(10.0))
                .child(button("gi-close", "Đóng", ButtonStyle::Glass, self.joining, &theme, move |_, _, cx| {
                    let _ = me.update(cx, |_, cx| cx.emit(CardSheetEvent::Close));
                }))
                .when(can_join, |el| {
                    el.child(button("gi-join", if self.joining { "Đang tham gia…" } else { "Tham gia" }, ButtonStyle::Prominent, self.joining, &theme, move |_, _, cx| {
                        let _ = me2.update(cx, |this, cx| this.join(cx));
                    }))
                }),
        )
    }
}

// MARK: Sticker set

pub struct StickerCategorySheet {
    conversation: String,
    category: Option<StickerCategory>,
    loading: bool,
    sending: Option<String>,
    error: Option<String>,
}

impl EventEmitter<CardSheetEvent> for StickerCategorySheet {}

impl StickerCategorySheet {
    pub fn new(conversation: String, cate_id: i64, cx: &mut Context<Self>) -> Self {
        let e = engine(cx);
        cx.spawn(async move |this, cx| {
            for attempt in 0..3 {
                if let Some(c) = e.sticker_category(cate_id).await {
                    let _ = this.update(cx, |this, cx| {
                        this.category = Some(c);
                        this.loading = false;
                        cx.notify();
                    });
                    return;
                }
                if attempt < 2 {
                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                cx.notify();
            });
        })
        .detach();
        StickerCategorySheet { conversation, category: None, loading: true, sending: None, error: None }
    }
}

impl Render for StickerCategorySheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let me = cx.entity().downgrade();
        let title = self.category.as_ref().map(|c| c.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Bộ sticker".into());
        let body: AnyElement = match self.category.clone() {
            Some(c) => {
                let mut grid = div().p(px(14.0)).flex().flex_wrap().gap(px(8.0));
                for s in &c.stickers {
                    let (sid, r, cid) = (s.sticker_id(), s.r#ref.clone(), c.id);
                    let busy = self.sending.as_deref() == Some(s.r#ref.as_str());
                    grid = grid.child(
                        div()
                            .id(SharedString::from(format!("cat-{}", s.r#ref)))
                            .size(px(72.0))
                            .rounded(px(10.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|st| st.bg(theme.hover()))
                            .when(busy, |el| el.opacity(0.4))
                            .child(remote_picture(Some(&s.url), 64.0, 64.0, 0.0, ObjectFit::Contain, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let e = engine(cx);
                                if this.sending.is_some() || e.state.borrow().open_conversation_id.as_deref() != Some(this.conversation.as_str()) {
                                    return;
                                }
                                this.sending = Some(r.clone());
                                this.error = None;
                                let conv = this.conversation.clone();
                                cx.spawn(async move |this, cx| {
                                    let o = e.send_category_sticker(&conv, cid, sid).await;
                                    let _ = this.update(cx, |this, cx| {
                                        this.sending = None;
                                        match o {
                                            CardActionOutcome::Done => cx.emit(CardSheetEvent::Close),
                                            CardActionOutcome::Locked(t) | CardActionOutcome::Failed(t) | CardActionOutcome::Unsupported(t) | CardActionOutcome::Pending(t) => this.error = Some(t),
                                            _ => {}
                                        }
                                        cx.notify();
                                    });
                                })
                                .detach();
                                cx.notify();
                            })),
                    );
                }
                div().id("cat-grid").flex_1().min_h(px(0.0)).overflow_y_scroll().child(grid).into_any_element()
            }
            None if self.loading => div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element(),
            None => div().flex_1().flex().items_center().justify_center().text_size(px(12.5)).text_color(theme.secondary()).child("Không tải được bộ sticker này. Hãy thử lại sau.").into_any_element(),
        };
        div()
            .id("cat-sheet")
            .relative()
            .child(crate::widgets::occluder())
            .w(px(420.0))
            .h(px(480.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .p(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .border_b_1()
                    .border_color(theme.separator())
                    .when_some(self.category.as_ref().and_then(|c| c.icon.clone()), |el, i| el.child(remote_picture(Some(&i), 28.0, 28.0, 0.0, ObjectFit::Contain, cx)))
                    .child(div().flex_1().truncate().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(button("cat-close", "Đóng", ButtonStyle::Glass, false, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |_, cx| cx.emit(CardSheetEvent::Close));
                    })),
            )
            .child(body)
            .when_some(self.error.clone(), |el, e| el.child(div().p(px(10.0)).text_center().text_size(px(12.0)).text_color(theme.orange()).child(e)))
    }
}

/// One card sheet presented by the chat.
pub enum CardSheetView {
    Profile(Entity<ProfileSheet>),
    Alias(Entity<AliasSheet>),
    GroupInvite(Entity<GroupInviteSheet>),
    Stickers(Entity<StickerCategorySheet>),
    Reminder(Entity<crate::reminders::ReminderDetailSheet>),
}

impl CardSheetView {
    pub fn element(&self) -> AnyElement {
        match self {
            CardSheetView::Profile(e) => e.clone().into_any_element(),
            CardSheetView::Alias(e) => e.clone().into_any_element(),
            CardSheetView::GroupInvite(e) => e.clone().into_any_element(),
            CardSheetView::Stickers(e) => e.clone().into_any_element(),
            CardSheetView::Reminder(e) => e.clone().into_any_element(),
        }
    }
}

/// Builds the sheet for a request; `on_close` is wired to its Close event by the caller.
pub fn build<T: 'static>(r: CardRequest, window: &mut Window, cx: &mut Context<T>, on_close: impl Fn(&mut T, &mut Context<T>) + Clone + 'static) -> CardSheetView {
    match r {
        CardRequest::Profile(p) => {
            let e = cx.new(|cx| ProfileSheet::new(p, window, cx));
            let f = on_close.clone();
            cx.subscribe(&e, move |this, _, _: &CardSheetEvent, cx| f(this, cx)).detach();
            CardSheetView::Profile(e)
        }
        CardRequest::Alias { conversation, draft } => {
            let e = cx.new(|cx| AliasSheet::new(conversation, draft, window, cx));
            let f = on_close.clone();
            cx.subscribe(&e, move |this, _, _: &CardSheetEvent, cx| f(this, cx)).detach();
            CardSheetView::Alias(e)
        }
        CardRequest::GroupInvite { conversation, url } => {
            let e = cx.new(|cx| GroupInviteSheet::new(conversation, url, cx));
            let f = on_close.clone();
            cx.subscribe(&e, move |this, _, _: &CardSheetEvent, cx| f(this, cx)).detach();
            CardSheetView::GroupInvite(e)
        }
        CardRequest::Stickers { conversation, cate_id } => {
            let e = cx.new(|cx| StickerCategorySheet::new(conversation, cate_id, cx));
            let f = on_close.clone();
            cx.subscribe(&e, move |this, _, _: &CardSheetEvent, cx| f(this, cx)).detach();
            CardSheetView::Stickers(e)
        }
        CardRequest::Reminder { conversation, message, open_list } => {
            let e = cx.new(|cx| crate::reminders::ReminderDetailSheet::new(conversation, message, open_list, cx));
            cx.subscribe(&e, move |this, _, _: &crate::reminders::ReminderSheetEvent, cx| on_close(this, cx)).detach();
            CardSheetView::Reminder(e)
        }
    }
}

/// Drops pending card state when another conversation opens.
pub fn reset() {
    CENTER.with(|c| {
        let mut c = c.borrow_mut();
        c.request = None;
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn utf16_prefix() {
        assert_eq!(super::prefix_utf16("ab😀c", 3), "ab");
        assert_eq!(super::prefix_utf16("ab😀c", 4), "ab😀");
        assert_eq!(super::key("c", "m", "r"), "c|m|r");
    }
}
