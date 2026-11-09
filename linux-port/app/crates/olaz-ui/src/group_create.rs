//! "Tạo nhóm" sheet (port of CreateGroupSheet.swift): name, optional picture, friends picked from the roster (recent first),
//! then the official create call. Opened by [`request`] and presented by the shell.

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FontWeight, Image, ImageFormat, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, img, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::group_create::{
    BLOCKED_TEXT, GroupAvatarImage, GroupComposeRoster, GroupComposeSelection, GroupCreateLimits, GroupCreateOutcome, INVALID_NAME_TEXT, RosterPerson,
    SelectionChange,
};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::Theme;
use crate::widgets::{ButtonStyle, alert, avatar, button};

thread_local! {
    static REQUEST: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Opens the sheet with `members` already picked.
pub fn request(members: Vec<String>, cx: &mut App) {
    REQUEST.with(|r| *r.borrow_mut() = Some(members));
    cx.refresh_windows();
}

pub fn take_request() -> Option<Vec<String>> {
    REQUEST.with(|r| r.borrow_mut().take())
}

pub enum GroupSheetEvent {
    Close,
}

pub struct CreateGroupSheet {
    limits: GroupCreateLimits,
    name: Entity<InputState>,
    search: Entity<InputState>,
    query: String,
    selection: GroupComposeSelection,
    avatar: Option<(GroupAvatarImage, Arc<Image>)>,
    creating: bool,
    notice: Option<(u64, String)>,
    notice_seq: u64,
    problem: Option<(String, String, bool)>,
    confirm_cancel: bool,
}

impl EventEmitter<GroupSheetEvent> for CreateGroupSheet {}

impl CreateGroupSheet {
    pub fn new(members: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Nhập tên nhóm..."));
        cx.subscribe_in(&name, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::Change = ev {
                let v = input.read(cx).value().to_string();
                let c = this.limits.clamped_name(&v);
                if c != v {
                    input.update(cx, |s, cx| s.set_value(c, window, cx));
                }
                cx.notify();
            }
        })
        .detach();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Nhập tên bạn bè"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        name.update(cx, |s, cx| s.focus(window, cx));
        let e = engine(cx);
        let mut selection = GroupComposeSelection { ids: members };
        if e.state.borrow().contacts.friends_loaded {
            let friends: std::collections::HashSet<String> = e.state.borrow().contacts.friends.iter().map(|f| f.id.clone()).collect();
            selection.keep(|id| friends.contains(id));
        }
        cx.spawn(async move |this, cx| {
            if let Some(l) = e.group_create_limits_when_ready(20).await {
                let _ = this.update(cx, |this, cx| {
                    let blocked = l.blocked.clone();
                    this.selection.keep(|id| !blocked.contains(id));
                    this.limits = l;
                    cx.notify();
                });
            }
        })
        .detach();
        CreateGroupSheet {
            limits: GroupCreateLimits::default(),
            name,
            search,
            query: String::new(),
            selection,
            avatar: None,
            creating: false,
            notice: None,
            notice_seq: 0,
            problem: None,
            confirm_cancel: false,
        }
    }

    fn roster(&self, cx: &App) -> GroupComposeRoster {
        let e = engine(cx);
        let self_id = e.state.borrow().self_conversation_id.clone();
        GroupComposeRoster::new(&e.visible_friends(), &e.visible_conversations(), self_id.as_deref(), &self.limits.official_accounts)
    }

    fn flash(&mut self, text: &str, cx: &mut Context<Self>) {
        self.notice_seq += 1;
        let seq = self.notice_seq;
        self.notice = Some((seq, text.into()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(2500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.notice.as_ref().is_some_and(|n| n.0 == seq) {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.selection.toggle(id, &self.limits) {
            SelectionChange::Added if !self.query.is_empty() => {
                self.search.update(cx, |s, cx| {
                    s.set_value("", window, cx);
                    s.focus(window, cx);
                });
                self.query.clear();
            }
            SelectionChange::OverLimit => {
                let t = self.limits.over_limit_text();
                self.flash(&t, cx)
            }
            SelectionChange::Blocked => self.flash(BLOCKED_TEXT, cx),
            _ => {}
        }
        cx.notify();
    }

    fn pick_avatar(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Chọn ảnh đại diện".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let made = cx.background_executor().spawn(async move { GroupAvatarImage::from_file(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                match made {
                    Ok(a) => {
                        let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, a.jpeg.clone()));
                        this.avatar = Some((a, image));
                    }
                    Err(e) => this.problem = Some(("Không thể dùng ảnh này".into(), e.text().into(), false)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn name(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_string()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        if self.selection.can_confirm() {
            self.confirm_cancel = true;
            cx.notify();
        } else {
            cx.emit(GroupSheetEvent::Close);
        }
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let name = self.name(cx);
        if !self.selection.can_confirm() || !self.limits.is_valid_name(&name) || self.creating {
            return;
        }
        self.creating = true;
        let (e, members, avatar) = (engine(cx), self.selection.ids.clone(), self.avatar.as_ref().map(|a| a.0.clone()));
        cx.spawn(async move |this, cx| {
            let outcome = e.create_group(&name, &members, avatar.as_ref()).await;
            let _ = this.update(cx, |this, cx| {
                this.creating = false;
                match outcome {
                    GroupCreateOutcome::Created { .. } | GroupCreateOutcome::Exists { .. } | GroupCreateOutcome::Pending(_) => cx.emit(GroupSheetEvent::Close),
                    GroupCreateOutcome::Invalid { text, dropped } => {
                        this.selection.keep(|id| !dropped.iter().any(|d| d == id));
                        this.problem = Some(("Không thể tạo nhóm".into(), text, false));
                    }
                    GroupCreateOutcome::Failed { title, message, closes } => {
                        this.problem = Some((title.unwrap_or_else(|| "Không thể tạo nhóm".into()), message, closes));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn row(&self, p: &RosterPerson, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let checked = self.selection.contains(&p.id);
        let blocked = self.limits.blocked.contains(&p.id);
        let id = p.id.clone();
        div()
            .id(SharedString::from(format!("gc-{}", p.id)))
            .px(px(16.0))
            .py(px(5.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .when(blocked, |el| el.opacity(0.55))
            .hover(|s| s.bg(theme.hover()))
            .child(icon(if checked { "checkmark.circle.fill" } else { "circle" }, 18.0, if checked { theme.accent } else { theme.tertiary() }))
            .child(avatar(p.avatar.as_deref(), &p.name, 34.0, cx))
            .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(13.5)).child(p.name.clone()))
            .when(blocked, |el| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child("Đã chặn")))
            .on_click(cx.listener(move |this, _, window, cx| this.toggle(&id, window, cx)))
            .into_any_element()
    }
}

impl Render for CreateGroupSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let roster = self.roster(cx);
        let name = self.name(cx);
        let name_ok = self.limits.is_valid_name(&name);
        let can_create = self.selection.can_confirm() && name_ok && !self.creating;
        let face: AnyElement = match &self.avatar {
            Some((_, image)) => img(image.clone()).size_full().rounded_full().object_fit(ObjectFit::Cover).into_any_element(),
            None => div().size_full().rounded_full().flex().items_center().justify_center().bg(theme.glass()).child(icon("camera.fill", 18.0, theme.secondary())).into_any_element(),
        };
        let identity = div()
            .px(px(20.0))
            .pb(px(12.0))
            .flex()
            .items_start()
            .gap(px(12.0))
            .child(div().id("gc-avatar").size(px(52.0)).flex_none().rounded_full().overflow_hidden().cursor_pointer().child(face).on_click(cx.listener(|this, _, _, cx| this.pick_avatar(cx))))
            .child(
                div()
                    .flex_1()
                    .pt(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(div().px(px(12.0)).py(px(7.0)).rounded_full().bg(theme.glass()).border_1().border_color(if name_ok { theme.glass_border() } else { theme.red() }).child(Input::new(&self.name).appearance(false).text_size(px(14.0))))
                    .child(div().pl(px(12.0)).text_size(px(11.5)).text_color(if name_ok { theme.tertiary() } else { theme.red() }).child(if name_ok { "Để trống để dùng tên các thành viên" } else { INVALID_NAME_TEXT }))
                    .when(self.avatar.is_some(), |el| {
                        el.child(div().id("gc-avatar-remove").pl(px(12.0)).text_size(px(11.5)).text_color(theme.red()).child("Xoá ảnh đại diện").on_click(cx.listener(|this, _, _, cx| {
                            this.avatar = None;
                            cx.notify();
                        })))
                    }),
            );
        let search = div().px(px(16.0)).pb(px(10.0)).child(
            div().h(px(32.0)).px(px(10.0)).rounded_full().bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().items_center().gap(px(6.0)).child(icon("magnifyingglass", 12.0, theme.secondary())).child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(13.0)))),
        );
        let chips = (!self.selection.ids.is_empty()).then(|| {
            let mut row = div().px(px(16.0)).pb(px(10.0)).flex().flex_wrap().gap(px(6.0));
            for id in self.selection.ids.clone() {
                let p = roster.person(&id).cloned();
                let label = p.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Người dùng Zalo".into());
                let id2 = id.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("chip-{id}")))
                        .pl(px(3.0))
                        .pr(px(8.0))
                        .py(px(3.0))
                        .rounded_full()
                        .bg(theme.glass())
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .child(avatar(p.as_ref().and_then(|p| p.avatar.as_deref()), &label, 20.0, cx))
                        .child(div().text_size(px(12.0)).child(label))
                        .child(icon("xmark", 9.0, theme.secondary()))
                        .on_click(cx.listener(move |this, _, window, cx| this.toggle(&id2, window, cx))),
                );
            }
            row
        });
        let mut list = div().flex().flex_col();
        let (friends_loaded, hidden_known) = {
            let e = engine(cx);
            let st = e.state.borrow();
            (st.contacts.friends_loaded, st.conversation_actions.hidden_known)
        };
        if !friends_loaded || !hidden_known {
            list = list.child(
                div()
                    .py(px(60.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .child(crate::widgets::spinner(12.0, theme.secondary()))
                    .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Đang tải danh sách bạn bè…")),
            );
        } else if roster.friends.is_empty() {
            list = list.child(crate::widgets::content_unavailable("person.2", "Chưa có bạn bè", "Nhóm được tạo từ danh sách bạn bè của bạn.", &theme));
        } else if !self.query.trim().is_empty() {
            let found = roster.matching(&self.query);
            if found.is_empty() {
                list = list.child(div().p(px(20.0)).text_center().text_size(px(12.5)).text_color(theme.secondary()).child(format!("Không có kết quả cho “{}”", self.query.trim())));
            }
            for p in &found {
                list = list.child(self.row(p, &theme, cx));
            }
        } else {
            let header = |t: String| div().px(px(16.0)).pt(px(10.0)).pb(px(4.0)).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(t);
            if !roster.recent.is_empty() {
                list = list.child(header("Trò chuyện gần đây".into()));
                for p in &roster.recent {
                    list = list.child(self.row(p, &theme, cx));
                }
            }
            let mut current = String::new();
            for p in &roster.friends {
                if p.section != current {
                    current = p.section.clone();
                    list = list.child(header(current.clone()));
                }
                list = list.child(self.row(p, &theme, cx));
            }
        }
        let me = cx.entity().downgrade();
        let me2 = me.clone();
        let n = self.selection.ids.len();
        let footer = div()
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.separator())
            .flex()
            .flex_col()
            .gap(px(8.0))
            .when_some(self.notice.clone(), |el, (_, t)| el.child(div().text_center().text_size(px(12.0)).text_color(theme.orange()).child(t)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().text_size(px(12.0)).text_color(theme.secondary()).child(if n < 2 { "Chọn ít nhất 2 người".to_string() } else { format!("Đã chọn {n}/{}", self.limits.max_members) }))
                    .child(button("gc-cancel", "Hủy", ButtonStyle::Glass, self.creating, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.cancel(cx));
                    }))
                    .child(button("gc-create", if self.creating { "Đang tạo…" } else { "Tạo nhóm" }, ButtonStyle::Prominent, !can_create, &theme, move |_, _, cx| {
                        let _ = me2.update(cx, |this, cx| this.create(cx));
                    })),
            );
        let overlay = if self.confirm_cancel {
            let me = cx.entity().downgrade();
            Some(alert("Bạn muốn hủy tạo nhóm này?", "", "Có", "Không", true, &theme, move |yes, cx| {
                let _ = me.update(cx, |this, cx| {
                    this.confirm_cancel = false;
                    if yes {
                        cx.emit(GroupSheetEvent::Close);
                    }
                    cx.notify();
                });
            }))
        } else {
            self.problem.clone().map(|(title, message, closes)| {
                let me = cx.entity().downgrade();
                alert(&title, &message, if closes { "Đã hiểu" } else { "OK" }, "Đóng", false, &theme, move |_, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.problem = None;
                        if closes {
                            cx.emit(GroupSheetEvent::Close);
                        }
                        cx.notify();
                    });
                })
            })
        };
        div()
            .id("create-group")
            .relative()
            .child(crate::widgets::occluder())
            .relative()
            .w(px(440.0))
            .h(px(640.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.cancel(cx);
                }
            }))
            .child(div().pt(px(18.0)).pb(px(14.0)).text_center().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).child("Tạo nhóm"))
            .child(identity)
            .child(search)
            .children(chips)
            .child(
                div()
                    .id("gc-list")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme.separator())
                    .child(list),
            )
            .child(footer)
            .children(overlay)
    }
}
