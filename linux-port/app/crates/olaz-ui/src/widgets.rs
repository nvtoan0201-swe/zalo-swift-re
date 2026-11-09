//! Shared components: remote images & avatars, spinner, buttons, badges, empty states.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, ElementId, FontWeight, Global, Hsla, Image, ImageFormat, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Transformation, Window, div,
    img, percentage, px,
};
use olaz_media::MediaLoader;

use crate::icon;
use crate::theme::{FONT, ROUNDED, Theme, with_alpha};

// MARK: remote images

#[derive(Clone)]
pub enum ImageState {
    Loading,
    Ready(gpui::ImageSource),
    Failed,
}

pub struct Images {
    pub loader: Arc<MediaLoader>,
    states: HashMap<String, ImageState>,
    /// Official resolutions per "conv|msg|kind": None = in flight.
    resolved: HashMap<String, Option<olaz_engine::media::MediaResolution>>,
}

impl Global for Images {}

impl Images {
    pub fn new(loader: Arc<MediaLoader>) -> Self {
        Self { loader, states: HashMap::new(), resolved: HashMap::new() }
    }
}

pub fn detect_format(b: &[u8]) -> Option<ImageFormat> {
    if b.starts_with(b"\x89PNG") {
        Some(ImageFormat::Png)
    } else if b.starts_with(&[0xFF, 0xD8]) {
        Some(ImageFormat::Jpeg)
    } else if b.starts_with(b"GIF8") {
        Some(ImageFormat::Gif)
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else if b.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if b.starts_with(b"<svg") || b.starts_with(b"<?xml") {
        Some(ImageFormat::Svg)
    } else {
        None
    }
}

pub fn image_from_bytes(bytes: Vec<u8>) -> Option<Arc<Image>> {
    let format = detect_format(&bytes)?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// The loaded image for `url`, starting a load (once) when unknown. Windows refresh when it lands.
pub fn remote_image(url: &str, cx: &mut App) -> ImageState {
    if !cx.has_global::<Images>() {
        return ImageState::Failed;
    }
    if let Some(s) = cx.global::<Images>().states.get(url) {
        return s.clone();
    }
    let loader = cx.global::<Images>().loader.clone();
    cx.global_mut::<Images>().states.insert(url.to_string(), ImageState::Loading);
    let fut = loader.data(url);
    let key = url.to_string();
    let bg = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let result = match fut.await {
            Ok(bytes) => {
                let decoded = bg.spawn(async move { (crate::decode::decode(&bytes, crate::decode::MAX_PIXELS), bytes) }).await;
                Ok(decoded)
            }
            Err(e) => Err(e),
        };
        let _ = cx.update(|cx| {
            let state = match result {
                Ok((decoded, bytes)) => match decoded {
                    Some(img) => ImageState::Ready(img.source()),
                    None => {
                        let head: Vec<u8> = bytes.iter().take(12).copied().collect();
                        olaz_core::log::write(format!("media fail kind=image class=undecodable head={head:02x?} len={}", bytes.len()));
                        ImageState::Failed
                    }
                },
                Err(_) => ImageState::Failed,
            };
            cx.global_mut::<Images>().states.insert(key, state);
            cx.refresh_windows();
        });
    })
    .detach();
    ImageState::Loading
}

/// Forget a failed image so the next render retries (retry buttons).
pub fn retry_image(url: &str, cx: &mut App) {
    if cx.has_global::<Images>() {
        let loader = cx.global::<Images>().loader.clone();
        loader.forget_failure(url);
        cx.global_mut::<Images>().states.remove(url);
    }
}

/// `nameInitials`: ≥2 words → first + last initials; 1 word → first two letters; uppercased.
pub fn name_initials(name: &str) -> String {
    let words: Vec<&str> = name.split(' ').filter(|w| w.chars().next().is_some_and(|c| c.is_alphabetic())).collect();
    let s: String = match words.len() {
        0 => name.chars().take(1).collect(),
        1 => words[0].chars().take(2).collect(),
        _ => {
            let mut s = String::new();
            s.extend(words[0].chars().take(1));
            s.extend(words[words.len() - 1].chars().take(1));
            s
        }
    };
    s.to_uppercase()
}

/// `RemoteAvatar(url, name, size)`: circle image, initials on a tint@0.18 circle as placeholder.
pub fn avatar(url: Option<&str>, name: &str, size: f32, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let state = url.map(|u| remote_image(u, cx));
    let base = div().size(px(size)).flex_none().rounded_full().overflow_hidden();
    match state {
        Some(ImageState::Ready(image)) => base.child(img(image).size(px(size)).rounded_full().object_fit(ObjectFit::Cover)).into_any_element(),
        _ => base
            .bg(with_alpha(theme.accent, 0.18))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .font_family(ROUNDED)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(size * 0.36))
                    .text_color(theme.accent)
                    .child(name_initials(name)),
            )
            .into_any_element(),
    }
}

/// A photo/sticker from a URL filling `w×h` (placeholder: quaternary + spinner; failure: icon).
pub fn remote_picture(url: Option<&str>, w: f32, h: f32, radius: f32, fit: ObjectFit, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let state = url.map(|u| remote_image(u, cx)).unwrap_or(ImageState::Failed);
    let base = div().w(px(w)).h(px(h)).flex_none().rounded(px(radius)).overflow_hidden();
    match state {
        ImageState::Ready(image) => base.child(img(image).w(px(w)).h(px(h)).rounded(px(radius)).object_fit(fit)).into_any_element(),
        ImageState::Loading => base.bg(theme.quaternary()).flex().items_center().justify_center().child(spinner(14.0, theme.secondary())).into_any_element(),
        ImageState::Failed => {
            let retry_url = url.map(str::to_string);
            base.bg(theme.quaternary())
                .flex()
                .flex_col()
                .gap(px(4.0))
                .items_center()
                .justify_center()
                .child(icon::icon("photo.badge.exclamationmark", 18.0, theme.secondary()))
                .when_some(retry_url, |el, u| {
                    el.child(
                        div()
                            .id(SharedString::from(format!("retry-{u}")))
                            .text_size(px(12.0))
                            .text_color(theme.secondary())
                            .cursor_pointer()
                            .child("Thử lại")
                            .on_click(move |_, _, cx| {
                                retry_image(&u, cx);
                                cx.refresh_windows();
                            }),
                    )
                })
                .into_any_element()
        }
    }
}

use gpui::prelude::FluentBuilder;

/// A message photo / video thumbnail: the CDN URL first; when it is missing or fails, the official loader
/// (`resolveMedia`) provides a local copy — or says it expired.
pub fn message_picture(conv: &str, msg: &str, url: Option<&str>, kind: olaz_engine::media::MediaKind, w: f32, h: f32, radius: f32, cx: &mut App) -> AnyElement {
    use olaz_engine::media::MediaResolution;
    let direct = url.map(|u| remote_image(u, cx));
    if matches!(direct, Some(ImageState::Ready(_)) | Some(ImageState::Loading)) {
        return remote_picture(url, w, h, radius, ObjectFit::Cover, cx);
    }
    let key = format!("{conv}|{msg}|{}", kind.raw());
    let known = cx.global::<Images>().resolved.get(&key).cloned();
    let theme = Theme::get(cx).clone();
    match known {
        Some(Some(MediaResolution::File(path))) => {
            let file_url = format!("file://{path}");
            remote_picture(Some(&file_url), w, h, radius, ObjectFit::Cover, cx)
        }
        Some(Some(MediaResolution::Expired)) => failure_box(w, h, radius, "clock.badge.exclamationmark", "Ảnh đã hết hạn", Some("Không còn trên máy chủ Zalo"), None, &theme),
        Some(Some(_)) => {
            let retry_key = key.clone();
            let retry_url = url.map(str::to_string);
            failure_box(w, h, radius, "photo.badge.exclamationmark", "Không tải được ảnh", None, Some(Box::new(move |cx: &mut App| {
                cx.global_mut::<Images>().resolved.remove(&retry_key);
                if let Some(u) = &retry_url {
                    retry_image(u, cx);
                }
                cx.refresh_windows();
            })), &theme)
        }
        Some(None) => div().w(px(w)).h(px(h)).rounded(px(radius)).bg(theme.quaternary()).flex().items_center().justify_center().child(spinner(14.0, theme.secondary())).into_any_element(),
        None => {
            cx.global_mut::<Images>().resolved.insert(key.clone(), None);
            let engine = crate::app::engine(cx);
            let (conv, msg) = (conv.to_string(), msg.to_string());
            cx.spawn(async move |cx| {
                let r = engine.resolve_media(&conv, &msg, kind, false).await;
                let _ = cx.update(|cx| {
                    cx.global_mut::<Images>().resolved.insert(key, Some(r));
                    cx.refresh_windows();
                });
            })
            .detach();
            div().w(px(w)).h(px(h)).rounded(px(radius)).bg(theme.quaternary()).flex().items_center().justify_center().child(spinner(14.0, theme.secondary())).into_any_element()
        }
    }
}

fn failure_box(
    w: f32,
    h: f32,
    radius: f32,
    symbol: &str,
    title: &str,
    detail: Option<&str>,
    retry: Option<Box<dyn Fn(&mut App)>>,
    theme: &Theme,
) -> AnyElement {
    let compact = h < 100.0;
    div()
        .w(px(w))
        .h(px(h))
        .rounded(px(radius))
        .bg(theme.quaternary())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(if compact { 4.0 } else { 7.0 }))
        .px(px(8.0))
        .child(icon::icon(symbol, if compact { 18.0 } else { 24.0 }, theme.secondary()))
        .when(!compact, |el| el.child(div().text_center().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(title.to_string())))
        .when_some(detail.filter(|_| !compact), |el, d| el.child(div().text_size(px(11.0)).text_color(theme.tertiary()).child(d.to_string())))
        .when_some(retry, |el, retry| {
            el.child(
                div()
                    .id("media-retry")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(px(12.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.label())
                    .cursor_pointer()
                    .child(icon::icon("arrow.clockwise", 12.0, theme.label()))
                    .child("Thử lại")
                    .on_click(move |_, _, cx| retry(cx)),
            )
        })
        .into_any_element()
}

// MARK: spinner

/// A rotating arc (ProgressView). `size` = diameter.
pub fn spinner(size: f32, color: Hsla) -> impl IntoElement {
    let id: ElementId = ElementId::Name(SharedString::from(format!("spinner-{}", size as u32)));
    div().size(px(size)).flex_none().child(
        gpui::svg()
            .path("icons/spinner.svg")
            .size(px(size))
            .text_color(color)
            .with_animation(id, Animation::new(Duration::from_millis(900)).repeat(), |s, t| {
                s.with_transformation(Transformation::rotate(percentage(t)))
            }),
    )
}

// MARK: buttons

pub enum ButtonStyle {
    /// Accent fill, white label.
    Prominent,
    /// Glass capsule, primary label.
    Glass,
    /// Plain accent text.
    Link,
    Destructive,
}

/// A layer that keeps the mouse from reaching what lies under a floating panel. Put it first so the panel's own content
/// (painted after it) still gets events: `occlude()` on the panel itself also swallows events meant for its scroll areas.
pub fn occluder() -> gpui::Div {
    div().absolute().inset_0().occlude()
}

/// A regular-size button; a disabled one is dimmed and ignores clicks.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    disabled: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let b = button_sized(id, label, style, false, theme, move |ev, window, cx| {
        if !disabled {
            on_click(ev, window, cx)
        }
    });
    if disabled { b.opacity(0.45).cursor_default() } else { b }
}

pub fn button_sized(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    large: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let (h, px_h, size) = if large { (36.0, 18.0, 14.0) } else { (28.0, 12.0, 13.0) };
    let label: SharedString = label.into();
    let base = div()
        .id(id)
        .relative()
        .children(crate::layout_dump::probe("AXButton", label.to_string()))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .text_size(px(size))
        .font_family(FONT)
        .cursor_pointer()
        .child(label)
        .on_click(on_click);
    match style {
        ButtonStyle::Prominent => base
            .h(px(h))
            .px(px(px_h))
            .rounded_full()
            .bg(theme.accent)
            .text_color(theme.white())
            .font_weight(FontWeight::MEDIUM)
            .hover(|s| s.opacity(0.92))
            .active(|s| s.opacity(0.8)),
        ButtonStyle::Glass => base
            .h(px(h))
            .px(px(px_h))
            .rounded_full()
            .bg(theme.glass())
            .border_1()
            .border_color(theme.glass_border())
            .text_color(theme.label())
            .font_weight(FontWeight::MEDIUM)
            .hover(|s| s.opacity(0.85)),
        ButtonStyle::Link => base.text_color(theme.accent).hover(|s| s.opacity(0.75)),
        ButtonStyle::Destructive => base.text_color(theme.red()).hover(|s| s.opacity(0.75)),
    }
}

/// A round glass icon button (size × size).
pub fn icon_button(
    id: impl Into<ElementId>,
    symbol: &str,
    glyph: f32,
    size: f32,
    color: Hsla,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(size))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(theme.hover()))
        .child(icon::icon(symbol, glyph, color))
        .on_click(on_click)
}

/// Unread badge: "99+" above 99; tint capsule (gray when muted).
pub fn badge(count: i64, color: Hsla, text: Hsla) -> impl IntoElement {
    let label = if count > 99 { "99+".to_string() } else { count.to_string() };
    div()
        .min_w(px(18.0))
        .h(px(18.0))
        .px(px(5.5))
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.0))
        .font_weight(FontWeight::BOLD)
        .text_color(text)
        .child(label)
}

/// `ContentUnavailableView(title, symbol, description)`.
pub fn content_unavailable(symbol: &str, title: &str, description: &str, theme: &Theme) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .px(px(24.0))
        .child(icon::icon(symbol, 34.0, theme.tertiary()))
        .child(static_text(div().text_size(px(17.0)).font_weight(FontWeight::BOLD).text_color(theme.label()), title))
        .when(!description.is_empty(), |el| el.child(static_text(div().max_w(px(360.0)).text_center().text_size(px(13.0)).text_color(theme.secondary()), description)))
}

/// A text element that also registers itself as `AXStaticText` in the layout dump.
pub fn static_text(el: gpui::Div, text: &str) -> gpui::Div {
    el.relative().child(text.to_string()).children(crate::layout_dump::probe("AXStaticText", text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials() {
        assert_eq!(name_initials("Nguyễn Văn An"), "NA");
        assert_eq!(name_initials("đặng"), "ĐẶ");
        assert_eq!(name_initials("123"), "1");
        assert_eq!(name_initials(""), "");
        assert_eq!(name_initials("Lan 2"), "LA");
    }

    #[test]
    fn formats() {
        assert_eq!(detect_format(b"\x89PNG...."), Some(ImageFormat::Png));
        assert_eq!(detect_format(&[0xFF, 0xD8, 0xFF]), Some(ImageFormat::Jpeg));
        assert_eq!(detect_format(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageFormat::Webp));
        assert_eq!(detect_format(b"nope"), None);
    }
}

/// The official confirm (ConfirmManagerV2) as a native alert: title, message, Cancel / OK.
pub fn confirm_alert(c: &olaz_engine::composer::ComposerConfirm, theme: &Theme) -> AnyElement {
    let c2 = c.clone();
    alert(&c.title, &c.message, &c.ok, &c.cancel, false, theme, move |ok, cx| crate::app::engine(cx).answer_composer_confirm(&c2, ok))
}

pub fn message_confirm_alert(c: &olaz_engine::message_actions::MessageActionConfirm, theme: &Theme) -> AnyElement {
    let id = c.id;
    alert(&c.title, &c.message, &c.ok, &c.cancel, c.destructive, theme, move |ok, cx| crate::app::engine(cx).answer_message_confirm(id, ok))
}

/// A centered two-button alert over a dimmed backdrop.
pub fn alert(
    title: &str,
    message: &str,
    ok: &str,
    cancel: &str,
    destructive: bool,
    theme: &Theme,
    on_answer: impl Fn(bool, &mut App) + 'static,
) -> AnyElement {
    let on_answer = std::rc::Rc::new(on_answer);
    let on_cancel = on_answer.clone();
    div()
        .id("alert-backdrop")
        .absolute()
        .inset_0()
        .bg(gpui::black().opacity(0.2))
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .w(px(300.0))
                .p(px(18.0))
                .rounded(px(14.0))
                .bg(theme.content_bg())
                .border_1()
                .border_color(theme.separator())
                .shadow_lg()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(div().text_center().text_size(px(13.5)).font_weight(FontWeight::BOLD).child(title.to_string()))
                .when(!message.is_empty(), |el| el.child(div().text_center().text_size(px(12.0)).text_color(theme.secondary()).child(message.to_string())))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .gap(px(8.0))
                        .pt(px(6.0))
                        .child(button("confirm-cancel", cancel.to_string(), ButtonStyle::Glass, false, theme, move |_, _, cx| on_cancel(false, cx)).flex_1())
                        .child(
                            button("confirm-ok", ok.to_string(), if destructive { ButtonStyle::Destructive } else { ButtonStyle::Prominent }, false, theme, move |_, _, cx| {
                                on_answer(true, cx)
                            })
                            .flex_1(),
                        ),
                ),
        )
        .into_any_element()
}
