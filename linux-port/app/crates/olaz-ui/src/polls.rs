//! Polls (port of PollViews.swift): the card in the chat, the vote / voters sheet and the create sheet. Sheets are requested
//! through [`request`] (from the chat card or the inspector) and presented by the open chat.

use std::cell::RefCell;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::chat::now_ms;
use olaz_engine::models::{ChatMessage, Kind, PollInfo};
use olaz_engine::polls::{
    CreateIssue, PollAction, PollCapabilities, PollConfig, PollCreateDraft, PollDetail, PollOption, PollPinConfirm, PollPinOutcome, PollVoteDraft,
    PollVoters,
};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, alert, avatar, button, spinner};

#[derive(Clone, Debug, PartialEq)]
pub enum PollRequest {
    Detail { poll_id: String, conversation: String, question: String, voters: bool },
    Create { conversation: String },
}

thread_local! {
    static REQUEST: RefCell<Option<PollRequest>> = const { RefCell::new(None) };
}

pub fn request(r: PollRequest, cx: &mut App) {
    REQUEST.with(|q| *q.borrow_mut() = Some(r));
    cx.refresh_windows();
}

pub fn take_request() -> Option<PollRequest> {
    REQUEST.with(|q| q.borrow_mut().take())
}

pub fn deadline_text(ms: f64) -> String {
    crate::format::local(ms).format("%H:%M %d/%m/%Y").to_string()
}

fn percent(w: f64) -> String {
    format!("{}%", (w * 100.0).round() as i64)
}

fn settings_lines(p: &PollDetail, single: bool, theme: &Theme) -> gpui::Div {
    let mut items: Vec<(&str, String, bool)> = vec![];
    if let Some(end) = p.expires {
        items.push(("clock", format!("Kết thúc lúc {}", deadline_text(end)), p.is_expired(now_ms())));
    }
    if p.multi {
        items.push(("checklist", "Chọn nhiều phương án".into(), false));
    } else if single {
        items.push(("checkmark.circle", "Chỉ chọn một phương án".into(), false));
    }
    if p.anonymous {
        items.push(("person.fill.questionmark", "Ẩn người bình chọn".into(), false));
    }
    if p.hide_preview {
        items.push(("eye.slash", "Ẩn kết quả khi chưa bình chọn".into(), false));
    }
    let mut col = div().flex().flex_col().gap(px(3.0));
    for (sym, text, red) in items {
        let color = if red { theme.red() } else { theme.secondary() };
        col = col.child(div().flex().items_center().gap(px(5.0)).text_size(px(11.5)).text_color(color).child(icon(sym, 10.5, color)).child(text));
    }
    col
}

/// One option with its result bar; `control` = (multi, on, enabled) when it can be ticked.
fn option_row(o: &PollOption, weight: f64, show_count: bool, control: Option<(bool, bool, bool)>, theme: &Theme) -> gpui::Div {
    let bar = div()
        .relative()
        .flex_1()
        .min_w(px(0.0))
        .rounded(px(8.0))
        .overflow_hidden()
        .bg(with_alpha(theme.label(), 0.05))
        .child(div().absolute().top_0().bottom_0().left_0().w(relative(weight.clamp(0.0, 1.0) as f32)).bg(with_alpha(theme.accent, if weight >= 1.0 { 0.28 } else { 0.16 })))
        .child(
            div()
                .relative()
                .px(px(10.0))
                .py(px(8.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(div().flex_1().text_size(px(13.0)).child(o.content.clone()))
                .when(o.voted && control.is_none(), |el| el.child(icon("checkmark.circle.fill", 12.0, theme.accent)))
                .when(show_count, |el| el.child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(o.votes.to_string()))),
        );
    let mut row = div().flex().items_center().gap(px(8.0));
    if let Some((multi, on, enabled)) = control {
        let sym = match (multi, on) {
            (true, true) => "checkmark.square.fill",
            (true, false) => "square",
            (false, true) => "largecircle.fill.circle",
            (false, false) => "circle",
        };
        row = row.child(div().when(!enabled, |el| el.opacity(0.5)).child(icon(sym, 16.0, if on { theme.accent } else { theme.secondary() })));
    }
    row.child(bar)
}

fn fallback_card(question: &str, theme: &Theme) -> gpui::Div {
    div()
        .w(px(280.0))
        .p(px(12.0))
        .rounded(px(14.0))
        .bg(theme.incoming_bubble())
        .flex()
        .gap(px(8.0))
        .child(icon("chart.bar.fill", 14.0, theme.accent))
        .child(div().flex_1().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).child(if question.is_empty() { "Bình chọn".to_string() } else { question.to_string() }))
}

/// The poll as it shows in the chat. Only the newest message of a poll carries the live card; older ones keep the header line.
pub fn poll_message(info: &PollInfo, m: &ChatMessage, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let e = engine(cx);
    let conv = if info.conversation_id.is_empty() { m.conversation_id.clone() } else { info.conversation_id.clone() };
    let newest = info.poll_id.is_empty() || {
        let s = e.state.borrow();
        s.messages
            .get(&m.conversation_id)
            .and_then(|l| l.iter().rev().find(|o| matches!(&o.kind, Kind::Poll(p) if p.poll_id == info.poll_id)).map(|o| o.id == m.id))
            .unwrap_or(true)
    };
    let open = {
        let (pid, conv, q) = (info.poll_id.clone(), conv.clone(), info.question.clone());
        move |voters: bool, cx: &mut App| request(PollRequest::Detail { poll_id: pid.clone(), conversation: conv.clone(), question: q.clone(), voters }, cx)
    };
    let mut col = div().max_w(px(300.0)).flex().flex_col().gap(px(8.0));
    if !info.header.is_empty() {
        let o = open.clone();
        col = col.child(
            div()
                .px(px(2.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.5))
                .text_color(theme.secondary())
                .child(icon("chart.bar.fill", 11.5, theme.accent))
                .child(div().flex_1().line_clamp(2).child(info.header.clone()))
                .when(!info.poll_id.is_empty(), |el| {
                    el.child(div().id(SharedString::from(format!("poll-see-{}", m.id))).text_color(theme.accent).child("Xem").on_click(move |_, _, cx| o(false, cx)))
                }),
        );
    }
    if info.poll_id.is_empty() {
        return col.child(fallback_card(&info.question, &theme)).into_any_element();
    }
    if !newest {
        return col.into_any_element();
    }
    e.watch_poll(&info.poll_id, &conv);
    let (detail, failed, pinned) = {
        let s = e.state.borrow();
        (s.polls.details.get(&info.poll_id).cloned(), s.polls.failed.contains(&info.poll_id), s.polls.pins.get(&info.poll_id).is_some_and(|p| p.pinned))
    };
    let Some(d) = detail else {
        if failed {
            let (pid, conv) = (info.poll_id.clone(), conv.clone());
            return col
                .child(fallback_card(&info.question, &theme))
                .child(button(SharedString::from(format!("poll-retry-{}", m.id)), "Thử lại", ButtonStyle::Glass, false, &theme, move |_, _, cx| engine(cx).retry_poll(&pid, &conv)))
                .into_any_element();
        }
        return col.child(fallback_card(&info.question, &theme).child(spinner(10.0, theme.secondary()))).into_any_element();
    };
    let shown = 3usize;
    let rest = d.options.len().saturating_sub(shown);
    let mut card = div().w(px(280.0)).p(px(12.0)).rounded(px(14.0)).bg(theme.incoming_bubble()).flex().flex_col().gap(px(8.0));
    if pinned {
        card = card.child(div().flex().items_center().gap(px(4.0)).text_size(px(11.0)).text_color(theme.accent).child(icon("pin.fill", 10.0, theme.accent)).child("Đã ghim"));
    }
    card = card
        .child(div().text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).line_clamp(4).child(if d.question.is_empty() { info.question.clone() } else { d.question.clone() }))
        .child(settings_lines(&d, false, &theme));
    if d.voter_count > 0 {
        let o = open.clone();
        let vis = d.voters_visible();
        card = card.child(
            div()
                .id(SharedString::from(format!("poll-voters-{}", m.id)))
                .flex()
                .items_center()
                .gap(px(3.0))
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.accent)
                .child(format!("{} người bình chọn", d.voter_count))
                .when(vis, |el| el.child(icon("chevron.right", 9.0, theme.accent)))
                .on_click(move |_, _, cx| o(vis, cx)),
        );
    }
    let mut opts = div().flex().flex_col().gap(px(5.0));
    for o in d.options.iter().take(shown) {
        opts = opts.child(option_row(o, d.weight(o), true, None, &theme));
    }
    card = card.child(opts);
    if rest > 0 {
        card = card.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(format!("* Còn {rest} lựa chọn khác")));
    }
    let title = if d.closed || (d.joined && d.hide_preview) {
        "Xem lựa chọn"
    } else if d.joined {
        "Đổi lựa chọn"
    } else {
        "Bình chọn"
    };
    let o = open.clone();
    card = card.child(button(SharedString::from(format!("poll-open-{}", m.id)), title, ButtonStyle::Glass, false, &theme, move |_, _, cx| o(false, cx)).w_full());
    col.child(card).into_any_element()
}

pub enum PollSheetEvent {
    Close,
}

// MARK: Detail sheet

enum Confirm {
    Lock,
    Hide,
    Exit,
    Pin(PollPinConfirm),
}

pub struct PollDetailSheet {
    poll_id: String,
    conversation: String,
    question: String,
    draft: Option<PollVoteDraft>,
    config: PollConfig,
    show_voters: bool,
    voters: Option<PollVoters>,
    voters_failed: bool,
    busy: bool,
    message: Option<String>,
    confirm: Option<Confirm>,
    menu_open: bool,
    added_inputs: Vec<(u64, Entity<InputState>)>,
}

impl EventEmitter<PollSheetEvent> for PollDetailSheet {}

impl PollDetailSheet {
    pub fn new(poll_id: String, conversation: String, question: String, voters: bool, cx: &mut Context<Self>) -> Self {
        let e = engine(cx);
        e.watch_poll(&poll_id, &conversation);
        let (pid, conv) = (poll_id.clone(), conversation.clone());
        cx.spawn(async move |this, cx| {
            let caps = e.poll_capabilities(&conv).await;
            let _ = this.update(cx, |this, cx| {
                this.config = caps.config;
                cx.notify();
            });
            e.refresh_poll_pin(&pid, &conv).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        let mut me = PollDetailSheet {
            poll_id,
            conversation,
            question,
            draft: None,
            config: PollConfig::default(),
            show_voters: voters,
            voters: None,
            voters_failed: false,
            busy: false,
            message: None,
            confirm: None,
            menu_open: false,
            added_inputs: vec![],
        };
        if voters {
            me.load_voters(cx);
        }
        me
    }

    fn detail(&self, cx: &App) -> Option<PollDetail> {
        engine(cx).state.borrow().polls.details.get(&self.poll_id).cloned()
    }

    fn load_voters(&mut self, cx: &mut Context<Self>) {
        self.voters_failed = false;
        let (e, pid) = (engine(cx), self.poll_id.clone());
        cx.spawn(async move |this, cx| {
            let v = e.poll_voters(&pid).await;
            let _ = this.update(cx, |this, cx| {
                match v {
                    Some(v) => this.voters = Some(v),
                    None if this.voters.is_none() => this.voters_failed = true,
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.show_voters && self.draft.as_ref().is_some_and(|d| d.has_changes()) {
            self.confirm = Some(Confirm::Exit);
            cx.notify();
        } else {
            cx.emit(PollSheetEvent::Close);
        }
    }

    fn add_row(&mut self, d: &PollDetail, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft.get_or_insert_with(|| PollVoteDraft::new(d));
        let id = draft.add_row();
        let n = d.options.len() + draft.added.len();
        let max = self.config.max_option;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Lựa chọn {n}")));
        cx.subscribe(&input, move |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let text: String = input.read(cx).value().chars().take(max.max(0) as usize).collect();
                if let Some(d) = this.draft.as_mut() {
                    d.set_added_text(id, &text);
                }
                cx.notify();
            }
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        self.added_inputs.push((id, input));
        cx.notify();
    }

    fn run(&mut self, action: PollAction, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx) else { return };
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        cx.notify();
        let (e, pid, conv) = (engine(cx), self.poll_id.clone(), self.conversation.clone());
        let draft = self.draft.clone().unwrap_or_else(|| PollVoteDraft::new(&d));
        cx.spawn(async move |this, cx| {
            let r = match action {
                PollAction::Vote => e.vote_poll(&pid, &conv, &draft, &d).await,
                PollAction::Lock => e.lock_poll(&pid, &conv).await,
                PollAction::Share => e.share_poll(&pid, &conv).await,
                PollAction::Unpin => e.unpin_poll(&pid, &conv).await,
                _ => return,
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if r.ok {
                    if action != PollAction::Unpin {
                        cx.emit(PollSheetEvent::Close);
                    }
                } else {
                    this.message = Some(r.message(action).into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pin(&mut self, confirming: Option<String>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        let (e, pid, conv) = (engine(cx), self.poll_id.clone(), self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let outcome = e.pin_poll(&pid, &conv, confirming.as_deref()).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    PollPinOutcome::Done => {}
                    PollPinOutcome::NeedsConfirmation(c) => this.confirm = Some(Confirm::Pin(c)),
                    PollPinOutcome::Failed(r) => this.message = Some(r.message(PollAction::Pin).into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_submit(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.detail(cx) else { return };
        let Some(draft) = self.draft.clone() else { return };
        if d.is_expired(now_ms()) {
            self.message = Some(format!("Bình chọn {} đã hết hạn", d.question));
        } else if draft.has_duplicates(&d) {
            self.message = Some("Phương án được thêm đã tồn tại".into());
        } else if d.hide_preview && !draft.votes_nothing() {
            self.confirm = Some(Confirm::Hide);
        } else {
            self.run(PollAction::Vote, cx);
        }
        cx.notify();
    }

    fn render_vote_page(&mut self, d: &PollDetail, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.draft.as_ref().is_none_or(|x| !x.has_changes()) {
            self.draft = Some(PollVoteDraft::new(d));
            self.added_inputs.clear();
        }
        let draft = self.draft.clone().unwrap();
        let preview = draft.applied(d);
        let locked = d.is_voting_locked(now_ms());
        let pinned = engine(cx).state.borrow().polls.pins.get(&self.poll_id).is_some_and(|p| p.pinned);
        let add_visible = d.allow_add && !locked && ((d.options.len() + draft.added.len()) as i64) < self.config.max_options;
        let mut byline = vec![];
        if !d.creator_name.is_empty() {
            byline.push(format!("Tạo bởi {}", d.creator_name));
        }
        if let Some(c) = d.created {
            byline.push(crate::format::local(c).format("%d/%m").to_string());
        }
        let mut col = div()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).child(if d.question.is_empty() { self.question.clone() } else { d.question.clone() }))
            .when(!byline.is_empty(), |el| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(byline.join(" - "))))
            .when(pinned, |el| el.child(div().flex().items_center().gap(px(4.0)).text_size(px(11.5)).text_color(theme.accent).child(icon("pin.fill", 10.0, theme.accent)).child("Đã ghim lên đầu trò chuyện")))
            .child(settings_lines(d, true, theme));
        if d.voter_count > 0 {
            let vis = d.voters_visible();
            col = col.child(
                div()
                    .id("poll-voters-link")
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if vis { theme.accent } else { theme.secondary() })
                    .child(format!("{} người bình chọn", d.voter_count))
                    .when(vis, |el| el.child(icon("chevron.right", 9.0, theme.accent)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if vis {
                            this.show_voters = true;
                            this.load_voters(cx);
                            cx.notify();
                        }
                    })),
            );
        }
        if d.results_hidden() && !locked {
            col = col.child(div().text_size(px(11.5)).text_color(theme.secondary()).child("Người tham gia chỉ có thể thấy kết quả sau khi bình chọn."));
        }
        let mut opts = div().flex().flex_col().gap(px(6.0));
        for o in &d.options {
            let shown = preview.options.iter().find(|x| x.id == o.id).cloned().unwrap_or_else(|| o.clone());
            let on = draft.selected.contains(&o.id);
            let id = o.id.clone();
            opts = opts.child(
                div()
                    .id(SharedString::from(format!("opt-{}", o.id)))
                    .child(option_row(&shown, preview.weight(&shown), !d.results_hidden(), Some((d.multi, on, !locked && !self.busy)), theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(d) = this.detail(cx) else { return };
                        if d.is_voting_locked(now_ms()) || this.busy {
                            return;
                        }
                        this.draft.get_or_insert_with(|| PollVoteDraft::new(&d)).toggle(&id);
                        cx.notify();
                    })),
            );
        }
        for (aid, input) in self.added_inputs.clone() {
            let Some(row) = draft.added.iter().find(|a| a.id == aid).cloned() else { continue };
            let dup = draft.is_duplicate(aid, d);
            let empty = row.text.trim().is_empty();
            let sym = match (d.multi, row.voted) {
                (true, true) => "checkmark.square.fill",
                (true, false) => "square",
                (false, true) => "largecircle.fill.circle",
                (false, false) => "circle",
            };
            opts = opts.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!("added-vote-{aid}")))
                                    .when(empty, |el| el.opacity(0.5))
                                    .child(icon(sym, 16.0, if row.voted { theme.accent } else { theme.secondary() }))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(d) = this.draft.as_mut() {
                                            let on = d.added.iter().find(|a| a.id == aid).is_some_and(|a| a.voted);
                                            let filled = d.added.iter().find(|a| a.id == aid).is_some_and(|a| !a.text.trim().is_empty());
                                            if filled {
                                                d.set_added_voted(aid, !on);
                                            }
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1().px(px(8.0)).py(px(5.0)).rounded(px(8.0)).border_1().border_color(theme.separator()).child(Input::new(&input).appearance(false).text_size(px(13.0))))
                            .child(
                                div()
                                    .id(SharedString::from(format!("added-remove-{aid}")))
                                    .child(icon("xmark.circle.fill", 13.0, theme.secondary()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(d) = this.draft.as_mut() {
                                            d.remove_added(aid);
                                        }
                                        this.added_inputs.retain(|(i, _)| *i != aid);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(dup, |el| el.child(div().pl(px(24.0)).text_size(px(11.0)).text_color(theme.red()).child("Phương án được thêm đã tồn tại"))),
            );
        }
        col = col.child(opts);
        if add_visible {
            let disabled = draft.has_duplicates(d);
            col = col.child(div().flex().child(button("poll-add-option", "+ Thêm lựa chọn", ButtonStyle::Glass, disabled, theme, {
                let me = cx.entity().downgrade();
                move |_, window, cx| {
                    let _ = me.update(cx, |this, cx| {
                        if let Some(d) = this.detail(cx) {
                            this.add_row(&d, window, cx);
                        }
                    });
                }
            })));
        }
        let lock_note = if d.closed {
            Some(("lock.fill", "Bình chọn đã đóng"))
        } else if d.is_expired(now_ms()) {
            Some(("clock", "Đã hết hạn"))
        } else if d.hide_preview && d.joined {
            Some(("lock.fill", "Không thể thay đổi bình chọn"))
        } else {
            None
        };
        if let Some((sym, text)) = lock_note {
            col = col.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.0)).text_color(theme.secondary()).child(icon(sym, 11.0, theme.secondary())).child(text));
        }
        col.into_any_element()
    }

    fn render_voters_page(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().p(px(16.0)).flex().flex_col().gap(px(14.0));
        match (&self.voters, self.voters_failed) {
            (Some(v), _) => {
                if v.restricted || v.groups.is_empty() {
                    col = col.child(div().text_size(px(12.5)).text_color(theme.secondary()).child(if v.restricted { "Danh sách người bình chọn không được hiển thị." } else { "Chưa có ai bình chọn." }));
                }
                for g in v.groups.clone() {
                    let mut gcol = div().flex().flex_col().gap(px(6.0)).child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(format!("{} ({})", g.content, g.votes)));
                    for u in &g.users {
                        gcol = gcol.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.0))
                                .child(avatar(u.avatar.as_deref(), &u.name, 28.0, cx))
                                .child(div().text_size(px(13.0)).child(u.name.clone()))
                                .when(u.is_me, |el| el.child(div().text_size(px(11.0)).text_color(theme.secondary()).child("Bạn"))),
                        );
                    }
                    col = col.child(gcol);
                }
            }
            (None, true) => {
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Không tải được danh sách người bình chọn"))
                        .child(button("voters-retry", "Thử lại", ButtonStyle::Glass, false, theme, {
                            let me = cx.entity().downgrade();
                            move |_, _, cx| {
                                let _ = me.update(cx, |this, cx| this.load_voters(cx));
                            }
                        })),
                )
            }
            _ => col = col.child(div().min_h(px(120.0)).flex().items_center().justify_center().child(spinner(12.0, theme.secondary()))),
        }
        col.into_any_element()
    }
}

impl Render for PollDetailSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let detail = self.detail(cx);
        let failed = engine(cx).state.borrow().polls.failed.contains(&self.poll_id);
        let pin_state = engine(cx).state.borrow().polls.pins.get(&self.poll_id).copied().unwrap_or_default();
        let body: AnyElement = match &detail {
            Some(d) if self.show_voters => self.render_voters_page(&theme, cx),
            Some(d) => {
                let d = d.clone();
                self.render_vote_page(&d, &theme, cx)
            }
            None if failed => {
                let (pid, conv) = (self.poll_id.clone(), self.conversation.clone());
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.0))
                    .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Có lỗi xảy ra khi lấy thông tin bình chọn"))
                    .child(button("poll-retry", "Thử lại", ButtonStyle::Glass, false, &theme, move |_, _, cx| engine(cx).retry_poll(&pid, &conv)))
                    .into_any_element()
            }
            None => div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element(),
        };
        let locked = detail.as_ref().is_none_or(|d| d.is_voting_locked(now_ms()));
        let can_confirm = match (&detail, &self.draft) {
            (Some(d), Some(x)) => x.has_changes() && !x.has_duplicates(d),
            _ => false,
        };
        let me = cx.entity().downgrade();
        let act = move |f: fn(&mut PollDetailSheet, &mut Context<PollDetailSheet>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    f(this, cx);
                    cx.notify();
                });
            }
        };
        let mut header = div().px(px(16.0)).py(px(12.0)).flex().items_center().gap(px(8.0)).border_b_1().border_color(theme.separator());
        if self.show_voters {
            header = header.child(div().id("poll-back").child(icon("chevron.left", 13.0, theme.label())).on_click(cx.listener(|this, _, _, cx| {
                this.show_voters = false;
                cx.notify();
            })));
        }
        header = header.child(div().flex_1().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(if self.show_voters { "Chi tiết bình chọn" } else { "Bình chọn" }));
        if let (Some(d), false) = (&detail, self.show_voters) {
            header = header.child(div().id("poll-menu").child(icon("ellipsis.circle", 16.0, theme.secondary())).on_click(cx.listener(|this, _, _, cx| {
                this.menu_open = !this.menu_open;
                cx.notify();
            })));
            if self.menu_open {
                let mut menu = div().absolute().top(px(44.0)).right(px(16.0)).w(px(230.0)).p(px(4.0)).rounded(px(10.0)).bg(theme.content_bg()).border_1().border_color(theme.separator()).shadow_lg().flex().flex_col();
                let item = |id: &'static str, label: &'static str, red: bool| {
                    div().id(id).px(px(10.0)).py(px(6.0)).rounded(px(6.0)).text_size(px(13.0)).when(red, |el| el.text_color(theme.red())).hover(|s| s.bg(theme.hover())).child(label)
                };
                if pin_state.offers_pin() {
                    menu = if pin_state.pinned {
                        menu.child(item("pm-unpin", "Bỏ ghim bình chọn", false).on_click(act(|t, cx| {
                            t.menu_open = false;
                            t.run(PollAction::Unpin, cx)
                        })))
                    } else {
                        menu.child(item("pm-pin", "Ghim lên đầu trò chuyện", false).on_click(act(|t, cx| {
                            t.menu_open = false;
                            t.pin(None, cx)
                        })))
                    };
                }
                menu = menu.child(item("pm-share", "Gửi vào nhóm", false).on_click(act(|t, cx| {
                    t.menu_open = false;
                    t.run(PollAction::Share, cx)
                })));
                if d.can_close && !d.closed {
                    menu = menu.child(item("pm-lock", "Khóa bình chọn", true).on_click(act(|t, _| {
                        t.menu_open = false;
                        t.confirm = Some(Confirm::Lock)
                    })));
                }
                header = header.child(menu);
            }
        }
        header = header.child(div().id("poll-close").child(icon("xmark", 12.0, theme.secondary())).on_click(act(|t, cx| t.close(cx))));
        let mut footer = div().px(px(16.0)).py(px(12.0)).flex().flex_col().gap(px(6.0)).border_t_1().border_color(theme.separator());
        if let Some(m) = &self.message {
            footer = footer.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.0)).text_color(theme.red()).child(icon("exclamationmark.triangle.fill", 11.0, theme.red())).child(m.clone()));
        }
        let mut buttons = div().flex().justify_end().gap(px(8.0));
        buttons = if self.show_voters || detail.is_none() || locked {
            buttons.child(button("poll-done", "Đóng", ButtonStyle::Glass, false, &theme, act(|_, cx| cx.emit(PollSheetEvent::Close))))
        } else {
            buttons
                .child(button("poll-cancel", "Hủy", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx))))
                .child(button("poll-confirm", if self.busy { "Đang gửi…" } else { "Xác nhận" }, ButtonStyle::Prominent, !can_confirm || self.busy, &theme, act(|t, cx| t.request_submit(cx))))
        };
        footer = footer.child(buttons);
        let confirm = self.confirm.as_ref().map(|c| {
            let me = cx.entity().downgrade();
            let (title, msg, ok, destructive) = match c {
                Confirm::Lock => ("Khóa bình chọn?", "Sau khi khóa, bạn & các thành viên khác sẽ không thể tiếp tục tham gia bình chọn".to_string(), "Khóa bình chọn", true),
                Confirm::Hide => ("Ẩn kết quả khi chưa bình chọn", "Bạn không thể thay đổi lựa chọn".into(), "Xác nhận", false),
                Confirm::Exit => ("Bạn chưa lưu bình chọn của mình. Thoát bình chọn?", String::new(), "Thoát", true),
                Confirm::Pin(p) => ("Cập nhật danh sách ghim", p.message(), "Cập nhật", false),
            };
            alert(title, &msg, ok, if matches!(c, Confirm::Lock) { "Không" } else { "Hủy" }, destructive, &theme, move |yes, cx| {
                let _ = me.update(cx, |this, cx| {
                    let c = this.confirm.take();
                    if yes {
                        match c {
                            Some(Confirm::Lock) => this.run(PollAction::Lock, cx),
                            Some(Confirm::Hide) => this.run(PollAction::Vote, cx),
                            Some(Confirm::Exit) => cx.emit(PollSheetEvent::Close),
                            Some(Confirm::Pin(p)) => this.pin(Some(p.r#ref), cx),
                            None => {}
                        }
                    }
                    cx.notify();
                });
            })
        });
        div()
            .id("poll-sheet")
            .relative()
            .child(crate::widgets::occluder())
            .relative()
            .w(px(440.0))
            .h(px(560.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .child(div().id("poll-body").flex_1().min_h(px(0.0)).overflow_y_scroll().child(body))
            .child(footer)
            .children(confirm)
    }
}

// MARK: Create sheet

#[derive(Clone, Copy, PartialEq)]
enum Deadline {
    None,
    Hours(i64),
}

pub struct PollCreateSheet {
    conversation: String,
    caps: PollCapabilities,
    loaded: bool,
    draft: PollCreateDraft,
    question: Entity<InputState>,
    options: Vec<Entity<InputState>>,
    deadline: Deadline,
    advanced: bool,
    busy: bool,
    message: Option<String>,
    confirm_discard: bool,
}

impl EventEmitter<PollSheetEvent> for PollCreateSheet {}

impl PollCreateSheet {
    pub fn new(conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let question = cx.new(|cx| InputState::new(window, cx).placeholder("Nội dung bình chọn?"));
        cx.subscribe(&question, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.draft.question = input.read(cx).value().chars().take(this.caps.config.max_question.max(0) as usize).collect();
                cx.notify();
            }
        })
        .detach();
        question.update(cx, |s, cx| s.focus(window, cx));
        let config = PollConfig::default();
        let mut me = PollCreateSheet {
            conversation: conversation.clone(),
            caps: PollCapabilities::default(),
            loaded: false,
            draft: PollCreateDraft::new(&config, now_ms()),
            question,
            options: vec![],
            deadline: Deadline::None,
            advanced: false,
            busy: false,
            message: None,
            confirm_discard: false,
        };
        for _ in 0..me.draft.options.len() {
            me.push_option_input(window, cx);
        }
        let e = engine(cx);
        cx.spawn(async move |this, cx| {
            let caps = e.poll_capabilities(&conversation).await;
            let _ = this.update(cx, |this, cx| {
                this.caps = caps;
                this.loaded = true;
                let missing = (caps.config.min_options as usize).saturating_sub(this.draft.options.len());
                // The inputs for the extra slots are created on the next render, where the window is at hand.
                for _ in 0..missing {
                    this.draft.options.push(String::new());
                }
                cx.notify();
            });
        })
        .detach();
        me
    }

    fn push_option_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.options.len();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Lựa chọn {}", index + 1)));
        cx.subscribe(&input, move |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let text: String = input.read(cx).value().chars().take(this.caps.config.max_option.max(0) as usize).collect();
                if let Some(slot) = this.draft.options.get_mut(index) {
                    *slot = text;
                }
                cx.notify();
            }
        })
        .detach();
        self.options.push(input);
        if self.draft.options.len() < self.options.len() {
            self.draft.options.push(String::new());
        }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        self.draft.has_deadline = self.deadline != Deadline::None;
        if let Deadline::Hours(h) = self.deadline {
            self.draft.deadline = now + h as f64 * 3_600_000.0;
        }
        if self.draft.issue(&self.caps.config, now).is_some() || !self.caps.can_create || self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        let (e, conv, draft, caps) = (engine(cx), self.conversation.clone(), self.draft.clone(), self.caps);
        cx.spawn(async move |this, cx| {
            let r = e.create_poll(&conv, &draft, &caps.config, caps.can_pin).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if r.ok {
                    if r.pin_skipped {
                        e.show_toast("Đã tạo bình chọn nhưng chưa ghim được (bảng ghim đã đầy hoặc chưa kiểm tra được). Bạn có thể ghim từ menu bình chọn.", Some(&conv));
                    }
                    cx.emit(PollSheetEvent::Close);
                } else {
                    this.message = Some(r.message(PollAction::Create).into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_close(&mut self, cx: &mut Context<Self>) {
        if !self.draft.question.trim().is_empty() || !self.draft.filled_options().is_empty() {
            self.confirm_discard = true;
            cx.notify();
        } else {
            cx.emit(PollSheetEvent::Close);
        }
    }
}

fn toggle_row(id: &'static str, label: &str, on: bool, enabled: bool, theme: &Theme, f: impl Fn(&mut PollCreateSheet) + 'static, cx: &mut Context<PollCreateSheet>) -> AnyElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.0))
        .py(px(3.0))
        .when(!enabled, |el| el.opacity(0.5))
        .child(div().flex_1().text_size(px(13.0)).child(label.to_string()))
        .child(
            div()
                .w(px(34.0))
                .h(px(20.0))
                .rounded_full()
                .p(px(2.0))
                .flex()
                .when(on, |el| el.justify_end().bg(theme.accent))
                .when(!on, |el| el.bg(theme.quaternary()))
                .child(div().size(px(16.0)).rounded_full().bg(gpui::white())),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if enabled {
                f(this);
                cx.notify();
            }
        }))
        .into_any_element()
}

impl Render for PollCreateSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        while self.options.len() < self.draft.options.len() {
            self.push_option_input(window, cx);
        }
        let config = self.caps.config;
        self.draft.has_deadline = self.deadline != Deadline::None;
        if let Deadline::Hours(h) = self.deadline {
            self.draft.deadline = now_ms() + h as f64 * 3_600_000.0;
        }
        let issue = self.draft.issue(&config, now_ms());
        let mut form = div().p(px(16.0)).flex().flex_col().gap(px(14.0));
        let notice = |t: &str| div().p(px(10.0)).rounded(px(10.0)).bg(with_alpha(theme.label(), 0.06)).flex().items_center().gap(px(6.0)).text_size(px(12.5)).text_color(theme.secondary()).child(icon("info.circle.fill", 12.0, theme.secondary())).child(t.to_string());
        if self.loaded && !self.caps.ready {
            form = form.child(notice("Chưa đọc được thông tin nhóm hoặc dịch vụ bình chọn của Zalo."));
        } else if self.loaded && !self.caps.can_create {
            form = form.child(notice("Chỉ trưởng/phó nhóm được phép tạo bình chọn"));
        }
        let field = |input: &Entity<InputState>| div().px(px(10.0)).py(px(6.0)).rounded(px(8.0)).border_1().border_color(theme.separator()).child(Input::new(input).appearance(false).text_size(px(13.0)));
        form = form.child(
            div().flex().flex_col().gap(px(4.0)).child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Chủ đề bình chọn")).child(field(&self.question)),
        );
        let mut opts = div().flex().flex_col().gap(px(6.0)).child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Các lựa chọn"));
        for (i, input) in self.options.clone().into_iter().enumerate() {
            let dup = self.draft.is_duplicate(i);
            opts = opts.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().flex().items_center().gap(px(6.0)).child(div().flex_1().child(field(&input))).when((self.options.len() as i64) > config.min_options, |el| {
                        el.child(div().id(SharedString::from(format!("rm-opt-{i}"))).child(icon("xmark.circle.fill", 13.0, theme.secondary())).on_click(cx.listener(move |this, _, window, cx| {
                            if i < this.draft.options.len() {
                                this.draft.options.remove(i);
                                // Rebuild the inputs so each keeps writing to its own slot.
                                let values = this.draft.options.clone();
                                this.options.clear();
                                for (j, v) in values.iter().enumerate() {
                                    this.push_option_input(window, cx);
                                    let inp = this.options[j].clone();
                                    let v = v.clone();
                                    inp.update(cx, |s, cx| s.set_value(v, window, cx));
                                }
                                this.draft.options = values;
                            }
                            cx.notify();
                        })))
                    }))
                    .when(dup, |el| el.child(div().text_size(px(11.0)).text_color(theme.red()).child("Bạn đã tạo trùng phương án."))),
            );
        }
        let can_add = (self.options.len() as i64) < config.max_options && !(0..self.draft.options.len()).any(|i| self.draft.is_duplicate(i));
        opts = opts.child(div().flex().child(button("poll-add", "+ Thêm lựa chọn", ButtonStyle::Glass, !can_add, &theme, {
            let me = cx.entity().downgrade();
            move |_, window, cx| {
                let _ = me.update(cx, |this, cx| {
                    this.push_option_input(window, cx);
                    cx.notify();
                });
            }
        })));
        form = form.child(opts);
        form = form.child(
            div()
                .id("poll-advanced")
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .child(icon(if self.advanced { "chevron.down" } else { "chevron.right" }, 11.0, theme.secondary()))
                .child("Thiết lập nâng cao")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.advanced = !this.advanced;
                    cx.notify();
                })),
        );
        if self.advanced {
            let mut deadlines = div().flex().flex_wrap().gap(px(6.0));
            for (i, (label, d)) in [("Không thời hạn", Deadline::None), ("1 giờ", Deadline::Hours(1)), ("1 ngày", Deadline::Hours(24)), ("3 ngày", Deadline::Hours(72)), ("1 tuần", Deadline::Hours(168))].into_iter().enumerate() {
                let sel = self.deadline == d;
                deadlines = deadlines.child(
                    div()
                        .id(SharedString::from(format!("dl-{i}")))
                        .px(px(10.0))
                        .py(px(4.0))
                        .rounded_full()
                        .text_size(px(12.0))
                        .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                        .when(!sel, |el| el.bg(theme.glass()))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.deadline = d;
                            cx.notify();
                        })),
                );
            }
            let end = (self.deadline != Deadline::None).then(|| format!("Kết thúc lúc {}", deadline_text(self.draft.deadline)));
            let mut adv = div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(div().text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Thời hạn bình chọn"))
                .child(deadlines)
                .when_some(end, |el, t| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(t)))
                .child(toggle_row("tg-pin", "Ghim lên đầu trò chuyện", self.draft.pin, self.caps.can_pin, &theme, |t| t.draft.pin = !t.draft.pin, cx))
                .child(toggle_row("tg-multi", "Chọn nhiều phương án", self.draft.multi, true, &theme, |t| t.draft.multi = !t.draft.multi, cx))
                .child(toggle_row("tg-add", "Có thể thêm phương án", self.draft.effective_allow_add(), !self.draft.hide_preview, &theme, |t| t.draft.allow_add = !t.draft.allow_add, cx));
            if config.enable_hide_preview || config.enable_anonymous {
                adv = adv.child(div().pt(px(6.0)).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child("Bình chọn ẩn danh"));
                if config.enable_hide_preview {
                    adv = adv.child(toggle_row("tg-hide", "Ẩn kết quả khi chưa bình chọn", self.draft.hide_preview, true, &theme, |t| t.draft.hide_preview = !t.draft.hide_preview, cx));
                }
                if config.enable_anonymous {
                    adv = adv.child(toggle_row("tg-anon", "Ẩn người bình chọn", self.draft.anonymous, true, &theme, |t| t.draft.anonymous = !t.draft.anonymous, cx));
                }
            }
            form = form.child(adv);
        }
        let hint = issue.as_ref().map(|i| match i {
            CreateIssue::EmptyQuestion => "Vui lòng đặt câu hỏi cho bình chọn.".to_string(),
            CreateIssue::TooFewOptions(n) => format!("Bạn không thể tạo bình chọn ít hơn {n} bình chọn"),
            CreateIssue::Duplicate => "Bạn đã tạo trùng phương án.".into(),
            CreateIssue::PastDeadline => "Vui lòng chọn thời gian tương lai".into(),
        });
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        let disabled = issue.is_some() || !self.caps.can_create || self.busy;
        let footer = div()
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.separator())
            .flex()
            .flex_col()
            .gap(px(6.0))
            .when_some(self.message.clone().or(hint), |el, t| el.child(div().text_size(px(11.5)).text_color(if self.message.is_some() { theme.red() } else { theme.secondary() }).child(t)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("pc-cancel", "Hủy", ButtonStyle::Glass, false, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.request_close(cx));
                    }))
                    .child(button("pc-create", if self.busy { "Đang tạo…" } else { "Tạo bình chọn" }, ButtonStyle::Prominent, disabled, &theme, move |_, _, cx| {
                        let _ = me2.update(cx, |this, cx| this.create(cx));
                    })),
            );
        let discard = self.confirm_discard.then(|| {
            let me = cx.entity().downgrade();
            alert("Bạn muốn hủy tạo bình chọn?", "", "Hủy tạo bình chọn", "Không", true, &theme, move |yes, cx| {
                let _ = me.update(cx, |this, cx| {
                    this.confirm_discard = false;
                    if yes {
                        cx.emit(PollSheetEvent::Close);
                    }
                    cx.notify();
                });
            })
        });
        div()
            .id("poll-create")
            .relative()
            .child(crate::widgets::occluder())
            .relative()
            .w(px(460.0))
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
                    .child(div().flex_1().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Tạo bình chọn"))
                    .child(div().id("pc-close").child(icon("xmark", 12.0, theme.secondary())).on_click(cx.listener(|this, _, _, cx| this.request_close(cx)))),
            )
            .child(div().id("pc-body").flex_1().min_h(px(0.0)).overflow_y_scroll().child(form))
            .child(footer)
            .children(discard)
    }
}

/// Polls seen in the loaded messages (newest first, each once) — the inspector's "Bảng tin nhóm".
pub fn loaded_polls(messages: &[ChatMessage]) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    messages
        .iter()
        .rev()
        .filter_map(|m| match &m.kind {
            Kind::Poll(p) if !p.poll_id.is_empty() && seen.insert(p.poll_id.clone()) => Some((p.poll_id.clone(), p.question.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_polls_are_unique_and_newest_first() {
        let m = |id: &str, pid: &str, q: &str| {
            ChatMessage::from_json(&serde_json::json!({"id": id, "kind": {"type": "poll", "pollId": pid, "question": q}}), "c").unwrap()
        };
        let list = vec![m("1", "p1", "A"), m("2", "p2", "B"), m("3", "p1", "A")];
        assert_eq!(loaded_polls(&list), vec![("p1".to_string(), "A".to_string()), ("p2".to_string(), "B".to_string())]);
        assert_eq!(percent(0.666), "67%");
    }
}
