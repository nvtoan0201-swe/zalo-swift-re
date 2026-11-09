//! Chat inspector (port of ChatInspector.swift): who the conversation is, quick actions, members of a group and the shared
//! photos / files / links with a "Xem tất cả" browser by month.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use olaz_engine::conversation_actions::{MenuPreset, PlanPin};
use olaz_engine::models::{ChatMessage, Kind, MentionCandidate};
use olaz_engine::shared_media::{ALL_KINDS, FeedPhase, SharedMediaFeed, SharedMediaKind, merged, months, preview};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{avatar, message_picture, spinner};

pub const PAGE_SIZE: usize = 30;
pub const WIDTH: f32 = 300.0;

thread_local! {
    /// Whether the inspector shows (kept across conversations, like the macOS app).
    pub static SHOWN: Cell<bool> = const { Cell::new(false) };
}

pub fn toggle_shown() {
    SHOWN.with(|s| s.set(!s.get()));
}

pub struct Inspector {
    pub conversation: String,
    members: Option<Vec<MentionCandidate>>,
    feeds: HashMap<SharedMediaKind, SharedMediaFeed>,
    browsing: Option<SharedMediaKind>,
    last_seen: Option<String>,
    _refresh: Option<Task<()>>,
}

impl Inspector {
    pub fn new(conversation: String, cx: &mut Context<Self>) -> Self {
        let mut me = Inspector { conversation, members: None, feeds: HashMap::new(), browsing: None, last_seen: None, _refresh: None };
        me.load_members(cx);
        me
    }

    fn feed(&self, kind: SharedMediaKind) -> SharedMediaFeed {
        self.feeds.get(&kind).cloned().unwrap_or_default()
    }

    fn load_members(&mut self, cx: &mut Context<Self>) {
        let e = engine(cx);
        let conv = self.conversation.clone();
        let is_group = e.state.borrow().conversations.iter().find(|c| c.id == conv).is_some_and(|c| c.is_group);
        if !is_group {
            self.members = Some(vec![]);
            return;
        }
        cx.spawn(async move |this, cx| {
            let found = e.mention_candidates(&conv, "", &[]).await;
            let _ = this.update(cx, |this, cx| {
                this.members = Some(found.into_iter().filter(|m| !m.is_all).collect());
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-reads the first page of every list (quietly once loaded), shortly after the newest message changed.
    fn refresh(&mut self, delay: bool, cx: &mut Context<Self>) {
        let e = engine(cx);
        let conv = self.conversation.clone();
        for kind in ALL_KINDS {
            let f = self.feeds.entry(kind).or_default();
            if !f.loaded {
                f.phase = FeedPhase::Loading;
            }
        }
        self._refresh = Some(cx.spawn(async move |this, cx| {
            if delay {
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
            for kind in ALL_KINDS {
                let page = e.shared_media(&conv, kind, None, PAGE_SIZE).await;
                let _ = this.update(cx, |this, cx| {
                    let f = this.feeds.entry(kind).or_default();
                    match page {
                        Some(p) => f.merge_first_page(p),
                        None if !f.loaded => f.phase = FeedPhase::Failed,
                        None => {}
                    }
                    cx.notify();
                });
            }
        }));
    }

    fn load_more(&mut self, kind: SharedMediaKind, retrying: bool, cx: &mut Context<Self>) {
        let f = self.feed(kind);
        if !f.loaded {
            self.refresh(false, cx);
            return;
        }
        let Some(cursor) = f.cursor.clone() else { return };
        if !f.more || !(f.phase == FeedPhase::Idle || (retrying && f.phase == FeedPhase::Failed)) {
            return;
        }
        self.feeds.entry(kind).or_default().phase = FeedPhase::Loading;
        let (e, conv) = (engine(cx), self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let page = e.shared_media(&conv, kind, Some(&cursor), PAGE_SIZE).await;
            let _ = this.update(cx, |this, cx| {
                let f = this.feeds.entry(kind).or_default();
                match page {
                    Some(p) => f.append(p),
                    None => f.phase = FeedPhase::Failed,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn section(title: &str, trailing: Option<AnyElement>, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .px(px(4.0))
                    .flex()
                    .items_center()
                    .child(div().flex_1().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(title.to_string()))
                    .children(trailing),
            )
    }

    fn list_box(theme: &Theme) -> gpui::Div {
        div().rounded(px(12.0)).bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().flex_col().overflow_hidden()
    }

    fn empty_line(text: &str, theme: &Theme) -> AnyElement {
        div().px(px(12.0)).min_h(px(38.0)).flex().items_center().text_size(px(12.0)).text_color(theme.secondary()).child(text.to_string()).into_any_element()
    }

    fn status_line(&self, kind: SharedMediaKind, empty: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let f = self.feed(kind);
        if f.is_pending() {
            return div().min_h(px(38.0)).flex().items_center().justify_center().child(spinner(12.0, theme.secondary())).into_any_element();
        }
        if f.did_fail() {
            return div()
                .px(px(12.0))
                .min_h(px(38.0))
                .flex()
                .items_center()
                .child(div().flex_1().text_size(px(12.0)).text_color(theme.secondary()).child("Không tải được"))
                .child(
                    div()
                        .id(SharedString::from(format!("retry-{}", kind.raw())))
                        .text_size(px(12.0))
                        .text_color(theme.accent)
                        .child("Thử lại")
                        .on_click(cx.listener(move |this, _, _, cx| this.load_more(kind, true, cx))),
                )
                .into_any_element();
        }
        Self::empty_line(empty, theme)
    }

    fn see_all(&self, kind: SharedMediaKind, has_items: bool, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        (has_items || self.feed(kind).did_fail()).then(|| {
            div()
                .id(SharedString::from(format!("see-all-{}", kind.raw())))
                .text_size(px(12.0))
                .text_color(theme.accent)
                .child("Xem tất cả")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.browsing = Some(kind);
                    cx.notify();
                }))
                .into_any_element()
        })
    }

    fn tile(&self, m: &ChatMessage, side: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let (url, kind) = match &m.kind {
            Kind::Photo { url, .. } => (url.clone(), olaz_engine::media::MediaKind::Photo),
            Kind::Video { thumbnail, .. } => (thumbnail.clone(), olaz_engine::media::MediaKind::Photo),
            _ => (None, olaz_engine::media::MediaKind::Photo),
        };
        let is_video = matches!(m.kind, Kind::Video { .. });
        let full = match &m.kind {
            Kind::Photo { url, .. } => url.clone().map(|u| (u, false)),
            Kind::Video { url, .. } => url.clone().map(|u| (u, true)),
            _ => None,
        };
        div()
            .id(SharedString::from(format!("tile-{}", m.id)))
            .size(px(side))
            .rounded(px(6.0))
            .overflow_hidden()
            .relative()
            .bg(theme.quaternary())
            .child(message_picture(&m.conversation_id, &m.id, url.as_deref(), kind, side, side, 0.0, cx))
            .when(is_video, |el| {
                el.child(div().absolute().inset_0().flex().items_center().justify_center().child(icon("play.circle.fill", side * 0.3, gpui::white())))
            })
            .on_click(move |_, window, cx| match &full {
                Some((u, false)) => crate::viewer::open_photo(u, window, cx),
                Some((u, true)) => crate::viewer::open_video(u, cx),
                None => {}
            })
            .into_any_element()
    }

    fn file_row(m: &ChatMessage, theme: &Theme) -> AnyElement {
        let Kind::File { name, size, url } = &m.kind else { return div().into_any_element() };
        let (name, url) = (name.clone(), url.clone());
        let ext = std::path::Path::new(&name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        div()
            .id(SharedString::from(format!("file-{}", m.id)))
            .px(px(12.0))
            .py(px(7.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .hover(|s| s.bg(theme.hover()))
            .child(
                div()
                    .size(px(30.0))
                    .rounded(px(7.0))
                    .bg(theme.accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(8.5))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.white())
                    .child(ext.chars().take(4).collect::<String>()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(12.5)).child(name.clone()))
                    .child(div().text_size(px(11.0)).text_color(theme.secondary()).child(format!("{} · {}", crate::format::bytes(*size as u64), crate::format::hhmm(m.date)))),
            )
            .on_click(move |_, _, cx| {
                if let Some(u) = &url {
                    crate::viewer::open_file(u, &name, cx)
                }
            })
            .into_any_element()
    }

    fn link_row(m: &ChatMessage, theme: &Theme) -> AnyElement {
        let Kind::Link { title, url, .. } = &m.kind else { return div().into_any_element() };
        let url = url.clone().unwrap_or_default();
        let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        let title = if title.is_empty() { url.clone() } else { title.clone() };
        div()
            .id(SharedString::from(format!("link-{}", m.id)))
            .px(px(12.0))
            .py(px(7.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .hover(|s| s.bg(theme.hover()))
            .child(div().size(px(22.0)).flex().items_center().justify_center().child(icon("link", 14.0, theme.accent)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(12.5)).child(title))
                    .child(div().truncate().text_size(px(11.0)).text_color(theme.secondary()).child(host)),
            )
            .on_click(move |_, _, cx| engine(cx).open_url(&url))
            .into_any_element()
    }

    fn render_overview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let id = self.conversation.clone();
        let (conv, self_id, loaded, friend, friends_loaded) = {
            let s = e.state.borrow();
            (
                s.conversations.iter().find(|c| c.id == id).cloned(),
                s.self_conversation_id.clone(),
                s.messages.get(&id).cloned().unwrap_or_default(),
                s.contacts.friends.iter().any(|f| f.id == id),
                s.contacts.friends_loaded,
            )
        };
        let is_self = self_id.as_deref() == Some(id.as_str());
        let is_group = conv.as_ref().is_some_and(|c| c.is_group);
        let name = crate::chat::conversation_name(&id, cx);
        let subtitle = if is_self {
            Some("Chỉ mình bạn thấy".to_string())
        } else if is_group {
            Some(conv.as_ref().filter(|c| c.members > 0).map(|c| format!("{} thành viên", c.members)).unwrap_or_else(|| "Nhóm".into()))
        } else if friend {
            Some("Bạn bè".into())
        } else if friends_loaded {
            Some("Chưa là bạn".into())
        } else {
            None
        };
        let av = if is_self {
            div().size(px(76.0)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(icon("cloud.fill", 32.0, theme.white())).into_any_element()
        } else {
            avatar(conv.as_ref().and_then(|c| c.avatar.as_deref()), &name, 76.0, cx)
        };
        let muted = e.is_conversation_muted(&id);
        let state_loaded = e.state.borrow().conversation_actions.loaded;
        let plan = e.conversation_menu_plan(&id, MenuPreset::ChatBox);
        let slot = |sid: &str, glyph: &str, title: &str| {
            div()
                .min_w(px(58.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(5.0))
                .child(
                    div()
                        .id(SharedString::from(sid.to_string()))
                        .size(px(40.0))
                        .rounded_full()
                        .bg(theme.glass())
                        .border_1()
                        .border_color(theme.glass_border())
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(theme.hover()))
                        .child(icon(glyph, 16.0, theme.label())),
                )
                .child(div().text_size(px(10.5)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(title.to_string()))
        };
        let mut actions = div().flex().justify_center().gap(px(18.0));
        if !state_loaded || plan.mute.is_some() {
            if muted {
                let id = id.clone();
                actions = actions.child(slot("insp-unmute", "bell", "Bật thông báo").on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| engine(cx).unmute_conversation(&id)));
            } else {
                let id = id.clone();
                actions = actions.child(
                    div().flex().flex_col().items_center().gap(px(5.0)).min_w(px(58.0)).child(
                        gpui_component::button::Button::new("insp-mute")
                            .child(icon("bell.slash", 16.0, theme.label()))
                            .dropdown_menu(move |mut menu, _, _| {
                                for d in olaz_engine::notifications::MUTE_DURATIONS {
                                    let id = id.clone();
                                    menu = menu.item(PopupMenuItem::new(d.title()).on_click(move |_, _, cx| engine(cx).mute_conversation(&id, d)));
                                }
                                menu
                            }),
                    ).child(div().text_size(px(10.5)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child("Tắt thông báo")),
                );
            }
        }
        match plan.pin {
            Some(PlanPin::Pin) => {
                let id = id.clone();
                actions = actions.child(slot("insp-pin", "pin", "Ghim").on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| engine(cx).pin_conversation(&id)));
            }
            Some(PlanPin::Unpin) => {
                let id = id.clone();
                actions = actions.child(slot("insp-unpin", "pin.slash", "Bỏ ghim").on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| engine(cx).unpin_conversation(&id)));
            }
            None => {}
        }
        if !is_group && !is_self && friend {
            let id = id.clone();
            actions = actions.child(slot("insp-group", "person.2.badge.plus", "Tạo nhóm").on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| crate::group_create::request(vec![id.clone()], cx)));
        }
        let mut body = div()
            .px(px(16.0))
            .pt(px(40.0))
            .pb(px(20.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .pt(px(14.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(av)
                    .child(div().text_center().text_size(px(17.0)).font_weight(FontWeight::BOLD).child(name))
                    .when_some(subtitle, |el, s| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(s))),
            )
            .child(actions);
        if is_group {
            // Bảng tin nhóm: create a poll, and the polls in the loaded messages.
            let polls = crate::polls::loaded_polls(&loaded);
            let mut board = Self::list_box(&theme).child({
                let conv = id.clone();
                div()
                    .id("insp-poll-create")
                    .px(px(12.0))
                    .min_h(px(38.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .hover(|s| s.bg(theme.hover()))
                    .child(icon("plus.circle", 15.0, theme.accent))
                    .child(div().text_size(px(13.0)).child("Tạo bình chọn"))
                    .on_click(move |_, _, cx| crate::polls::request(crate::polls::PollRequest::Create { conversation: conv.clone() }, cx))
            });
            for (pid, q) in polls.iter().take(5) {
                let (conv, pid2, q2) = (id.clone(), pid.clone(), q.clone());
                board = board.child(
                    div()
                        .id(SharedString::from(format!("insp-poll-{pid}")))
                        .px(px(12.0))
                        .min_h(px(38.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .hover(|s| s.bg(theme.hover()))
                        .child(icon("chart.bar.fill", 14.0, theme.accent))
                        .child(div().flex_1().line_clamp(2).text_size(px(13.0)).child(if q.is_empty() { "Bình chọn".to_string() } else { q.clone() }))
                        .on_click(move |_, _, cx| {
                            crate::polls::request(crate::polls::PollRequest::Detail { poll_id: pid2.clone(), conversation: conv.clone(), question: q2.clone(), voters: false }, cx)
                        }),
                );
            }
            if polls.is_empty() {
                board = board.child(Self::empty_line("Chưa có bình chọn trong nội dung đã tải", &theme));
            }
            body = body.child(Self::section("Bảng tin nhóm", None, &theme).child(board));
            let members = self.members.clone();
            let total = conv.as_ref().map(|c| c.members).unwrap_or(0);
            let section = Self::section("Thành viên", (total > 0).then(|| div().text_size(px(12.0)).text_color(theme.tertiary()).child(total.to_string()).into_any_element()), &theme);
            body = body.child(match members {
                None => section.child(div().min_h(px(40.0)).flex().items_center().justify_center().child(spinner(12.0, theme.secondary()))),
                Some(list) if list.is_empty() => section.child(Self::list_box(&theme).child(Self::empty_line("Không đọc được danh sách thành viên", &theme))),
                Some(list) => {
                    let mut b = Self::list_box(&theme);
                    for m in list.iter().take(8) {
                        let mid = m.id.clone();
                        b = b.child(
                            div()
                                .id(SharedString::from(format!("member-{}", m.id)))
                                .px(px(12.0))
                                .min_h(px(38.0))
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .hover(|s| s.bg(theme.hover()))
                                .child(avatar(m.avatar.as_deref(), &m.name, 26.0, cx))
                                .child(div().truncate().text_size(px(13.0)).child(m.name.clone()))
                                .on_click(move |_, _, cx| engine(cx).open_contact_conversation(&mid)),
                        );
                    }
                    if total > list.len().min(8) as i64 {
                        b = b.child(Self::empty_line(&format!("và {} thành viên khác", total - list.len().min(8) as i64), &theme));
                    }
                    section.child(b)
                }
            });
        }
        // Photos and videos
        let images = preview(SharedMediaKind::Images, &self.feed(SharedMediaKind::Images).items, &loaded);
        let see = self.see_all(SharedMediaKind::Images, !images.is_empty(), &theme, cx);
        let mut sec = Self::section("Ảnh và video", see, &theme);
        if images.is_empty() {
            sec = sec.child(Self::list_box(&theme).child(self.status_line(SharedMediaKind::Images, "Chưa có ảnh hoặc video", &theme, cx)));
        } else {
            let side = ((WIDTH - 32.0 - 8.0) / 3.0).floor();
            let mut grid = div().flex().flex_wrap().gap(px(4.0));
            for m in &images {
                grid = grid.child(self.tile(m, side, cx));
            }
            sec = sec.child(grid);
        }
        body = body.child(sec);
        for (kind, title, empty) in [(SharedMediaKind::Files, "Tệp", "Chưa có tệp"), (SharedMediaKind::Links, "Liên kết", "Chưa có liên kết")] {
            let items = preview(kind, &self.feed(kind).items, &loaded);
            let see = self.see_all(kind, !items.is_empty(), &theme, cx);
            let mut b = Self::list_box(&theme);
            if items.is_empty() {
                b = b.child(self.status_line(kind, empty, &theme, cx));
            } else {
                for m in &items {
                    b = b.child(if kind == SharedMediaKind::Files { Self::file_row(m, &theme) } else { Self::link_row(m, &theme) });
                }
            }
            body = body.child(Self::section(title, see, &theme).child(b));
        }
        div().id("inspector-overview").size_full().overflow_y_scroll().child(body).into_any_element()
    }

    fn render_browser(&mut self, kind: SharedMediaKind, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let loaded = e.state.borrow().messages.get(&self.conversation).cloned().unwrap_or_default();
        let f = self.feed(kind);
        let items = merged(&f.items, &loaded, kind, f.loaded_since());
        let mut tabs = div().flex().gap(px(4.0)).p(px(3.0)).rounded_full().bg(theme.glass());
        for k in ALL_KINDS {
            let sel = k == kind;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("browse-{}", k.raw())))
                    .flex_1()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded_full()
                    .text_center()
                    .text_size(px(12.0))
                    .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                    .child(k.title())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.browsing = Some(k);
                        cx.notify();
                    })),
            );
        }
        let mut list = div().flex().flex_col().gap(px(10.0)).px(px(16.0)).pb(px(16.0));
        if items.is_empty() {
            list = list.child(Self::list_box(&theme).child(self.status_line(kind, "Chưa có nội dung", &theme, cx)));
        }
        let side = ((WIDTH - 32.0 - 8.0) / 3.0).floor();
        for month in months(&items) {
            list = list.child(div().pt(px(4.0)).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(month.title.clone()));
            if kind == SharedMediaKind::Images {
                let mut grid = div().flex().flex_wrap().gap(px(4.0));
                for m in &month.items {
                    grid = grid.child(self.tile(m, side, cx));
                }
                list = list.child(grid);
            } else {
                let mut b = Self::list_box(&theme);
                for m in &month.items {
                    b = b.child(if kind == SharedMediaKind::Files { Self::file_row(m, &theme) } else { Self::link_row(m, &theme) });
                }
                list = list.child(b);
            }
        }
        if f.loaded && f.more {
            let busy = f.phase == FeedPhase::Loading;
            list = list.child(
                div()
                    .id("browse-more")
                    .py(px(8.0))
                    .flex()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.accent)
                    .child(if busy { spinner(12.0, theme.secondary()).into_any_element() } else { "Tải thêm".into_any_element() })
                    .on_click(cx.listener(move |this, _, _, cx| this.load_more(kind, true, cx))),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(52.0))
                    .pb(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("browse-back")
                            .size(px(26.0))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(theme.hover()))
                            .child(icon("chevron.left", 13.0, theme.label()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.browsing = None;
                                cx.notify();
                            })),
                    )
                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child("Kho lưu trữ")),
            )
            .child(div().px(px(16.0)).pb(px(10.0)).child(tabs))
            .child(div().id("browse-list").flex_1().min_h(px(0.0)).overflow_y_scroll().child(list))
            .into_any_element()
    }
}

impl Render for Inspector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let newest = engine(cx).state.borrow().messages.get(&self.conversation).and_then(|l| l.last().map(|m| m.id.clone()));
        if newest != self.last_seen || self.feeds.is_empty() {
            let had = self.feeds.values().any(|f| f.loaded);
            self.last_seen = newest;
            self.refresh(had, cx);
        }
        let body = match self.browsing {
            Some(kind) => self.render_browser(kind, cx),
            None => self.render_overview(cx),
        };
        let r = crate::chrome::inner_radius(window);
        div().w(px(WIDTH)).flex_none().h_full().border_l_1().border_color(theme.separator()).rounded_tr(r).rounded_br(r).bg(with_alpha(theme.content_bg(), 1.0)).child(body)
    }
}

#[allow(dead_code)]
fn unused(_: ObjectFit, _: &App) {}
