//! Native stand-ins for what the official message menu opens (MessageActionViews.swift): the "Chia sẻ" picker, the
//! "N tin nhắn đã xóa · Khôi phục" snackbar and the pinned bar above the chat.

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::message_actions::{DeleteUndo, ForwardRequest, MessageActionOutcome, PinKind, PinnedItem, PinnedList};
use olaz_engine::models::Kind;

use crate::app::engine;
use crate::icon::icon;
use crate::shell::fold;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, avatar, button, remote_picture};

/// Official `forward_conv_limit` (live config and default: 100).
pub const MAX_TARGETS: usize = 100;

pub struct ForwardSheet {
    pub request: ForwardRequest,
    search: Entity<InputState>,
    needle: String,
    selected: Vec<String>,
    busy: bool,
    failure: Option<String>,
}

impl ForwardSheet {
    pub fn new(request: ForwardRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Tìm kiếm hội thoại cần chia sẻ"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.needle = input.read(cx).value().trim().to_string();
                cx.notify();
            }
        })
        .detach();
        search.update(cx, |s, cx| s.focus(window, cx));
        ForwardSheet { request, search, needle: String::new(), selected: Vec::new(), busy: false, failure: None }
    }

    fn toggle(&mut self, id: &str) {
        if let Some(i) = self.selected.iter().position(|s| s == id) {
            self.selected.remove(i);
        } else if self.selected.len() < MAX_TARGETS {
            self.selected.push(id.to_string());
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() || self.busy {
            return;
        }
        self.busy = true;
        self.failure = None;
        cx.notify();
        let (message, targets, conv) = (self.request.message.clone(), self.selected.clone(), self.request.conversation_id.clone());
        let engine = engine(cx);
        cx.spawn(async move |this, cx| {
            let outcome = engine.forward(&message, targets).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    MessageActionOutcome::Done(text) => {
                        engine.show_toast(text.as_deref().unwrap_or("Đã chia sẻ tin nhắn"), Some(&conv));
                        engine.dismiss_forward();
                    }
                    MessageActionOutcome::Pending(text) => {
                        engine.show_toast(&text, Some(&conv));
                        engine.dismiss_forward();
                    }
                    MessageActionOutcome::Failed(text) => this.failure = Some(text),
                    MessageActionOutcome::Confirm { .. } => this.failure = Some("Có lỗi khi chia sẻ tin nhắn".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn preview(&self, theme: &Theme, cx: &mut App) -> AnyElement {
        let m = &self.request.message;
        let thumb = match &m.kind {
            Kind::Photo { url, .. } if !m.is_e2ee => url.clone(),
            Kind::Video { thumbnail, .. } => thumbnail.clone(),
            Kind::Gif { url } | Kind::Sticker { url } => url.clone(),
            Kind::Link { thumbnail, .. } => thumbnail.clone(),
            _ => None,
        };
        div()
            .w_full()
            .p(px(10.0))
            .rounded(px(10.0))
            .bg(with_alpha(theme.label(), 0.05))
            .flex()
            .items_center()
            .gap(px(10.0))
            .when_some(thumb, |el, t| el.child(remote_picture(Some(&t), 44.0, 44.0, 8.0, ObjectFit::Cover, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_size(px(11.0)).text_color(theme.secondary()).child("Nội dung chia sẻ"))
                    .child(div().text_size(px(12.5)).line_clamp(3).child(if m.text.is_empty() { "Tin nhắn".to_string() } else { m.text.clone() })),
            )
            .into_any_element()
    }
}

impl Render for ForwardSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let engine = engine(cx);
        let self_id = engine.state.borrow().self_conversation_id.clone();
        let mut convs = engine.visible_conversations();
        convs.sort_by(|a, b| b.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0).total_cmp(&a.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0)));
        let title = |c: &olaz_engine::ConversationSummary| if Some(&c.id) == self_id.as_ref() { "Cloud của tôi".to_string() } else { c.display_name() };
        let needle = fold(&self.needle);
        let rows: Vec<AnyElement> = convs
            .iter()
            .filter(|c| needle.is_empty() || fold(&title(c)).contains(&needle))
            .map(|c| {
                let on = self.selected.contains(&c.id);
                let disabled = !on && self.selected.len() >= MAX_TARGETS;
                let id = c.id.clone();
                div()
                    .id(SharedString::from(format!("fwd-{}", c.id)))
                    .px(px(16.0))
                    .py(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .when(disabled, |el| el.opacity(0.5))
                    .hover(|s| s.bg(theme.hover()))
                    .child(icon(if on { "checkmark.circle.fill" } else { "circle" }, 16.0, if on { theme.accent } else { theme.tertiary() }))
                    .child(avatar(c.avatar.as_deref(), &c.display_name(), 30.0, cx))
                    .child(div().flex_1().min_w(px(0.0)).text_size(px(13.0)).truncate().child(title(c)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle(&id);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let hint = if self.selected.is_empty() { "Chọn hội thoại để chia sẻ".to_string() } else { format!("Đã chọn {}", self.selected.len()) };
        let disabled = self.selected.is_empty() || self.busy;
        let me = cx.entity().downgrade();
        let preview = self.preview(&theme, cx);
        div()
            .id("forward-sheet")
            .on_key_down(|ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    crate::app::engine(cx).dismiss_forward();
                }
            })
            .w(px(440.0))
            .h(px(580.0))
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
                    .px(px(16.0))
                    .py(px(12.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.separator())
                    .child(div().flex_1().text_size(px(14.0)).font_weight(FontWeight::BOLD).child("Chia sẻ"))
                    .child(
                        div()
                            .id("fwd-close")
                            .size(px(24.0))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(theme.hover()))
                            .child(icon("xmark", 11.0, theme.secondary()))
                            .on_click(|_, _, cx| crate::app::engine(cx).dismiss_forward()),
                    ),
            )
            .child(
                div().p(px(16.0)).flex().flex_col().gap(px(10.0)).child(preview).child(
                    div()
                        .px(px(10.0))
                        .py(px(5.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.separator())
                        .child(Input::new(&self.search).appearance(false).text_size(px(13.0))),
                ),
            )
            .child(div().id("fwd-list").flex_1().min_h(px(0.0)).overflow_y_scroll().children(rows))
            .child(
                div()
                    .px(px(16.0))
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(theme.separator())
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.0))
                            .text_color(if self.failure.is_some() { theme.red() } else { theme.secondary() })
                            .child(self.failure.clone().unwrap_or(hint)),
                    )
                    .child(button("fwd-cancel", "Hủy", ButtonStyle::Glass, false, &theme, |_, _, cx| crate::app::engine(cx).dismiss_forward()))
                    .child(button("fwd-send", if self.busy { "Đang chia sẻ…" } else { "Chia sẻ" }, ButtonStyle::Prominent, disabled, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.send(cx));
                    })),
            )
    }
}

/// "N tin nhắn đã xóa · Khôi phục (s)", counting down to its deadline.
pub fn delete_undo_bar(undo: &DeleteUndo, theme: &Theme) -> AnyElement {
    let left = ((undo.deadline_ms - olaz_engine::chat::now_ms()) / 1000.0).ceil().max(0.0) as i64;
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .pl(px(14.0))
        .pr(px(10.0))
        .py(px(6.0))
        .rounded_full()
        .bg(theme.content_bg())
        .border_1()
        .border_color(theme.separator())
        .shadow_lg()
        .child(icon("trash", 12.0, theme.secondary()))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(format!("{} tin nhắn đã xóa", undo.count)))
        .child(button("undo-delete", format!("Khôi phục ({left})"), ButtonStyle::Glass, false, theme, |_, _, cx| engine(cx).undo_delete()))
        .child(
            div()
                .id("undo-dismiss")
                .size(px(22.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(theme.hover()))
                .child(icon("xmark", 10.0, theme.secondary()))
                .on_click(|_, _, cx| engine(cx).dismiss_delete_undo()),
        )
        .into_any_element()
}

pub fn pin_caption(item: &PinnedItem) -> String {
    if item.sender.is_empty() { item.label().to_string() } else { format!("{} của {}", item.label(), item.sender) }
}

pub fn pin_title(item: &PinnedItem) -> String {
    let text = if item.title.is_empty() { item.label().to_string() } else { item.title.clone() };
    text.chars().take(60).collect()
}

/// The conversation's pins above its messages: the newest pin, the others in a list toggled by "+N ghim".
pub fn pinned_bar(
    conv: &str,
    list: &PinnedList,
    expanded: bool,
    open: impl Fn(&PinnedItem, &mut Window, &mut App) + Clone + 'static,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Option<AnyElement> {
    let first = list.items.first().filter(|_| list.ready)?.clone();
    let theme = Theme::get(cx).clone();
    let conv = conv.to_string();
    let open_first = open.clone();
    let first_click = first.clone();
    let mut bar = div()
        .mx(px(16.0))
        .mt(px(6.0))
        .mb(px(2.0))
        .px(px(12.0))
        .py(px(7.0))
        .rounded(px(14.0))
        .bg(theme.glass())
        .border_1()
        .border_color(theme.glass_border())
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(icon("pin.fill", 12.0, theme.accent))
        .child(
            div()
                .id("pin-first")
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .when_some(first.thumbnail.clone(), |el, t| el.child(remote_picture(Some(&t), 28.0, 28.0, 5.0, ObjectFit::Cover, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(div().text_size(px(11.0)).text_color(theme.secondary()).truncate().child(pin_caption(&first)))
                        .child(div().text_size(px(12.5)).truncate().child(if first.title.is_empty() { first.label().to_string() } else { first.title.clone() })),
                )
                .on_click(move |_, window, cx| open_first(&first_click, window, cx)),
        );
    if list.items.len() > 1 {
        bar = bar.child(button("pins-more", format!("+{} ghim", list.items.len() - 1), ButtonStyle::Glass, false, &theme, move |_, window, cx| toggle(window, cx)));
    } else if list.can_pin {
        let (item, conv) = (first.clone(), conv.clone());
        bar = bar.child(
            div()
                .id("pin-remove")
                .size(px(22.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(theme.hover()))
                .child(icon("pin.slash", 12.0, theme.secondary()))
                .on_click(move |_, _, cx| engine(cx).request_unpin_item(&item, &conv)),
        );
    }
    let mut out = div().flex().flex_col().child(bar);
    if expanded && list.items.len() > 1 {
        let mut panel = div()
            .mx(px(16.0))
            .mb(px(4.0))
            .p(px(6.0))
            .rounded(px(12.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_md()
            .flex()
            .flex_col();
        for (i, item) in list.items.iter().enumerate() {
            let (open, it) = (open.clone(), item.clone());
            let (unpin_item, conv) = (item.clone(), conv.clone());
            panel = panel.child(
                div()
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w(px(0.0)).text_size(px(12.5)).truncate().child(pin_title(item)))
                    .child(button(
                        SharedString::from(format!("pin-open-{i}")),
                        if item.kind == PinKind::Message { "Xem tin nhắn" } else { "Xem" },
                        ButtonStyle::Link,
                        false,
                        &theme,
                        move |_, window, cx| open(&it, window, cx),
                    ))
                    .when(list.can_pin, |el| {
                        el.child(button(SharedString::from(format!("pin-unpin-{i}")), "Bỏ ghim", ButtonStyle::Destructive, false, &theme, move |_, _, cx| {
                            engine(cx).request_unpin_item(&unpin_item, &conv)
                        }))
                    }),
            );
        }
        out = out.child(panel);
    }
    Some(out.into_any_element())
}
