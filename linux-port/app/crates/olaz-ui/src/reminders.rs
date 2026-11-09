//! Reminder cards (ReminderViews.swift): the e-card `action.open.reminder`. Which variant to draw, who may answer and my
//! answer come from the official reminder controller (`reminder_watch`); RSVP goes through it on a click. While it has nothing
//! to draw, the static card built from the message stays.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, Context, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px,
};
use olaz_engine::models::{Answer, ChatMessage, ECard, Person, ReminderResponders, ReminderSnapshot, ReminderVariant};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, avatar, button, spinner};

#[derive(Default)]
struct Watches {
    snapshots: HashMap<String, ReminderSnapshot>,
    watching: HashSet<String>,
    /// Reminder ids with an answer being sent.
    busy: HashSet<String>,
}

thread_local! {
    static W: RefCell<Watches> = RefCell::new(Watches::default());
}

fn snapshot(key: &str) -> Option<ReminderSnapshot> {
    W.with(|w| w.borrow().snapshots.get(key).cloned())
}

/// Follows the official controller for one message (long-poll by signature) until it has nothing more to say.
fn ensure_watch(conv: &str, msg: &str, cx: &mut App) {
    let key = format!("{conv}|{msg}");
    if !W.with(|w| w.borrow_mut().watching.insert(key.clone())) {
        return;
    }
    let (e, conv, msg) = (engine(cx), conv.to_string(), msg.to_string());
    cx.spawn(async move |cx| {
        let mut sig = String::new();
        let mut misses = 0;
        loop {
            match e.reminder_watch(&conv, &msg, &sig).await {
                Some(next) => {
                    sig = next.sig.clone();
                    misses = 0;
                    W.with(|w| w.borrow_mut().snapshots.insert(key.clone(), next));
                    let _ = cx.update(|cx| cx.refresh_windows());
                }
                None => {
                    misses += 1;
                    if misses >= 3 {
                        W.with(|w| w.borrow_mut().watching.remove(&key));
                        return;
                    }
                    cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                }
            }
            if e.state.borrow().open_conversation_id.as_deref() != Some(conv.as_str()) {
                W.with(|w| w.borrow_mut().watching.remove(&key));
                return;
            }
        }
    })
    .detach();
}

pub fn respond(conv: &str, msg: &str, reminder: &str, accept: bool, cx: &mut App) {
    let scope = if reminder.is_empty() { msg.to_string() } else { reminder.to_string() };
    if !W.with(|w| w.borrow_mut().busy.insert(scope.clone())) {
        return;
    }
    cx.refresh_windows();
    let (e, conv, msg) = (engine(cx), conv.to_string(), msg.to_string());
    cx.spawn(async move |cx| {
        let (ok, text, snap) = e.respond_to_reminder(&conv, &msg, accept).await;
        W.with(|w| {
            let mut w = w.borrow_mut();
            w.busy.remove(&scope);
            if let Some(s) = snap {
                w.snapshots.insert(format!("{conv}|{msg}"), s);
            }
        });
        if !ok {
            e.show_toast(if text.is_empty() { "Có lỗi xảy ra" } else { &text }, Some(&conv));
        }
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

fn is_busy(s: &ReminderSnapshot, msg: &str) -> bool {
    let scope = if s.id.is_empty() { msg } else { s.id.as_str() };
    W.with(|w| w.borrow().busy.contains(scope)) || s.pending.is_some()
}

fn date_time(ms: f64) -> String {
    let d = crate::format::local(ms);
    format!("{} {}", weekday(&d), d.format("%d/%m/%Y %H:%M"))
}

fn weekday(d: &chrono::DateTime<chrono::Local>) -> &'static str {
    use chrono::Datelike;
    match d.weekday() {
        chrono::Weekday::Mon => "Thứ Hai",
        chrono::Weekday::Tue => "Thứ Ba",
        chrono::Weekday::Wed => "Thứ Tư",
        chrono::Weekday::Thu => "Thứ Năm",
        chrono::Weekday::Fri => "Thứ Sáu",
        chrono::Weekday::Sat => "Thứ Bảy",
        chrono::Weekday::Sun => "Chủ Nhật",
    }
}

fn calendar(start: Option<f64>, theme: &Theme) -> AnyElement {
    use chrono::Datelike;
    let d = start.map(crate::format::local);
    div()
        .w(px(64.0))
        .flex_none()
        .rounded(px(8.0))
        .overflow_hidden()
        .border_1()
        .border_color(theme.separator())
        .bg(theme.content_bg())
        .flex()
        .flex_col()
        .items_center()
        .child(div().w_full().py(px(3.0)).bg(theme.red()).text_center().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(gpui::white()).child(d.map(|d| weekday(&d)).unwrap_or("")))
        .child(div().pt(px(2.0)).text_size(px(22.0)).font_weight(FontWeight::BOLD).child(d.map(|d| d.format("%d").to_string()).unwrap_or_else(|| "--".into())))
        .child(div().pb(px(4.0)).text_size(px(10.5)).text_color(theme.secondary()).child(d.map(|d| format!("Tháng {}", d.month())).unwrap_or_default()))
        .into_any_element()
}

fn meta(start: Option<f64>, repeat: &str, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .when_some(start, |el, s| el.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.0)).text_color(theme.secondary()).child(icon("clock", 11.0, theme.secondary())).child(date_time(s))))
        .when(!repeat.is_empty(), |el| el.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.0)).text_color(theme.secondary()).child(icon("repeat", 11.0, theme.secondary())).child(repeat.to_string())))
}

fn rsvp_buttons(s: &ReminderSnapshot, conv: &str, msg: &str, theme: &Theme) -> AnyElement {
    let locked = is_busy(s, msg);
    let mut row = div().flex().items_center();
    for (i, (title, accept)) in [("Từ chối", false), ("Tham gia", true)].into_iter().enumerate() {
        let sending = locked && (s.pending == Some(if accept { Answer::Accept } else { Answer::Reject }) || s.pending.is_none());
        let (c, m, r) = (conv.to_string(), msg.to_string(), s.id.clone());
        if i > 0 {
            row = row.child(div().w(px(1.0)).h(px(24.0)).bg(theme.separator()));
        }
        row = row.child(
            div()
                .id(SharedString::from(format!("rsvp-{msg}-{accept}")))
                .flex_1()
                .py(px(10.0))
                .flex()
                .justify_center()
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if accept { theme.accent } else { theme.secondary() })
                .when(!locked, |el| el.hover(|st| st.bg(theme.hover())))
                .child(if sending && locked { spinner(12.0, theme.secondary()).into_any_element() } else { title.into_any_element() })
                .on_click(move |_, _, cx| {
                    if !locked {
                        respond(&c, &m, &r, accept, cx)
                    }
                }),
        );
    }
    row.into_any_element()
}

fn rsvp_status(s: &ReminderSnapshot, conv: &str, msg: &str, theme: &Theme) -> AnyElement {
    let mut row = div().flex().items_center().gap(px(8.0)).child(
        div().flex_1().text_size(px(12.0)).text_color(theme.secondary()).child(if s.mine == Some(Answer::Accept) { "Bạn xác nhận: Tham gia" } else { "Bạn xác nhận: Từ chối" }),
    );
    if is_busy(s, msg) {
        row = row.child(spinner(12.0, theme.secondary()));
    } else if s.can_rsvp && !s.deleted {
        // "Thay đổi" switches to the other answer (the official menu offers only the one not chosen).
        let to_accept = s.mine != Some(Answer::Accept);
        let (c, m, r) = (conv.to_string(), msg.to_string(), s.id.clone());
        row = row.child(
            div()
                .id(SharedString::from(format!("rsvp-change-{msg}")))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.accent)
                .child(if to_accept { "Đổi sang Tham gia" } else { "Đổi sang Từ chối" })
                .on_click(move |_, _, cx| respond(&c, &m, &r, to_accept, cx)),
        );
    }
    row.into_any_element()
}

fn open_detail(conv: &str, msg: &str, open_list: bool, cx: &mut App) {
    crate::cards::request(crate::cards::CardRequest::Reminder { conversation: conv.to_string(), message: msg.to_string(), open_list }, cx);
}

/// The reminder card in the chat.
pub fn reminder_card(card: &ECard, m: &ChatMessage, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let (conv, msg) = (m.conversation_id.clone(), m.id.clone());
    ensure_watch(&conv, &msg, cx);
    let snap = snapshot(&format!("{conv}|{msg}")).filter(|s| s.is_drawn());
    let shell = || div().w(px(300.0)).rounded(px(16.0)).overflow_hidden().bg(theme.incoming_bubble()).flex().flex_col();
    let Some(s) = snap else {
        return shell()
            .child(
                div()
                    .p(px(12.0))
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .child(calendar(card.reminder_start, &theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(3).child(if card.reminder_title.is_empty() { "Nhắc hẹn".to_string() } else { card.reminder_title.clone() }))
                            .child(meta(card.reminder_start, &card.reminder_repeat, &theme)),
                    ),
            )
            .into_any_element();
    };
    match s.variant {
        ReminderVariant::Alert => {
            let (c, mm) = (conv.clone(), msg.clone());
            shell()
                .child(
                    div()
                        .p(px(12.0))
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .child(icon("alarm.fill", 20.0, theme.orange()))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(3.0))
                                .when(s.only_me, |el| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child("(Chỉ bạn nhìn thấy)")))
                                .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(3).when(s.deleted, |el| el.line_through()).child(format!("Nhắc hẹn: {}", s.title)))
                                .when_some(s.start, |el, st| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(date_time(st)))),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("rem-detail-{msg}")))
                        .border_t_1()
                        .border_color(theme.separator())
                        .py(px(10.0))
                        .flex()
                        .justify_center()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.accent)
                        .hover(|st| st.bg(theme.hover()))
                        .child("Xem chi tiết")
                        .on_click(move |_, _, cx| open_detail(&c, &mm, false, cx)),
                )
                .into_any_element()
        }
        ReminderVariant::DemotedCreate | ReminderVariant::DemotedAlert => {
            let when = s.start.map(date_time).unwrap_or_default();
            let text = if s.variant == ReminderVariant::DemotedAlert {
                format!("Nhắc hẹn: {} - {when}", s.title)
            } else if s.only_me {
                format!("(Chỉ bạn nhìn thấy) Bạn đã tạo nhắc hẹn mới {} - {when}.", s.title)
            } else {
                format!("{} tạo nhắc hẹn mới {} - {when}.", s.creator_name, s.title)
            };
            let (c, mm) = (conv.clone(), msg.clone());
            div()
                .max_w(px(360.0))
                .px(px(20.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.5))
                .child(div().text_center().text_color(theme.secondary()).child(text))
                .child(div().id(SharedString::from(format!("rem-see-{msg}"))).font_weight(FontWeight::MEDIUM).text_color(theme.accent).child("Xem").on_click(move |_, _, cx| open_detail(&c, &mm, false, cx)))
                .into_any_element()
        }
        _ => {
            let (c, mm) = (conv.clone(), msg.clone());
            let (c2, m2) = (conv.clone(), msg.clone());
            let mut col = shell().child(
                div()
                    .id(SharedString::from(format!("rem-card-{msg}")))
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .when(s.only_me, |el| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child("(Chỉ bạn nhìn thấy)")))
                    .when(!s.creator_name.is_empty(), |el| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(format!("{} tạo nhắc hẹn", s.creator_name))))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(14.0))
                            .child(calendar(s.start, &theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(3).when(s.deleted, |el| el.line_through()).child(if s.title.is_empty() { "Nhắc hẹn".to_string() } else { s.title.clone() }))
                                    .child(meta(s.start, &s.repeat_label, &theme)),
                            ),
                    )
                    .when(s.deleted, |el| el.child(div().text_size(px(12.0)).text_color(theme.orange()).child("Nhắc hẹn đã bị xoá")))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| open_detail(&c, &mm, false, cx)),
            );
            if s.has_responders() {
                let deleted = s.deleted;
                col = col.child(
                    div()
                        .id(SharedString::from(format!("rem-list-{msg}")))
                        .px(px(12.0))
                        .pb(px(10.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if deleted { theme.secondary() } else { theme.accent })
                        .child(icon("person.2", 11.0, if deleted { theme.secondary() } else { theme.accent }))
                        .child(div().flex_1().child("Danh sách tham gia"))
                        .when(!deleted, |el| el.child(icon("chevron.right", 9.0, theme.accent)))
                        .on_click(move |_, _, cx| {
                            if !deleted {
                                open_detail(&c2, &m2, true, cx)
                            }
                        }),
                );
            }
            if s.shows_rsvp_buttons() {
                col = col.child(div().border_t_1().border_color(theme.separator()).child(rsvp_buttons(&s, &conv, &msg, &theme)));
            } else if s.shows_rsvp_status() {
                col = col.child(div().border_t_1().border_color(theme.separator()).px(px(12.0)).py(px(8.0)).child(rsvp_status(&s, &conv, &msg, &theme)));
            }
            col.into_any_element()
        }
    }
}

pub enum ReminderSheetEvent {
    Close,
}

#[derive(Clone, PartialEq)]
enum Responders {
    Idle,
    Loading,
    Failed,
    Ready(ReminderResponders),
}

pub struct ReminderDetailSheet {
    conversation: String,
    message: String,
    show_list: bool,
    accept_tab: bool,
    responders: Responders,
    _load: Option<Task<()>>,
}

impl EventEmitter<ReminderSheetEvent> for ReminderDetailSheet {}

impl ReminderDetailSheet {
    pub fn new(conversation: String, message: String, open_list: bool, cx: &mut Context<Self>) -> Self {
        ensure_watch(&conversation, &message, cx);
        let mut me = ReminderDetailSheet { conversation, message, show_list: open_list, accept_tab: true, responders: Responders::Idle, _load: None };
        if open_list {
            me.load(cx);
        }
        me
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.responders = Responders::Loading;
        let (e, c, m) = (engine(cx), self.conversation.clone(), self.message.clone());
        self._load = Some(cx.spawn(async move |this, cx| {
            let r = e.reminder_responders(&c, &m).await;
            let _ = this.update(cx, |this, cx| {
                this.responders = match r {
                    Some(r) => Responders::Ready(r),
                    None => Responders::Failed,
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn people(&self, list: &[Person], more: i64, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if list.is_empty() {
            return div().min_h(px(80.0)).flex().items_center().justify_center().text_size(px(12.5)).text_color(theme.secondary()).child(if self.accept_tab { "Chưa có ai tham gia." } else { "Chưa có ai từ chối." }).into_any_element();
        }
        let mut col = div().flex().flex_col().gap(px(10.0));
        for p in list {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(avatar(p.avatar.as_deref(), &p.name, 30.0, cx))
                    .child(div().truncate().text_size(px(13.0)).child(p.name.clone()))
                    .when(p.is_me, |el| el.child(div().text_size(px(11.0)).text_color(theme.secondary()).child("Bạn"))),
            );
        }
        if more > 0 {
            col = col.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(format!("và {more} người khác")));
        }
        div().id("rem-people").h(px(280.0)).overflow_y_scroll().child(col).into_any_element()
    }
}

impl Render for ReminderDetailSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let key = format!("{}|{}", self.conversation, self.message);
        let snap = snapshot(&key);
        let watching = W.with(|w| w.borrow().watching.contains(&key));
        let me = cx.entity().downgrade();
        let close = button("rem-close", "Đóng", ButtonStyle::Glass, false, &theme, move |_, _, cx| {
            let _ = me.update(cx, |_, cx| cx.emit(ReminderSheetEvent::Close));
        });
        let body = if self.show_list && snap.as_ref().is_none_or(|s| s.has_responders()) {
            let totals = match &self.responders {
                Responders::Ready(r) => Some((r.accept_total, r.reject_total)),
                _ => None,
            };
            let tab = |id: &'static str, label: &str, on: bool, accept: bool, cx: &mut Context<Self>| {
                div()
                    .id(id)
                    .flex_1()
                    .py(px(4.0))
                    .rounded_full()
                    .text_center()
                    .text_size(px(12.5))
                    .when(on, |el| el.bg(theme.accent).text_color(theme.white()))
                    .child(label.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.accept_tab = accept;
                        cx.notify();
                    }))
            };
            let a_label = totals.map(|t| format!("Tham gia {}", t.0)).unwrap_or_else(|| "Tham gia".into());
            let r_label = totals.map(|t| format!("Từ chối {}", t.1)).unwrap_or_else(|| "Từ chối".into());
            let content = match self.responders.clone() {
                Responders::Idle | Responders::Loading => div().min_h(px(80.0)).flex().items_center().justify_center().child(spinner(12.0, theme.secondary())).into_any_element(),
                Responders::Failed => div()
                    .min_h(px(80.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Có lỗi xảy ra"))
                    .child(button("rem-retry", "Thử lại", ButtonStyle::Glass, false, &theme, {
                        let me = cx.entity().downgrade();
                        move |_, _, cx| {
                            let _ = me.update(cx, |this, cx| this.load(cx));
                        }
                    }))
                    .into_any_element(),
                Responders::Ready(r) => {
                    if self.accept_tab {
                        self.people(&r.accept, r.accept_more, &theme, cx)
                    } else {
                        self.people(&r.reject, r.reject_more, &theme, cx)
                    }
                }
            };
            div()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().id("rem-back").child(icon("chevron.left", 13.0, theme.label())).on_click(cx.listener(|this, _, _, cx| {
                            this.show_list = false;
                            cx.notify();
                        })))
                        .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Danh sách tham gia")),
                )
                .child(div().flex().gap(px(4.0)).p(px(3.0)).rounded_full().bg(theme.glass()).child(tab("rem-tab-a", &a_label, self.accept_tab, true, cx)).child(tab("rem-tab-r", &r_label, !self.accept_tab, false, cx)))
                .child(content)
        } else {
            let mut col = div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(14.0))
                        .child(calendar(snap.as_ref().and_then(|s| s.start), &theme))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .child(div().text_size(px(16.0)).font_weight(FontWeight::BOLD).when(snap.as_ref().is_some_and(|s| s.deleted), |el| el.line_through()).child(snap.as_ref().map(|s| s.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Nhắc hẹn".into())))
                                .child(meta(snap.as_ref().and_then(|s| s.start), snap.as_ref().map(|s| s.repeat_label.as_str()).unwrap_or(""), &theme)),
                        ),
                );
            match &snap {
                Some(s) => {
                    if s.only_me {
                        col = col.child(div().text_size(px(12.0)).text_color(theme.secondary()).child("Chỉ bạn nhìn thấy"));
                    } else if !s.creator_name.is_empty() {
                        col = col.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.5)).text_color(theme.secondary()).child(icon("person.fill", 11.0, theme.secondary())).child(format!("{} tạo nhắc hẹn", s.creator_name)));
                    }
                    if let (true, Some(summary)) = (s.has_responders(), s.responder_summary()) {
                        let deleted = s.deleted;
                        col = col.child(
                            div()
                                .id("rem-summary")
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if deleted { theme.secondary() } else { theme.accent })
                                .child(icon("person.2", 11.0, if deleted { theme.secondary() } else { theme.accent }))
                                .child(summary)
                                .when(!deleted, |el| el.child(icon("chevron.right", 9.0, theme.accent)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !deleted {
                                        this.show_list = true;
                                        this.load(cx);
                                    }
                                })),
                        );
                    }
                    if s.deleted {
                        col = col.child(div().text_size(px(12.5)).text_color(theme.orange()).child("Nhắc hẹn đã bị xoá"));
                    } else if s.shows_rsvp_buttons() {
                        col = col.child(div().rounded(px(12.0)).bg(with_alpha(theme.label(), 0.06)).child(rsvp_buttons(s, &self.conversation, &self.message, &theme)));
                    } else if s.shows_rsvp_status() {
                        col = col.child(rsvp_status(s, &self.conversation, &self.message, &theme));
                    }
                }
                None if !watching => col = col.child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Không tải được chi tiết nhắc hẹn lúc này.")),
                None => col = col.child(spinner(12.0, theme.secondary())),
            }
            col
        };
        div()
            .id("reminder-sheet")
            .relative()
            .child(crate::widgets::occluder())
            .w(px(360.0))
            .p(px(22.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(body)
            .child(div().flex().justify_end().child(close))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_line() {
        use chrono::TimeZone;
        let ms = chrono::Local.with_ymd_and_hms(2026, 11, 9, 8, 5, 0).unwrap().timestamp_millis() as f64;
        assert_eq!(super::date_time(ms), "Thứ Hai 09/11/2026 08:05");
    }
}
