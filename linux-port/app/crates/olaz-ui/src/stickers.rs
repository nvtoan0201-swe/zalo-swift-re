//! Sticker picker of the composer and the typing-suggestion strip (StickerPicker.swift). Every list comes from the official
//! sticker services; a sticker is sent only by a click.

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString,
    RenderImage, StatefulInteractiveElement, Styled, StyledImage, Task, Window, div, img, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::models::CardActionOutcome;
use olaz_engine::stickers::{StickerAnimation, StickerEntry, StickerSource, StickerTab};

use crate::app::engine;
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ImageState, remote_image, remote_picture, spinner};

/// Cuts a sprite strip (`frames` equal square pictures side by side, the official `stickerSpriteUrl`) into pictures.
/// `None` when the sheet is not such a strip — nothing animated is better than slices of something else.
pub fn slice_sprite(sheet: &image::RgbaImage, frames: usize) -> Option<Vec<image::RgbaImage>> {
    if frames < 2 {
        return None;
    }
    let width = sheet.width() / frames as u32;
    if width == 0 || (width as i64 - sheet.height() as i64).abs() > 2 {
        return None;
    }
    Some((0..frames as u32).map(|i| image::imageops::crop_imm(sheet, i * width, 0, width, sheet.height()).to_image()).collect())
}

enum Sprite {
    Loading,
    Ready(Vec<std::sync::Arc<RenderImage>>),
    Failed,
}

thread_local! {
    static SPRITES: std::cell::RefCell<std::collections::HashMap<String, Sprite>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The pictures of a sprite sheet, loading it (once) when unknown; windows refresh when it lands.
fn sprite_frames(url: &str, frames: usize, cx: &mut App) -> Option<Vec<std::sync::Arc<RenderImage>>> {
    let known = SPRITES.with(|m| match m.borrow().get(url) {
        Some(Sprite::Ready(p)) => Some(Some(p.clone())),
        Some(_) => Some(None),
        None => None,
    });
    if let Some(k) = known {
        return k;
    }
    if !cx.has_global::<crate::widgets::Images>() {
        return None;
    }
    SPRITES.with(|m| m.borrow_mut().insert(url.to_string(), Sprite::Loading));
    let fut = cx.global::<crate::widgets::Images>().loader.data(url);
    let bg = cx.background_executor().clone();
    let key = url.to_string();
    cx.spawn(async move |cx| {
        let pictures = match fut.await {
            Ok(bytes) => bg
                .spawn(async move {
                    // Decoded at about `frames` × 128 px wide (cells show at 64 pt, 2×), not at the sheet's full size.
                    let sheet = olaz_media::decode::decode_rgba(&bytes, (frames as u32 * 128).max(256))?;
                    let slices = slice_sprite(&sheet, frames)?;
                    Some(
                        slices
                            .into_iter()
                            .map(|mut p| {
                                for px in p.pixels_mut() {
                                    px.0.swap(0, 2); // RGBA → BGRA
                                }
                                std::sync::Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(p)]))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .await,
            Err(_) => None,
        };
        let _ = cx.update(|cx| {
            SPRITES.with(|m| m.borrow_mut().insert(key, pictures.map(Sprite::Ready).unwrap_or(Sprite::Failed)));
            cx.refresh_windows();
        });
    })
    .detach();
    None
}

pub const NOT_READY: &str = "Cuộc trò chuyện chưa sẵn sàng, hãy thử lại sau giây lát.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    Loading,
    Ready,
    Failed,
}

pub struct StickerPicker {
    conversation: String,
    tabs: Vec<StickerTab>,
    tabs_state: Load,
    selected: Option<i64>,
    stickers: Vec<StickerEntry>,
    grid_state: Load,
    search: Entity<InputState>,
    query: String,
    found: Option<Vec<StickerEntry>>,
    search_failed: bool,
    sending: Option<String>,
    error: Option<String>,
    _grid: Option<Task<()>>,
    _search: Option<Task<()>>,
    /// The cell under the pointer: its animated source plays (`animatesOnHover`).
    hovered: Option<String>,
    /// Repaints at the sprite's frame rate while a sprite cell is hovered.
    _anim: Option<Task<()>>,
}

pub struct StickerSent;
impl gpui::EventEmitter<StickerSent> for StickerPicker {}

impl StickerPicker {
    pub fn new(conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Tìm kiếm sticker"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().to_string();
                this.run_search(cx);
            }
        })
        .detach();
        let mut me = StickerPicker {
            conversation,
            tabs: vec![],
            tabs_state: Load::Loading,
            selected: None,
            stickers: vec![],
            grid_state: Load::Loading,
            search,
            query: String::new(),
            found: None,
            search_failed: false,
            sending: None,
            error: None,
            _grid: None,
            _search: None,
            hovered: None,
            _anim: None,
        };
        me.load_tabs(cx);
        me
    }

    fn load_tabs(&mut self, cx: &mut Context<Self>) {
        let e = engine(cx);
        cx.spawn(async move |this, cx| {
            for attempt in 0..3 {
                if let Some(list) = e.sticker_tabs().await {
                    let _ = this.update(cx, |this, cx| {
                        let first = list.first().map(|t| t.id);
                        if this.selected.is_none_or(|s| !list.iter().any(|t| t.id == s)) {
                            this.selected = first;
                        }
                        this.tabs = list;
                        this.tabs_state = Load::Ready;
                        this.load_grid(cx);
                        cx.notify();
                    });
                    return;
                }
                if attempt < 2 {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.tabs_state = Load::Failed;
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.selected != Some(id) {
            self.selected = Some(id);
            self.load_grid(cx);
        }
    }

    fn load_grid(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else { return };
        self.grid_state = Load::Loading;
        self.stickers.clear();
        let e = engine(cx);
        self._grid = Some(cx.spawn(async move |this, cx| {
            for attempt in 0..2 {
                if let Some(list) = e.stickers_in_tab(id).await {
                    let _ = this.update(cx, |this, cx| {
                        if this.selected == Some(id) {
                            this.stickers = list;
                            this.grid_state = Load::Ready;
                            cx.notify();
                        }
                    });
                    return;
                }
                if attempt < 1 {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.grid_state = Load::Failed;
                cx.notify();
            });
        }));
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let text = self.query.trim().to_string();
        self.found = None;
        self.search_failed = false;
        cx.notify();
        if text.is_empty() {
            self._search = None;
            return;
        }
        let e = engine(cx);
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let result = e.search_stickers(&text).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Some(r) => this.found = Some(r),
                    None => this.search_failed = true,
                }
                cx.notify();
            });
        }));
    }

    fn send(&mut self, sticker: StickerEntry, source: StickerSource, cx: &mut Context<Self>) {
        if self.sending.is_some() {
            return;
        }
        let e = engine(cx);
        if e.state.borrow().open_conversation_id.as_deref() != Some(self.conversation.as_str()) {
            self.error = Some(NOT_READY.into());
            cx.notify();
            return;
        }
        self.sending = Some(sticker.id());
        self.error = None;
        cx.notify();
        let conv = self.conversation.clone();
        cx.spawn(async move |this, cx| {
            let outcome = e.send_sticker(&sticker, &conv, source, None, -1).await;
            let _ = this.update(cx, |this, cx| {
                this.sending = None;
                match outcome {
                    CardActionOutcome::Done => cx.emit(StickerSent),
                    CardActionOutcome::Locked(t) | CardActionOutcome::Failed(t) | CardActionOutcome::Unsupported(t) | CardActionOutcome::Pending(t) => {
                        this.error = Some(t)
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The still picture; while hovered, the animated source on top once loaded (sprite sheet cut into pictures, or
    /// the GIF/WebP that GPUI plays itself).
    fn thumb(&self, s: &StickerEntry, cx: &mut Context<Self>) -> AnyElement {
        let still = remote_picture(Some(&s.url), 64.0, 64.0, 0.0, ObjectFit::Contain, cx);
        let hovered = self.hovered.as_deref() == Some(s.id().as_str());
        let animated: Option<AnyElement> = match (&s.animation, hovered) {
            (Some(StickerAnimation::Sprite { url, frames, frame_ms }), true) => sprite_frames(url, *frames as usize, cx).map(|pictures| {
                // Real elapsed time (not the display clock, which visual tests freeze).
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                let tick = (now / (*frame_ms).max(1)) as usize;
                img(pictures[tick % pictures.len()].clone()).size(px(64.0)).object_fit(ObjectFit::Contain).into_any_element()
            }),
            (Some(StickerAnimation::Animated { url }), true) => match remote_image(url, cx) {
                ImageState::Ready(source) => Some(img(source).size(px(64.0)).object_fit(ObjectFit::Contain).into_any_element()),
                _ => None,
            },
            _ => None,
        };
        match animated {
            Some(a) => div().size(px(64.0)).child(a).into_any_element(),
            None => still,
        }
    }

    fn grid(&self, items: &[StickerEntry], source: StickerSource, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut grid = div().p(px(10.0)).flex().flex_wrap().gap(px(6.0));
        for s in items {
            let busy = self.sending.as_deref() == Some(s.id().as_str());
            let sticker = s.clone();
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("st-{}", s.id())))
                    .size(px(72.0))
                    .rounded(px(10.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|st| st.bg(theme.hover()))
                    .when(busy, |el| el.opacity(0.4))
                    .child(self.thumb(s, cx))
                    .on_hover(cx.listener({
                        let id = s.id();
                        let frame_ms = match &s.animation {
                            Some(StickerAnimation::Sprite { frame_ms, .. }) => Some((*frame_ms).max(16) as u64),
                            _ => None,
                        };
                        move |this, hovering: &bool, _, cx| {
                            if *hovering {
                                this.hovered = Some(id.clone());
                                this._anim = frame_ms.map(|ms| {
                                    let id = id.clone();
                                    cx.spawn(async move |this, cx| loop {
                                        cx.background_executor().timer(Duration::from_millis(ms)).await;
                                        let still = this.update(cx, |this, cx| {
                                            cx.notify();
                                            this.hovered.as_deref() == Some(id.as_str())
                                        });
                                        if !matches!(still, Ok(true)) {
                                            break;
                                        }
                                    })
                                });
                            } else if this.hovered.as_deref() == Some(id.as_str()) {
                                this.hovered = None;
                                this._anim = None;
                            }
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.send(sticker.clone(), source, cx))),
            );
        }
        div().id(SharedString::from(format!("grid-{:?}", source as u8))).flex_1().min_h(px(0.0)).overflow_y_scroll().child(grid).into_any_element()
    }
}

fn message(text: &str, theme: &Theme) -> AnyElement {
    div().flex_1().p(px(16.0)).flex().items_center().justify_center().text_center().text_size(px(12.5)).text_color(theme.secondary()).child(text.to_string()).into_any_element()
}

fn loading(theme: &Theme) -> AnyElement {
    div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element()
}

impl Render for StickerPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let searching = !self.query.trim().is_empty();
        let content = if searching {
            match (&self.found, self.search_failed) {
                (Some(f), _) if f.is_empty() => message("Không có Sticker phù hợp. Vui lòng thử từ khóa khác.", &theme),
                (Some(f), _) => {
                    let f = f.clone();
                    self.grid(&f, StickerSource::Search, &theme, cx)
                }
                (None, true) => message("Không tìm được sticker. Hãy thử lại sau.", &theme),
                _ => loading(&theme),
            }
        } else {
            match self.tabs_state {
                Load::Loading => loading(&theme),
                Load::Failed => message("Không tải được danh sách sticker. Hãy thử lại sau.", &theme),
                Load::Ready if self.tabs.is_empty() => message("Bạn chưa có bộ sticker nào.", &theme),
                Load::Ready => match self.grid_state {
                    Load::Loading => loading(&theme),
                    Load::Failed => message("Không tải được bộ sticker này. Hãy thử lại sau.", &theme),
                    Load::Ready if self.stickers.is_empty() => message("Bộ sticker này chưa có sticker nào.", &theme),
                    Load::Ready => {
                        let items = self.stickers.clone();
                        let source = if self.selected == Some(StickerTab::RECENT_ID) { StickerSource::Recent } else { StickerSource::Category };
                        self.grid(&items, source, &theme, cx)
                    }
                },
            }
        };
        let strip = (!searching && self.tabs_state == Load::Ready && !self.tabs.is_empty()).then(|| {
            let mut row = div().flex().gap(px(4.0)).px(px(8.0)).py(px(6.0));
            for tab in &self.tabs {
                let selected = self.selected == Some(tab.id);
                let id = tab.id;
                row = row.child(
                    div()
                        .id(SharedString::from(format!("tab-{id}")))
                        .flex_none()
                        .size(px(34.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |el| el.bg(with_alpha(theme.accent, 0.16)))
                        .hover(|s| s.bg(theme.hover()))
                        .child(if tab.is_recent() {
                            icon("clock", 16.0, theme.secondary()).into_any_element()
                        } else {
                            remote_picture(tab.icon.as_deref(), 26.0, 26.0, 0.0, ObjectFit::Contain, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx))),
                );
            }
            div().id("sticker-tabs").border_t_1().border_color(theme.separator()).overflow_x_scroll().child(row)
        });
        div()
            .id("sticker-picker")
            .relative()
            .child(crate::widgets::occluder())
            .w(px(360.0))
            .h(px(420.0))
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
                    .px(px(12.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .border_b_1()
                    .border_color(theme.separator())
                    .child(icon("magnifyingglass", 12.5, theme.secondary()))
                    .child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(13.0)))),
            )
            .child(content)
            .when_some(self.error.clone(), |el, e| el.child(div().px(px(12.0)).py(px(6.0)).text_center().text_size(px(12.0)).text_color(theme.orange()).child(e)))
            .children(strip)
    }
}

/// The suggestion strip above the composer while typing.
pub fn typing_strip(
    stickers: &[StickerEntry],
    sending: Option<&str>,
    pick: impl Fn(&StickerEntry, usize, &mut App) + Clone + 'static,
    dismiss: impl Fn(&mut App) + 'static,
    theme: &Theme,
    cx: &mut App,
) -> AnyElement {
    let mut row = div().flex().gap(px(6.0)).px(px(8.0)).py(px(6.0));
    for (i, s) in stickers.iter().enumerate() {
        let (pick, sticker) = (pick.clone(), s.clone());
        row = row.child(
            div()
                .id(SharedString::from(format!("sug-{}", s.id())))
                .flex_none()
                .size(px(56.0))
                .rounded(px(10.0))
                .hover(|st| st.bg(theme.hover()))
                .when(sending == Some(s.id().as_str()), |el| el.opacity(0.4))
                .child(remote_picture(Some(&s.url), 56.0, 56.0, 0.0, ObjectFit::Contain, cx))
                .on_click(move |_, _, cx| pick(&sticker, i, cx)),
        );
    }
    div()
        .h(px(70.0))
        .flex()
        .items_center()
        .rounded(px(16.0))
        .bg(theme.glass())
        .border_1()
        .border_color(theme.glass_border())
        .child(div().id("sticker-suggestions").flex_1().overflow_x_scroll().child(row))
        .child(
            div()
                .id("sticker-suggest-hide")
                .px(px(10.0))
                .child(icon("xmark", 11.0, theme.secondary()))
                .on_click(move |_, _, cx| dismiss(cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    #[test]
    fn sprite_slicing() {
        let mut sheet = RgbaImage::new(300, 100);
        for x in 0..300 {
            for y in 0..100 {
                sheet.put_pixel(x, y, Rgba([(x / 100) as u8 * 80, 0, 0, 255]));
            }
        }
        let p = super::slice_sprite(&sheet, 3).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[2].dimensions(), (100, 100));
        assert_eq!(p[1].get_pixel(50, 50).0[0], 80);
        assert_eq!(p[2].get_pixel(0, 0).0[0], 160);
        // Not a strip of square pictures, or a single frame: no animation.
        assert!(super::slice_sprite(&sheet, 2).is_none());
        assert!(super::slice_sprite(&sheet, 1).is_none());
    }
}
