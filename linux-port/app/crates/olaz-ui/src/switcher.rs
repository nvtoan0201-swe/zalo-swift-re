//! Quick switcher (port of QuickSwitcher.swift): Ctrl+K panel to jump to a conversation, a contact or a command.
//! Arrow keys move, Enter opens, Esc closes.

use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::ZaloEngine;
use olaz_engine::contacts::fold;

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{avatar, badge};

pub const MAX_CONVERSATION_HITS: usize = 8;
pub const MAX_CONTACT_HITS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Inbox,
    Unread,
    Contacts,
    ZCloud,
    CreateGroup,
    Settings,
    Tour,
}

pub const COMMANDS: [Command; 7] = [Command::Inbox, Command::Unread, Command::Contacts, Command::ZCloud, Command::CreateGroup, Command::Settings, Command::Tour];

impl Command {
    pub fn title(self) -> &'static str {
        match self {
            Command::Inbox => "Đi tới Tin nhắn",
            Command::Unread => "Đi tới Chưa đọc",
            Command::Contacts => "Đi tới Danh bạ",
            Command::ZCloud => "Đi tới zCloud",
            Command::CreateGroup => "Tạo nhóm…",
            Command::Settings => "Mở Cài đặt…",
            Command::Tour => "Hướng dẫn sử dụng Olaz",
        }
    }
    fn symbol(self) -> &'static str {
        match self {
            Command::Inbox => "tray",
            Command::Unread => "bubble.left.and.text.bubble.right",
            Command::Contacts => "person.2",
            Command::ZCloud => "externaldrive",
            Command::CreateGroup => "person.2.badge.plus",
            Command::Settings => "gearshape",
            Command::Tour => "questionmark.circle",
        }
    }
    fn shortcut(self) -> Option<&'static str> {
        match self {
            Command::Settings => Some("Ctrl+,"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Leading {
    Avatar(Option<String>, String),
    Cloud,
    Symbol(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Conversation(String),
    Contact(String),
    Command(Command),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: String,
    pub leading: Leading,
    pub title: String,
    pub subtitle: Option<String>,
    pub unread: i64,
    pub shortcut: Option<&'static str>,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// The panel's sections for `query` (pure, unit tested).
pub fn sections(engine: &ZaloEngine, query: &str) -> Vec<Section> {
    let needle = fold(query.trim());
    let self_id = engine.state.borrow().self_conversation_id.clone();
    let title_of = |id: &str, name: String| if Some(id) == self_id.as_deref() { crate::shell::SELF_NAME.to_string() } else { name };
    let mut recent = engine.visible_conversations();
    recent.sort_by(|a, b| b.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0).total_cmp(&a.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0)));
    let conversations: Vec<_> = if needle.is_empty() {
        recent.into_iter().take(8).collect()
    } else {
        recent.into_iter().filter(|c| fold(&title_of(&c.id, c.display_name())).contains(&needle)).take(MAX_CONVERSATION_HITS).collect()
    };
    let mut out = vec![];
    if !conversations.is_empty() {
        out.push(Section {
            title: if needle.is_empty() { "Gần đây" } else { "Cuộc trò chuyện" },
            rows: conversations
                .iter()
                .map(|c| {
                    let is_self = Some(&c.id) == self_id.as_ref();
                    let subtitle = match &c.preview {
                        Some(p) if !p.text.is_empty() => Some(format!("{}{}", if p.from_me { "Bạn: " } else { "" }, p.text)),
                        _ if c.is_group && c.members > 0 => Some(format!("{} thành viên", c.members)),
                        _ => None,
                    };
                    Row {
                        id: format!("c:{}", c.id),
                        leading: if is_self { Leading::Cloud } else { Leading::Avatar(c.avatar.clone(), c.display_name()) },
                        title: title_of(&c.id, c.display_name()),
                        subtitle,
                        unread: c.unread,
                        shortcut: None,
                        target: Target::Conversation(c.id.clone()),
                    }
                })
                .collect(),
        });
    }
    if !needle.is_empty() {
        let listed: std::collections::HashSet<&str> = conversations.iter().map(|c| c.id.as_str()).collect();
        let friends = engine.visible_friends().into_iter().filter(|f| !listed.contains(f.id.as_str()) && f.sort_key.contains(&needle)).take(MAX_CONTACT_HITS).map(|f| Row {
            id: format!("p:{}", f.id),
            leading: Leading::Avatar(f.avatar.clone(), f.name.clone()),
            title: f.name.clone(),
            subtitle: Some("Bạn bè".into()),
            unread: 0,
            shortcut: None,
            target: Target::Contact(f.id.clone()),
        });
        let groups = engine.visible_groups().into_iter().filter(|g| !listed.contains(g.id.as_str()) && g.sort_key.contains(&needle)).take(MAX_CONTACT_HITS).map(|g| Row {
            id: format!("p:{}", g.id),
            leading: Leading::Avatar(g.avatar.clone(), g.name.clone()),
            title: g.name.clone(),
            subtitle: Some(if g.members > 0 { format!("{} thành viên", g.members) } else { "Nhóm".into() }),
            unread: 0,
            shortcut: None,
            target: Target::Contact(g.id.clone()),
        });
        let contacts: Vec<Row> = friends.chain(groups).take(MAX_CONTACT_HITS).collect();
        if !contacts.is_empty() {
            out.push(Section { title: "Danh bạ", rows: contacts });
        }
    }
    let commands: Vec<Row> = COMMANDS
        .iter()
        .filter(|c| needle.is_empty() || fold(c.title()).contains(&needle))
        .map(|c| Row {
            id: format!("k:{c:?}"),
            leading: Leading::Symbol(c.symbol()),
            title: c.title().into(),
            subtitle: None,
            unread: 0,
            shortcut: c.shortcut(),
            target: Target::Command(*c),
        })
        .collect();
    if !commands.is_empty() {
        out.push(Section { title: "Lệnh", rows: commands });
    }
    out
}

/// What the shell does after a pick.
pub enum SwitcherEvent {
    Close,
    Pick(Target),
}

impl EventEmitter<SwitcherEvent> for QuickSwitcher {}

pub struct QuickSwitcher {
    search: Entity<InputState>,
    query: String,
    highlighted: Option<String>,
}

impl QuickSwitcher {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Đi tới cuộc trò chuyện, người hoặc lệnh"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                this.query = input.read(cx).value().to_string();
                this.highlighted = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.open_current(cx),
            _ => {}
        })
        .detach();
        search.update(cx, |s, cx| s.focus(window, cx));
        QuickSwitcher { search, query: String::new(), highlighted: None }
    }

    fn rows(&self, cx: &gpui::App) -> Vec<Row> {
        sections(&engine(cx), &self.query).into_iter().flat_map(|s| s.rows).collect()
    }

    fn current(&self, rows: &[Row]) -> Option<Row> {
        self.highlighted.as_ref().and_then(|h| rows.iter().find(|r| &r.id == h)).or(rows.first()).cloned()
    }

    fn open_current(&mut self, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if let Some(r) = self.current(&rows) {
            cx.emit(SwitcherEvent::Pick(r.target));
        }
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return;
        }
        let cur = self.current(&rows).and_then(|c| rows.iter().position(|r| r.id == c.id)).unwrap_or(0) as i64;
        let next = (cur + delta).clamp(0, rows.len() as i64 - 1) as usize;
        self.highlighted = Some(rows[next].id.clone());
        cx.notify();
    }
}

impl Render for QuickSwitcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let sections = sections(&e, &self.query);
        let rows: Vec<Row> = sections.iter().flat_map(|s| s.rows.clone()).collect();
        let current = self.current(&rows).map(|r| r.id);
        let mut list = div().id("switcher-results").max_h(px(360.0)).overflow_y_scroll().p(px(6.0)).flex().flex_col();
        if sections.is_empty() {
            let text = if !e.state.borrow().conversation_actions.hidden_known {
                "Đang đồng bộ hội thoại…".to_string()
            } else if self.query.trim().is_empty() {
                "Chưa có cuộc trò chuyện".into()
            } else {
                format!("Không có kết quả cho “{}”", self.query.trim())
            };
            list = list.child(div().py(px(28.0)).text_center().text_size(px(13.0)).text_color(theme.secondary()).child(text));
        }
        for section in sections {
            list = list.child(div().px(px(10.0)).pt(px(8.0)).pb(px(4.0)).text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(section.title));
            for row in section.rows {
                let hl = current.as_deref() == Some(row.id.as_str());
                let fg = if hl { theme.white() } else { theme.label() };
                let leading: AnyElement = match &row.leading {
                    Leading::Avatar(url, name) => avatar(url.as_deref(), name, 28.0, cx),
                    Leading::Cloud => div().size(px(28.0)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(icon("cloud.fill", 13.0, theme.white())).into_any_element(),
                    Leading::Symbol(s) => div()
                        .size(px(28.0))
                        .rounded(px(8.0))
                        .bg(if hl { with_alpha(theme.white(), 0.2) } else { theme.quaternary() })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(s, 13.0, fg))
                        .into_any_element(),
                };
                let (target, id) = (row.target.clone(), row.id.clone());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("sw-{}", row.id)))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .text_color(fg)
                        .when(hl, |el| el.bg(theme.accent))
                        .child(leading)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .child(div().truncate().text_size(px(13.5)).font_weight(if row.unread > 0 { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(row.title.clone()))
                                .when_some(row.subtitle.clone(), |el, s| {
                                    el.child(div().truncate().text_size(px(11.5)).text_color(if hl { with_alpha(theme.white(), 0.8) } else { theme.secondary() }).child(s))
                                }),
                        )
                        .when(row.unread > 0, |el| el.child(badge(row.unread, if hl { theme.white() } else { theme.accent }, if hl { theme.accent } else { theme.white() })))
                        .when_some(row.shortcut, |el, s| el.child(div().text_size(px(11.5)).text_color(if hl { with_alpha(theme.white(), 0.8) } else { theme.secondary() }).child(s)))
                        .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                            if *h && this.highlighted.as_deref() != Some(id.as_str()) {
                                this.highlighted = Some(id.clone());
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(SwitcherEvent::Pick(target.clone())))),
                );
            }
        }
        let hint = |k: &'static str, l: &'static str| {
            div().flex().items_center().gap(px(5.0)).child(div().px(px(5.0)).rounded(px(4.0)).bg(theme.quaternary()).text_size(px(10.5)).child(k)).child(l)
        };
        div()
            .id("switcher")
            .relative()
            .child(crate::widgets::occluder())
            .w(px(560.0))
            .rounded(px(22.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| match ev.keystroke.key.as_str() {
                "down" => {
                    this.step(1, cx);
                    cx.stop_propagation();
                }
                "up" => {
                    this.step(-1, cx);
                    cx.stop_propagation();
                }
                "escape" => cx.emit(SwitcherEvent::Close),
                _ => {}
            }))
            .child(
                div()
                    .h(px(56.0))
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .border_b_1()
                    .border_color(theme.separator())
                    .child(icon("magnifyingglass", 17.0, theme.secondary()))
                    .child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(17.0)))),
            )
            .child(list)
            .child(
                div()
                    .px(px(16.0))
                    .py(px(8.0))
                    .border_t_1()
                    .border_color(theme.separator())
                    .flex()
                    .gap(px(14.0))
                    .text_size(px(11.0))
                    .text_color(theme.secondary())
                    .child(hint("↑↓", "Chọn"))
                    .child(hint("Enter", "Mở"))
                    .child(hint("Esc", "Đóng")),
            )
    }
}

/// The tour card: one step at a time, centered over the shell.
pub fn tour_card(step: usize, on_next: impl Fn(&mut gpui::App) + 'static, on_skip: impl Fn(&mut gpui::App) + 'static, theme: &Theme) -> AnyElement {
    let s = &crate::tour::STEPS[step.min(crate::tour::STEPS.len() - 1)];
    let last = step + 1 >= crate::tour::STEPS.len();
    div()
        .id("tour")
        .relative()
            .child(crate::widgets::occluder())
        .w(px(380.0))
        .p(px(20.0))
        .rounded(px(18.0))
        .bg(theme.content_bg())
        .border_1()
        .border_color(theme.separator())
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(div().flex().items_center().gap(px(10.0)).child(icon(s.symbol, 20.0, theme.accent)).child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(s.title)))
        .child(div().text_size(px(13.0)).text_color(theme.secondary()).child(s.message))
        .child(
            div()
                .pt(px(6.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().text_size(px(11.5)).text_color(theme.tertiary()).child(format!("{}/{}", step + 1, crate::tour::STEPS.len())))
                .when(!last, |el| el.child(crate::widgets::button("tour-skip", "Bỏ qua", crate::widgets::ButtonStyle::Glass, false, theme, move |_, _, cx| on_skip(cx))))
                .child(crate::widgets::button("tour-next", if last { "Xong" } else { "Tiếp" }, crate::widgets::ButtonStyle::Prominent, false, theme, move |_, _, cx| on_next(cx))),
        )
        .into_any_element()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::Arc;

    pub fn test_engine(dir: &std::path::Path) -> Rc<ZaloEngine> {
        let t = olaz_core::exec::TestExecutor::new();
        ZaloEngine::new(
            t.exec(),
            olaz_core::paths::XdgPaths::rooted(dir),
            olaz_engine::Services {
                media: None,
                secrets: Arc::new(olaz_main::secrets::MemoryStore::default()),
                open_url: Rc::new(|_| {}),
                badge: None,
                webhost_binary: "/nonexistent".into(),
                debug: false,
            },
        )
    }

    fn conv(id: &str, name: &str, date: f64) -> olaz_engine::ConversationSummary {
        olaz_engine::ConversationSummary {
            id: id.into(),
            is_group: false,
            name: name.into(),
            avatar: None,
            pinned: false,
            members: 0,
            unread: 0,
            preview: Some(olaz_engine::engine::Preview { text: "hi".into(), date: Some(date), from_me: true }),
        }
    }

    #[test]
    fn sections_follow_the_query() {
        let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let e = test_engine(dir.path());
        {
            let mut s = e.state.borrow_mut();
            s.conversations = vec![conv("1", "Đặng Ánh", 1.0), conv("2", "Bình", 3.0), conv("h", "Ẩn", 5.0), conv("me", "My Documents", 2.0)];
            s.self_conversation_id = Some("me".into());
            s.conversation_actions.apply(&serde_json::json!({"hidden": {"enabled": true, "ids": ["h"]}}));
            s.contacts.apply(&serde_json::json!({"friends": [{"id": "1", "name": "Đặng Ánh"}, {"id": "9", "name": "Anh Chín"}], "groups": [{"id": "g", "name": "Nhóm Anh", "members": 4}]}));
        }
        let empty = sections(&e, "");
        assert_eq!(empty[0].title, "Gần đây");
        let ids: Vec<_> = empty[0].rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["c:2", "c:me", "c:1"], "newest first, hidden chats never listed");
        assert_eq!(empty[0].rows[1].title, "Cloud của tôi");
        assert_eq!(empty[0].rows[1].subtitle.as_deref(), Some("Bạn: hi"));
        assert_eq!(empty.last().unwrap().rows.len(), COMMANDS.len());
        let anh = sections(&e, "anh");
        assert_eq!(anh[0].rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["c:1"]);
        assert_eq!(anh[1].title, "Danh bạ");
        assert_eq!(anh[1].rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["p:9", "p:g"], "listed conversations are not repeated");
        assert_eq!(anh[1].rows[1].subtitle.as_deref(), Some("4 thành viên"));
        let cmd = sections(&e, "cai dat");
        assert_eq!(cmd.len(), 1);
        assert_eq!(cmd[0].rows[0].target, Target::Command(Command::Settings));
        assert!(sections(&e, "zzzz").is_empty());
    }
}
