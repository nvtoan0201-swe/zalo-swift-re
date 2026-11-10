//! Conversation actions UI (ConversationActionViews.swift, HiddenChatViews.swift): the official context menu (sidebar row =
//! `Thread`, chat header = `ChatBox`), the delete confirmation, the full pin board, the hidden-chat PIN sheet and the toast.

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{OtpEvent, OtpInput, OtpState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::Disableable;
use olaz_engine::conversation_actions::{
    ConversationActionResult, ConversationPrompt, FlowKind, HiddenChatFlow, MenuPreset, PinBoard, PlanArchive, PlanHide, PlanMute, PlanPin, PlanRead,
    Screen, Step, ToastKind, WRONG_PIN,
};
use olaz_engine::notifications::MUTE_DURATIONS;

use crate::app::engine;
use crate::icon::icon;
use crate::theme::Theme;
use crate::widgets::{ButtonStyle, alert, avatar, button, spinner};

/// Menu items in the official order: pin · (Khác/Ưu tiên · Phân loại · đã đọc/chưa đọc) · (thông báo · ẩn) · xóa.
pub fn conversation_menu(menu: PopupMenu, id: &str, preset: MenuPreset, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let e = engine(cx);
    let loaded = e.state.borrow().conversation_actions.loaded;
    let mut menu = menu.min_w(px(220.0));
    let plan = e.conversation_menu_plan(id, preset);
    if !loaded {
        // The adapter has not published yet: only what does not depend on it.
        return mute_items(menu, id, e.is_conversation_muted(id), window, cx);
    }
    let id_s = id.to_string();
    let item = |label: &'static str, f: fn(&olaz_engine::ZaloEngine, &str)| {
        let id = id_s.clone();
        PopupMenuItem::new(label).on_click(move |_, _, cx| f(&engine(cx), &id))
    };
    match plan.pin {
        Some(PlanPin::Pin) => menu = menu.item(item("Ghim hội thoại", |e, id| e.pin_conversation(id))),
        Some(PlanPin::Unpin) => menu = menu.item(item("Bỏ ghim hội thoại", |e, id| e.unpin_conversation(id))),
        None => {}
    }
    let labels_shown = e.state.borrow().conversation_extras.loaded;
    if plan.pin.is_some() && (plan.archive.is_some() || labels_shown || plan.read.is_some()) {
        menu = menu.separator();
    }
    match plan.archive {
        Some(PlanArchive::MoveToOther) => menu = menu.item(item("Chuyển sang mục Khác", |e, id| e.move_conversation(id, true))),
        Some(PlanArchive::MoveToFocus) => menu = menu.item(item("Chuyển sang Tin nhắn", |e, id| e.move_conversation(id, false))),
        None => {}
    }
    menu = crate::labels::label_submenu(menu, id, window, cx);
    match plan.read {
        Some(PlanRead::MarkRead) => menu = menu.item(item("Đánh dấu đã đọc", |e, id| e.mark_conversation_read(id))),
        Some(PlanRead::MarkUnread) => menu = menu.item(item("Đánh dấu chưa đọc", |e, id| e.mark_conversation_unread(id))),
        None => {}
    }
    if (plan.mute.is_some() || plan.hide.is_some()) && (plan.pin.is_some() || plan.archive.is_some() || labels_shown || plan.read.is_some()) {
        menu = menu.separator();
    }
    if plan.mute.is_some() {
        menu = mute_items(menu, id, plan.mute == Some(PlanMute::Unmute), window, cx);
    }
    match plan.hide {
        Some(PlanHide::Hide) => {
            menu = menu.item(item("Ẩn trò chuyện", |e, id| e.set_conversation_prompt(Some(ConversationPrompt::Hide(id.into())))))
        }
        Some(PlanHide::Unhide) => {
            menu = menu.item(item("Bỏ ẩn trò chuyện", |e, id| e.set_conversation_prompt(Some(ConversationPrompt::Unhide(id.into())))))
        }
        None => {}
    }
    if plan.can_delete || plan.can_leave {
        menu = menu.separator();
    }
    if plan.can_delete {
        menu = menu.item(item("Xóa hội thoại", |e, id| e.request_delete_conversation(id)));
    }
    if plan.can_leave {
        menu = menu.item(item("Rời nhóm", |e, id| e.request_leave_group(id)));
    }
    menu
}

fn mute_items(mut menu: PopupMenu, id: &str, muted: bool, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let e = engine(cx);
    if muted {
        let info = e.notification_settings().mute_for(id, olaz_engine::notifications::now_s());
        if let Some(info) = info {
            menu = menu.item(PopupMenuItem::new(info.status_text(crate::format::now())).disabled(true));
        }
        let id = id.to_string();
        return menu.item(PopupMenuItem::new("Bật thông báo").on_click(move |_, _, cx| engine(cx).unmute_conversation(&id)));
    }
    let id = id.to_string();
    menu.submenu("Tắt thông báo", window, cx, move |mut sub, _, _| {
        for d in MUTE_DURATIONS {
            let id = id.clone();
            sub = sub.item(PopupMenuItem::new(d.title()).on_click(move |_, _, cx| engine(cx).mute_conversation(&id, d)));
        }
        sub
    })
}

pub fn conversation_name(id: &str, cx: &App) -> String {
    let e = engine(cx);
    let s = e.state.borrow();
    if s.self_conversation_id.as_deref() == Some(id) {
        return crate::shell::SELF_NAME.into();
    }
    s.conversations.iter().find(|c| c.id == id).map(|c| c.display_name()).unwrap_or_else(|| if id.starts_with('g') { "Nhóm".into() } else { "Người dùng Zalo".into() })
}

// MARK: Pin board

pub struct PinBoardSheet {
    pub conversation_id: String,
    board: Option<PinBoard>,
    load_failed: bool,
    removed: Vec<String>,
    busy: bool,
}

impl PinBoardSheet {
    pub fn new(conversation_id: String, cx: &mut Context<Self>) -> Self {
        let mut me = PinBoardSheet { conversation_id, board: None, load_failed: false, removed: vec![], busy: false };
        me.load(cx);
        me
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (e, id) = (engine(cx), self.conversation_id.clone());
        cx.spawn(async move |this, cx| {
            let board = e.pin_board(&id).await;
            let _ = this.update(cx, |this, cx| {
                this.load_failed = board.is_none();
                this.board = board;
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let (e, id, removed) = (engine(cx), self.conversation_id.clone(), self.removed.clone());
        cx.spawn(async move |this, cx| {
            let result = e.pin_replacing(removed, &id).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if result.ok {
                    e.set_conversation_prompt(None);
                } else {
                    // The unpin may have gone through while the pin failed: show the board as it is now.
                    this.removed.clear();
                    this.board = None;
                    this.load(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PinBoardSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let body: AnyElement = match &self.board {
            Some(board) => {
                let mut list = div().id("pin-board-list").max_h(px(340.0)).overflow_y_scroll().flex().flex_col();
                for id in board.items.iter().filter(|i| !self.removed.contains(i)) {
                    let hidden = engine(cx).state.borrow().conversation_actions.is_hidden(id);
                    let name = if hidden { "Trò chuyện ẩn".to_string() } else { conversation_name(id, cx) };
                    let av = if hidden {
                        div().size(px(34.0)).rounded_full().bg(theme.quaternary()).flex().items_center().justify_center().child(icon("lock.fill", 13.0, theme.secondary())).into_any_element()
                    } else {
                        let url = engine(cx).state.borrow().conversations.iter().find(|c| &c.id == id).and_then(|c| c.avatar.clone());
                        avatar(url.as_deref(), &name, 34.0, cx)
                    };
                    let rid = id.clone();
                    list = list.child(
                        div()
                            .px(px(20.0))
                            .py(px(6.0))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(av)
                            .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.5)).child(name))
                            .child(button(SharedString::from(format!("unpin-{id}")), "Bỏ ghim", ButtonStyle::Glass, false, &theme, {
                                let me = cx.entity().downgrade();
                                move |_, _, cx| {
                                    let _ = me.update(cx, |this, cx| {
                                        this.removed.push(rid.clone());
                                        cx.notify();
                                    });
                                }
                            })),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div().px(px(20.0)).pb(px(10.0)).text_center().text_size(px(13.0)).child(format!(
                            "Bạn chỉ được ghim tối đa {} trò chuyện.\nĐể ghim trò chuyện {}, vui lòng bỏ ghim ít nhất {} trò chuyện bên dưới.",
                            board.limit,
                            conversation_name(&self.conversation_id, cx),
                            board.need
                        )),
                    )
                    .child(list)
                    .into_any_element()
            }
            None if self.load_failed => div().h(px(200.0)).flex().items_center().justify_center().text_color(theme.secondary()).child("Không tải được danh sách ghim").into_any_element(),
            None => div().h(px(200.0)).flex().items_center().justify_center().child(spinner(14.0, theme.secondary())).into_any_element(),
        };
        let can_confirm = self.board.as_ref().is_some_and(|b| b.frees_slot(self.removed.len())) && !self.busy;
        let me = cx.entity().downgrade();
        sheet_frame(&theme, 440.0)
            .child(div().pt(px(18.0)).pb(px(10.0)).text_center().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Ghim hội thoại"))
            .child(body)
            .child(
                div()
                    .p(px(16.0))
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("pin-board-cancel", "Hủy", ButtonStyle::Glass, false, &theme, |_, _, cx| engine(cx).set_conversation_prompt(None)))
                    .child(button("pin-board-ok", "Ghim hội thoại", ButtonStyle::Prominent, !can_confirm, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.confirm(cx));
                    })),
            )
    }
}

fn sheet_frame(theme: &Theme, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .rounded(px(14.0))
        .bg(theme.content_bg())
        .border_1()
        .border_color(theme.separator())
        .shadow_lg()
        .flex()
        .flex_col()
}

// MARK: Hidden chat sheet

pub struct HiddenChatSheet {
    pub flow: HiddenChatFlow,
    otp: Entity<OtpState>,
    busy: bool,
    /// Settings window: results shown inline instead of the shell toast.
    pub inline_result: Option<String>,
    inline: bool,
    on_unlocked: Option<std::rc::Rc<dyn Fn(&str, &mut App)>>,
}

impl HiddenChatSheet {
    pub fn new(flow: HiddenChatFlow, inline: bool, on_unlocked: Option<std::rc::Rc<dyn Fn(&str, &mut App)>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let otp = cx.new(|cx| OtpState::new(4, window, cx).masked(true));
        cx.subscribe_in(&otp, window, |this, state, ev: &OtpEvent, window, cx| {
            if *ev == OtpEvent::Complete {
                let pin = state.read(cx).value().to_string();
                state.update(cx, |s, cx| s.set_value("", window, cx));
                this.submit(&pin, cx);
            }
        })
        .detach();
        otp.update(cx, |s, cx| s.focus(window, cx));
        HiddenChatSheet { flow, otp, busy: false, inline_result: None, inline, on_unlocked }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.inline {
            cx.emit(SheetClosed);
        } else {
            engine(cx).set_conversation_prompt(None);
        }
    }

    fn finish(&mut self, result: ConversationActionResult, cx: &mut Context<Self>) {
        if self.inline {
            self.inline_result = Some(result.text.clone());
        } else {
            engine(cx).report_conv(&result);
        }
        if result.ok {
            self.close(cx);
        }
    }

    fn submit(&mut self, pin: &str, cx: &mut Context<Self>) {
        let step = self.flow.submit(pin);
        self.run(step, cx);
        cx.notify();
    }

    fn run(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == Step::None {
            return;
        }
        self.busy = true;
        let e = engine(cx);
        let id = self.flow.conversation_id().map(String::from);
        cx.spawn(async move |this, cx| {
            match step {
                Step::None => {}
                Step::Hide(pin) => {
                    let r = e.hide_conversation(id.as_deref().unwrap_or(""), &pin, false).await;
                    let _ = this.update(cx, |this, cx| {
                        if r.bad_pin {
                            this.flow.fail(&r.text);
                        } else {
                            e.report_conv(&r);
                            if r.ok {
                                this.close(cx);
                            }
                        }
                    });
                }
                Step::HideWithNewPin(pin) => {
                    let r = e.hide_conversation(id.as_deref().unwrap_or(""), &pin, true).await;
                    let _ = this.update(cx, |this, _| {
                        if r.ok {
                            this.flow.succeeded();
                        } else if r.pin_exists {
                            this.flow.pin_already_exists();
                        } else {
                            this.flow.fail(&r.text);
                        }
                    });
                }
                Step::Unhide(pin) => {
                    let r = e.unhide_conversation(id.as_deref().unwrap_or(""), pin.as_deref()).await;
                    let _ = this.update(cx, |this, cx| {
                        if r.bad_pin {
                            this.flow.fail(&r.text);
                        } else {
                            e.report_conv(&r);
                            if r.ok {
                                this.close(cx);
                            }
                        }
                    });
                }
                Step::VerifyToOpen(pin) => {
                    let ok = e.check_hidden_chat_pin(&pin).await;
                    let _ = this.update(cx, |this, cx| {
                        if ok {
                            let id = id.clone().unwrap_or_default();
                            e.unlock_hidden_conversation(&id);
                            if let Some(f) = this.on_unlocked.clone() {
                                f(&id, cx);
                            }
                            this.close(cx);
                        } else {
                            this.flow.fail(WRONG_PIN);
                        }
                    });
                }
                Step::VerifyOld(pin) => {
                    let ok = e.check_hidden_chat_pin(&pin).await;
                    let _ = this.update(cx, |this, _| if ok { this.flow.old_pin_verified() } else { this.flow.fail(WRONG_PIN) });
                }
                Step::SetPin(pin, changed) => {
                    let r = e.set_hidden_chat_pin(&pin, changed).await;
                    let _ = this.update(cx, |this, _| {
                        if r.ok {
                            this.flow.succeeded();
                        } else if r.pin_exists {
                            this.flow.pin_already_exists();
                        } else {
                            this.flow.fail(&r.text);
                        }
                    });
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn reset_pin(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let e = engine(cx);
        cx.spawn(async move |this, cx| {
            let r = e.reset_hidden_chat_pin().await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.finish(r, cx);
                cx.notify();
            });
        })
        .detach();
    }
}

pub struct SheetClosed;
impl gpui::EventEmitter<SheetClosed> for HiddenChatSheet {}

impl Render for HiddenChatSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let name = self.flow.conversation_id().map(|id| conversation_name(id, cx)).unwrap_or_default();
        let text = |t: String| div().text_center().text_size(px(13.5)).child(t);
        let pin_entry = |prompt: Option<&str>, notice: bool| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.0))
                .when_some(prompt.map(String::from), |el, p| el.child(div().text_size(px(13.5)).child(p)))
                .child(OtpInput::new(&self.otp).disabled(self.busy))
                .when(notice, |el| {
                    el.child(
                        div()
                            .text_center()
                            .text_size(px(12.0))
                            .text_color(theme.secondary())
                            .child("Chú ý: Ghi nhớ mã PIN để xem lại trò chuyện. Trò chuyện sẽ mất nếu bạn cài đặt lại mã PIN."),
                    )
                })
        };
        let content: AnyElement = match self.flow.screen {
            Screen::Introduce => text("Trò chuyện sẽ được ẩn khỏi danh sách tin nhắn. Chỉ có bạn mới có thể tìm thấy và mở lại chúng bằng cách sử dụng mã PIN.".into()).into_any_element(),
            Screen::CreatePin => pin_entry(Some("Nhập mã PIN"), true).into_any_element(),
            Screen::ConfirmPin => pin_entry(Some("Nhập lại mã PIN của bạn"), true).into_any_element(),
            Screen::EnterPin => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(pin_entry(None, false))
                .child(div().text_center().text_size(px(12.0)).text_color(theme.secondary()).child("Quên mã PIN? Bạn phải cài đặt lại mã."))
                .into_any_element(),
            Screen::UnhideConfirm => text(format!("Bạn có muốn hiện lại trò chuyện {name} ?")).into_any_element(),
            Screen::OldPin => pin_entry(Some("Nhập mã PIN cũ"), false).into_any_element(),
            Screen::NewPin => pin_entry(Some("Nhập mã PIN mới"), false).into_any_element(),
            Screen::RepeatPin => pin_entry(Some("Xác nhận lại mã PIN mới"), false).into_any_element(),
            Screen::ResetConfirm => text("Khi đặt lại mã PIN, tất cả trò chuyện đang được ẩn sẽ bị xoá (bao gồm toàn bộ nội dung cuộc trò chuyện). Bạn có muốn tiếp tục?".into()).into_any_element(),
            Screen::HideSuccess => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(text(format!("Bạn đã ẩn trò chuyện {name} thành công.")))
                .child(text("Để mở các trò chuyện đã ẩn, bạn có thể nhập mã PIN đã cài đặt hoặc tên trò chuyện ẩn tại khung tìm kiếm.".into()))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.secondary())
                        .child("Hướng dẫn: Ngoài ra bạn có thể vào Cài đặt > Tin nhắn > Thiết lập ẩn trò chuyện để thay đổi hoặc đặt lại mã PIN."),
                )
                .into_any_element(),
            Screen::SetPinSuccess | Screen::ChangeSuccess => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(icon("checkmark.circle.fill", 40.0, theme.green()))
                .child(text(
                    if self.flow.screen == Screen::SetPinSuccess {
                        "Chúc mừng bạn đã cài mã PIN thành công, bạn có thể dùng mã PIN này để ẩn các trò chuyện và mở lại các trò chuyện đã ẩn bằng cách tìm kiếm theo tên hoặc mã PIN vừa cài đặt."
                    } else {
                        "Mã PIN của bạn đã được đổi thành công"
                    }
                    .into(),
                ))
                .into_any_element(),
        };
        let me = cx.entity().downgrade();
        let act = move |f: fn(&mut HiddenChatSheet, &mut Context<HiddenChatSheet>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    f(this, cx);
                    cx.notify();
                });
            }
        };
        let busy = self.busy;
        let mut buttons = div().flex().justify_center().gap(px(8.0));
        buttons = match self.flow.screen {
            Screen::Introduce => buttons
                .child(button("hc-cancel", "Hủy", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx))))
                .child(button("hc-set", "Đặt mã PIN", ButtonStyle::Prominent, false, &theme, act(|t, _| t.flow.start_setting_pin()))),
            Screen::UnhideConfirm => buttons
                .child(button("hc-cancel", "Hủy", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx))))
                .child(button("hc-unhide", "Bỏ ẩn trò chuyện", ButtonStyle::Prominent, busy, &theme, act(|t, cx| t.run(Step::Unhide(None), cx)))),
            Screen::ResetConfirm => buttons
                .child(button("hc-close", "Đóng", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx))))
                .child(button("hc-reset", "Đặt lại mã PIN", ButtonStyle::Destructive, busy, &theme, act(|t, cx| t.reset_pin(cx)))),
            Screen::HideSuccess | Screen::SetPinSuccess | Screen::ChangeSuccess => {
                buttons.child(button("hc-done", "Đóng", ButtonStyle::Prominent, false, &theme, act(|t, cx| t.close(cx))))
            }
            Screen::RepeatPin | Screen::ConfirmPin => buttons
                .child(button("hc-back", "Quay lại", ButtonStyle::Glass, false, &theme, act(|t, _| t.flow.back())))
                .child(button("hc-cancel", "Hủy", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx)))),
            _ => buttons.child(button("hc-cancel", "Hủy", ButtonStyle::Glass, false, &theme, act(|t, cx| t.close(cx)))),
        };
        sheet_frame(&theme, 380.0)
            .p(px(22.0))
            .gap(px(16.0))
            .items_center()
            .child(div().text_center().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(self.flow.title()))
            .child(content)
            .when_some(self.flow.error.clone(), |el, e| el.child(div().text_center().text_size(px(12.5)).text_color(theme.red()).child(e)))
            .when_some(self.inline_result.clone(), |el, r| el.child(div().text_center().text_size(px(12.0)).text_color(theme.secondary()).child(r)))
            .child(buttons)
    }
}

// MARK: Presenter

/// Sheets currently presented for the engine's `conversation_prompt` (kept by the shell so their state survives renders).
#[derive(Default)]
pub struct PromptHost {
    pub shown: Option<ConversationPrompt>,
    pub pin_board: Option<Entity<PinBoardSheet>>,
    pub hidden: Option<Entity<HiddenChatSheet>>,
    pub labels: Option<Entity<crate::labels::LabelManagerSheet>>,
}

impl PromptHost {
    /// Creates / drops the sheet entities to match the engine's prompt.
    pub fn sync(&mut self, on_unlocked: std::rc::Rc<dyn Fn(&str, &mut App)>, window: &mut Window, cx: &mut App) {
        let e = engine(cx);
        let prompt = e.state.borrow().conversation_prompt.clone();
        if prompt == self.shown {
            return;
        }
        self.pin_board = None;
        self.hidden = None;
        self.labels = None;
        let (has_pin, unlocked) = {
            let s = e.state.borrow();
            (s.conversation_actions.has_hidden_pin, s.conversation_ui.unlocked_id.clone())
        };
        let flow = match &prompt {
            Some(ConversationPrompt::PinBoard(id)) => {
                let id = id.clone();
                self.pin_board = Some(cx.new(|cx| PinBoardSheet::new(id, cx)));
                None
            }
            Some(ConversationPrompt::LabelManager) => {
                self.labels = Some(cx.new(crate::labels::LabelManagerSheet::new));
                None
            }
            Some(ConversationPrompt::Hide(id)) => Some(HiddenChatFlow::new(FlowKind::Hide(id.clone()), has_pin)),
            Some(ConversationPrompt::Unhide(id)) => Some(HiddenChatFlow::new(FlowKind::Unhide(id.clone(), unlocked.as_deref() != Some(id)), has_pin)),
            Some(ConversationPrompt::Unlock(id)) => Some(HiddenChatFlow::new(FlowKind::Open(id.clone()), true)),
            _ => None,
        };
        if let Some(flow) = flow {
            self.hidden = Some(cx.new(|cx| HiddenChatSheet::new(flow, false, Some(on_unlocked), window, cx)));
        }
        self.shown = prompt;
    }

    pub fn render(&self, theme: &Theme, cx: &App) -> Option<AnyElement> {
        let prompt = self.shown.clone()?;
        if let ConversationPrompt::Delete(id) = prompt {
            return Some(alert(
                olaz_engine::conversation_actions::DELETE_LINE1,
                olaz_engine::conversation_actions::DELETE_LINE2,
                olaz_engine::conversation_actions::DELETE_CONFIRM,
                olaz_engine::conversation_actions::DELETE_CANCEL,
                true,
                theme,
                move |ok, cx| {
                    let e = engine(cx);
                    e.set_conversation_prompt(None);
                    if ok {
                        e.confirm_delete_conversation(&id);
                    }
                },
            ));
        }
        if let ConversationPrompt::LeaveGroup { id, owner } = prompt {
            use olaz_engine::conversation_actions::{DELETE_CANCEL, LEAVE_CONFIRM, LEAVE_MESSAGE, LEAVE_OWNER_MESSAGE, LEAVE_TITLE};
            return Some(alert(LEAVE_TITLE, if owner { LEAVE_OWNER_MESSAGE } else { LEAVE_MESSAGE }, LEAVE_CONFIRM, DELETE_CANCEL, true, theme, move |ok, cx| {
                let e = engine(cx);
                e.set_conversation_prompt(None);
                if ok {
                    e.confirm_leave_group(&id);
                }
            }));
        }
        let _ = cx;
        let sheet: AnyElement = match (&self.pin_board, &self.hidden, &self.labels) {
            (Some(p), _, _) => p.clone().into_any_element(),
            (_, Some(h), _) => h.clone().into_any_element(),
            (_, _, Some(l)) => l.clone().into_any_element(),
            _ => return None,
        };
        Some(
            div()
                .id("conv-prompt-backdrop")
                .absolute()
                .inset_0()
                .bg(gpui::black().opacity(0.2))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(|ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        engine(cx).set_conversation_prompt(None);
                    }
                })
                .child(sheet)
                .into_any_element(),
        )
    }
}

/// Toast of the latest conversation action, for 3.2 s.
pub fn conversation_toast(theme: &Theme, cx: &App) -> Option<AnyElement> {
    let toast = engine(cx).state.borrow().conversation_ui.toast.clone()?;
    let (sym, color) = match toast.kind {
        ToastKind::Success => ("checkmark.circle.fill", theme.green()),
        ToastKind::Neutral => ("info.circle.fill", theme.secondary()),
        ToastKind::Failure => ("exclamationmark.triangle.fill", theme.orange()),
    };
    Some(
        div()
            .absolute()
            .bottom(px(22.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .max_w(px(420.0))
                    .px(px(14.0))
                    .py(px(9.0))
                    .rounded_full()
                    .bg(theme.content_bg())
                    .border_1()
                    .border_color(theme.separator())
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(icon(sym, 13.0, color))
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).line_clamp(2).child(toast.text)),
            )
            .into_any_element(),
    )
}
