//! Main shell: sidebar, conversation list, detail (chat) — three columns.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState,
    MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, list, prelude::FluentBuilder,
    px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::ContextMenuExt;
use olaz_engine::conversation_actions::MenuPreset;
use olaz_engine::conversations::{ConversationTypeFilter, STRANGER_BOX_ID};
use olaz_engine::{Change, ConversationSummary, ZaloEngine};

use crate::app::engine;
use crate::chat::ChatView;
use crate::chrome::{self, TITLEBAR_HEIGHT};
use crate::format;
use crate::icon::{icon, icon_weighted};
use crate::theme::{Theme, with_alpha};
use crate::widgets::{avatar, badge, content_unavailable, spinner};

pub const SELF_NAME: &str = "Cloud của tôi";

#[derive(Clone, Debug, PartialEq)]
pub enum Destination {
    Inbox,
    Other,
    Unread,
    Strangers,
    Label(String),
    MyCloud,
    Friends,
    Groups,
    Requests,
    AddFriend,
    ZCloud,
}

impl Destination {
    pub fn title(&self) -> &'static str {
        match self {
            Destination::Inbox | Destination::MyCloud | Destination::Label(_) => "Tin nhắn",
            Destination::Other => "Khác",
            Destination::Unread => "Chưa đọc",
            Destination::Strangers => "Người lạ",
            Destination::Friends => "Bạn bè",
            Destination::Groups => "Nhóm và cộng đồng",
            Destination::Requests => "Lời mời kết bạn",
            Destination::AddFriend => "Thêm bạn",
            Destination::ZCloud => "zCloud",
        }
    }
    fn contacts_kind(&self) -> Option<crate::contacts::ContactsKind> {
        use crate::contacts::ContactsKind;
        match self {
            Destination::Friends => Some(ContactsKind::Friends),
            Destination::Groups => Some(ContactsKind::Groups),
            Destination::Requests => Some(ContactsKind::Requests),
            Destination::AddFriend => Some(ContactsKind::AddFriend),
            _ => None,
        }
    }
    fn is_conversation_list(&self) -> bool {
        matches!(self, Destination::Inbox | Destination::Other | Destination::Unread | Destination::Strangers | Destination::Label(_) | Destination::MyCloud)
    }
    /// `officialFilter`.
    fn official_filter(&self, default_tab: ConversationTypeFilter) -> Option<(ConversationTypeFilter, Vec<String>)> {
        match self {
            Destination::Inbox | Destination::MyCloud => Some((default_tab, vec![])),
            Destination::Other => Some((ConversationTypeFilter::Other, vec![])),
            Destination::Unread => Some((ConversationTypeFilter::Unread, vec![])),
            Destination::Label(id) => Some((default_tab, vec![id.clone()])),
            _ => None,
        }
    }
}

/// One row of the flattened conversation list.
#[derive(Clone, Debug, PartialEq)]
pub enum ListItem {
    Banner(&'static str),
    PinRow(Vec<String>),
    Row(String),
    StrangerBox,
}

impl ListItem {
    fn key(&self) -> String {
        match self {
            ListItem::Banner(k) => format!("banner:{k}"),
            ListItem::PinRow(ids) => format!("pins:{}", ids.join(",")),
            ListItem::Row(id) => format!("row:{id}"),
            ListItem::StrangerBox => "stranger_box".into(),
        }
    }
}

/// Diacritic- and case-insensitive containment for searching names.
pub fn fold(s: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let _ = s.graphemes(true);
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'à' | 'á' | 'ả' | 'ã' | 'ạ' | 'ă' | 'ằ' | 'ắ' | 'ẳ' | 'ẵ' | 'ặ' | 'â' | 'ầ' | 'ấ' | 'ẩ' | 'ẫ' | 'ậ' => 'a',
            'è' | 'é' | 'ẻ' | 'ẽ' | 'ẹ' | 'ê' | 'ề' | 'ế' | 'ể' | 'ễ' | 'ệ' => 'e',
            'ì' | 'í' | 'ỉ' | 'ĩ' | 'ị' => 'i',
            'ò' | 'ó' | 'ỏ' | 'õ' | 'ọ' | 'ô' | 'ồ' | 'ố' | 'ổ' | 'ỗ' | 'ộ' | 'ơ' | 'ờ' | 'ớ' | 'ở' | 'ỡ' | 'ợ' => 'o',
            'ù' | 'ú' | 'ủ' | 'ũ' | 'ụ' | 'ư' | 'ừ' | 'ứ' | 'ử' | 'ữ' | 'ự' => 'u',
            'ỳ' | 'ý' | 'ỷ' | 'ỹ' | 'ỵ' => 'y',
            'đ' => 'd',
            other => other,
        })
        .collect()
}

/// Items of the list column, pure for testing.
pub fn list_items(engine: &ZaloEngine, destination: &Destination, search: &str) -> (Vec<ListItem>, Vec<String>) {
    let s = engine.state.borrow();
    let by_id: HashMap<&str, &ConversationSummary> = s.conversations.iter().map(|c| (c.id.as_str(), c)).collect();
    let self_id = s.self_conversation_id.clone();
    let name_of = |c: &ConversationSummary| if Some(&c.id) == self_id.as_ref() { SELF_NAME.to_string() } else { c.display_name() };
    let actions = &s.conversation_actions;
    let query = search.trim();
    if !query.is_empty() {
        // Hidden chats only show up when the query is the PIN (official sidebar search).
        let q = fold(query);
        let by_pin = s.conversation_ui.pin_search_ids.iter().filter(|id| by_id.contains_key(id.as_str())).map(|id| ListItem::Row(id.clone()));
        let by_name = s.conversations.iter().filter(|c| !actions.hidden_ids.contains(&c.id) && fold(&name_of(c)).contains(&q)).map(|c| ListItem::Row(c.id.clone()));
        let rows: Vec<ListItem> = by_pin.chain(by_name).collect();
        return (rows, vec![]);
    }
    // Hidden chats are removed in every mode (the controller may still list one right after a local hide).
    let by_id: HashMap<&str, &ConversationSummary> = by_id.into_iter().filter(|(id, _)| !actions.is_hidden(id)).collect();
    let extras = &s.conversation_extras;
    let mut ids: Vec<String> = if extras.loaded {
        match destination {
            Destination::Strangers => extras.stranger_folder_ids.clone(),
            _ => extras.list_ids.clone(),
        }
    } else {
        s.conversations.iter().map(|c| c.id.clone()).collect()
    };
    ids.retain(|id| id == STRANGER_BOX_ID || by_id.contains_key(id.as_str()));
    if !extras.loaded && *destination == Destination::Unread {
        ids.retain(|id| by_id.get(id.as_str()).is_some_and(|c| c.unread > 0));
    }
    let mut items = Vec::new();
    match destination {
        Destination::Strangers => items.push(ListItem::Banner("strangers")),
        Destination::Other => items.push(ListItem::Banner("other")),
        _ => {}
    }
    let mut pins = Vec::new();
    for id in ids {
        if id == STRANGER_BOX_ID {
            if extras.stranger_box.chats > 0 {
                items.push(ListItem::StrangerBox);
            }
            continue;
        }
        let Some(c) = by_id.get(id.as_str()) else { continue };
        if matches!(destination, Destination::Inbox | Destination::MyCloud) && (c.pinned || actions.is_pinned(&c.id)) {
            pins.push(id.clone());
        } else {
            items.push(ListItem::Row(id.clone()));
        }
    }
    (items, pins)
}

pub struct ShellView {
    pub destination: Destination,
    pub selection: Option<String>,
    show_sidebar: bool,
    search: Entity<InputState>,
    search_text: String,
    list_state: ListState,
    list_keys: Vec<String>,
    list_items: Vec<ListItem>,
    pins: Vec<String>,
    chat: Option<Entity<ChatView>>,
    account_menu: bool,
    tour_pending: bool,
    pub pending_open: Option<String>,
    prompts: crate::conversation_actions::PromptHost,
    contacts: Option<Entity<crate::contacts::ContactsView>>,
    cloud: Option<Entity<crate::cloud::CloudView>>,
    group_sheet: Option<Entity<crate::group_create::CreateGroupSheet>>,
    last_removal: u64,
    last_search: String,
    _pin_search: Option<gpui::Task<()>>,
    switcher: Option<Entity<crate::switcher::QuickSwitcher>>,
    pub switcher_requested: bool,
    pub pending_destination: Option<Destination>,
    tour_step: Option<usize>,
}

thread_local! {
    static SHELLS: std::cell::RefCell<Vec<gpui::WeakEntity<ShellView>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A card asked for a sidebar section (`action.open.tab.*`).
pub fn open_section(target: olaz_engine::card_actions::SidebarTarget, cx: &mut App) {
    use olaz_engine::card_actions::SidebarTarget;
    let d = match target {
        SidebarTarget::Chats => Destination::Inbox,
        SidebarTarget::Groups => Destination::Groups,
        SidebarTarget::Contacts => Destination::Friends,
    };
    SHELLS.with(|s| {
        for w in s.borrow().iter() {
            let _ = w.update(cx, |shell, cx| {
                shell.pending_destination = Some(d.clone());
                cx.notify();
            });
        }
    });
}

/// Ctrl+K anywhere: toggles the quick switcher of every shell (applied on its next render, where the window is at hand).
pub fn request_switcher(cx: &mut App) {
    SHELLS.with(|s| {
        for w in s.borrow().iter() {
            let _ = w.update(cx, |shell, cx| {
                shell.switcher_requested = true;
                cx.notify();
            });
        }
    });
}

/// Ctrl+Alt+S or the list header button: hides / shows the sidebar column of every shell.
pub fn toggle_sidebar(cx: &mut App) {
    SHELLS.with(|s| {
        for w in s.borrow().iter() {
            let _ = w.update(cx, |shell, cx| {
                shell.show_sidebar = !shell.show_sidebar;
                cx.notify();
            });
        }
    });
}

/// `--ui-render`: the fixture's `shell` block plus the scene's switcher / inspector, applied on the next render.
pub fn apply_render_state(shell: &serde_json::Map<String, serde_json::Value>, inspector: bool, switcher: bool, cx: &mut App) {
    let destination = match shell.get("destination").and_then(|v| v.as_str()) {
        Some("other") => Some(Destination::Other),
        Some("unread") => Some(Destination::Unread),
        Some("strangers") => Some(Destination::Strangers),
        Some("myCloud") => Some(Destination::MyCloud),
        Some("friends") => Some(Destination::Friends),
        Some("groups") => Some(Destination::Groups),
        Some("requests") => Some(Destination::Requests),
        Some("addFriend") => Some(Destination::AddFriend),
        Some("zcloud") => Some(Destination::ZCloud),
        _ => None,
    };
    let selection = shell.get("selection").and_then(|v| v.as_str()).map(str::to_string);
    let inspector = inspector || shell.get("inspector").and_then(|v| v.as_bool()).unwrap_or(false);
    let switcher = switcher || shell.get("quickSwitcher").and_then(|v| v.as_bool()).unwrap_or(false);
    crate::inspector::SHOWN.with(|s| s.set(inspector));
    SHELLS.with(|s| {
        for w in s.borrow().iter() {
            let _ = w.update(cx, |shell, cx| {
                shell.pending_destination = destination.clone();
                shell.pending_open = selection.clone();
                shell.switcher_requested = switcher;
                cx.notify();
            });
        }
    });
}

/// Engine changes reach the shell (navigation requests, message updates handled by views on render).
pub fn engine_changed(changes: &[Change], cx: &mut App) {
    let _ = changes;
    let request = engine(cx).state.borrow_mut().navigation_request.take();
    if let Some(id) = request {
        SHELLS.with(|s| {
            for w in s.borrow().iter() {
                let _ = w.update(cx, |shell, cx| {
                    // Applied on the next render, where the window is at hand.
                    shell.pending_open = Some(id.clone());
                    cx.notify();
                });
            }
        });
    }
}

impl ShellView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        SHELLS.with(|s| s.borrow_mut().push(cx.entity().downgrade()));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Tìm kiếm"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.search_text = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        Self {
            destination: Destination::Inbox,
            selection: None,
            show_sidebar: true,
            search,
            search_text: String::new(),
            list_state: ListState::new(0, ListAlignment::Top, px(400.0)),
            list_keys: Vec::new(),
            list_items: Vec::new(),
            pins: Vec::new(),
            chat: None,
            account_menu: false,
            tour_pending: false,
            pending_open: None,
            prompts: Default::default(),
            contacts: None,
            cloud: None,
            group_sheet: None,
            last_removal: 0,
            last_search: String::new(),
            _pin_search: None,
            switcher: None,
            switcher_requested: false,
            pending_destination: None,
            tour_step: None,
        }
    }

    pub fn reset(&mut self) {
        self.destination = Destination::Inbox;
        self.selection = None;
        self.chat = None;
        self.search_text.clear();
    }

    pub fn start_tour_later(&mut self, _cx: &mut Context<Self>) {
        self.tour_pending = true;
    }

    pub fn open_conversation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.destination.is_conversation_list() {
            self.set_destination(Destination::Inbox, window, cx);
        }
        self.select(Some(id.to_string()), window, cx);
    }

    fn set_destination(&mut self, d: Destination, window: &mut Window, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let default_tab = engine.state.borrow().conversation_extras.default_tab();
        if let Some((kind, labels)) = d.official_filter(default_tab) {
            engine.set_conversation_filter(kind, labels);
        }
        if d == Destination::MyCloud {
            let self_id = engine.state.borrow().self_conversation_id.clone();
            self.destination = Destination::MyCloud;
            if let Some(id) = self_id {
                self.select(Some(id), window, cx);
            }
        } else {
            if self.destination == Destination::MyCloud && d == Destination::Inbox {
                self.select(None, window, cx);
            }
            self.destination = d;
        }
        if self.destination == Destination::ZCloud {
            if self.cloud.is_none() {
                self.cloud = Some(cx.new(|cx| crate::cloud::CloudView::new(window, cx)));
            }
        } else {
            self.cloud = None;
        }
        let kind = self.destination.contacts_kind();
        if kind != self.contacts.as_ref().map(|c| c.read(cx).kind) {
            self.contacts = kind.map(|k| cx.new(|cx| crate::contacts::ContactsView::new(k, window, cx)));
        }
        cx.notify();
    }

    fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection == id {
            return;
        }
        let engine = engine(cx);
        olaz_core::log::write(format!("shell selection {}…", id.as_deref().unwrap_or("-").chars().take(4).collect::<String>()));
        self.selection = id.clone();
        crate::cards::reset();
        // The bypass belongs to the one conversation opened with the PIN.
        let unlocked = engine.state.borrow().conversation_ui.unlocked_id.clone();
        if unlocked.is_some() && unlocked != id {
            engine.state.borrow_mut().conversation_ui.unlocked_id = None;
        }
        match &id {
            Some(id) if !engine.state.borrow().conversation_actions.hidden_known => {
                // Until the hidden-chat state is known any conversation could be hidden: nothing is opened.
                let _ = id;
                engine.close_conversation();
                self.chat = None;
            }
            Some(id) if engine.is_conversation_locked(id) && !engine.unlock_from_pin_search(id) => {
                engine.close_conversation();
                self.chat = None;
                engine.set_conversation_prompt(Some(olaz_engine::conversation_actions::ConversationPrompt::Unlock(id.clone())));
            }
            Some(id) => {
                engine.open_conversation(id);
                let conv = id.clone();
                self.chat = Some(cx.new(|cx| ChatView::new(conv, window, cx)));
            }
            None => {
                engine.close_conversation();
                self.chat = None;
            }
        }
        cx.notify();
    }

    fn sync_list(&mut self, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let (items, pins) = list_items(&engine, &self.destination, &self.search_text);
        let mut all = Vec::new();
        if !pins.is_empty() {
            // Pinned tiles: 4 per row in the 340-wide column.
            for chunk in pins.chunks(4) {
                all.push(ListItem::PinRow(chunk.to_vec()));
            }
        }
        all.extend(items);
        let keys: Vec<String> = all.iter().map(ListItem::key).collect();
        if keys != self.list_keys {
            splice_keys(&self.list_state, &self.list_keys, &keys);
            self.list_keys = keys;
        }
        self.list_items = all;
        self.pins = pins;
    }

    // MARK: sidebar

    fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let (extras_loaded, archived, self_id, unread_inbox) = {
            let s = engine.state.borrow();
            let unread = s.conversations.iter().filter(|c| c.unread > 0).count();
            (s.conversation_extras.loaded, s.conversation_extras.archived_enabled, s.self_conversation_id.clone(), unread)
        };
        let _ = extras_loaded;
        let highlighted = match (&self.destination, &self.selection) {
            (Destination::MyCloud, _) => Destination::MyCloud,
            (Destination::Inbox, Some(sel)) if Some(sel) == self_id.as_ref() => Destination::MyCloud,
            (Destination::Label(_), _) => Destination::Inbox,
            (d, _) => d.clone(),
        };
        let row = |id: &'static str, d: Destination, symbol: &'static str, title: &'static str, count: usize, cx: &mut Context<Self>| {
            let selected = highlighted == d;
            let fg = theme.label();
            div()
                .id(id)
                .h(px(28.0))
                .px(px(8.0))
                .mx(px(10.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .when(selected, |el| el.bg(theme.selection_tint()))
                .when(!selected, |el| el.hover(|s| s.bg(theme.hover())))
                .child(icon(symbol, 14.0, theme.accent))
                .child(crate::widgets::static_text(div().flex_1().text_size(px(13.0)).text_color(fg), title))
                .when(count > 0, |el| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(count.to_string())))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.search_text.clear();
                    if d == Destination::Inbox && matches!(this.destination, Destination::Label(_)) {
                        this.set_destination(Destination::Inbox, window, cx);
                    } else {
                        this.set_destination(d.clone(), window, cx);
                    }
                }))
        };
        let section = |title: &'static str| div().px(px(18.0)).pt(px(14.0)).pb(px(4.0)).text_size(px(11.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.tertiary()).child(title);

        let (counts_friends, counts_requests) = (0usize, engine.state.borrow().contacts.pending_request_count.max(0) as usize);
        let mut col = div()
            .size_full()
            .flex()
            .flex_col()
            .child(chrome::drag_area("sidebar-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none().flex().items_center().px(px(18.0)).child(
                div().text_size(px(15.0)).font_weight(FontWeight::BOLD).child("Olaz"),
            ))
            .child(row("sb-inbox", Destination::Inbox, "tray", "Tin nhắn", unread_inbox, cx));
        if archived {
            col = col.child(row("sb-other", Destination::Other, "archivebox", "Khác", 0, cx));
        }
        col = col
            .child(row("sb-unread", Destination::Unread, "bubble.left.and.text.bubble.right", "Chưa đọc", 0, cx))
            .child(row("sb-strangers", Destination::Strangers, "person.crop.circle.badge.questionmark", "Người lạ", 0, cx))
            .child(section("Lưu trữ"));
        if self_id.is_some() {
            col = col.child(row("sb-cloud", Destination::MyCloud, "cloud", "Cloud của tôi", 0, cx));
        }
        col = col.child(row("sb-zcloud", Destination::ZCloud, "externaldrive", "zCloud", 0, cx));
        col = col
            .child(section("Danh bạ"))
            .child(row("sb-friends", Destination::Friends, "person", "Bạn bè", counts_friends, cx))
            .child(row("sb-groups", Destination::Groups, "person.2", "Nhóm và cộng đồng", 0, cx))
            .child(row("sb-requests", Destination::Requests, "person.crop.circle.badge.plus", "Lời mời kết bạn", counts_requests, cx))
            .child(row("sb-add-friend", Destination::AddFriend, "person.badge.plus", "Thêm bạn", 0, cx))
            .child(div().flex_1())
            .child(self.render_account_footer(cx));
        let _ = window;
        let r = if window.is_maximized() { 0.0 } else { chrome::WINDOW_RADIUS - 1.0 };
        div().w(px(236.0)).flex_none().h_full().rounded_l(px(r)).bg(theme.sidebar_bg()).border_r_1().border_color(theme.separator()).child(col).into_any_element()
    }

    fn render_account_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let profile = engine.state.borrow().profile.clone();
        let name = profile.as_ref().map(|p| p.display_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Tài khoản của tôi".into());
        let av = avatar(profile.as_ref().and_then(|p| p.avatar.as_deref()), &name, 30.0, cx);
        let menu_open = self.account_menu;
        let footer = div()
            .id("account-footer")
            .mx(px(8.0))
            .my(px(6.0))
            .p(px(8.0))
            .rounded(px(12.0))
            .flex()
            .items_center()
            .gap(px(9.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme.quaternary()))
            .child(av)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).truncate().child(name.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .child(div().size(px(6.0)).rounded_full().bg(theme.green()))
                            .child(div().text_size(px(11.0)).text_color(theme.secondary()).child("Đã kết nối")),
                    ),
            )
            .child(icon("chevron.up.chevron.down", 9.0, theme.secondary()))
            // The shell closes the menu on any mouse-down: keep presses on the footer and the menu from reaching it,
            // or the menu disappears before the click (on release) lands on an item.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.account_menu = !this.account_menu;
                cx.notify();
            }));
        div()
            .relative()
            .child(footer)
            .when(menu_open, |el| {
                let item = |id: &'static str, symbol: &'static str, title: &'static str, destructive: bool, theme: &Theme| {
                    div()
                        .id(id)
                        .h(px(24.0))
                        .px(px(10.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(13.0))
                        .text_color(if destructive { theme.red() } else { theme.label() })
                        .hover(|s| s.bg(with_alpha(theme.accent, 0.15)))
                        .cursor_pointer()
                        .child(icon(symbol, 13.0, if destructive { theme.red() } else { theme.label() }))
                        .child(title)
                };
                // Deferred: painted above the list column it overlaps (it lives in the sidebar's tree).
                el.child(gpui::deferred(
                    div()
                        .id("account-menu")
                        .absolute()
                        .bottom(px(60.0))
                        .left(px(10.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(260.0))
                        .p(px(5.0))
                        .rounded(px(10.0))
                        .bg(theme.content_bg())
                        .border_1()
                        .border_color(theme.separator())
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .child(div().px(px(10.0)).py(px(4.0)).text_size(px(12.0)).text_color(theme.tertiary()).child(name.clone()))
                        .child(item("menu-settings", "gearshape", "Cài đặt…", false, &theme).on_click(cx.listener(|this, _, window, cx| {
                            this.account_menu = false;
                            crate::settings_window::open(window, cx);
                            cx.notify();
                        })))
                        .child({
                            let e = crate::app::engine(cx);
                            let can = e.state.borrow().can_start_transfer && e.state.borrow().transfer.is_none();
                            item("menu-transfer", "arrow.triangle.2.circlepath", "Đồng bộ tin nhắn từ điện thoại…", false, &theme)
                                .when(!can, |el| el.opacity(0.45))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.account_menu = false;
                                    let e = crate::app::engine(cx);
                                    if can {
                                        e.start_transfer();
                                    } else {
                                        // Unavailable: say why (or the generic text) instead of doing nothing.
                                        let r = e.state.borrow().transfer_blocked_reason.clone().unwrap_or_default();
                                        e.show_conv_toast(olaz_engine::conversation_actions::ToastKind::Neutral, olaz_engine::transfer::transfer_blocked_text(&r));
                                    }
                                    cx.notify();
                                }))
                        })
                        .child(div().h(px(1.0)).my(px(4.0)).bg(theme.separator()))
                        .child(item("menu-logout", "rectangle.portrait.and.arrow.right", "Đăng xuất", true, &theme).on_click(cx.listener(|this, _, _, cx| {
                            this.account_menu = false;
                            this.reset();
                            crate::app::engine(cx).log_out();
                            cx.notify();
                        }))),
                ).with_priority(1))
            })
            .into_any_element()
    }

    // MARK: list column

    fn render_list_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        self.sync_list(cx);
        let (unread_chats, has_any) = {
            let s = engine.state.borrow();
            (s.conversations.iter().filter(|c| c.unread > 0).count(), !s.conversations.is_empty() && s.conversation_actions.hidden_known)
        };
        let subtitle = if unread_chats > 0 { format!("{unread_chats} cuộc trò chuyện chưa đọc") } else { "Đã đọc hết".to_string() };
        let label_name = match &self.destination {
            Destination::Label(id) => engine.state.borrow().conversation_extras.labels.iter().find(|l| &l.id == id).map(|l| l.name.clone()),
            _ => None,
        };
        let subtitle = match label_name {
            Some(l) => format!("{l} · {subtitle}"),
            None => subtitle,
        };
        let header = chrome::drag_area("list-toolbar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(15.0)).font_weight(FontWeight::BOLD).child(self.destination.title()))
                    .child(div().truncate().text_size(px(11.0)).text_color(theme.secondary()).child(subtitle)),
            )
            .child(
                div()
                    .id("toggle-sidebar")
                    .flex_none()
                    .size(px(28.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover()))
                    .child(icon("sidebar.left", 16.0, if self.show_sidebar { theme.secondary() } else { theme.accent }))
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new("Ẩn / hiện thanh bên").action(&crate::keymap::ToggleSidebar, None).build(window, cx)
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| toggle_sidebar(cx)),
            );
        let search = div()
            .px(px(12.0))
            .pb(px(8.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .h(px(30.0))
                    .px(px(10.0))
                    .rounded_full()
                    .bg(theme.glass())
                    .border_1()
                    .border_color(theme.glass_border())
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(icon_weighted("magnifyingglass", 12.5, theme.secondary(), false))
                    .child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(13.0))))
                    .when(self.search_text.is_empty(), |el| el.child(div().text_size(px(11.0)).text_color(theme.tertiary()).child("Ctrl+K"))),
            );
        let body: AnyElement = if !has_any {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child(spinner(14.0, theme.secondary()))
                .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Đang đồng bộ hội thoại…"))
                .into_any_element()
        } else if self.list_items.is_empty() {
            let (symbol, title, desc) = if !self.search_text.trim().is_empty() {
                ("magnifyingglass", format!("Không có kết quả cho “{}”", self.search_text.trim()), "Thử tìm theo tên người hoặc nhóm.".to_string())
            } else {
                match self.destination {
                    Destination::Unread => ("checkmark.circle", "Không có tin nhắn chưa đọc".into(), "Bạn đã đọc hết mọi cuộc trò chuyện.".into()),
                    Destination::Strangers => ("person.crop.circle.badge.questionmark", "Không có tin nhắn từ người lạ".into(), "Tin nhắn từ người chưa kết bạn sẽ hiện ở đây.".into()),
                    Destination::Other => ("archivebox", "Mục Khác đang trống".into(), "Chuột phải vào một cuộc trò chuyện để chuyển nó sang Khác.".into()),
                    Destination::Label(_) => ("tag", "Chưa có cuộc trò chuyện".into(), "Chuột phải vào một cuộc trò chuyện và chọn Phân loại để gắn nhãn này.".into()),
                    _ => ("bubble.left.and.bubble.right", "Chưa có cuộc trò chuyện".into(), "Cuộc trò chuyện mới sẽ hiện ở đây.".into()),
                }
            };
            div().flex_1().child(content_unavailable(symbol, &title, &desc, &theme)).into_any_element()
        } else {
            let items = self.list_items.clone();
            let me = cx.entity().downgrade();
            div()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    list(self.list_state.clone(), move |ix, window, cx| {
                        let Some(item) = items.get(ix).cloned() else { return div().into_any_element() };
                        match me.upgrade() {
                            Some(shell) => shell.update(cx, |this, cx| this.render_item(&item, window, cx)),
                            None => div().into_any_element(),
                        }
                    })
                    .size_full(),
                )
                .into_any_element()
        };
        let _ = window;
        div()
            .w(px(340.0))
            .flex_none()
            .h_full()
            .bg(theme.list_bg())
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.separator())
            .child(header)
            .child(search)
            .child(body)
            .into_any_element()
    }

    fn render_item(&mut self, item: &ListItem, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match item {
            ListItem::Row(id) => self.render_row(id, cx),
            ListItem::PinRow(ids) => self.render_pin_row(ids, cx),
            ListItem::StrangerBox => self.render_stranger_box(cx),
            ListItem::Banner(kind) => self.render_banner(kind, window, cx),
        }
    }

    fn render_banner(&mut self, kind: &str, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let (symbol, lead, message) = match kind {
            "strangers" => (
                "shield.lefthalf.filled",
                "Tin nhắn từ người lạ",
                "\nNgười gửi chưa có trong danh bạ. Trả lời để chuyển vào Tin nhắn.\nLưu ý rủi ro lừa đảo từ người lạ.",
            ),
            _ => ("archivebox", "Khác", " giữ các nhóm đông người và tin ít quan trọng, không làm phiền bạn."),
        };
        div()
            .pt(px(4.0))
            .px(px(10.0))
            .pb(px(8.0))
            .child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .px(px(11.0))
                    .py(px(9.0))
                    .rounded(px(12.0))
                    .bg(crate::theme::fade(theme.quaternary(), 0.7))
                    .child(div().w(px(20.0)).child(icon(symbol, 15.0, theme.accent)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(12.0))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(lead))
                            .child(div().text_color(theme.secondary()).child(message.trim_start().to_string())),
                    ),
            )
            .into_any_element()
    }

    fn render_row(&mut self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let (conv, self_id, typing, label) = {
            let s = engine.state.borrow();
            let conv = s.conversations.iter().find(|c| c.id == id).cloned();
            let label = s.conversation_extras.label_of(id).map(|l| (l.name.clone(), l.color()));
            (conv, s.self_conversation_id.clone(), s.typing.get(id).cloned(), label)
        };
        let Some(conv) = conv else { return div().into_any_element() };
        let is_self = Some(&conv.id) == self_id.as_ref();
        let selected = self.selection.as_deref() == Some(id);
        let name = if is_self { SELF_NAME.to_string() } else { conv.display_name() };
        let unread = conv.unread > 0;
        let now = crate::format::now();
        let time = conv.preview.as_ref().and_then(|p| p.date).map(|d| format::list_time(d, now)).unwrap_or_default();
        let (preview, preview_color) = match (&typing, &conv.preview) {
            (Some(t), _) => (format!("{t} đang soạn tin…"), theme.accent),
            (None, Some(p)) if !p.text.is_empty() => (
                if p.from_me { format!("Bạn: {}", p.text) } else { p.text.clone() },
                if unread { theme.label() } else { theme.secondary() },
            ),
            _ => (
                if conv.is_group && conv.members > 0 {
                    format!("{} thành viên", conv.members)
                } else if is_self {
                    "Lưu trữ và truy cập nhanh những nội dung quan trọng".into()
                } else if conv.is_group {
                    "Nhóm".into()
                } else {
                    "Bạn bè".into()
                },
                theme.secondary(),
            ),
        };
        let fg = theme.label();
        let id_owned = id.to_string();
        let av = self.conversation_avatar(&conv, is_self, 44.0, cx);
        div()
            .id(SharedString::from(format!("row-{id}")))
            .px(px(10.0))
            .py(px(1.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.0))
                    .px(px(8.0))
                    .py(px(7.0))
                    .rounded(px(10.0))
                    .when(selected, |el| el.bg(theme.selection_tint()))
                    .when(!selected, |el| el.hover(|s| s.bg(theme.hover())))
                    .child(av)
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
                                    .items_center()
                                    .gap(px(5.0))
                                    .child(
                                        div()
                                            .flex_shrink(1.0)
                                            .min_w(px(0.0))
                                            .truncate()
                                            .text_size(px(13.5))
                                            .font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                                            .text_color(fg)
                                            .relative()
                                            .children(crate::layout_dump::probe("AXStaticText", name.clone()))
                                            .child(name),
                                    )
                                    .when_some(label, |el, (lname, color)| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .px(px(5.0))
                                                .rounded(px(4.0))
                                                .bg(with_alpha(crate::theme::hex(color), 0.15))
                                                .text_size(px(9.5))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(crate::theme::hex(color))
                                                .child(lname),
                                        )
                                    })
                                    .child(div().flex_1().min_w(px(4.0)))
                                    .child(crate::widgets::static_text(div().flex_none().text_size(px(11.5)).text_color(theme.secondary()), &time)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(6.0))
                                    .child(crate::widgets::static_text(div().flex_1().min_w(px(0.0)).h(px(32.0)).text_size(px(12.5)).line_height(px(16.0)).line_clamp(2).text_color(preview_color), &preview))
                                    .when(unread, |el| {
                                        let (bg, text) = (theme.accent, theme.white());
                                        el.child(div().flex_none().child(badge(conv.unread, bg, text)))
                                    }),
                            ),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.select(Some(id_owned.clone()), window, cx)))
            .context_menu({
                let id = id.to_string();
                move |menu, window, cx| crate::conversation_actions::conversation_menu(menu, &id, MenuPreset::Thread, window, cx)
            })
            .into_any_element()
    }

    fn conversation_avatar(&self, conv: &ConversationSummary, is_self: bool, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        if is_self {
            return div()
                .size(px(size))
                .flex_none()
                .rounded_full()
                .bg(theme.content_bg())
                .border_1()
                .border_color(theme.separator())
                .flex()
                .items_center()
                .justify_center()
                .child(icon("cloud", size * 0.43, theme.accent))
                .into_any_element();
        }
        let base = avatar(conv.avatar.as_deref(), &conv.display_name(), size, cx);
        if !conv.is_group {
            return base;
        }
        let b = (size * 0.34).max(15.0);
        div()
            .relative()
            .flex_none()
            .child(base)
            .child(
                div()
                    .absolute()
                    .right(px(-2.0))
                    .bottom(px(-2.0))
                    .size(px(b))
                    .rounded_full()
                    .bg(theme.content_bg())
                    .border_1()
                    .border_color(theme.separator())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("person.2.fill", b * 0.46, theme.secondary())),
            )
            .into_any_element()
    }

    fn render_pin_row(&mut self, ids: &[String], cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let mut row = div().flex().gap(px(4.0)).px(px(10.0)).pt(px(2.0)).pb(px(6.0));
        for id in ids {
            let (conv, self_id) = {
                let s = engine.state.borrow();
                (s.conversations.iter().find(|c| &c.id == id).cloned(), s.self_conversation_id.clone())
            };
            let Some(conv) = conv else { continue };
            let is_self = Some(&conv.id) == self_id.as_ref();
            let selected = self.selection.as_deref() == Some(id.as_str());
            let tile_name = if is_self {
                "Cloud".to_string()
            } else if conv.is_group {
                conv.display_name()
            } else {
                conv.display_name().split_whitespace().last().unwrap_or("").to_string()
            };
            let av = self.conversation_avatar(&conv, is_self, 54.0, cx);
            let unread = conv.unread;
            let id_owned = id.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("tile-{id}")))
                    .flex_1()
                    .max_w(px(110.0))
                    .pt(px(8.0))
                    .pb(px(6.0))
                    .rounded(px(14.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(5.0))
                    .when(selected, |el| el.bg(with_alpha(theme.accent, 0.2)))
                    .when(!selected, |el| el.hover(|s| s.bg(theme.quaternary())))
                    .child(div().relative().child(av).when(unread > 0, |el| el.child(div().absolute().top(px(-4.0)).right(px(-7.0)).child(badge(unread, theme.accent, theme.white())))))
                    .child(div().px(px(2.0)).w_full().text_center().truncate().text_size(px(11.0)).font_weight(if unread > 0 { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).child(tile_name))
                    .on_click(cx.listener(move |this, _, window, cx| this.select(Some(id_owned.clone()), window, cx)))
                    .context_menu({
                        let id = id.clone();
                        move |menu, window, cx| crate::conversation_actions::conversation_menu(menu, &id, MenuPreset::Thread, window, cx)
                    }),
            );
        }
        row.into_any_element()
    }

    fn render_stranger_box(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let b = engine.state.borrow().conversation_extras.stranger_box.clone();
        let subtitle = format!("{} hội thoại", b.chats);
        div()
            .id("stranger-box")
            .px(px(10.0))
            .py(px(1.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(8.0))
                    .py(px(7.0))
                    .rounded(px(10.0))
                    .hover(|s| s.bg(theme.hover()))
                    .child(div().size(px(44.0)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(icon("person.crop.circle.badge.questionmark.fill", 19.0, theme.white())))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(13.5)).font_weight(if b.unread_chats > 0 { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }).child("Tin nhắn từ người lạ"))
                            .child(div().text_size(px(12.5)).text_color(theme.secondary()).child(subtitle)),
                    )
                    .when(b.unread_chats > 0, |el| el.child(badge(b.unread_chats, theme.accent, theme.white()))),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.search_text.clear();
                this.set_destination(Destination::Strangers, window, cx);
            }))
            .into_any_element()
    }

    // MARK: detail

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let locked = self.selection.clone().filter(|id| engine(cx).is_conversation_locked(id));
        if let (Some(id), true) = (locked, self.destination.is_conversation_list()) {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .child(chrome::drag_area("detail-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none())
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(10.0))
                        .child(icon("lock.fill", 34.0, theme.tertiary()))
                        .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Trò chuyện ẩn"))
                        .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Nhập mã PIN để mở trò chuyện"))
                        .child(crate::widgets::button("unlock-hidden", "Nhập mã PIN", crate::widgets::ButtonStyle::Prominent, false, &theme, move |_, _, cx| {
                            engine(cx).set_conversation_prompt(Some(olaz_engine::conversation_actions::ConversationPrompt::Unlock(id.clone())))
                        })),
                )
                .into_any_element();
        }
        match (&self.chat, self.destination.is_conversation_list()) {
            (Some(chat), true) => chat.clone().into_any_element(),
            (_, false) => div()
                .flex_1()
                .flex()
                .flex_col()
                .child(chrome::drag_area("detail-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none())
                .child(content_unavailable("person.2", self.destination.title(), "Danh bạ sẽ hiện ở đây.", &theme))
                .into_any_element(),
            _ => div()
                .flex_1()
                .flex()
                .flex_col()
                .child(chrome::drag_area("detail-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none())
                .child(content_unavailable(
                    "bubble.left.and.bubble.right",
                    "Chọn một cuộc trò chuyện",
                    "Chọn ở danh sách bên cạnh, hoặc nhấn Ctrl+K để đi tới một cuộc trò chuyện.",
                    &theme,
                ))
                .into_any_element(),
        }
    }
}

/// Applies the difference between two key lists to a `ListState` as one splice (common prefix/suffix kept),
/// so scroll position and measured heights survive updates.
pub fn splice_keys(state: &ListState, old: &[String], new: &[String]) {
    let prefix = old.iter().zip(new.iter()).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let old_range = prefix..old.len() - suffix;
    let count = new.len() - suffix - prefix;
    if old.is_empty() {
        state.reset(new.len());
    } else {
        state.splice(old_range, count);
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        if let Some(id) = self.pending_open.take() {
            self.open_conversation(&id, window, cx);
        }
        self.sync_conversation_actions(window, cx);
        if let Some(members) = crate::group_create::take_request() {
            let sheet = cx.new(|cx| crate::group_create::CreateGroupSheet::new(members, window, cx));
            cx.subscribe(&sheet, |this, _, _: &crate::group_create::GroupSheetEvent, cx| {
                this.group_sheet = None;
                cx.notify();
            })
            .detach();
            self.group_sheet = Some(sheet);
        }
        if let Some(d) = self.pending_destination.take() {
            self.set_destination(d, window, cx);
        }
        if std::mem::take(&mut self.tour_pending) {
            self.tour_step = Some(0);
        }
        if std::mem::take(&mut self.switcher_requested) {
            if self.switcher.take().is_none() {
                let sw = cx.new(|cx| crate::switcher::QuickSwitcher::new(window, cx));
                cx.subscribe_in(&sw, window, |this, _, ev: &crate::switcher::SwitcherEvent, window, cx| {
                    this.switcher = None;
                    if let crate::switcher::SwitcherEvent::Pick(target) = ev {
                        this.perform_switch(target.clone(), window, cx);
                    }
                    cx.notify();
                })
                .detach();
                self.switcher = Some(sw);
            }
        }
        // Like a split view: with the inspector open on a narrow window, the sidebar column gives way to the chat.
        let cramped = crate::inspector::SHOWN.with(|s| s.get())
            && self.chat.is_some()
            && self.destination.is_conversation_list()
            && window.viewport_size().width < px(1320.0);
        let list_col = if self.destination.is_conversation_list() { Some(self.render_list_column(window, cx)) } else { None };
        // The sidebar is the only way between sections outside the conversation list, so it can be hidden only there.
        let sidebar = if (self.show_sidebar || list_col.is_none()) && !cramped { Some(self.render_sidebar(window, cx)) } else { None };
        let detail = match (&self.contacts, &self.cloud) {
            (Some(c), _) => c.clone().into_any_element(),
            (_, Some(c)) => c.clone().into_any_element(),
            _ => self.render_detail(cx),
        };
        let r = if window.is_maximized() { 0.0 } else { chrome::WINDOW_RADIUS - 1.0 };
        div()
            .size_full()
            .flex()
            .rounded(px(r))
            .bg(theme.content_bg())
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.account_menu {
                    this.account_menu = false;
                    cx.notify();
                }
            }))
            .relative()
            .children(sidebar)
            .children(list_col)
            .child(div().flex_1().min_w(px(0.0)).h_full().flex().child(detail))
            .child(div().absolute().top(px(14.0)).right(px(chrome::CONTROLS_INSET)).child(chrome::window_controls(window, cx)))
            .children(crate::conversation_actions::conversation_toast(&theme, cx))
            .children(crate::transfer::overlay(&theme, cx))
            .when_some(self.switcher.clone(), |el, sw| {
                el.child(
                    div()
                        .id("switcher-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.10))
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(px(84.0))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.switcher = None;
                            cx.notify();
                        }))
                        .child(sw),
                )
            })
            .when_some(self.tour_step, |el, step| {
                let me = cx.entity().downgrade();
                let me2 = me.clone();
                el.child(div().absolute().inset_0().bg(gpui::black().opacity(0.15)).flex().items_center().justify_center().child(crate::switcher::tour_card(
                    step,
                    move |cx| {
                        let _ = me.update(cx, |this, cx| {
                            let next = step + 1;
                            this.tour_step = (next < crate::tour::STEPS.len()).then_some(next);
                            cx.notify();
                        });
                    },
                    move |cx| {
                        let _ = me2.update(cx, |this, cx| {
                            this.tour_step = None;
                            cx.notify();
                        });
                    },
                    &theme,
                )))
            })
            .children(self.prompts.render(&theme, cx))
            .when_some(self.group_sheet.clone(), |el, sheet| {
                el.child(
                    div()
                        .id("group-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(sheet),
                )
            })
    }
}

impl ShellView {
    fn perform_switch(&mut self, target: crate::switcher::Target, window: &mut Window, cx: &mut Context<Self>) {
        use crate::switcher::{Command, Target};
        match target {
            Target::Conversation(id) => self.open_conversation(&id, window, cx),
            Target::Contact(id) => engine(cx).open_contact_conversation(&id),
            Target::Command(Command::Inbox) => self.set_destination(Destination::Inbox, window, cx),
            Target::Command(Command::Unread) => self.set_destination(Destination::Unread, window, cx),
            Target::Command(Command::Contacts) => self.set_destination(Destination::Friends, window, cx),
            Target::Command(Command::ZCloud) => self.set_destination(Destination::ZCloud, window, cx),
            Target::Command(Command::CreateGroup) => crate::group_create::request(vec![], cx),
            Target::Command(Command::Settings) => crate::settings_window::open(window, cx),
            Target::Command(Command::Tour) => self.tour_step = Some(0),
        }
    }

    /// Removals leave the detail pane, a locked conversation closes, the PIN search runs, the prompt sheets follow the engine.
    fn sync_conversation_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let removal = engine.state.borrow().conversation_ui.removal.clone();
        if let Some((seq, ids)) = removal.filter(|(seq, _)| *seq != self.last_removal) {
            self.last_removal = seq;
            if self.selection.as_ref().is_some_and(|s| ids.contains(s)) {
                self.select(None, window, cx);
            }
        }
        if let Some(sel) = self.selection.clone() {
            if self.chat.is_some() && engine.state.borrow().conversation_actions.hidden_known && engine.is_conversation_locked(&sel) {
                engine.close_conversation();
                self.chat = None;
            } else if self.chat.is_none() && engine.state.borrow().conversation_actions.hidden_known && !engine.is_conversation_locked(&sel) {
                // The hidden state arrived / the PIN unlocked it: open it now.
                let s = self.selection.take();
                self.select(s, window, cx);
            }
        }
        if self.search_text != self.last_search {
            let query = self.search_text.trim().to_string();
            let changed_from = std::mem::replace(&mut self.last_search, self.search_text.clone());
            let e = engine.clone();
            self._pin_search = Some(cx.spawn(async move |this, cx| {
                let matches = e.hidden_conversations_matching(&query).await;
                // The search really changed and no longer is the PIN: the bypass of an earlier result ends.
                if matches.as_ref().is_none_or(|m| m.is_empty()) && !changed_from.is_empty() {
                    e.relock_hidden();
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            }));
        }
        let me = cx.entity().downgrade();
        let on_unlocked: Rc<dyn Fn(&str, &mut App)> = Rc::new(move |id: &str, cx: &mut App| {
            let id = id.to_string();
            let _ = me.update(cx, |this, cx| {
                this.pending_open = Some(id);
                this.selection = None;
                cx.notify();
            });
        });
        self.prompts.sync(on_unlocked, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold("Nguyễn Đức Anh"), "nguyen duc anh");
        assert!(fold("Gia Đình").contains(&fold("gia dinh")));
    }
}

#[allow(dead_code)]
fn unused(_: Rc<()>) {}
