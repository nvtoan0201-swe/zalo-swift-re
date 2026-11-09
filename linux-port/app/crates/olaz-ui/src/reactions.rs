//! Reaction detail ("Biểu cảm", port of ReactionDetailView.swift): who reacted with what, from the official reaction cache
//! (`__zaloChat.reactors`). Opened from a message's reaction chips.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, FontWeight, Image, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Window, div, img, prelude::FluentBuilder, px,
};
use olaz_engine::models::{Reaction, ReactionDetail, ReactionIcon, ReactionUser};

use crate::app::engine;
use crate::theme::Theme;
use crate::widgets::{ButtonStyle, avatar, button, image_from_bytes, spinner};

thread_local! {
    static ICONS: RefCell<HashMap<String, Option<Arc<Image>>>> = RefCell::new(HashMap::new());
}

/// `ReactionIconView`: the official PNG from the asar at `size`, else the emoji text at size × 0.85.
pub fn reaction_icon(icon: &ReactionIcon, size: f32, cx: &App) -> AnyElement {
    let image = (!icon.image.is_empty())
        .then(|| {
            ICONS.with(|m| {
                m.borrow_mut().entry(icon.image.clone()).or_insert_with(|| engine(cx).reaction_image(&icon.image).and_then(image_from_bytes)).clone()
            })
        })
        .flatten();
    match image {
        Some(i) => img(i).size(px(size)).into_any_element(),
        None => div().text_size(px(size * 0.85)).child(icon.emoji.clone()).into_any_element(),
    }
}

#[derive(Clone, PartialEq)]
enum Phase {
    Loading,
    Loaded(ReactionDetail),
    Failed,
}

pub struct ReactionDetailView {
    pub conversation: String,
    pub message_id: String,
    /// The message's reactions when last loaded: a change reloads (keeping the shown list on a failed refresh).
    reactions: Vec<Reaction>,
    phase: Phase,
    selected: Option<String>,
}

impl ReactionDetailView {
    pub fn new(conversation: String, message_id: String, reactions: Vec<Reaction>, cx: &mut Context<Self>) -> Self {
        let mut me = ReactionDetailView { conversation, message_id, reactions, phase: Phase::Loading, selected: None };
        me.load(cx);
        me
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Failed {
            self.phase = Phase::Loading;
        }
        let (e, msg, conv) = (engine(cx), self.message_id.clone(), self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let detail = e.reactors(&msg, &conv).await;
            let _ = this.update(cx, |this, cx| {
                match detail {
                    Some(d) => this.phase = Phase::Loaded(d),
                    None if matches!(this.phase, Phase::Loaded(_)) => {}
                    None => this.phase = Phase::Failed,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn row(user: &ReactionUser, theme: &Theme, cx: &mut App) -> AnyElement {
        let mut icons = div().flex().items_center().gap(px(4.0));
        for i in &user.icons {
            icons = icons.child(reaction_icon(&i.icon, 18.0, cx));
        }
        icons = icons.child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(ReactionDetail::short_count(user.total)));
        div()
            .h(px(48.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(avatar(user.avatar.as_deref(), &user.name, 32.0, cx))
            .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(user.name.clone()))
            .child(div().pl(px(8.0)).child(icons))
            .into_any_element()
    }
}

impl Render for ReactionDetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        // Reload when the message's reactions change.
        let current = engine(cx)
            .state
            .borrow()
            .messages
            .get(&self.conversation)
            .and_then(|l| l.iter().find(|m| m.id == self.message_id).map(|m| m.reactions.clone()));
        if let Some(r) = current.filter(|r| *r != self.reactions) {
            self.reactions = r;
            self.load(cx);
        }
        let message = |text: &'static str, retry: bool, cx: &mut Context<Self>| {
            div()
                .min_h(px(120.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .child(div().text_size(px(12.5)).text_color(theme.secondary()).child(text))
                .when(retry, |el| {
                    let me = cx.entity().downgrade();
                    el.child(button("reactors-retry", "Thử lại", ButtonStyle::Glass, false, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.load(cx));
                    }))
                })
                .into_any_element()
        };
        let body: Vec<AnyElement> = match self.phase.clone() {
            Phase::Loading => vec![div().min_h(px(120.0)).flex().items_center().justify_center().child(spinner(12.0, theme.secondary())).into_any_element()],
            Phase::Failed => vec![message("Không tải được danh sách biểu cảm", true, cx)],
            Phase::Loaded(d) if d.is_empty() => vec![message("Chưa có biểu cảm nào", false, cx)],
            Phase::Loaded(d) => {
                let selected = self.selected.clone().filter(|k| d.tabs.iter().any(|t| &t.icon.kind == k));
                let mut tabs = div().id("reaction-tabs").h(px(36.0)).px(px(8.0)).flex().gap(px(2.0)).overflow_x_scroll();
                let mut add_tab = |id: SharedString, icon: Option<&ReactionIcon>, total: i64, kind: Option<String>, cx: &mut Context<Self>| {
                    let sel = selected == kind;
                    let color = if sel { theme.accent } else { theme.secondary() };
                    tabs = std::mem::replace(&mut tabs, div().id("x")).child(
                        div()
                            .id(id)
                            .h(px(36.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .text_color(color)
                            .border_b_2()
                            .border_color(if sel { theme.accent } else { gpui::transparent_black() })
                            .child(match icon {
                                Some(i) => reaction_icon(i, 18.0, cx),
                                None => div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child("Tất cả").into_any_element(),
                            })
                            .child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).child(ReactionDetail::short_count(total)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = kind.clone();
                                cx.notify();
                            })),
                    );
                };
                add_tab("rt-all".into(), None, d.total(), None, cx);
                for t in &d.tabs {
                    add_tab(SharedString::from(format!("rt-{}", t.icon.kind)), Some(&t.icon), t.total, Some(t.icon.kind.clone()), cx);
                }
                let rows = d.rows(selected.as_deref());
                let height = (rows.len() as f32 * 48.0 + 8.0).min(288.0);
                let mut list = div().id("reaction-rows").h(px(height)).py(px(4.0)).overflow_y_scroll().flex().flex_col();
                for u in &rows {
                    list = list.child(Self::row(u, &theme, cx));
                }
                vec![tabs.into_any_element(), div().h(px(1.0)).bg(theme.separator()).into_any_element(), list.into_any_element()]
            }
        };
        div()
            .id("reaction-detail")
            .w(px(320.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .flex()
            .flex_col()
            .child(div().py(px(10.0)).text_center().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child("Biểu cảm"))
            .child(div().h(px(1.0)).bg(theme.separator()))
            .children(body)
    }
}
