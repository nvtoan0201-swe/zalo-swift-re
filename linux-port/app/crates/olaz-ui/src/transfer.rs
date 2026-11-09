//! Phone → PC transfer overlay (port of TransferView.swift): the official offer, waiting for the phone, progress, done / failed.

use gpui::{AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px, relative};
use olaz_engine::transfer::{MessageTransferState, TransferPhase};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, button, spinner};

fn route(theme: &Theme) -> AnyElement {
    let device = |sym: &str| div().size(px(64.0)).rounded_full().bg(with_alpha(theme.accent, 0.14)).flex().items_center().justify_center().child(icon(sym, 28.0, theme.accent));
    div()
        .flex()
        .items_center()
        .gap(px(14.0))
        .child(device("iphone"))
        .child(icon("arrow.right", 18.0, theme.secondary()))
        .child(device("icons/laptop.svg"))
        .into_any_element()
}

fn title(t: &str) -> AnyElement {
    div().text_center().text_size(px(20.0)).font_weight(FontWeight::BOLD).child(t.to_string()).into_any_element()
}

fn body(t: &str, theme: &Theme) -> AnyElement {
    div().text_center().text_size(px(13.5)).text_color(theme.secondary()).child(t.to_string()).into_any_element()
}

fn secondary_actions(theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .flex()
        .gap(px(10.0))
        .pt(px(4.0))
        .child(button("tr-hide", "Ẩn", ButtonStyle::Glass, false, theme, |_, _, cx| engine(cx).dismiss_transfer()).flex_1())
        .child(button("tr-cancel", "Hủy đồng bộ", ButtonStyle::Glass, false, theme, |_, _, cx| engine(cx).respond_to_transfer(false)).flex_1().text_color(theme.red()))
        .into_any_element()
}

fn card(s: &MessageTransferState, theme: &Theme) -> AnyElement {
    let mut col = div().w(px(420.0)).p(px(30.0)).rounded(px(28.0)).bg(theme.content_bg()).border_1().border_color(theme.separator()).shadow_lg().flex().flex_col().items_center().gap(px(18.0));
    let full = |id: &'static str, label: &'static str, style: ButtonStyle, f: fn(&mut App)| button(id, label, style, false, theme, move |_, _, cx| f(cx)).w_full();
    col = match &s.phase {
        TransferPhase::Offer => col
            .child(route(theme))
            .child(title("Đồng bộ tin nhắn từ điện thoại?"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(body(s.official_description.as_deref().or(s.message.as_deref()).unwrap_or("Để đảm bảo tính bảo mật, Zalo chỉ hiển thị tin nhắn từ sau lần đăng nhập đầu tiên trên thiết bị này."), theme))
                    .child(body(s.official_suggestion.as_deref().unwrap_or("Đồng bộ từ điện thoại để xem đầy đủ nội dung các cuộc trò chuyện."), theme)),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(full("tr-accept", "Đồng bộ ngay", ButtonStyle::Prominent, |cx| engine(cx).respond_to_transfer(true)))
                    .child(full("tr-later", "Để sau", ButtonStyle::Glass, |cx| engine(cx).respond_to_transfer(false))),
            ),
        TransferPhase::WaitingForPhone => col
            .child(route(theme))
            .child(title("Mở Zalo trên điện thoại để xác nhận"))
            .child(body(s.waiting_detail(), theme))
            .child(div().px(px(14.0)).py(px(8.0)).rounded_full().bg(with_alpha(theme.label(), 0.07)).flex().items_center().gap(px(8.0)).child(spinner(11.0, theme.secondary())).child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child("Đang chờ xác nhận từ điện thoại…")))
            .child(secondary_actions(theme)),
        TransferPhase::Transferring => {
            let p = s.progress;
            let ring = div()
                .size(px(112.0))
                .rounded_full()
                .border_4()
                .border_color(with_alpha(theme.accent, 0.25))
                .flex()
                .items_center()
                .justify_center()
                .child(match p {
                    Some(p) => div().text_size(px(20.0)).font_weight(FontWeight::BOLD).child(format!("{}%", (p * 100.0).round() as i64)).into_any_element(),
                    None => spinner(20.0, theme.accent).into_any_element(),
                });
            col.child(ring)
                .child(title("Đang đồng bộ tin nhắn"))
                .when_some(p, |el, p| el.child(div().w_full().h(px(6.0)).rounded_full().bg(theme.quaternary()).child(div().h_full().rounded_full().bg(theme.accent).w(relative(p as f32)))))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(s.transfer_detail()))
                        .when_some(s.stage_index.zip(s.stage_count), |el, (i, n)| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(format!("Bước {i}/{n} · {}", s.stage_title()))))
                        .child(div().pt(px(2.0)).text_size(px(12.0)).text_color(theme.tertiary()).child("Giữ Zalo mở trên điện thoại cho đến khi hoàn tất.")),
                )
                .child(secondary_actions(theme))
        }
        TransferPhase::Finished => col
            .child(div().size(px(96.0)).rounded_full().bg(with_alpha(theme.accent, 0.14)).flex().items_center().justify_center().child(icon("checkmark.circle.fill", 58.0, theme.accent)))
            .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).child("Hoàn tất"))
            .child(body("Tin nhắn từ điện thoại đã được đồng bộ sang máy tính.", theme))
            .child(full("tr-done", "Xong", ButtonStyle::Prominent, |cx| engine(cx).dismiss_transfer())),
        TransferPhase::Failed(reason) => col
            .child(div().size(px(88.0)).rounded_full().bg(with_alpha(theme.orange(), 0.15)).flex().items_center().justify_center().child(icon("exclamationmark.triangle.fill", 34.0, theme.orange())))
            .child(title("Đồng bộ không thành công"))
            .child(body(s.message.as_deref().unwrap_or(reason), theme))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .when(s.can_retry, |el| el.child(full("tr-retry", "Thử lại", ButtonStyle::Prominent, |cx| engine(cx).respond_to_transfer(true))))
                    .child(full("tr-close", "Đóng", ButtonStyle::Glass, |cx| engine(cx).dismiss_transfer())),
            ),
    };
    col.into_any_element()
}

/// The modal over the whole window while a transfer is shown.
pub fn overlay(theme: &Theme, cx: &mut App) -> Option<AnyElement> {
    let s = engine(cx).state.borrow().transfer.clone()?;
    let offer = s.phase == TransferPhase::Offer;
    Some(
        div()
            .id("transfer-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.32))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(move |ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    if offer { engine(cx).respond_to_transfer(false) } else { engine(cx).dismiss_transfer() }
                }
            })
            .child(div().p(px(24.0)).child(card(&s, theme)))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn compiles() {}
}
