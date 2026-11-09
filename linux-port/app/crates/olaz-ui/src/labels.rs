//! "Phân loại" (port of ConversationLabelViews.swift): the row submenu, the label manager and the label editor.
//! All writes go through the official label manager; results come back as `conversationExtras.notice`.

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use olaz_engine::conversation_actions::ConversationPrompt;
use olaz_engine::conversations::{Label, cap_label_name, parse_hex_color, reordered_labels, ZALO_BLUE};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::Theme;
use crate::widgets::{ButtonStyle, alert, button, occluder};

fn dot(hex: &str, size: f32) -> gpui::Div {
    div().size(px(size)).flex_none().rounded_full().bg(rgb(parse_hex_color(hex).unwrap_or(ZALO_BLUE)))
}

/// Row submenu "Phân loại": none · labels · divider · "Quản lý thẻ phân loại…". Only when extras are loaded.
pub fn label_submenu(menu: PopupMenu, id: &str, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let (loaded, labels, current) = {
        let e = engine(cx);
        let s = e.state.borrow();
        let x = &s.conversation_extras;
        (x.loaded, x.labels.clone(), x.label_by_conversation.get(id).cloned())
    };
    if !loaded {
        return menu;
    }
    let id = id.to_string();
    menu.submenu("Phân loại", window, cx, move |mut sub, _, _| {
        let cid = id.clone();
        sub = sub.item(PopupMenuItem::new("Không có phân loại").checked(current.is_none()).on_click(move |_, _, cx| engine(cx).set_conversation_label(&cid, None)));
        for l in &labels {
            let (cid, lid, name, hex) = (id.clone(), l.id.clone(), l.name.clone(), l.color_hex.clone());
            sub = sub.item(
                PopupMenuItem::element(move |_, _| div().flex().items_center().gap(px(8.0)).child(dot(&hex, 11.0)).child(name.clone()))
                    .checked(current.as_deref() == Some(l.id.as_str()))
                    .on_click(move |_, _, cx| engine(cx).set_conversation_label(&cid, Some(&lid))),
            );
        }
        sub.separator().item(PopupMenuItem::new("Quản lý thẻ phân loại…").on_click(|_, _, cx| engine(cx).set_conversation_prompt(Some(ConversationPrompt::LabelManager))))
    })
}

fn notice_seq(cx: &App) -> i64 {
    engine(cx).state.borrow().conversation_extras.notice.as_ref().map(|n| n.seq).unwrap_or(0)
}

#[derive(Clone)]
struct DraggedLabel(usize, String);

impl Render for DraggedLabel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx);
        div().px(px(10.0)).py(px(4.0)).rounded(px(8.0)).bg(theme.content_bg()).shadow_md().text_size(px(13.0)).child(self.1.clone())
    }
}

pub struct LabelManagerSheet {
    editor: Option<Entity<LabelEditor>>,
    pending_delete: Option<Label>,
    error: Option<String>,
    base_seq: i64,
}

impl LabelManagerSheet {
    pub fn new(cx: &mut Context<Self>) -> Self {
        LabelManagerSheet { editor: None, pending_delete: None, error: None, base_seq: notice_seq(cx) }
    }

    fn edit(&mut self, existing: Option<Label>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        self.editor = Some(cx.new(|cx| {
            LabelEditor::new(
                existing,
                std::rc::Rc::new(move |cx: &mut App| {
                    let _ = me.update(cx, |this, cx| {
                        this.editor = None;
                        this.base_seq = notice_seq(cx);
                        cx.notify();
                    });
                }),
                window,
                cx,
            )
        }));
        cx.notify();
    }
}

impl Render for LabelManagerSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let (labels, max) = {
            let e = engine(cx);
            let s = e.state.borrow();
            let x = &s.conversation_extras;
            // Deletes / reorders report here; create and rename report through the editor.
            if self.editor.is_none() {
                if let Some(n) = x.notice.as_ref().filter(|n| n.seq > self.base_seq) {
                    self.base_seq = n.seq;
                    self.error = (!n.ok).then(|| n.text.clone());
                }
            }
            (x.labels.clone(), x.limits.max_labels)
        };
        let me = cx.entity().downgrade();
        let header = div()
            .p(px(16.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Quản lý thẻ phân loại"))
            .child(div().text_size(px(12.0)).text_color(theme.secondary()).child(format!("{}/{}", labels.len(), max)))
            .child(div().flex_1())
            .child(button("label-add", "Thêm phân loại", ButtonStyle::Prominent, labels.len() as i64 >= max, &theme, {
                let me = me.clone();
                move |_, window, cx| {
                    let _ = me.update(cx, |this, cx| this.edit(None, window, cx));
                }
            }))
            .child(button("label-done", "Xong", ButtonStyle::Glass, false, &theme, |_, _, cx| engine(cx).set_conversation_prompt(None)));
        let body: AnyElement = if labels.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .px(px(40.0))
                .child(icon("tag", 30.0, theme.secondary()))
                .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Chưa có phân loại"))
                .child(div().text_center().text_size(px(13.0)).text_color(theme.secondary()).child("Tạo thẻ phân loại để ghi nhớ & nhận biết hội thoại dễ dàng hơn"))
                .into_any_element()
        } else {
            let ids: Vec<String> = labels.iter().map(|l| l.id.clone()).collect();
            let mut list = div().id("label-list").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(8.0)).flex().flex_col();
            for (i, l) in labels.iter().enumerate() {
                let (edit_l, del_l, ids) = (l.clone(), l.clone(), ids.clone());
                let (me_e, me_d) = (me.clone(), me.clone());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("label-row-{}", l.id)))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .hover(|s| s.bg(theme.hover()))
                        .on_drag(DraggedLabel(i, l.name.clone()), |d, _, _, cx| cx.new(|_| d.clone()))
                        .drag_over::<DraggedLabel>(move |s, _, _, cx| s.bg(Theme::get(cx).hover()))
                        .on_drop(move |d: &DraggedLabel, _, cx| {
                            let next = reordered_labels(&ids, d.0, i);
                            if next != ids {
                                engine(cx).reorder_conversation_labels(next);
                            }
                        })
                        .child(dot(&l.color_hex, 12.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(1.0))
                                .child(div().truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(l.name.clone()))
                                .child(div().text_size(px(11.0)).text_color(theme.secondary()).child(format!("{} hội thoại", l.conv_ids.len()))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("label-edit-{}", l.id)))
                                .p(px(5.0))
                                .rounded(px(6.0))
                                .hover(|s| s.bg(theme.hover()))
                                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Sửa phân loại").build(window, cx))
                                .child(icon("pencil", 13.0, theme.secondary()))
                                .on_click(move |_, window, cx| {
                                    let _ = me_e.update(cx, |this, cx| this.edit(Some(edit_l.clone()), window, cx));
                                }),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("label-del-{}", l.id)))
                                .p(px(5.0))
                                .rounded(px(6.0))
                                .hover(|s| s.bg(theme.hover()))
                                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Xóa phân loại").build(window, cx))
                                .child(icon("trash", 13.0, theme.red()))
                                .on_click(move |_, _, cx| {
                                    let _ = me_d.update(cx, |this, cx| {
                                        this.pending_delete = Some(del_l.clone());
                                        cx.notify();
                                    });
                                }),
                        ),
                );
            }
            list.into_any_element()
        };
        let delete_alert = self.pending_delete.clone().map(|l| {
            let me = me.clone();
            alert("Xóa thẻ phân loại?", &format!("Phân loại \"{}\" sẽ bị xóa khỏi mọi hội thoại.", l.name), "Xóa phân loại", "Không", true, &theme, move |ok, cx| {
                if ok {
                    engine(cx).delete_conversation_label(&l.id);
                }
                let _ = me.update(cx, |this, cx| {
                    this.pending_delete = None;
                    cx.notify();
                });
            })
        });
        let editor = self.editor.clone().map(|ed| {
            div().absolute().inset_0().bg(gpui::black().opacity(0.2)).flex().items_center().justify_center().child(occluder()).child(ed).into_any_element()
        });
        div()
            .relative()
            .w(px(460.0))
            .h(px(440.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
            .when_some(self.error.clone(), |el, e| el.child(div().px(px(16.0)).pb(px(10.0)).text_size(px(12.0)).text_color(theme.red()).child(e)))
            .children(editor)
            .children(delete_alert)
    }
}

pub struct LabelEditor {
    existing: Option<Label>,
    name: Entity<InputState>,
    color_hex: String,
    submitting: bool,
    error: Option<String>,
    base_seq: i64,
    on_close: std::rc::Rc<dyn Fn(&mut App)>,
}

impl LabelEditor {
    fn new(existing: Option<Label>, on_close: std::rc::Rc<dyn Fn(&mut App)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (max, palette0) = {
            let e = engine(cx);
            let s = e.state.borrow();
            (s.conversation_extras.limits.max_name_length, s.conversation_extras.limits.palette.first().cloned())
        };
        let initial = existing.as_ref().map(|l| l.name.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Nhập tên thẻ phân loại").default_value(initial));
        cx.subscribe_in(&name, window, move |this: &mut Self, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                let v = input.read(cx).value().to_string();
                let capped = cap_label_name(&v, max);
                if capped != v {
                    input.update(cx, |s, cx| s.set_value(capped, window, cx));
                }
                this.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit(cx),
            _ => {}
        })
        .detach();
        name.update(cx, |s, cx| s.focus(window, cx));
        let color_hex = existing.as_ref().map(|l| l.color_hex.clone()).or(palette0).unwrap_or_else(|| "#0068FF".into());
        LabelEditor { existing, name, color_hex, submitting: false, error: None, base_seq: notice_seq(cx), on_close }
    }

    fn trimmed(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_string()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.trimmed(cx);
        if name.is_empty() || self.submitting {
            return;
        }
        self.submitting = true;
        self.base_seq = notice_seq(cx);
        let e = engine(cx);
        match &self.existing {
            Some(l) => e.update_conversation_label(&l.id, &name, &self.color_hex),
            None => e.create_conversation_label(&name, &self.color_hex, vec![]),
        }
        cx.notify();
    }
}

impl Render for LabelEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let (max, palette, notice) = {
            let e = engine(cx);
            let s = e.state.borrow();
            let x = &s.conversation_extras;
            (x.limits.max_name_length, x.limits.palette.clone(), x.notice.clone())
        };
        // Closes on the success notice; a failure shows its text.
        if self.submitting {
            if let Some(n) = notice.filter(|n| n.seq > self.base_seq) {
                self.base_seq = n.seq;
                self.submitting = false;
                if n.ok {
                    let close = self.on_close.clone();
                    cx.defer(move |cx| close(cx));
                } else {
                    self.error = Some(n.text);
                }
            }
        }
        let count = self.name.read(cx).value().chars().count();
        let blank = self.trimmed(cx).is_empty();
        let me = cx.entity().downgrade();
        let mut swatches = div().flex().gap(px(10.0));
        for hex in palette {
            let selected = hex.eq_ignore_ascii_case(&self.color_hex);
            let (me, h) = (me.clone(), hex.clone());
            swatches = swatches.child(
                dot(&hex, 24.0)
                    .id(SharedString::from(format!("swatch-{hex}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |el| el.child(icon("checkmark", 11.0, gpui::white())))
                    .on_click(move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| {
                            this.color_hex = h.clone();
                            cx.notify();
                        });
                    }),
            );
        }
        let close = self.on_close.clone();
        div()
            .id("label-editor")
            .w(px(380.0))
            .p(px(20.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .on_key_down({
                let close = close.clone();
                move |ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        close(cx);
                    }
                }
            })
            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(if self.existing.is_none() { "Thêm mới thẻ phân loại" } else { "Sửa thẻ phân loại" }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_size(px(12.0)).text_color(theme.secondary()).child("Tên thẻ phân loại"))
                    .child(div().px(px(10.0)).py(px(6.0)).rounded(px(8.0)).bg(theme.glass()).border_1().border_color(theme.glass_border()).child(Input::new(&self.name).appearance(false).text_size(px(13.5))))
                    .child(div().text_size(px(11.0)).text_color(theme.tertiary()).child(format!("{count}/{max}"))),
            )
            .child(div().flex().flex_col().gap(px(8.0)).child(div().text_size(px(12.0)).text_color(theme.secondary()).child("Màu")).child(swatches))
            .when_some(self.error.clone(), |el, e| el.child(div().text_size(px(12.0)).text_color(theme.red()).child(e)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("label-editor-cancel", "Hủy", ButtonStyle::Glass, false, &theme, move |_, _, cx| close(cx)))
                    .child(button("label-editor-ok", if self.existing.is_none() { "Thêm" } else { "Lưu" }, ButtonStyle::Prominent, blank || self.submitting, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| this.submit(cx));
                    })),
            )
    }
}
