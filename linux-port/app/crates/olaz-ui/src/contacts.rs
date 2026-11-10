//! "Danh bạ" (ContactsView.swift, ContactsRequestsView.swift): friends, groups, friend requests and add friend, fed by
//! `engine.contacts`. A list column plus a card in the detail column; every action goes through the official services.

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, list, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use olaz_engine::contacts::{FoundUser, FriendRequest, SearchStatus, fold};

use crate::app::engine;
use crate::chrome::{self, TITLEBAR_HEIGHT};
use crate::icon::icon;
use crate::shell::splice_keys;
use crate::theme::Theme;
use crate::widgets::{ButtonStyle, avatar, button, content_unavailable, spinner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactsKind {
    Friends,
    Groups,
    Requests,
    AddFriend,
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Section(String),
    Friend(String),
    Group(String),
    Request(String, bool),
}

impl Item {
    fn key(&self) -> String {
        match self {
            Item::Section(s) => format!("s:{s}"),
            Item::Friend(id) => format!("f:{id}"),
            Item::Group(id) => format!("g:{id}"),
            Item::Request(id, r) => format!("r:{id}:{r}"),
        }
    }
}

/// "5 phút trước" style text.
pub fn ago(ms: f64, now_ms: f64) -> String {
    let s = ((now_ms - ms) / 1000.0).max(0.0);
    if s < 60.0 {
        return "vừa xong".into();
    }
    let (n, unit) = if s < 3600.0 {
        (s / 60.0, "phút")
    } else if s < 86_400.0 {
        (s / 3600.0, "giờ")
    } else if s < 30.0 * 86_400.0 {
        (s / 86_400.0, "ngày")
    } else if s < 365.0 * 86_400.0 {
        (s / (30.0 * 86_400.0), "tháng")
    } else {
        (s / (365.0 * 86_400.0), "năm")
    };
    format!("{} {unit} trước", n.floor() as i64)
}

pub struct ContactsView {
    pub kind: ContactsKind,
    search: Entity<InputState>,
    query: String,
    phone: Entity<InputState>,
    message: Entity<TextareaState>,
    message_for: Option<String>,
    pub selection: Option<String>,
    list_state: ListState,
    keys: Vec<String>,
    items: Vec<Item>,
    busy: std::collections::HashSet<String>,
    sending: bool,
    last_notice: Option<i64>,
}

impl ContactsView {
    pub fn new(kind: ContactsKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(if kind == ContactsKind::Groups { "Tìm nhóm" } else { "Tìm bạn" }));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        let phone = cx.new(|cx| InputState::new(window, cx).placeholder("Số điện thoại"));
        cx.subscribe(&phone, |this, _, ev: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.find(cx);
            }
            cx.notify();
        })
        .detach();
        let message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 3));
        cx.subscribe(&message, |_, _, _: &InputEvent, cx| cx.notify()).detach();
        if kind == ContactsKind::Requests {
            engine(cx).refresh_contacts();
        }
        if kind == ContactsKind::AddFriend {
            phone.update(cx, |s, cx| s.focus(window, cx));
        }
        ContactsView {
            kind,
            search,
            query: String::new(),
            phone,
            message,
            message_for: None,
            selection: None,
            list_state: ListState::new(0, ListAlignment::Top, px(400.0)),
            keys: vec![],
            items: vec![],
            busy: Default::default(),
            sending: false,
            last_notice: None,
        }
    }

    fn find(&mut self, cx: &mut Context<Self>) {
        let phone = self.phone.read(cx).value().to_string();
        engine(cx).find_friend_by_phone(&phone);
    }

    fn build_items(&self, cx: &App) -> Vec<Item> {
        let e = engine(cx);
        let q = fold(self.query.trim());
        match self.kind {
            ContactsKind::Friends => {
                let mut friends: Vec<_> = e.visible_friends().into_iter().filter(|f| q.is_empty() || f.sort_key.contains(&q) || fold(&f.zalo_name).contains(&q)).collect();
                friends.sort_by(|a, b| {
                    let ka = (a.section == "#", a.section.clone(), a.sort_key.clone());
                    let kb = (b.section == "#", b.section.clone(), b.sort_key.clone());
                    ka.cmp(&kb)
                });
                let mut out = vec![];
                let mut current = String::new();
                for f in friends {
                    if f.section != current {
                        current = f.section.clone();
                        out.push(Item::Section(current.clone()));
                    }
                    out.push(Item::Friend(f.id));
                }
                out
            }
            ContactsKind::Groups => {
                let mut groups: Vec<_> = e.visible_groups().into_iter().filter(|g| q.is_empty() || g.sort_key.contains(&q)).collect();
                groups.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
                groups.into_iter().map(|g| Item::Group(g.id)).collect()
            }
            ContactsKind::Requests => {
                let s = e.state.borrow();
                let received = &s.contacts.received;
                let sent: Vec<&FriendRequest> = s.contacts.sent.iter().filter(|r| !received.iter().any(|x| x.id == r.id)).collect();
                let mut out = vec![];
                if !received.is_empty() {
                    out.push(Item::Section("Đã nhận".into()));
                    out.extend(received.iter().map(|r| Item::Request(r.id.clone(), true)));
                }
                if !sent.is_empty() {
                    out.push(Item::Section("Đã gửi".into()));
                    out.extend(sent.iter().map(|r| Item::Request(r.id.clone(), false)));
                }
                out
            }
            ContactsKind::AddFriend => vec![],
        }
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selection = Some(id.to_string());
        cx.notify();
    }

    fn render_item(&mut self, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let row = |id: &str, selected: bool| {
            div()
                .id(SharedString::from(format!("c-{id}")))
                .mx(px(10.0))
                .w(px(320.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(10.0))
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(10.0))
                .when(selected, |el| el.bg(theme.selection_tint()))
                .when(!selected, |el| el.hover(|s| s.bg(theme.hover())))
        };
        match item {
            Item::Section(t) => div().px(px(20.0)).pt(px(10.0)).pb(px(4.0)).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(t.clone()).into_any_element(),
            Item::Friend(id) => {
                let Some(f) = e.state.borrow().contacts.friends.iter().find(|f| &f.id == id).cloned() else { return div().into_any_element() };
                let selected = self.selection.as_deref() == Some(id.as_str());
                let (id1, id2) = (id.clone(), id.clone());
                row(id, selected)
                    .child(online_avatar(f.avatar.as_deref(), &f.name, 36.0, f.online, &theme, cx))
                    .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(f.name.clone()))
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            engine(cx).open_contact_conversation(&id1);
                        } else {
                            this.select(&id1, cx);
                        }
                    }))
                    .context_menu_friend(id2)
                    .into_any_element()
            }
            Item::Group(id) => {
                let Some(g) = e.state.borrow().contacts.groups.iter().find(|g| &g.id == id).cloned() else { return div().into_any_element() };
                let selected = self.selection.as_deref() == Some(id.as_str());
                let id1 = id.clone();
                row(id, selected)
                    .child(avatar(g.avatar.as_deref(), &g.name, 36.0, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(g.name.clone()))
                            .child(div().text_size(px(11.5)).text_color(theme.secondary()).child(format!("{} thành viên", g.members))),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            engine(cx).open_contact_conversation(&id1);
                        } else {
                            this.select(&id1, cx);
                        }
                    }))
                    .into_any_element()
            }
            Item::Request(id, received) => {
                let r = {
                    let s = e.state.borrow();
                    let list = if *received { &s.contacts.received } else { &s.contacts.sent };
                    list.iter().find(|r| &r.id == id).cloned()
                };
                let Some(r) = r else { return div().into_any_element() };
                let selected = self.selection.as_deref() == Some(id.as_str());
                let id1 = id.clone();
                let now = olaz_engine::chat::now_ms();
                row(id, selected)
                    .child(avatar(r.avatar.as_deref(), &r.name, 40.0, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.0))
                                    .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(r.name.clone()))
                                    .when_some(r.date, |el, d| el.child(div().text_size(px(10.5)).text_color(theme.tertiary()).child(ago(d, now)))),
                            )
                            .when(!r.message.is_empty(), |el| el.child(div().truncate().text_size(px(12.0)).text_color(theme.secondary()).child(r.message.clone()))),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(&id1, cx)))
                    .into_any_element()
            }
        }
    }

    fn title(&self, cx: &App) -> (&'static str, String) {
        let e = engine(cx);
        match self.kind {
            ContactsKind::Friends => ("Bạn bè", format!("{} bạn bè", e.visible_friends().len())),
            ContactsKind::Groups => ("Nhóm và cộng đồng", format!("{} nhóm", e.visible_groups().len())),
            ContactsKind::Requests => ("Lời mời kết bạn", format!("{} lời mời đang chờ", e.state.borrow().contacts.pending_request_count)),
            ContactsKind::AddFriend => ("Thêm bạn", String::new()),
        }
    }

    fn render_list_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let (title, subtitle) = self.title(cx);
        let items = self.build_items(cx);
        let keys: Vec<String> = items.iter().map(Item::key).collect();
        if keys != self.keys {
            splice_keys(&self.list_state, &self.keys, &keys);
            self.keys = keys;
        }
        self.items = items;
        let (friends_loaded, groups_loaded, hidden_known, loading_requests) = {
            let s = e.state.borrow();
            (s.contacts.friends_loaded, s.contacts.groups_loaded, s.conversation_actions.hidden_known, s.contacts.loading_requests)
        };
        let header = chrome::drag_area("contacts-toolbar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .px(px(16.0))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(15.0)).font_weight(FontWeight::BOLD).child(title))
                    .when(!subtitle.is_empty(), |el| el.child(div().text_size(px(11.0)).text_color(theme.secondary()).child(subtitle))),
            )
            .when(self.kind == ContactsKind::Groups, |el| {
                el.child(
                    div()
                        .id("contacts-new-group")
                        .size(px(26.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(theme.hover()))
                        .child(icon("person.2.badge.plus", 14.0, theme.accent))
                        .on_click(|_, _, cx| crate::group_create::request(vec![], cx)),
                )
            })
            .when(self.kind == ContactsKind::Requests, |el| {
                el.child(if loading_requests {
                    spinner(13.0, theme.secondary()).into_any_element()
                } else {
                    div()
                        .id("contacts-refresh")
                        .size(px(26.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(theme.hover()))
                        .child(icon("arrow.clockwise", 13.0, theme.secondary()))
                        .on_click(|_, _, cx| engine(cx).refresh_contacts())
                        .into_any_element()
                })
            });
        let search_bar = matches!(self.kind, ContactsKind::Friends | ContactsKind::Groups).then(|| {
            div().px(px(12.0)).pb(px(8.0)).child(
                div()
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded_full()
                    .bg(theme.glass())
                    .border_1()
                    .border_color(theme.glass_border())
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon("magnifyingglass", 12.5, theme.secondary()))
                    .child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(13.0)))),
            )
        });
        let body: AnyElement = match self.kind {
            ContactsKind::AddFriend => self.render_add_friend_form(&theme, cx),
            ContactsKind::Friends if !friends_loaded || !hidden_known => loading("Đang tải danh sách bạn bè…", &theme),
            ContactsKind::Groups if !groups_loaded || !hidden_known => loading("Đang tải danh sách nhóm…", &theme),
            ContactsKind::Requests if self.items.is_empty() && loading_requests => loading("Đang tải lời mời…", &theme),
            _ if self.items.is_empty() => {
                let (sym, t, d): (&str, String, String) = if !self.query.trim().is_empty() {
                    ("magnifyingglass", format!("Không có kết quả cho “{}”", self.query.trim()), String::new())
                } else {
                    match self.kind {
                        ContactsKind::Friends => ("person.2", "Chưa có bạn bè".into(), "Tìm bạn bằng số điện thoại để gửi lời mời kết bạn.".into()),
                        ContactsKind::Groups => ("person.2", "Chưa tham gia nhóm nào".into(), "Tạo nhóm để cùng nhau trò chuyện và trao đổi với bạn bè đồng nghiệp".into()),
                        _ => ("person.crop.circle.badge.checkmark", "Không có lời mời mới".into(), "Lời mời kết bạn sẽ hiện ở đây.".into()),
                    }
                };
                div().flex_1().child(content_unavailable(sym, &t, &d, &theme)).into_any_element()
            }
            _ => {
                let items = self.items.clone();
                let me = cx.entity().downgrade();
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        list(self.list_state.clone(), move |ix, _, cx| {
                            let Some(item) = items.get(ix).cloned() else { return div().into_any_element() };
                            me.upgrade().map(|v| v.update(cx, |this, cx| this.render_item(&item, cx))).unwrap_or_else(|| div().into_any_element())
                        })
                        .size_full(),
                    )
                    .into_any_element()
            }
        };
        div()
            .w(px(340.0))
            .flex_none()
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.separator())
            .child(header)
            .children(search_bar)
            .child(body)
            .into_any_element()
    }

    fn render_add_friend_form(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let searching = engine(cx).state.borrow().contacts.search.status == SearchStatus::Searching;
        let empty = self.phone.read(cx).value().trim().is_empty();
        let me = cx.entity().downgrade();
        div()
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(div().px(px(10.0)).py(px(7.0)).rounded_full().bg(theme.glass()).text_size(px(13.0)).font_weight(FontWeight::MEDIUM).child("🇻🇳 +84"))
                    .child(
                        div()
                            .flex_1()
                            .px(px(12.0))
                            .py(px(5.0))
                            .rounded_full()
                            .bg(theme.glass())
                            .border_1()
                            .border_color(theme.glass_border())
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(icon("phone.fill", 12.0, theme.secondary()))
                            .child(div().flex_1().child(Input::new(&self.phone).appearance(false).text_size(px(13.5)))),
                    ),
            )
            .child(button("find-friend", if searching { "Đang tìm…" } else { "Tìm kiếm" }, ButtonStyle::Prominent, empty || searching, theme, move |_, _, cx| {
                let _ = me.update(cx, |this, cx| this.find(cx));
            }))
            .child(div().text_size(px(11.5)).text_color(theme.secondary()).child("Nhập số điện thoại Việt Nam, hoặc số quốc tế bắt đầu bằng +"))
            .into_any_element()
    }

    fn render_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let toolbar = chrome::drag_area("contact-detail-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none();
        let body: AnyElement = match self.kind {
            ContactsKind::Friends => match self.selection.as_ref().and_then(|id| e.state.borrow().contacts.friends.iter().find(|f| &f.id == id).cloned()) {
                Some(f) => {
                    let status = if f.online { Some("Đang hoạt động".to_string()) } else { f.last_active.map(|d| format!("Truy cập {}", ago(d, olaz_engine::chat::now_ms()))) };
                    let id = f.id.clone();
                    card_header(f.avatar.as_deref(), &f.name, Some(&f.zalo_name), status.as_deref(), f.online, &theme, cx)
                        .child(button("friend-message", "Nhắn tin", ButtonStyle::Prominent, false, &theme, move |_, _, cx| engine(cx).open_contact_conversation(&id)))
                        .into_any_element()
                }
                None => content_unavailable("person", "Chọn một người bạn", "Chọn ở danh sách bên cạnh để xem thông tin, nhấn đúp để nhắn tin.", &theme).into_any_element(),
            },
            ContactsKind::Groups => match self.selection.as_ref().and_then(|id| e.state.borrow().contacts.groups.iter().find(|g| &g.id == id).cloned()) {
                Some(g) => {
                    let id = g.id.clone();
                    let status = (g.members > 0).then(|| format!("{} thành viên", g.members));
                    card_header(g.avatar.as_deref(), &g.name, None, status.as_deref(), false, &theme, cx)
                        .child(button("group-open", "Mở trò chuyện", ButtonStyle::Prominent, false, &theme, move |_, _, cx| engine(cx).open_contact_conversation(&id)))
                        .into_any_element()
                }
                None => content_unavailable("person.2", "Chọn một nhóm", "Chọn ở danh sách bên cạnh để xem thông tin, nhấn đúp để mở trò chuyện.", &theme).into_any_element(),
            },
            ContactsKind::Requests => self.render_request_detail(&theme, cx),
            ContactsKind::AddFriend => self.render_found_user(window, &theme, cx),
        };
        div().flex_1().min_w(px(0.0)).h_full().flex().flex_col().child(toolbar).child(div().flex_1().min_h(px(0.0)).flex().flex_col().child(body)).into_any_element()
    }

    fn render_request_detail(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let e = engine(cx);
        let selected = self.selection.as_ref().and_then(|id| {
            let s = e.state.borrow();
            s.contacts.received.iter().find(|r| &r.id == id).map(|r| (r.clone(), true)).or_else(|| s.contacts.sent.iter().find(|r| &r.id == id).map(|r| (r.clone(), false)))
        });
        let Some((r, received)) = selected else {
            let empty = {
                let s = e.state.borrow();
                s.contacts.received.is_empty() && s.contacts.sent.is_empty()
            };
            return if empty {
                content_unavailable("person.crop.circle.badge.checkmark", "Không có lời mời mới", "Lời mời kết bạn sẽ hiện ở đây.", theme).into_any_element()
            } else {
                content_unavailable("person.crop.circle.badge.questionmark", "Chọn một lời mời", "Chọn lời mời ở danh sách bên cạnh để xem và trả lời.", theme).into_any_element()
            };
        };
        let when = r.date.map(|d| ago(d, olaz_engine::chat::now_ms()));
        let status = if received {
            when.map(|w| format!("Gửi lời mời {w}")).unwrap_or_else(|| "Muốn kết bạn với bạn".into())
        } else {
            when.map(|w| format!("Bạn đã gửi lời mời {w}")).unwrap_or_else(|| "Đang chờ người này chấp nhận".into())
        };
        let busy = self.busy.contains(&r.id);
        let me = cx.entity().downgrade();
        let act = move |id: String, which: u8| {
            let me = me.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    this.busy.insert(id.clone());
                    let e = engine(cx);
                    match which {
                        0 => e.accept_friend_request(&id),
                        1 => e.decline_friend_request(&id),
                        _ => e.cancel_friend_request(&id),
                    }
                    cx.notify();
                });
            }
        };
        let mut buttons = div().flex().gap(px(10.0)).items_center();
        buttons = if received {
            buttons
                .child(button("req-decline", "Từ chối", ButtonStyle::Glass, busy, theme, act(r.id.clone(), 1)))
                .child(button("req-accept", "Đồng ý", ButtonStyle::Prominent, busy, theme, act(r.id.clone(), 0)))
        } else {
            buttons.child(button("req-cancel", "Thu hồi lời mời", ButtonStyle::Glass, busy, theme, act(r.id.clone(), 2)))
        };
        if busy {
            buttons = buttons.child(spinner(13.0, theme.secondary()));
        }
        card_header(r.avatar.as_deref(), &r.name, None, Some(&status), false, theme, cx)
            .child(buttons)
            .when(!r.message.is_empty(), |el| {
                el.child(
                    div()
                        .mt(px(16.0))
                        .w(px(380.0))
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Lời nhắn"))
                        .child(div().px(px(12.0)).py(px(8.0)).rounded(px(14.0)).bg(theme.quaternary()).text_size(px(13.0)).child(r.message.clone())),
                )
            })
            .into_any_element()
    }

    fn render_found_user(&mut self, window: &mut Window, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let search = engine(cx).state.borrow().contacts.search.clone();
        match search.status {
            SearchStatus::Idle => content_unavailable("person.crop.circle.badge.plus", "Thêm bạn", "Tìm bạn bằng số điện thoại để gửi lời mời kết bạn.", theme).into_any_element(),
            SearchStatus::Searching => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(spinner(14.0, theme.secondary()))
                .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Đang tìm…"))
                .into_any_element(),
            SearchStatus::Error => content_unavailable("exclamationmark.triangle", "Không tìm thấy", search.error.as_deref().unwrap_or("Không tìm thấy người dùng."), theme).into_any_element(),
            SearchStatus::Found => match search.user {
                Some(u) => self.found_user_card(&u, window, theme, cx),
                None => div().into_any_element(),
            },
        }
    }

    fn found_user_card(&mut self, u: &FoundUser, window: &mut Window, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.message_for.as_deref() != Some(u.id.as_str()) {
            self.message_for = Some(u.id.clone());
            self.sending = false;
            let text = u.default_message.clone();
            self.message.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let mut message = self.message.read(cx).value().to_string();
        if message.chars().count() as i64 > u.max_message {
            message = message.chars().take(u.max_message as usize).collect();
            let m = message.clone();
            self.message.update(cx, |s, cx| s.set_value(m, window, cx));
        }
        let note = if u.is_self {
            Some("Đây là tài khoản của bạn.")
        } else if u.is_friend {
            Some("Đã là bạn bè.")
        } else if u.requesting {
            Some("Người này đã gửi lời mời kết bạn cho bạn. Hãy chấp nhận ở mục “Lời mời kết bạn”.")
        } else if u.requested {
            Some("Bạn đã gửi lời mời kết bạn. Đang chờ người này chấp nhận.")
        } else {
            None
        };
        let bio = (!u.bio.is_empty()).then(|| u.bio.clone());
        let mut card = card_header(u.avatar.as_deref(), &u.name, Some(&u.zalo_name), bio.as_deref(), false, theme, cx);
        if u.is_friend {
            let id = u.id.clone();
            card = card.child(button("found-message", "Nhắn tin", ButtonStyle::Prominent, false, theme, move |_, _, cx| engine(cx).open_contact_conversation(&id)));
        }
        if let Some(note) = note {
            return card.child(div().mt(px(16.0)).text_size(px(13.0)).text_color(theme.secondary()).child(note)).into_any_element();
        }
        let (id, me) = (u.id.clone(), cx.entity().downgrade());
        let can_send = !self.sending && !message.trim().is_empty();
        card.child(
            div()
                .mt(px(16.0))
                .w(px(380.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Lời nhắn"))
                .child(div().p(px(8.0)).rounded(px(10.0)).border_1().border_color(theme.separator()).child(Textarea::new(&self.message).appearance(false).text_size(px(13.0))))
                .child(div().flex().justify_end().text_size(px(11.0)).text_color(theme.tertiary()).child(format!("{}/{}", message.chars().count(), u.max_message)))
                .child(button("send-request", if self.sending { "Đang gửi…" } else { "Gửi lời mời kết bạn" }, ButtonStyle::Prominent, !can_send, theme, move |_, _, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.sending = true;
                        let msg = this.message.read(cx).value().to_string();
                        engine(cx).send_friend_request(&id, &msg);
                        cx.notify();
                    });
                })),
        )
        .into_any_element()
    }
}

fn loading(text: &str, theme: &Theme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .child(spinner(14.0, theme.secondary()))
        .child(div().text_size(px(12.5)).text_color(theme.secondary()).child(text.to_string()))
        .into_any_element()
}

fn online_avatar(url: Option<&str>, name: &str, size: f32, online: bool, theme: &Theme, cx: &mut App) -> AnyElement {
    let dot = size * 0.3;
    div()
        .relative()
        .flex_none()
        .child(avatar(url, name, size, cx))
        .when(online, |el| {
            el.child(div().absolute().right(px(-1.0)).bottom(px(-1.0)).size(px(dot)).rounded_full().bg(theme.green()).border_2().border_color(theme.content_bg()))
        })
        .into_any_element()
}

/// Contacts-style card header: big avatar, name, optional second name and status line; actions are appended by the caller.
fn card_header(url: Option<&str>, name: &str, secondary: Option<&str>, status: Option<&str>, online: bool, theme: &Theme, cx: &mut App) -> gpui::Div {
    div()
        .flex_1()
        .pt(px(40.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(5.0))
        .child(div().pb(px(6.0)).child(online_avatar(url, name, 104.0, online, theme, cx)))
        .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_center().child(name.to_string()))
        .when_some(secondary.filter(|s| !s.is_empty() && *s != name).map(String::from), |el, s| {
            el.child(div().text_size(px(13.0)).text_color(theme.secondary()).child(s))
        })
        .when_some(status.filter(|s| !s.is_empty()).map(String::from), |el, s| {
            el.child(div().pb(px(14.0)).text_size(px(12.5)).text_color(if online { theme.green() } else { theme.secondary() }).child(s))
        })
}

trait FriendMenuExt: Sized {
    fn context_menu_friend(self, id: String) -> AnyElement;
}

impl<E: gpui_component::menu::ContextMenuExt + IntoElement + 'static> FriendMenuExt for E {
    fn context_menu_friend(self, id: String) -> AnyElement {
        self.context_menu(move |menu, _, _| {
            let id = id.clone();
            menu.item(gpui_component::menu::PopupMenuItem::new("Nhắn tin").on_click(move |_, _, cx| engine(cx).open_contact_conversation(&id)))
        })
        .into_any_element()
    }
}

impl Render for ContactsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        // A finished accept / decline / cancel / send clears the busy marks; the notice shows as a toast.
        let notice = engine(cx).state.borrow().contacts.notice.clone();
        let fresh = notice.as_ref().filter(|n| Some(n.seq) != self.last_notice).cloned();
        if let Some(n) = fresh {
            let first = self.last_notice.is_none();
            self.last_notice = Some(n.seq);
            self.busy.clear();
            self.sending = false;
            if !first && !n.text.is_empty() {
                engine(cx).show_conv_toast(
                    if n.ok { olaz_engine::conversation_actions::ToastKind::Success } else { olaz_engine::conversation_actions::ToastKind::Failure },
                    &n.text,
                );
            }
        }
        let list_col = self.render_list_column(cx);
        let detail = self.render_detail(window, cx);
        let r = crate::chrome::inner_radius(window);
        div().size_full().flex().rounded_tr(r).rounded_br(r).bg(theme.content_bg()).child(list_col).child(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_texts() {
        let now = 10_000_000_000.0;
        assert_eq!(ago(now - 30_000.0, now), "vừa xong");
        assert_eq!(ago(now - 5.0 * 60_000.0, now), "5 phút trước");
        assert_eq!(ago(now - 3.0 * 3_600_000.0, now), "3 giờ trước");
        assert_eq!(ago(now - 2.0 * 86_400_000.0, now), "2 ngày trước");
    }
}
