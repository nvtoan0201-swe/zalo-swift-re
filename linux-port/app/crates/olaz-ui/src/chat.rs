//! Chat pane: toolbar header, message list, composer.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement, ListAlignment,
    ListState, ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, list,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::button::ButtonVariants;
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use olaz_engine::models::{Availability, CardActionOutcome, ChatMessage, Kind, MenuDelete, MenuStar, Mention, Status};

use crate::app::engine;
use crate::chrome::{self, TITLEBAR_HEIGHT};
use crate::format;
use crate::icon::icon;
use crate::shell::{SELF_NAME, splice_keys};
use crate::theme::{Theme, with_alpha};
use crate::widgets::{avatar, remote_picture, spinner};

pub const GROUP_GAP_MS: f64 = 300_000.0;

/// One row of the flattened message list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Loading,
    Day { id: String, date: f64 },
    Message { id: String, first: bool, last: bool, status: bool },
}

impl Row {
    fn key(&self) -> String {
        match self {
            Row::Loading => "loading".into(),
            Row::Day { id, .. } => format!("day-{id}"),
            Row::Message { id, first, last, status } => format!("m-{id}-{}{}{}", *first as u8, *last as u8, *status as u8),
        }
    }
}

/// Day separators, grouping (same sender & side, < 300 s, same day, neither standalone) and the status line
/// under my most recent non-standalone message.
pub fn build_rows(messages: &[ChatMessage], loading_older: bool) -> Vec<Row> {
    let mut rows = Vec::with_capacity(messages.len() + 8);
    if loading_older {
        rows.push(Row::Loading);
    }
    let status_id = messages.iter().rev().find(|m| m.is_mine && !m.kind.is_standalone()).map(|m| m.id.clone());
    let grouped = |a: &ChatMessage, b: &ChatMessage| {
        !a.kind.is_standalone()
            && !b.kind.is_standalone()
            && a.sender_id == b.sender_id
            && a.is_mine == b.is_mine
            && (b.date - a.date).abs() < GROUP_GAP_MS
            && format::same_day(a.date, b.date)
    };
    for (i, m) in messages.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &messages[j]);
        let next = messages.get(i + 1);
        if prev.is_none_or(|p| !format::same_day(p.date, m.date)) {
            rows.push(Row::Day { id: m.id.clone(), date: m.date });
        }
        rows.push(Row::Message {
            id: m.id.clone(),
            first: prev.is_none_or(|p| !grouped(p, m)),
            last: next.is_none_or(|n| !grouped(m, n)),
            status: status_id.as_deref() == Some(m.id.as_str()),
        });
    }
    rows
}

pub fn status_text(s: Status) -> &'static str {
    match s {
        Status::Sending => "Đang gửi",
        Status::Sent => "Đã gửi",
        Status::Delivered => "Đã nhận",
        Status::Seen => "Đã xem",
        Status::Failed => "Gửi thất bại",
    }
}

/// `PhotoView` size: scale = min(280/w, 340/h, 1.5), at least 80×60; unknown → 240×180.
pub fn photo_size(w: i64, h: i64) -> (f32, f32) {
    if w > 0 && h > 0 {
        let s = (280.0 / w as f32).min(340.0 / h as f32).min(1.5);
        ((w as f32 * s).max(80.0), (h as f32 * s).max(60.0))
    } else {
        (240.0, 180.0)
    }
}

pub struct ChatView {
    pub conversation: String,
    list_state: ListState,
    keys: Vec<String>,
    rows: Vec<Row>,
    messages: Vec<ChatMessage>,
    composer: Entity<TextareaState>,
    draft: String,
    reply_to: Option<ChatMessage>,
    hovered: Option<String>,
    picker_for: Option<String>,
    loading_older: bool,
    older_requested_at: Option<std::time::Instant>,
    last_marked: Option<String>,
    near_top: bool,
    _load_task: Option<Task<()>>,
    send_error: Option<String>,
    plus_menu: bool,
    forward: Option<Entity<crate::message_actions::ForwardSheet>>,
    sticker_picker: Option<Entity<crate::stickers::StickerPicker>>,
    emoji_picker: Option<Entity<crate::emoji_picker::EmojiPicker>>,
    inspector: Option<Entity<crate::inspector::Inspector>>,
    poll_detail: Option<Entity<crate::polls::PollDetailSheet>>,
    /// "Biểu cảm" popover of a message's reactions.
    reaction_detail: Option<Entity<crate::reactions::ReactionDetailView>>,
    poll_create: Option<Entity<crate::polls::PollCreateSheet>>,
    card_sheet: Option<crate::cards::CardSheetView>,
    mention_query: Option<(usize, String)>,
    mention_candidates: Vec<olaz_engine::models::MentionCandidate>,
    mention_selected: usize,
    picked_mentions: Vec<crate::mentions::Picked>,
    _mention_task: Option<Task<()>>,
    typing_stickers: olaz_engine::stickers::TypingStickerState,
    suggest_sending: Option<String>,
    _suggest: Option<Task<()>>,
    pending_suggestion: Option<(olaz_engine::stickers::StickerEntry, usize)>,
    pins_expanded: bool,
    _undo_tick: Option<Task<()>>,
}

impl ChatView {
    pub fn new(conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let engine = engine(cx);
        let name = conversation_name(&conversation, cx);
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 6).submit_on_enter(true).placeholder(format!("Tin nhắn tới {name}"))
        });
        cx.subscribe_in(&composer, window, |this, state, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                this.draft = state.read(cx).value().to_string();
                let cursor = state.read(cx).cursor();
                this.update_mention_query(cursor, cx);
                this.typing_stickers.dismissed = false;
                this.schedule_sticker_suggestions(cx);
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } if !*shift => {
                if this.mentions_open() {
                    this.pick_mention(this.mention_selected, window, cx);
                } else {
                    this.send(window, cx)
                }
            }
            _ => {}
        })
        .detach();
        composer.update(cx, |s, cx| s.focus(window, cx));
        // Failed draft of this conversation comes back.
        let restored = engine.state.borrow_mut().failed_drafts.take(&conversation);
        if let Some(d) = restored {
            composer.update(cx, |s, cx| s.set_value(d.text.clone(), window, cx));
        }
        let list_state = ListState::new(0, ListAlignment::Bottom, px(600.0));
        list_state.set_follow_mode(gpui::FollowMode::Tail);
        let me = cx.entity().downgrade();
        list_state.set_scroll_handler(move |ev, _, cx| {
            let at_top = ev.visible_range.start == 0;
            let _ = me.update(cx, |this, cx| {
                if at_top != this.near_top {
                    this.near_top = at_top;
                    if at_top {
                        this.maybe_load_older(cx);
                    }
                }
            });
        });
        Self {
            conversation,
            list_state,
            keys: Vec::new(),
            rows: Vec::new(),
            messages: Vec::new(),
            composer,
            draft: String::new(),
            reply_to: None,
            hovered: None,
            picker_for: None,
            loading_older: false,
            older_requested_at: None,
            last_marked: None,
            near_top: false,
            _load_task: None,
            send_error: None,
            plus_menu: false,
            forward: None,
            sticker_picker: None,
            emoji_picker: None,
            inspector: None,
            poll_detail: None,
            reaction_detail: None,
            poll_create: None,
            card_sheet: None,
            mention_query: None,
            mention_candidates: vec![],
            mention_selected: 0,
            picked_mentions: vec![],
            _mention_task: None,
            typing_stickers: Default::default(),
            suggest_sending: None,
            _suggest: None,
            pending_suggestion: None,
            pins_expanded: false,
            _undo_tick: None,
        }
    }

    fn maybe_load_older(&mut self, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let has_more = engine.state.borrow().has_more_history.get(&self.conversation).copied().unwrap_or(false);
        if !has_more || self.loading_older || self.messages.is_empty() {
            return;
        }
        // Debounce 250 ms, spinner gives up after 6 s.
        let conv = self.conversation.clone();
        self._load_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update(cx, |this, cx| {
                if !this.near_top {
                    return;
                }
                this.loading_older = true;
                this.older_requested_at = Some(std::time::Instant::now());
                engine.load_older_messages(&conv);
                cx.notify();
            });
            cx.background_executor().timer(Duration::from_secs(6)).await;
            let _ = this.update(cx, |this, cx| {
                this.loading_older = false;
                cx.notify();
            });
        }));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let reply = self.reply_to.take();
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.draft.clear();
        self.list_state.scroll_to_end();
        let engine = engine(cx);
        let conv = self.conversation.clone();
        let mentions: Vec<Mention> = crate::mentions::resolve(&text, &self.picked_mentions);
        self.picked_mentions.clear();
        self.close_mentions();
        cx.spawn(async move |this, cx| {
            let ok = engine.send_text(&text, &conv, reply.as_ref().map(|m| m.id.as_str()), &mentions).await;
            if !ok {
                let draft = olaz_engine::models::ComposerDraft { text: text.clone(), mentions, reply_to: reply.map(Box::new) };
                engine.state.borrow_mut().failed_drafts.stash(draft, &conv, false);
                let _ = this.update(cx, |this, cx| {
                    this.send_error = Some("Nội dung đã được trả lại vào ô soạn thảo. Hãy thử gửi lại.".into());
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// "Ảnh hoặc Video" / "Tệp": portal file chooser → official upload path.
    fn pick_and_send(&mut self, media: bool, cx: &mut Context<Self>) {
        self.plus_menu = false;
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(if media { "Chọn ảnh hoặc video".into() } else { "Chọn tệp".into() }),
        });
        let conv = self.conversation.clone();
        let engine = engine(cx);
        cx.spawn(async move |_, _| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let outcome = if media { engine.send_media(paths, &conv).await } else { engine.send_files(paths, &conv).await };
            engine.report(&outcome, &conv);
        })
        .detach();
        cx.notify();
    }

    /// Dropped files: pictures and videos through the photo path, everything else as files.
    fn send_dropped(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let is_media = |p: &std::path::PathBuf| {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "bmp" | "jxl" | "mp4" | "mov" | "mkv" | "webm")
        };
        let (media, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(is_media);
        let conv = self.conversation.clone();
        let engine = engine(cx);
        cx.spawn(async move |_, _| {
            if !media.is_empty() {
                let o = engine.send_media(media, &conv).await;
                engine.report(&o, &conv);
            }
            if !files.is_empty() {
                let o = engine.send_files(files, &conv).await;
                engine.report(&o, &conv);
            }
        })
        .detach();
    }

    fn send_my_card(&mut self, cx: &mut Context<Self>) {
        self.plus_menu = false;
        let conv = self.conversation.clone();
        let engine = engine(cx);
        cx.spawn(async move |_, _| {
            let Some(card) = engine.my_card().await else {
                engine.show_toast("Chưa đọc được hồ sơ của bạn từ Zalo. Hãy thử lại sau giây lát.", Some(&conv));
                return;
            };
            let o = engine.send_my_card(&conv, card.with_phone && !card.phone.is_empty()).await;
            engine.report(&o, &conv);
        })
        .detach();
        cx.notify();
    }

    fn send_like(&mut self, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let conv = self.conversation.clone();
        self.list_state.scroll_to_end();
        cx.spawn(async move |this, cx| {
            if !engine.send_text("👍", &conv, None, &[]).await {
                let _ = this.update(cx, |this, cx| {
                    this.send_error = Some("Không gửi được biểu tượng 👍. Hãy thử lại.".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Pull the engine's messages, rebuild rows, splice the list; mark read.
    fn sync(&mut self, window: &Window, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let messages = engine.state.borrow().messages.get(&self.conversation).cloned();
        let messages = messages.unwrap_or_default();
        if messages.len() > self.messages.len() && self.loading_older && messages.first().map(|m| &m.id) != self.messages.first().map(|m| &m.id) {
            self.loading_older = false;
        }
        // Restore draft from a failure that arrived while open.
        let restored = engine.state.borrow_mut().failed_drafts.take(&self.conversation);
        let _ = restored;
        self.messages = messages;
        let rows = build_rows(&self.messages, self.loading_older);
        let keys: Vec<String> = rows.iter().map(Row::key).collect();
        if keys != self.keys {
            splice_keys(&self.list_state, &self.keys, &keys);
            self.keys = keys;
        }
        self.rows = rows;
        self.sync_message_actions(window, cx);
        // Mark read: newest incoming not yet marked, while the window is active.
        if window.is_window_active() {
            let last_incoming = self.messages.iter().rev().find(|m| !m.is_mine).map(|m| m.id.clone());
            let unread = engine.state.borrow().conversations.iter().find(|c| c.id == self.conversation).map(|c| c.unread).unwrap_or(0);
            if (last_incoming.is_some() && last_incoming != self.last_marked) || unread > 0 {
                if self.last_marked != last_incoming || unread > 0 {
                    self.last_marked = last_incoming;
                    engine.mark_read(&self.conversation);
                }
            }
        }
    }

    fn mentions_open(&self) -> bool {
        self.mention_query.is_some() && !self.mention_candidates.is_empty()
    }

    fn close_mentions(&mut self) {
        self.mention_query = None;
        self.mention_candidates.clear();
        self.mention_selected = 0;
        self._mention_task = None;
    }

    /// Group chats: the "@query" before the cursor asks the official member list (debounced).
    fn update_mention_query(&mut self, cursor: usize, cx: &mut Context<Self>) {
        crate::mentions::prune(&self.draft, &mut self.picked_mentions);
        let is_group = engine(cx).state.borrow().conversations.iter().find(|c| c.id == self.conversation).is_some_and(|c| c.is_group);
        let q = if is_group { crate::mentions::mention_query(&self.draft, cursor) } else { None };
        if q == self.mention_query {
            return;
        }
        let Some((_, query)) = q.clone() else {
            self.close_mentions();
            return;
        };
        self.mention_query = q;
        let (e, conv) = (engine(cx), self.conversation.clone());
        let excluding: Vec<String> = self.picked_mentions.iter().map(|p| p.uid.clone()).collect();
        self._mention_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let found = e.mention_candidates(&conv, &query, &excluding).await;
            let _ = this.update(cx, |this, cx| {
                if this.mention_query.as_ref().is_some_and(|(_, q)| *q == query) {
                    this.mention_candidates = found.into_iter().take(8).collect();
                    this.mention_selected = 0;
                    cx.notify();
                }
            });
        }));
    }

    /// Replaces "@query" with "@Name " and remembers the mention.
    fn pick_mention(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((start, _)), Some(c)) = (self.mention_query.clone(), self.mention_candidates.get(index).cloned()) else { return };
        let label = format!("@{}", if c.is_all { "All" } else { c.name.as_str() });
        let cursor = self.composer.read(cx).cursor();
        let replacement = format!("{label} ");
        self.composer.update(cx, |s, cx| {
            s.set_selected_range(start..cursor, cx);
            s.replace(replacement, window, cx);
        });
        self.picked_mentions.push(crate::mentions::Picked { uid: c.id.clone(), label, is_all: c.is_all });
        self.draft = self.composer.read(cx).value().to_string();
        self.close_mentions();
        cx.notify();
    }

    fn render_mentions(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.mentions_open() {
            return None;
        }
        let mut list = div().w(px(320.0)).p(px(4.0)).rounded(px(12.0)).bg(theme.content_bg()).border_1().border_color(theme.separator()).shadow_lg().flex().flex_col();
        for (i, c) in self.mention_candidates.clone().into_iter().enumerate() {
            let sel = i == self.mention_selected;
            let av = if c.is_all {
                div().size(px(28.0)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(icon("at", 13.0, theme.white())).into_any_element()
            } else {
                avatar(c.avatar.as_deref(), &c.name, 28.0, cx)
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("mention-{i}")))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                    .when(!sel, |el| el.hover(|s| s.bg(theme.hover())))
                    .child(av)
                    .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.0)).child(if c.is_all { "Báo cho cả nhóm".to_string() } else { c.name.clone() }))
                    .when(c.is_all, |el| el.child(div().text_size(px(12.0)).child("@All")))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_mention(i, window, cx))),
            );
        }
        Some(list.into_any_element())
    }

    /// Asks the official suggestion logic for stickers matching the draft, after the official debounce. Only while "Gợi ý Sticker"
    /// is on (typed text never leaves the composer otherwise).
    fn schedule_sticker_suggestions(&mut self, cx: &mut Context<Self>) {
        let e = engine(cx);
        let text = self.draft.clone();
        let conv = self.conversation.clone();
        if text.trim().is_empty() {
            self.typing_stickers.clear();
            self._suggest = None;
            return;
        }
        self._suggest = Some(cx.spawn(async move |this, cx| {
            e.load_sticker_suggest_if_needed().await;
            let setting = e.state.borrow().sticker_suggest;
            if setting.is_some_and(|s| !s.enabled) || setting.is_none() {
                return;
            }
            let delay = olaz_engine::stickers::StickerSuggestSetting::delay(setting.as_ref());
            cx.background_executor().timer(Duration::from_millis(delay as u64)).await;
            if this.update(cx, |this, _| this.typing_stickers.is_held(&text)).unwrap_or(true) {
                return;
            }
            let found = e.typing_sticker_suggestions(&conv, &text).await;
            let _ = this.update(cx, |this, cx| {
                if this.draft == text {
                    this.typing_stickers.offer(found, &text);
                    cx.notify();
                }
            });
        }));
    }

    fn send_suggested_sticker(&mut self, sticker: olaz_engine::stickers::StickerEntry, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.suggest_sending.is_some() {
            return;
        }
        let draft = olaz_engine::models::ComposerDraft { text: self.draft.clone(), mentions: vec![], reply_to: self.reply_to.clone().map(Box::new) };
        let taken = self.typing_stickers.take(draft);
        // The click takes the whole box first (official `_clearBoxChatAndClosePopup`).
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.draft.clear();
        self.suggest_sending = Some(sticker.id());
        let (e, conv) = (engine(cx), self.conversation.clone());
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let outcome = e.send_sticker(&sticker, &conv, olaz_engine::stickers::StickerSource::Suggestion, Some(&taken.typed), index as i64).await;
            let _ = this.update(cx, |this, cx| {
                this.suggest_sending = None;
                match outcome {
                    CardActionOutcome::Done => {}
                    CardActionOutcome::Pending(t) => {
                        this.typing_stickers.hold(&taken.typed);
                        this.send_error = Some(t);
                    }
                    CardActionOutcome::Locked(t) | CardActionOutcome::Failed(t) | CardActionOutcome::Unsupported(t) => {
                        // Give the box back.
                        let text = taken.draft.text.clone();
                        let composer = this.composer.clone();
                        let _ = window_handle.update(cx, |_, window, cx| composer.update(cx, |s, cx| s.set_value(text, window, cx)));
                        this.send_error = Some(t);
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn present_poll_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::polls::{PollRequest, PollSheetEvent};
        let Some(r) = crate::polls::take_request() else { return };
        match r {
            PollRequest::Detail { poll_id, conversation, question, voters } => {
                let sheet = cx.new(|cx| crate::polls::PollDetailSheet::new(poll_id, conversation, question, voters, cx));
                cx.subscribe(&sheet, |this, _, _: &PollSheetEvent, cx| {
                    this.poll_detail = None;
                    cx.notify();
                })
                .detach();
                self.poll_detail = Some(sheet);
            }
            PollRequest::Create { conversation } => {
                let sheet = cx.new(|cx| crate::polls::PollCreateSheet::new(conversation, window, cx));
                cx.subscribe(&sheet, |this, _, _: &PollSheetEvent, cx| {
                    this.poll_create = None;
                    cx.notify();
                })
                .detach();
                self.poll_create = Some(sheet);
            }
        }
    }

    fn toggle_emoji_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.emoji_picker.take().is_none() {
            self.sticker_picker = None;
            let picker = cx.new(|cx| crate::emoji_picker::EmojiPicker::new(window, cx));
            cx.subscribe_in(&picker, window, |this, _, ev: &crate::emoji_picker::EmojiPicked, window, cx| {
                // Insert at the cursor (replacing a selection), keep the picker open.
                this.composer.update(cx, |s, cx| s.replace(ev.0.clone(), window, cx));
                this.draft = this.composer.read(cx).value().to_string();
                cx.notify();
            })
            .detach();
            self.emoji_picker = Some(picker);
        }
        cx.notify();
    }

    fn toggle_sticker_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sticker_picker.take().is_some() {
            cx.notify();
            return;
        }
        self.emoji_picker = None;
        let conv = self.conversation.clone();
        let picker = cx.new(|cx| crate::stickers::StickerPicker::new(conv, window, cx));
        cx.subscribe(&picker, |this, _, _: &crate::stickers::StickerSent, cx| {
            this.sticker_picker = None;
            cx.notify();
        })
        .detach();
        self.sticker_picker = Some(picker);
        cx.notify();
    }

    /// Presents the forward picker of this conversation and ticks the undo countdown.
    fn sync_message_actions(&mut self, window: &Window, cx: &mut Context<Self>) {
        let engine = engine(cx);
        let request = engine.state.borrow().forward_request.clone().filter(|r| r.conversation_id == self.conversation);
        match request {
            Some(r) if self.forward.as_ref().is_none_or(|f| f.read(cx).request != r) => {
                let window_handle = window.window_handle();
                let me = cx.entity().downgrade();
                // Creating an entity that needs `&mut Window` must happen outside this borrow of the window.
                cx.defer(move |cx| {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        let sheet = cx.new(|cx| crate::message_actions::ForwardSheet::new(r, window, cx));
                        let _ = me.update(cx, |this, cx| {
                            this.forward = Some(sheet);
                            cx.notify();
                        });
                    });
                });
            }
            None => self.forward = None,
            _ => {}
        }
        let undo = engine.state.borrow().delete_undo.clone().filter(|u| u.conversation_id == self.conversation);
        if undo.is_some() && self._undo_tick.is_none() {
            self._undo_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    let keep = this
                        .update(cx, |this, cx| {
                            cx.notify();
                            crate::app::engine(cx).state.borrow().delete_undo.as_ref().is_some_and(|u| u.conversation_id == this.conversation)
                        })
                        .unwrap_or(false);
                    if !keep {
                        let _ = this.update(cx, |this, _| this._undo_tick = None);
                        break;
                    }
                }
            }));
        }
    }

    /// Scrolls to a pinned message if it is loaded; other pins show their text.
    fn open_pin(&mut self, item: &olaz_engine::message_actions::PinnedItem, cx: &mut Context<Self>) {
        use olaz_engine::message_actions::PinKind;
        let engine = engine(cx);
        self.pins_expanded = false;
        match item.kind {
            PinKind::Message => match item.message_in(&self.messages) {
                Some(target) => {
                    let id = target.id.clone();
                    if let Some(ix) = self.rows.iter().position(|r| matches!(r, Row::Message { id: rid, .. } if *rid == id)) {
                        self.list_state.scroll_to_reveal_item(ix);
                        self.hovered = Some(id);
                    }
                }
                None => engine.show_toast("Tin nhắn đã ghim chưa được tải. Kéo lên để xem tin nhắn cũ hơn.", Some(&self.conversation)),
            },
            _ => engine.show_toast(if item.title.is_empty() { item.label() } else { &item.title }, Some(&self.conversation)),
        }
        cx.notify();
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let (conv, typing, self_id) = {
            let s = engine.state.borrow();
            (s.conversations.iter().find(|c| c.id == self.conversation).cloned(), s.typing.get(&self.conversation).cloned(), s.self_conversation_id.clone())
        };
        let is_self = Some(&self.conversation) == self_id.as_ref();
        let name = conversation_name(&self.conversation, cx);
        let subtitle: Option<(String, Hsla)> = if let Some(t) = typing {
            Some((format!("{t} đang soạn tin…"), theme.accent))
        } else if let Some(c) = conv.as_ref().filter(|c| c.is_group && c.members > 0) {
            Some((format!("{} thành viên", c.members), theme.secondary()))
        } else if self.messages.last().is_some_and(|m| m.is_e2ee) {
            Some(("Mã hóa đầu cuối".into(), theme.secondary()))
        } else {
            None
        };
        // Without the adapter's data the menu would only offer the mute items, which stay in the sidebar row menu.
        let more_menu = if engine.state.borrow().conversation_actions.loaded {
            engine.conversation_menu_plan(&self.conversation, olaz_engine::conversation_actions::MenuPreset::ChatBox)
        } else {
            Default::default()
        };
        let av: AnyElement = if is_self {
            div().size(px(30.0)).rounded_full().bg(theme.content_bg()).border_1().border_color(theme.separator()).flex().items_center().justify_center().child(icon("cloud", 13.0, theme.accent)).into_any_element()
        } else {
            avatar(conv.as_ref().and_then(|c| c.avatar.as_deref()), &name, 30.0, cx)
        };
        chrome::drag_area("chat-toolbar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .pl(px(16.0))
            // The window buttons sit over the inspector when it shows.
            .pr(px(if crate::inspector::SHOWN.with(|s| s.get()) { 16.0 } else { 130.0 }))
            .flex()
            .items_center()
            .gap(px(10.0))
            .border_b_1()
            .border_color(theme.separator())
            .child(av)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(crate::widgets::static_text(div().truncate().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD), &name))
                    .when_some(subtitle, |el, (text, color)| el.child(div().text_size(px(11.5)).text_color(color).child(text))),
            )
            .child(
                div()
                    .id("toggle-inspector")
                    .size(px(28.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(theme.hover()))
                    .child(icon("info.circle", 15.0, if crate::inspector::SHOWN.with(|s| s.get()) { theme.accent } else { theme.secondary() }))
                    .on_click(|_, window, _| {
                        crate::inspector::toggle_shown();
                        window.refresh();
                    }),
            )
            .when(!more_menu.is_empty(), |el| {
                let conv = self.conversation.clone();
                el.child(
                    gpui_component::button::Button::new("conv-more")
                        .ghost()
                        .compact()
                        .rounded_full()
                        .child(icon("ellipsis", 14.0, theme.secondary()))
                        .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, window, cx| {
                            crate::conversation_actions::conversation_menu(menu, &conv, olaz_engine::conversation_actions::MenuPreset::ChatBox, window, cx)
                        }),
                )
            })
            .into_any_element()
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        match row {
            Row::Loading => div().w_full().py(px(10.0)).flex().justify_center().child(spinner(14.0, theme.secondary())).into_any_element(),
            Row::Day { date, .. } => {
                let now = crate::format::now();
                div()
                    .w_full()
                    .pt(px(14.0))
                    .pb(px(6.0))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .gap(px(4.0))
                            .px(px(11.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(theme.glass())
                            .border_1()
                            .border_color(theme.glass_border())
                            .text_size(px(11.0))
                            .text_color(theme.secondary())
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format::day_label(date, now)))
                            .child(format::hhmm(date)),
                    )
                    .into_any_element()
            }
            Row::Message { id, first, last, status } => {
                let Some(m) = self.messages.iter().find(|m| m.id == id).cloned() else { return div().into_any_element() };
                self.render_message(&m, first, last, status, cx)
            }
        }
    }

    fn render_message(&mut self, m: &ChatMessage, first: bool, last: bool, status: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let scale = theme.text_scale;
        let is_group = engine(cx).state.borrow().conversations.iter().find(|c| c.id == self.conversation).is_some_and(|c| c.is_group);
        match &m.kind {
            Kind::System => {
                return notice_row(None, &m.text, &theme).into_any_element();
            }
            Kind::Notice { symbol, text } => {
                return notice_row(Some(symbol), if text.is_empty() { &m.text } else { text }, &theme).into_any_element();
            }
            Kind::ECard(card) if card.style == olaz_engine::models::ECardStyle::Reminder => {
                return div().w_full().py(px(8.0)).flex().justify_center().child(crate::reminders::reminder_card(card, m, cx)).into_any_element();
            }
            Kind::ECard(card) => {
                let title = if card.title.is_empty() { m.text.clone() } else { card.title.clone() };
                let bar = crate::cards::button_bar(m, 320.0, &theme);
                let stickers = crate::cards::sticker_suggestions(m, cx);
                return div()
                    .w_full()
                    .py(px(8.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .w(px(320.0))
                            .rounded(px(16.0))
                            .overflow_hidden()
                            .bg(theme.incoming_bubble())
                            .flex()
                            .flex_col()
                            .when(card.image.is_some(), |el| el.child(remote_picture(card.image.as_deref(), 320.0, 160.0, 0.0, ObjectFit::Cover, cx)))
                            .child(
                                div()
                                    .p(px(12.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child(title))
                                    .when(!card.description.is_empty(), |el| el.child(div().text_size(px(12.5)).text_color(theme.secondary()).child(card.description.clone()))),
                            )
                            .children(bar),
                    )
                    .children(stickers.map(|s| div().w_full().flex().justify_center().child(s)))
                    .into_any_element();
            }
            _ => {}
        }
        let mine = m.is_mine;
        let hovered = self.hovered.as_deref() == Some(m.id.as_str()) || self.picker_for.as_deref() == Some(m.id.as_str());
        let content = self.render_content(m, last, cx);
        let card_bar = crate::cards::button_bar(m, 300.0, &theme);
        let sticker_row = crate::cards::sticker_suggestions(m, cx);
        let content = if card_bar.is_some() || sticker_row.is_some() {
            div().flex().flex_col().gap(px(4.0)).child(content).children(card_bar.map(|b| div().rounded(px(12.0)).bg(theme.incoming_bubble()).overflow_hidden().child(b))).children(sticker_row).into_any_element()
        } else {
            content
        };
        let reactions = (!m.reactions.is_empty()).then(|| {
            let (rconv, rid, rlist) = (self.conversation.clone(), m.id.clone(), m.reactions.clone());
            let mut chips = div()
                .id(SharedString::from(format!("reactions-{}", m.id)))
                .flex()
                .gap(px(3.0))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (c, i, r) = (rconv.clone(), rid.clone(), rlist.clone());
                    this.reaction_detail = Some(cx.new(|cx| crate::reactions::ReactionDetailView::new(c, i, r, cx)));
                    cx.notify();
                }));
            for r in &m.reactions {
                chips = chips.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .px(px(7.0))
                        .py(px(3.0))
                        .rounded_full()
                        .bg(if r.includes_me { with_alpha(theme.accent, 0.4) } else { theme.glass() })
                        .border_1()
                        .border_color(theme.glass_border())
                        .child(div().text_size(px(14.0)).child(r.icon.emoji.clone()))
                        .when(r.count > 1, |el| el.child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(r.count.to_string()))),
                );
            }
            chips
        });
        let msg_id = m.id.clone();
        let msg_for_reply = m.clone();
        let conv = self.conversation.clone();
        let hover_bar = hovered.then(|| {
            let mut bar = div()
                .flex()
                .items_center()
                .p(px(3.0))
                .rounded_full()
                .bg(theme.glass())
                .border_1()
                .border_color(theme.glass_border())
                .child(div().px(px(6.0)).text_size(px(10.5)).text_color(theme.secondary()).child(format::hhmm(m.date)));
            if m.is_e2ee {
                bar = bar.child(div().pr(px(4.0)).child(icon("lock.fill", 9.0, theme.tertiary())));
            }
            if m.kind != Kind::Recalled {
                let picker_id = msg_id.clone();
                bar = bar
                    .child(
                        div()
                            .id(SharedString::from(format!("react-{msg_id}")))
                            .w(px(26.0))
                            .h(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|s| s.bg(theme.hover()))
                            .child(icon("face.smiling", 13.0, theme.secondary()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.picker_for = if this.picker_for.as_deref() == Some(picker_id.as_str()) { None } else { Some(picker_id.clone()) };
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("reply-{msg_id}")))
                            .w(px(26.0))
                            .h(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|s| s.bg(theme.hover()))
                            .child(icon("arrowshape.turn.up.left", 13.0, theme.secondary()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.reply_to = Some(msg_for_reply.clone());
                                this.composer.update(cx, |s, cx| s.focus(window, cx));
                                cx.notify();
                            })),
                    );
            }
            bar
        });
        let picker = (self.picker_for.as_deref() == Some(m.id.as_str())).then(|| {
            let mut p = div().flex().gap(px(2.0)).p(px(6.0)).rounded_full().bg(theme.content_bg()).border_1().border_color(theme.separator()).shadow_md();
            for e in REACTION_EMOJIS {
                let (conv, id) = (conv.clone(), m.id.clone());
                p = p.child(
                    div()
                        .id(SharedString::from(format!("pick-{id}-{e}")))
                        .size(px(38.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(23.0))
                        
                        .hover(|s| s.bg(with_alpha(theme.accent, 0.22)))
                        .child(e)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            engine(cx).react(e, &id, &conv);
                            this.picker_for = None;
                            cx.notify();
                        })),
                );
            }
            p
        });
        let on_wallpaper = crate::wallpaper::is_active(cx);
        let sender_name = (is_group && !mine && first && !m.sender_name.is_empty()).then(|| {
            div().pl(px(12.0)).flex().child(crate::wallpaper::legible(
                div().text_size(px(11.5 * scale.min(1.12))).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(m.sender_name.clone()),
                on_wallpaper,
                &theme,
            ))
        });
        let status_line = (status && mine).then(|| {
            div().px(px(4.0)).pt(px(1.0)).flex().child(crate::wallpaper::legible(
                div().text_size(px(10.5)).text_color(if m.status == Status::Failed { theme.red() } else { theme.tertiary() }).child(status_text(m.status)),
                on_wallpaper,
                &theme,
            ))
        });
        let leading_avatar: Option<AnyElement> = (is_group && !mine).then(|| {
            if last {
                avatar(m.sender_avatar.as_deref(), &m.sender_name, 28.0, cx)
            } else {
                div().w(px(28.0)).flex_none().into_any_element()
            }
        });
        let id = m.id.clone();
        let id2 = m.id.clone();
        let menu_message = m.clone();
        let view = cx.entity().downgrade();
        let stack = div()
            .relative()
            .flex()
            .flex_col()
            .when(mine, |el| el.items_end())
            .when(!mine, |el| el.items_start())
            .gap(px(2.0))
            .max_w(px(520.0))
            .min_w(px(0.0))
            .flex_shrink(1.0)
            .children(sender_name)
            .child(
                div()
                    .relative()
                    .max_w_full()
                    .child(content)
                    .when_some(reactions, |el, r| {
                        // Like the macOS design: chips sit on the bubble's bottom edge, at its trailing end.
                        el.child(div().absolute().bottom(px(-19.0)).right(px(8.0)).child(r))
                    }),
            )
            .children(status_line)
            .when_some(hover_bar, |el, bar| el.child(div().absolute().bottom(px(-26.0)).when(mine, |el| el.right_0()).when(!mine, |el| el.left_0()).child(bar)))
            .when_some(picker, |el, p| el.child(div().absolute().top(px(-50.0)).when(mine, |el| el.right_0()).when(!mine, |el| el.left_0()).child(p)));
        div()
            .id(SharedString::from(format!("msg-{id}")))
            .w_full()
            .px(px(16.0))
            .pt(px(if first { 8.0 } else { 2.0 }))
            .pb(px(if m.reactions.is_empty() { 0.0 } else { 18.0 }))
            .when(hovered, |el| el.pb(px(26.0)))
            .flex()
            .items_end()
            .gap(px(10.0))
            .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                if *hovering {
                    this.hovered = Some(id.clone());
                } else if this.hovered.as_deref() == Some(id2.as_str()) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .when(mine, |el| el.child(div().flex_1().min_w(px(100.0))))
            .children(leading_avatar)
            .child(stack)
            .when(!mine, |el| el.child(div().flex_1().min_w(px(100.0))))
            .context_menu(move |menu, window, cx| message_menu(menu, &menu_message, view.clone(), window, cx))
            .into_any_element()
    }

    /// Bubble chrome: mine → accent fill + white text; others → incoming gray; muted → stroke.
    fn bubble(&self, m: &ChatMessage, last: bool, muted: bool, padding: (f32, f32), body: AnyElement, cx: &App) -> AnyElement {
        let theme = Theme::get(cx);
        let mine = m.is_mine;
        let (big, small) = (18.0, 6.0);
        let (tl, tr, bl, br) = if mine { (big, big, big, if last { 18.0 } else { small }) } else { (big, big, if last { 18.0 } else { small }, big) };
        let quote = m.quote.as_ref().map(|q| {
            div()
                .flex()
                .gap(px(6.0))
                .px(px(6.0))
                .py(px(4.0))
                .rounded(px(8.0))
                .bg(if mine { with_alpha(theme.white(), 0.18) } else { crate::theme::fade(theme.label(), 0.06) })
                .child(div().w(px(2.5)).rounded_full().bg(if mine { with_alpha(theme.white(), 0.8) } else { theme.accent }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(div().text_size(px(11.5 * theme.text_scale)).font_weight(FontWeight::SEMIBOLD).child(q.sender_name.clone()))
                        .child(div().text_size(px(12.0 * theme.text_scale)).opacity(0.85).line_clamp(2).child(q.text.clone())),
                )
        });
        div()
            .max_w_full()
            .px(px(padding.0))
            .py(px(padding.1))
            .rounded_tl(px(tl))
            .rounded_tr(px(tr))
            .rounded_bl(px(bl))
            .rounded_br(px(br))
            .when(muted, |el| el.border_1().border_color(crate::theme::fade(theme.label(), 0.2)).text_color(theme.secondary()))
            .when(!muted && mine, |el| el.bg(theme.accent).text_color(theme.white()))
            .when(!muted && !mine, |el| el.bg(theme.incoming_bubble()).text_color(theme.label()))
            .when(m.status == Status::Sending, |el| el.opacity(0.7))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .children(quote)
            .child(body)
            .into_any_element()
    }

    fn text_body(&self, m: &ChatMessage, cx: &App) -> AnyElement {
        let theme = Theme::get(cx);
        crate::richtext::message_text(&m.text, &m.mentions, m.is_mine, 14.0 * theme.text_scale, theme)
    }

    fn render_content(&mut self, m: &ChatMessage, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let scale = theme.text_scale;
        match &m.kind {
            Kind::Text => {
                let body = div().relative().child(self.text_body(m, cx)).children(crate::layout_dump::probe("AXStaticText", m.text.clone())).into_any_element();
                self.bubble(m, last, false, (12.0, 8.0), body, cx)
            }
            Kind::Recalled => self.bubble(
                m,
                last,
                true,
                (12.0, 8.0),
                div().text_size(px(13.5)).italic().child("Tin nhắn đã được thu hồi").into_any_element(),
                cx,
            ),
            Kind::Unsupported { raw_type } => {
                let label = if raw_type.is_empty() { m.text.clone() } else { format!("[{raw_type}]") };
                self.bubble(m, last, true, (12.0, 8.0), div().text_size(px(13.5)).child(label).into_any_element(), cx)
            }
            Kind::Placeholder { symbol, text } => self.bubble(
                m,
                last,
                true,
                (12.0, 8.0),
                div().flex().items_center().gap(px(6.0)).text_size(px(13.0)).child(icon(symbol, 13.0, theme.secondary())).child(text.clone()).into_any_element(),
                cx,
            ),
            Kind::Sticker { url } => remote_picture(url.as_deref(), 130.0, 130.0, 0.0, ObjectFit::Contain, cx),
            Kind::Gif { url } => div()
                .relative()
                .child(remote_picture(url.as_deref(), 260.0, 200.0, 16.0, ObjectFit::Cover, cx))
                .child(
                    div()
                        .absolute()
                        .bottom(px(8.0))
                        .left(px(8.0))
                        .px(px(6.0))
                        .py(px(2.0))
                        .rounded_full()
                        .bg(gpui::black().opacity(0.5))
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.white())
                        .child("GIF"),
                )
                .into_any_element(),
            Kind::Photo { url, width, height } => {
                let (w, h) = photo_size(*width, *height);
                let caption = (!m.text.is_empty() && !m.text.starts_with('[')).then(|| {
                    let body = self.text_body(m, cx);
                    self.bubble(m, false, false, (12.0, 8.0), body, cx)
                });
                let url_click = url.clone();
                let origin = (self.conversation.clone(), m.id.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("photo-{}", m.id)))
                            .cursor_pointer()
                            .child(crate::widgets::message_picture(&self.conversation, &m.id, url.as_deref(), olaz_engine::media::MediaKind::Photo, w, h, 16.0, cx))
                            .on_click(move |_, _, cx| {
                                if let Some(u) = &url_click {
                                    crate::viewer::open_photo_of(u, Some(origin.clone()), cx);
                                }
                            }),
                    )
                    .children(caption)
                    .into_any_element()
            }
            Kind::Video { thumbnail, duration, url } => {
                let link = url.clone();
                div()
                    .id(SharedString::from(format!("video-{}", m.id)))
                    .relative()
                    .cursor_pointer()
                    .child(crate::widgets::message_picture(&self.conversation, &m.id, thumbnail.as_deref(), olaz_engine::media::MediaKind::VideoThumb, 260.0, 160.0, 16.0, cx))
                    .child(
                        div().absolute().inset_0().flex().items_center().justify_center().child(
                            div().size(px(46.0)).rounded_full().bg(gpui::black().opacity(0.45)).flex().items_center().justify_center().child(icon("play.fill", 18.0, theme.white())),
                        ),
                    )
                    .when(*duration > 0, |el| {
                        el.child(
                            div()
                                .absolute()
                                .bottom(px(8.0))
                                .right(px(8.0))
                                .px(px(6.0))
                                .py(px(2.0))
                                .rounded_full()
                                .bg(gpui::black().opacity(0.5))
                                .text_size(px(10.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.white())
                                .child(format::clock(*duration)),
                        )
                    })
                    .on_click(move |_, _, cx| {
                        if let Some(u) = &link {
                            crate::viewer::open_video(u, cx);
                        }
                    })
                    .into_any_element()
            }
            Kind::Voice { url, duration } => {
                let mine = m.is_mine;
                let link = url.clone();
                let body = div()
                    .min_w(px(130.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("voice-{}", m.id)))
                            .size(px(28.0))
                            .rounded_full()
                            .bg(if mine { theme.white() } else { theme.accent })
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(icon("play.fill", 12.0, if mine { theme.accent } else { theme.white() }))
                            .on_click(move |_, _, cx| {
                                if let Some(u) = &link {
                                    crate::viewer::play_audio(u, cx);
                                }
                            }),
                    )
                    .child(icon("waveform", 20.0, if mine { theme.white() } else { theme.label() }))
                    .child(div().text_size(px(12.5 * scale)).child(format::clock(*duration)))
                    .into_any_element();
                self.bubble(m, last, false, (12.0, 8.0), body, cx)
            }
            Kind::File { name, size, url } => {
                let ext = std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                let symbol = match ext.as_str() {
                    "pdf" => "doc.richtext.fill",
                    "zip" | "rar" | "7z" | "gz" => "doc.zipper",
                    "xls" | "xlsx" | "csv" => "tablecells.fill",
                    "mp3" | "m4a" | "wav" => "music.note",
                    "mp4" | "mov" | "mkv" => "film.fill",
                    _ => "doc.fill",
                };
                let link = url.clone();
                let file_name = name.clone();
                let (save_link, save_name) = (url.clone(), name.clone());
                // Two sibling targets (no nested click handlers): the file info opens the file, ↓ saves it.
                div()
                    .id(SharedString::from(format!("file-{}", m.id)))
                    .p(px(10.0))
                    .rounded(px(16.0))
                    .bg(theme.incoming_bubble())
                    .when(m.status == Status::Sending, |el| el.opacity(0.7))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("file-open-{}", m.id)))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(div().size(px(38.0)).rounded(px(9.0)).bg(theme.accent).flex().items_center().justify_center().child(icon(symbol, 20.0, theme.white())))
                            .child(
                                div()
                                    .max_w(px(200.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(2).child(name.clone()))
                                    .child(div().text_size(px(11.5)).opacity(0.75).child(format::bytes((*size).max(0) as u64))),
                            )
                            .on_click(move |_, _, cx| {
                                if let Some(u) = &link {
                                    crate::viewer::open_file(u, &file_name, cx);
                                }
                            }),
                    )
                    .child(
                        // "Lưu về máy…": the system save dialog picks the folder and name.
                        // The whole right end of the card (full height) is the target, not just the 16-pt glyph.
                        div()
                            .id(SharedString::from(format!("file-save-{}", m.id)))
                            .self_stretch()
                            .w(px(40.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.hover()))
                            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Lưu về máy…").build(window, cx))
                            .child(icon("arrow.down.circle", 16.0, theme.secondary()))
                            .on_click(move |_, _, cx| {
                                if let Some(u) = &save_link {
                                    crate::viewer::save_file_as(u, &save_name, cx);
                                }
                            }),
                    )
                    .into_any_element()
            }
            Kind::Link { title, url, thumbnail, description, source } => {
                let show_text = !m.text.is_empty() && Some(&m.text) != url.as_ref() && &m.text != title && !m.text.starts_with("[Link]");
                let text_bubble = show_text.then(|| {
                    let body = self.text_body(m, cx);
                    self.bubble(m, false, false, (12.0, 8.0), body, cx)
                });
                let link = url.clone();
                let link_conv = m.conversation_id.clone();
                let card = div()
                    .id(SharedString::from(format!("link-{}", m.id)))
                    .w(px(280.0))
                    .rounded(px(16.0))
                    .overflow_hidden()
                    .bg(theme.incoming_bubble())
                    .cursor_pointer()
                    .when(thumbnail.is_some(), |el| el.child(remote_picture(thumbnail.as_deref(), 280.0, 140.0, 0.0, ObjectFit::Cover, cx)))
                    .child(
                        div()
                            .p(px(10.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(2).child(if title.is_empty() { url.clone().unwrap_or_default() } else { title.clone() }))
                            .when(!description.is_empty(), |el| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).line_clamp(2).child(description.clone())))
                            .when(!source.is_empty(), |el| el.child(div().text_size(px(11.0)).text_color(theme.tertiary()).child(source.clone()))),
                    )
                    .on_click(move |_, _, cx| {
                        if let Some(u) = &link {
                            crate::cards::open_link(&link_conv, u, cx);
                        }
                    });
                div().flex().flex_col().gap(px(3.0)).children(text_bubble).child(card).into_any_element()
            }
            Kind::ContactCard(card) => crate::cards::contact_card(card, m, cx),
            Kind::Location(info) => {
                let link = info.map_url();
                div()
                    .id(SharedString::from(format!("loc-{}", m.id)))
                    .w(px(260.0))
                    .rounded(px(16.0))
                    .overflow_hidden()
                    .bg(theme.incoming_bubble())
                    .cursor_pointer()
                    .when(info.thumbnail.is_some(), |el| el.child(remote_picture(info.thumbnail.as_deref(), 260.0, 130.0, 0.0, ObjectFit::Cover, cx)))
                    .child(
                        div()
                            .p(px(10.0))
                            .flex()
                            .gap(px(8.0))
                            .child(icon("mappin.circle.fill", 18.0, theme.red()))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(if info.title.is_empty() { "Vị trí".to_string() } else { info.title.clone() }))
                                    .when(!info.address.is_empty(), |el| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(info.address.clone()))),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        if let Some(u) = &link {
                            engine(cx).open_url(u);
                        }
                    })
                    .into_any_element()
            }
            Kind::Call { video, missed, duration, title, detail } => {
                let mine = m.is_mine;
                let ink = if *missed { theme.red() } else if mine { theme.white() } else { theme.accent };
                let symbol = match (video, missed) {
                    (true, false) => "video.fill",
                    (true, true) => "video.slash.fill",
                    (false, false) => "phone.fill",
                    (false, true) => "phone.arrow.down.left.fill",
                };
                let title = if !title.is_empty() { title.clone() } else if *video { "Cuộc gọi video".into() } else { "Cuộc gọi thoại".into() };
                let detail = if !detail.is_empty() {
                    detail.clone()
                } else if *missed {
                    "Cuộc gọi nhỡ".into()
                } else if *duration > 0 {
                    format::clock(*duration)
                } else {
                    "Đã kết thúc".into()
                };
                let body = div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().size(px(34.0)).rounded_full().bg(with_alpha(ink, 0.15)).flex().items_center().justify_center().child(icon(symbol, 17.0, ink)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).when(*missed, |el| el.text_color(theme.red())).child(title))
                            .child(div().text_size(px(11.5)).opacity(0.75).child(detail)),
                    )
                    .into_any_element();
                self.bubble(m, last, false, (10.0, 10.0), body, cx)
            }
            Kind::Poll(info) => crate::polls::poll_message(info, m, cx),
            Kind::OAMessage(parts) if !parts.is_empty() => {
                // Official OA message: a lead card (image, title, description) and rows; a click opens the page.
                let mut card = div().w(px(300.0)).rounded(px(16.0)).overflow_hidden().bg(theme.incoming_bubble()).flex().flex_col();
                for (i, part) in parts.iter().enumerate() {
                    let url = part.url.clone().and_then(|u| olaz_engine::models::sanitized_link(Some(u)));
                    let conv = m.conversation_id.clone();
                    let row = if part.is_header {
                        div()
                            .flex()
                            .flex_col()
                            .when(part.thumb.is_some(), |el| el.child(remote_picture(part.thumb.as_deref(), 300.0, 158.0, 0.0, ObjectFit::Cover, cx)))
                            .child(
                                div()
                                    .p(px(10.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(3).child(part.title.clone()))
                                    .when(!part.description.is_empty(), |el| el.child(div().text_size(px(12.5)).text_color(theme.secondary()).line_clamp(3).child(part.description.clone()))),
                            )
                    } else {
                        div()
                            .p(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(div().flex_1().min_w(px(0.0)).text_size(px(13.0)).line_clamp(3).child(part.title.clone()))
                            .when(part.thumb.is_some(), |el| el.child(remote_picture(part.thumb.as_deref(), 52.0, 52.0, 8.0, ObjectFit::Cover, cx)))
                    };
                    card = card.when(i > 0, |el| el.child(div().h(px(1.0)).bg(theme.separator()))).child(
                        div()
                            .id(SharedString::from(format!("oa-{}-{i}", m.id)))
                            .when(url.is_some(), |el| el.cursor_pointer().hover(|s| s.bg(theme.hover())))
                            .child(row)
                            .on_click(move |_, _, cx| {
                                if let Some(u) = &url {
                                    crate::cards::open_link(&conv, u, cx);
                                }
                            }),
                    );
                }
                card.into_any_element()
            }
            Kind::StickerSet { title, thumbnail } | Kind::ZInstant { title, thumbnail, .. } => div()
                .w(px(260.0))
                .p(px(10.0))
                .rounded(px(16.0))
                .bg(theme.incoming_bubble())
                .flex()
                .items_center()
                .gap(px(10.0))
                .when(thumbnail.is_some(), |el| el.child(remote_picture(thumbnail.as_deref(), 48.0, 48.0, 8.0, ObjectFit::Cover, cx)))
                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(if title.is_empty() { m.text.clone() } else { title.clone() }))
                .into_any_element(),
            _ => {
                let body = self.text_body(m, cx);
                self.bubble(m, last, false, (12.0, 8.0), body, cx)
            }
        }
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let typing = engine.state.borrow().typing.get(&self.conversation).cloned();
        let empty = self.draft.trim().is_empty();
        let reply = self.reply_to.clone().map(|r| {
            let who = if r.is_mine { "chính mình".to_string() } else { r.sender_name.clone() };
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(16.0))
                .bg(theme.glass())
                .border_1()
                .border_color(theme.glass_border())
                .child(div().w(px(2.5)).h(px(28.0)).rounded_full().bg(theme.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(div().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).child(format!("Trả lời {who}")))
                        .child(div().text_size(px(12.0)).text_color(theme.secondary()).truncate().child(if r.text.is_empty() { "Tin nhắn".to_string() } else { r.text.clone() })),
                )
                .child(
                    div()
                        .id("cancel-reply")
                        .cursor_pointer()
                        .child(icon("xmark", 12.0, theme.secondary()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reply_to = None;
                            cx.notify();
                        })),
                )
        });
        let setting = engine.state.borrow().sticker_suggest.map(|s| s.enabled);
        let suggestions = self.typing_stickers.is_visible(&self.draft, false, setting).then(|| {
            let me = cx.entity().downgrade();
            let me2 = me.clone();
            crate::stickers::typing_strip(
                &self.typing_stickers.stickers,
                self.suggest_sending.as_deref(),
                move |s, i, cx| {
                    let (s, me) = (s.clone(), me.clone());
                    // Needs the window for the composer: run on the next frame through the view.
                    let _ = me.update(cx, |this, cx| {
                        this.pending_suggestion = Some((s, i));
                        cx.notify();
                    });
                },
                move |cx| {
                    let _ = me2.update(cx, |this, cx| {
                        this.typing_stickers.dismissed = true;
                        cx.notify();
                    });
                },
                &theme,
                cx,
            )
        });
        let send: AnyElement = if empty {
            div()
                .id("send-like")
                .size(px(36.0))
                .flex_none()
                .rounded_full()
                .bg(theme.glass())
                .border_1()
                .border_color(theme.glass_border())
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(px(18.0))
                .child("👍")
                .on_click(cx.listener(|this, _, _, cx| this.send_like(cx)))
                .into_any_element()
        } else {
            div()
                .id("send")
                .size(px(36.0))
                .flex_none()
                .rounded_full()
                .bg(theme.accent)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(icon("arrow.up", 15.0, theme.white()))
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
                .into_any_element()
        };
        div()
            .flex_none()
            .px(px(16.0))
            .pt(px(8.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .when_some(typing, |el, name| {
                el.child(
                    div().flex().child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(11.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(theme.glass())
                            .border_1()
                            .border_color(theme.glass_border())
                            .text_size(px(11.5))
                            .text_color(theme.secondary())
                            .child(icon("ellipsis.bubble", 11.5, theme.secondary()))
                            .child(format!("{name} đang soạn tin…")),
                    ),
                )
            })
            .children(self.render_mentions(&theme, cx))
            .children(suggestions)
            .children(reply)
            .when_some(self.send_error.clone(), |el, e| {
                el.child(
                    div()
                        .id("send-error")
                        .flex()
                        .gap(px(6.0))
                        .items_center()
                        .text_size(px(12.0))
                        .text_color(theme.red())
                        .child(icon("exclamationmark.triangle.fill", 12.0, theme.red()))
                        .child(div().flex_1().child(e))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.send_error = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(8.0))
                    .child(self.render_plus(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(38.0))
                            .pl(px(14.0))
                            .pr(px(6.0))
                            .py(px(2.0))
                            .rounded(px(19.0))
                            .bg(theme.glass())
                            .border_1()
                            .border_color(theme.glass_border())
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    // The Textarea turns these keys into its own actions; take them first while suggestions show.
                                    .capture_action(cx.listener(|this, _: &gpui_component::input::MoveDown, _, cx| {
                                        if this.mentions_open() {
                                            this.mention_selected = (this.mention_selected + 1) % this.mention_candidates.len();
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                    }))
                                    .capture_action(cx.listener(|this, _: &gpui_component::input::MoveUp, _, cx| {
                                        if this.mentions_open() {
                                            let n = this.mention_candidates.len();
                                            this.mention_selected = (this.mention_selected + n - 1) % n;
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                    }))
                                    .capture_action(cx.listener(|this, _: &gpui_component::input::IndentInline, window, cx| {
                                        if this.mentions_open() {
                                            this.pick_mention(this.mention_selected, window, cx);
                                            cx.stop_propagation();
                                        }
                                    }))
                                    .capture_action(cx.listener(|this, _: &gpui_component::input::Escape, _, cx| {
                                        if this.mentions_open() {
                                            this.close_mentions();
                                            cx.stop_propagation();
                                            cx.notify();
                                        }
                                    }))
                                    .child(Textarea::new(&self.composer).appearance(false)),
                            )
                            .child(
                                div()
                                    .id("emoji-button")
                                    .flex_none()
                                    .size(px(28.0))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(theme.hover()))
                                    .when(self.emoji_picker.is_some(), |el| el.bg(with_alpha(theme.accent, 0.16)))
                                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Biểu tượng cảm xúc").build(window, cx))
                                    .child(icon("face.smiling", 15.0, if self.emoji_picker.is_some() { theme.accent } else { theme.secondary() }))
                                    .on_click(cx.listener(|this, _, window, cx| this.toggle_emoji_picker(window, cx))),
                            )
                            .child(
                                div()
                                    .id("sticker-button")
                                    .flex_none()
                                    .size(px(28.0))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|s| s.bg(theme.hover()))
                                    .when(self.sticker_picker.is_some(), |el| el.bg(with_alpha(theme.accent, 0.16)))
                                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Sticker").build(window, cx))
                                    .child(icon("face.dashed", 15.0, if self.sticker_picker.is_some() { theme.accent } else { theme.secondary() }))
                                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sticker_picker(window, cx))),
                            ),
                    )
                    .child(send),
            )
            .into_any_element()
    }
}

impl ChatView {
    fn render_plus(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let is_group = engine(cx).state.borrow().conversations.iter().find(|c| c.id == self.conversation).is_some_and(|c| c.is_group);
        let item = |id: &'static str, symbol: &'static str, title: &'static str| {
            div()
                .id(id)
                .h(px(28.0))
                .px(px(10.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .text_size(px(13.0))
                .cursor_pointer()
                .hover(|s| s.bg(with_alpha(theme.accent, 0.15)))
                .child(icon(symbol, 13.0, theme.label()))
                .child(title)
        };
        let _ = is_group;
        div()
            .relative()
            .pr(px(8.0))
            .child(
                div()
                    .id("plus")
                    .size(px(36.0))
                    .rounded_full()
                    .bg(theme.glass())
                    .border_1()
                    .border_color(theme.glass_border())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(icon("plus", 16.0, theme.label()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.plus_menu = !this.plus_menu;
                        cx.notify();
                    })),
            )
            .when(self.plus_menu, |el| {
                el.child(
                    div()
                        .absolute()
                        .bottom(px(44.0))
                        .left_0()
                        .w(px(220.0))
                        .p(px(5.0))
                        .rounded(px(10.0))
                        .bg(theme.content_bg())
                        .border_1()
                        .border_color(theme.separator())
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .child(item("plus-media", "photo.on.rectangle", "Ảnh hoặc Video").on_click(cx.listener(|this, _, _, cx| this.pick_and_send(true, cx))))
                        .child(item("plus-file", "paperclip", "Tệp").on_click(cx.listener(|this, _, _, cx| this.pick_and_send(false, cx))))
                        .child(item("plus-card", "person.crop.rectangle", "Danh thiếp của tôi").on_click(cx.listener(|this, _, _, cx| this.send_my_card(cx))))
                        .when(is_group, |el| {
                            el.child(item("plus-poll", "chart.bar.fill", "Bình chọn").on_click(cx.listener(|this, _, _, cx| {
                                this.plus_menu = false;
                                crate::polls::request(crate::polls::PollRequest::Create { conversation: this.conversation.clone() }, cx);
                            })))
                        }),
                )
            })
            .into_any_element()
    }
}

fn notice_row(symbol: Option<&String>, text: &str, theme: &Theme) -> impl IntoElement {
    div().w_full().py(px(6.0)).px(px(40.0)).flex().justify_center().child(
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(4.0))
            .rounded_full()
            .bg(theme.glass())
            .text_size(px(12.0))
            .text_color(theme.secondary())
            .when_some(symbol, |el, s| el.child(icon(s, 12.0, theme.secondary())))
            .child(div().text_center().child(text.to_string())),
    )
}

pub fn conversation_name(id: &str, cx: &App) -> String {
    let engine = engine(cx);
    let s = engine.state.borrow();
    if s.self_conversation_id.as_deref() == Some(id) {
        return SELF_NAME.into();
    }
    s.conversations.iter().find(|c| c.id == id).map(|c| c.display_name()).unwrap_or_else(|| "Người dùng Zalo".into())
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        self.sync(window, cx);
        if let Some((sticker, index)) = self.pending_suggestion.take() {
            self.send_suggested_sticker(sticker, index, window, cx);
        }
        self.present_poll_request(window, cx);
        if let Some(r) = crate::cards::take_request() {
            self.card_sheet = Some(crate::cards::build(r, window, cx, |this: &mut ChatView, cx| {
                this.card_sheet = None;
                cx.notify();
            }));
        }
        let header = self.render_header(cx);
        let loaded = engine(cx).state.borrow().messages.contains_key(&self.conversation);
        let body: AnyElement = if !loaded {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child(spinner(14.0, theme.secondary()))
                .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Đang tải tin nhắn…"))
                .into_any_element()
        } else if self.messages.is_empty() {
            let name = conversation_name(&self.conversation, cx);
            div()
                .flex_1()
                .child(crate::widgets::content_unavailable(
                    "bubble.left",
                    "Chưa có tin nhắn",
                    &format!("Tin nhắn cũ nằm trên điện thoại của bạn. Đồng bộ để xem lịch sử trò chuyện với {name} trên máy tính."),
                    &theme,
                ))
                .into_any_element()
        } else {
            let me = cx.entity().downgrade();
            div()
                .flex_1()
                .min_h(px(0.0))
                .pt(px(8.0))
                .child(
                    list(self.list_state.clone(), move |ix, _window, cx| match me.upgrade() {
                        Some(view) => view.update(cx, |this, cx| this.render_row(ix, cx)),
                        None => div().into_any_element(),
                    })
                    .size_full(),
                )
                .into_any_element()
        };
        let composer = self.render_composer(cx);
        let pins = engine(cx).state.borrow().pins.get(&self.conversation).cloned();
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        let pinned_bar = pins.and_then(|list| {
            crate::message_actions::pinned_bar(
                &self.conversation,
                &list,
                self.pins_expanded,
                move |item, _, cx| {
                    let _ = me.update(cx, |this, cx| this.open_pin(item, cx));
                },
                move |_, cx| {
                    let _ = me2.update(cx, |this, cx| {
                        this.pins_expanded = !this.pins_expanded;
                        cx.notify();
                    });
                },
                cx,
            )
        });
        let undo = engine(cx).state.borrow().delete_undo.clone().filter(|u| u.conversation_id == self.conversation);
        let forward = self.forward.clone();
        let toast = engine(cx).state.borrow().toast.clone().filter(|t| t.conversation.as_ref().is_none_or(|c| c == &self.conversation));
        let chat = div()
            .id("chat-root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .rounded_tr(crate::chrome::inner_radius(window))
            .rounded_br(crate::chrome::inner_radius(window))
            .bg(theme.content_bg())
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| this.send_dropped(paths.0.to_vec(), cx)))
            .children(crate::wallpaper::background(crate::chrome::inner_radius(window), cx))
            .child(header)
            .children(pinned_bar)
            .child(body)
            .child(composer)
            .when_some(
                self.poll_detail.clone().map(|p| p.into_any_element()).or(self.poll_create.clone().map(|p| p.into_any_element())).or(self.card_sheet.as_ref().map(|c| c.element())),
                |el, sheet| {
                el.child(
                    div()
                        .id("poll-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(sheet),
                )
            })
            .when_some(self.reaction_detail.clone(), |el, detail| {
                el.child(
                    div()
                        .id("reaction-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.reaction_detail = None;
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                this.reaction_detail = None;
                                cx.notify();
                            }
                        }))
                        .child(div().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(detail)),
                )
            })
            .when_some(self.sticker_picker.clone(), |el, p| el.child(div().absolute().bottom(px(64.0)).right(px(60.0)).child(p)))
            .when_some(self.emoji_picker.clone(), |el, p| {
                el.child(
                    div()
                        .id("emoji-backdrop")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.emoji_picker = None;
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                this.emoji_picker = None;
                                cx.notify();
                            }
                        }))
                        .child(div().absolute().bottom(px(64.0)).right(px(60.0)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(p)),
                )
            })
            .when_some(undo, |el, u| {
                el.child(
                    div()
                        .absolute()
                        .bottom(px(84.0))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(crate::message_actions::delete_undo_bar(&u, &theme)),
                )
            })
            .when_some(forward, |el, f| {
                el.child(
                    div()
                        .id("forward-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(f),
                )
            })
            .when_some(toast, |el, t| {
                el.child(
                    div().absolute().top(px(TITLEBAR_HEIGHT + 12.0)).left_0().right_0().flex().justify_center().child(
                        div()
                            .max_w(px(420.0))
                            .px(px(14.0))
                            .py(px(8.0))
                            .rounded(px(12.0))
                            .bg(theme.content_bg())
                            .border_1()
                            .border_color(theme.separator())
                            .shadow_lg()
                            .text_size(px(12.5))
                            .child(t.text),
                    ),
                )
            });
        let shown = crate::inspector::SHOWN.with(|s| s.get());
        if shown && self.inspector.is_none() {
            let conv = self.conversation.clone();
            self.inspector = Some(cx.new(|cx| crate::inspector::Inspector::new(conv, cx)));
        }
        let inspector = self.inspector.clone().filter(|_| shown);
        div().size_full().flex().child(div().flex_1().min_w(px(0.0)).h_full().child(chat)).children(inspector)
    }
}

pub const REACTION_EMOJIS: [&str; 6] = ["❤️", "👍", "😆", "😮", "😢", "😡"];

/// What "Lưu về máy…" saves for a message: its media URL and a suggested file name.
fn save_target(m: &ChatMessage) -> Option<(String, String)> {
    let ext_of = |url: &str, fallback: &str| {
        let path = url.split(['?', '#']).next().unwrap_or("");
        std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| fallback.to_string())
    };
    match &m.kind {
        Kind::File { name, url: Some(u), .. } => Some((u.clone(), name.clone())),
        Kind::Photo { url: Some(u), .. } => Some((u.clone(), format!("zalo-{}.{}", m.id, ext_of(u, "jpg")))),
        Kind::Video { url: Some(u), .. } => Some((u.clone(), format!("zalo-{}.{}", m.id, ext_of(u, "mp4")))),
        Kind::Voice { url: Some(u), .. } => Some((u.clone(), format!("zalo-{}.{}", m.id, ext_of(u, "m4a")))),
        _ => None,
    }
}

/// One entry of the message context menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry {
    Reply,
    Forward { blocked: bool },
    CopyText(String),
    CopyPhoto,
    CopyCaption(String),
    /// Linux: "Lưu về máy…" through the system save dialog (url, suggested file name).
    SaveAs { url: String, name: String },
    React,
    Separator,
    Pin,
    Unpin,
    Star,
    Unstar,
    StarBlocked,
    ProtectedNote,
    Recall,
    Delete { plain: bool },
}

impl MenuEntry {
    pub fn label(&self) -> &'static str {
        match self {
            MenuEntry::Reply => "Trả lời",
            MenuEntry::Forward { .. } => "Chia sẻ",
            MenuEntry::CopyText(_) => "Copy tin nhắn",
            MenuEntry::CopyPhoto => "Copy hình ảnh",
            MenuEntry::CopyCaption(_) => "Copy chú thích",
            MenuEntry::SaveAs { .. } => "Lưu về máy…",
            MenuEntry::React => "Biểu cảm",
            MenuEntry::Separator => "",
            MenuEntry::Pin => "Ghim tin nhắn",
            MenuEntry::Unpin => "Bỏ ghim tin nhắn",
            MenuEntry::Star | MenuEntry::StarBlocked => "Đánh dấu tin nhắn",
            MenuEntry::Unstar => "Bỏ đánh dấu tin nhắn",
            MenuEntry::ProtectedNote => olaz_engine::message_actions::PROTECTED,
            MenuEntry::Recall => "Thu hồi",
            MenuEntry::Delete { plain: true } => "Xóa",
            MenuEntry::Delete { plain: false } => "Xóa chỉ ở phía tôi",
        }
    }
}

/// The official message menu (MessageBubble.swift), in its order: quick actions (reply, forward), copy, reactions, pin / star,
/// then recall and delete. Which items show comes from the official builder (`message.menu`); copying follows "Chặn chia sẻ".
pub fn menu_plan(m: &ChatMessage) -> Vec<MenuEntry> {
    let actions = &m.menu;
    let mut out = Vec::new();
    if m.kind != Kind::Recalled {
        out.push(MenuEntry::Reply);
        if actions.forward != Availability::Hidden {
            out.push(MenuEntry::Forward { blocked: actions.forward == Availability::Blocked });
        }
        if !actions.is_protected {
            if let Some(text) = m.copyable_text() {
                out.push(MenuEntry::CopyText(text));
            }
            if matches!(m.kind, Kind::Photo { .. }) {
                out.push(MenuEntry::CopyPhoto);
            }
            if let Some(caption) = m.caption() {
                out.push(MenuEntry::CopyCaption(caption));
            }
            if let Some((url, name)) = save_target(m) {
                out.push(MenuEntry::SaveAs { url, name });
            }
        }
        out.push(MenuEntry::React);
        if actions.pin || actions.star.is_some() {
            out.push(MenuEntry::Separator);
            if actions.pinned {
                out.push(MenuEntry::Unpin);
            } else if actions.pin {
                out.push(MenuEntry::Pin);
            }
            match actions.star {
                Some(MenuStar::Unstar) => out.push(MenuEntry::Unstar),
                Some(MenuStar::Star) => out.push(MenuEntry::Star),
                Some(MenuStar::Blocked) => out.push(MenuEntry::StarBlocked),
                None => {}
            }
        }
        if actions.is_protected {
            out.push(MenuEntry::ProtectedNote);
        }
    }
    if actions.recall || actions.delete.is_some() {
        out.push(MenuEntry::Separator);
        if actions.recall && m.kind != Kind::Recalled {
            out.push(MenuEntry::Recall);
        }
        if let Some(delete) = actions.delete {
            out.push(MenuEntry::Delete { plain: delete == MenuDelete::Plain });
        }
    }
    out
}

fn message_menu(menu: PopupMenu, m: &ChatMessage, view: gpui::WeakEntity<ChatView>, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let mut menu = menu.min_w(px(200.0));
    for entry in menu_plan(m) {
        let label = entry.label();
        let mm = m.clone();
        menu = match entry {
            MenuEntry::Separator => menu.separator(),
            MenuEntry::ProtectedNote => menu.item(PopupMenuItem::label(label)),
            MenuEntry::StarBlocked => menu.item(PopupMenuItem::new(label).disabled(true)),
            MenuEntry::React => {
                let (conv, id) = (m.conversation_id.clone(), m.id.clone());
                menu.submenu(label, window, cx, move |mut sub, _, _| {
                    for e in REACTION_EMOJIS {
                        let (conv, id) = (conv.clone(), id.clone());
                        sub = sub.item(
                            PopupMenuItem::element(move |_, _| div().text_size(px(16.0)).child(e))
                                .on_click(move |_, _, cx| engine(cx).react(e, &id, &conv)),
                        );
                    }
                    sub
                })
            }
            MenuEntry::Reply => {
                let v = view.clone();
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let mm = mm.clone();
                    v.update(cx, |this, cx| {
                        this.reply_to = Some(mm);
                        this.composer.update(cx, |s, cx| s.focus(window, cx));
                        cx.notify();
                    })
                    .ok();
                }))
            }
            entry => {
                let blocked = matches!(entry, MenuEntry::Forward { blocked: true });
                menu.item(PopupMenuItem::new(label).disabled(blocked).on_click(move |_, _, cx| {
                    let e = engine(cx);
                    match &entry {
                        MenuEntry::Forward { .. } => e.request_forward(&mm),
                        MenuEntry::CopyText(t) | MenuEntry::CopyCaption(t) => e.copy_text(t, &mm.conversation_id),
                        MenuEntry::CopyPhoto => e.copy_photo(&mm),
                        MenuEntry::SaveAs { url, name } => crate::viewer::save_file_as(url, name, cx),
                        MenuEntry::Pin => e.pin_message(&mm, None),
                        MenuEntry::Unpin => e.request_unpin_message(&mm),
                        MenuEntry::Star => e.set_starred(&mm, true),
                        MenuEntry::Unstar => e.set_starred(&mm, false),
                        MenuEntry::Recall => e.recall(&mm),
                        MenuEntry::Delete { .. } => e.delete_for_me(&mm, false),
                        _ => {}
                    }
                }))
            }
        };
    }
    menu
}

#[allow(dead_code)]
fn unused(_: FontStyle) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(id: &str, sender: &str, mine: bool, date: f64, kind: &str) -> ChatMessage {
        ChatMessage::from_json(&json!({"id": id, "senderId": sender, "isMine": mine, "date": date, "kind": {"type": kind}}), "c").unwrap()
    }

    #[test]
    fn rows_group_and_separate() {
        let base = chrono::Local::now().timestamp_millis() as f64 - 3_600_000.0;
        let ms = vec![
            msg("1", "a", false, base, "text"),
            msg("2", "a", false, base + 10_000.0, "text"),
            msg("3", "a", false, base + 400_000.0, "text"),
            msg("4", "me", true, base + 401_000.0, "text"),
            msg("5", "x", false, base + 402_000.0, "system"),
            msg("6", "me", true, base + 403_000.0, "text"),
        ];
        let rows = build_rows(&ms, false);
        assert!(matches!(rows[0], Row::Day { .. }));
        let flags: Vec<(String, bool, bool, bool)> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Message { id, first, last, status } => Some((id.clone(), *first, *last, *status)),
                _ => None,
            })
            .collect();
        assert_eq!(flags[0], ("1".into(), true, false, false));
        assert_eq!(flags[1], ("2".into(), false, true, false), "grouped < 300 s");
        assert_eq!(flags[2], ("3".into(), true, true, false), "≥ 300 s starts a new run");
        assert_eq!(flags[3], ("4".into(), true, true, false), "side change");
        assert_eq!(flags[5], ("6".into(), true, true, true), "status under my latest");
        let with_loading = build_rows(&ms, true);
        assert_eq!(with_loading[0], Row::Loading);
    }

    #[test]
    fn save_as_targets() {
        let m = |kind: serde_json::Value| ChatMessage::from_json(&serde_json::json!({"id": "42", "text": "", "kind": kind, "menu": {}}), "c").unwrap();
        let file = m(serde_json::json!({"type": "file", "name": "Báo cáo.xlsx", "size": 10, "url": "https://f.example.test/x/abc?sig=1"}));
        assert_eq!(save_target(&file), Some(("https://f.example.test/x/abc?sig=1".into(), "Báo cáo.xlsx".into())));
        assert!(menu_plan(&file).iter().any(|e| e.label() == "Lưu về máy…"));
        let photo = m(serde_json::json!({"type": "photo", "url": "https://p.example.test/a/b.webp?x=1"}));
        assert_eq!(save_target(&photo).map(|t| t.1), Some("zalo-42.webp".into()));
        let video = m(serde_json::json!({"type": "video", "url": "https://v.example.test/v"}));
        assert_eq!(save_target(&video).map(|t| t.1), Some("zalo-42.mp4".into()));
        let text = m(serde_json::json!({"type": "text"}));
        assert_eq!(save_target(&text), None);
    }

    #[test]
    fn menu_follows_the_official_order() {
        let m = |menu: serde_json::Value, kind: serde_json::Value, text: &str| {
            ChatMessage::from_json(&serde_json::json!({"id": "1", "text": text, "kind": kind, "menu": menu}), "c").unwrap()
        };
        let full = m(
            serde_json::json!({"forward": "ok", "recall": true, "delete": "forMe", "pin": true, "star": "star"}),
            serde_json::json!({"type": "text"}),
            "Xin chào",
        );
        let labels: Vec<&str> = menu_plan(&full).iter().map(MenuEntry::label).collect();
        assert_eq!(
            labels,
            ["Trả lời", "Chia sẻ", "Copy tin nhắn", "Biểu cảm", "", "Ghim tin nhắn", "Đánh dấu tin nhắn", "", "Thu hồi", "Xóa chỉ ở phía tôi"]
        );
        let protected = m(
            serde_json::json!({"forward": "blocked", "protected": true, "pinned": true, "pin": true, "star": "blocked", "delete": "plain"}),
            serde_json::json!({"type": "photo"}),
            "Biển",
        );
        assert_eq!(
            menu_plan(&protected),
            [
                MenuEntry::Reply,
                MenuEntry::Forward { blocked: true },
                MenuEntry::React,
                MenuEntry::Separator,
                MenuEntry::Unpin,
                MenuEntry::StarBlocked,
                MenuEntry::ProtectedNote,
                MenuEntry::Separator,
                MenuEntry::Delete { plain: true }
            ]
        );
        let photo = m(serde_json::json!({}), serde_json::json!({"type": "photo"}), "Biển");
        assert_eq!(menu_plan(&photo), [MenuEntry::Reply, MenuEntry::CopyPhoto, MenuEntry::CopyCaption("Biển".into()), MenuEntry::React]);
        let recalled = m(serde_json::json!({"recall": true, "delete": "plain"}), serde_json::json!({"type": "recalled"}), "");
        assert_eq!(menu_plan(&recalled), [MenuEntry::Separator, MenuEntry::Delete { plain: true }]);
    }

    #[test]
    fn photo_sizes() {
        assert_eq!(photo_size(0, 0), (240.0, 180.0));
        assert_eq!(photo_size(100, 100), (150.0, 150.0));
        assert_eq!(photo_size(4000, 3000), (280.0, 210.0));
        assert_eq!(photo_size(1000, 4000), (85.0, 340.0));
        assert_eq!(photo_size(10, 2000), (80.0, 340.0));
    }
}
