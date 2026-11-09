//! Chat wallpaper (ChatStyle.swift `ChatWallpaperBackground`): the image chosen in Settings behind the
//! whole chat pane; one made for the other appearance (night in light mode, a light one in dark mode) is faded.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AnyElement, App, Image, ImageFormat, IntoElement, ObjectFit, ParentElement, Styled, StyledImage, div, img, px};

use crate::settings::AppPrefs;
use crate::theme::{Theme, with_alpha};

/// `olaz.wallpaper` values and their Settings titles, in the grid order.
pub const OPTIONS: [(&str, &str); 5] = [("none", "Không"), ("mist", "Sương"), ("silk", "Lụa"), ("lotus", "Sen"), ("night", "Đêm")];

thread_local! {
    static IMAGES: RefCell<HashMap<String, Option<Arc<Image>>>> = RefCell::new(HashMap::new());
}

/// The embedded `wallpaper-<id>.jpg`, decoded once.
pub fn image(id: &str) -> Option<Arc<Image>> {
    if id == "none" || !OPTIONS.iter().any(|(o, _)| *o == id) {
        return None;
    }
    IMAGES.with(|m| {
        m.borrow_mut()
            .entry(id.to_string())
            .or_insert_with(|| crate::assets::bytes(&format!("wallpaper-{id}.jpg")).map(|b| Arc::new(Image::from_bytes(ImageFormat::Jpeg, b.into_owned()))))
            .clone()
    })
}

/// 1 when the wallpaper matches the appearance, else 0.16 (light) / 0.12 (dark).
pub fn opacity(id: &str, dark: bool) -> f32 {
    if (id == "night") == dark {
        1.0
    } else if dark {
        0.12
    } else {
        0.16
    }
}

pub fn current(cx: &App) -> String {
    cx.try_global::<AppPrefs>().map(|p| p.prefs.wallpaper.clone()).unwrap_or_else(|| "none".into())
}

pub fn is_active(cx: &App) -> bool {
    image(&current(cx)).is_some()
}

/// The image filling its parent (scaled to fill, clipped), or nothing for "none".
pub fn fill(id: &str, dark: bool) -> Option<AnyElement> {
    fill_rounded(id, dark, px(0.0))
}

/// [`fill`] with rounded right corners (the chat pane at the window's right edge; GPUI clips children to rectangles).
pub fn fill_rounded(id: &str, dark: bool, right_radius: gpui::Pixels) -> Option<AnyElement> {
    let image = image(id)?;
    Some(
        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .opacity(opacity(id, dark))
            .child(img(image).size_full().object_fit(ObjectFit::Cover).rounded_tr(right_radius).rounded_br(right_radius))
            .into_any_element(),
    )
}

/// Background layer for the chat pane.
pub fn background(right_radius: gpui::Pixels, cx: &App) -> Option<AnyElement> {
    fill_rounded(&current(cx), Theme::get(cx).dark, right_radius)
}

/// Small secondary text drawn straight on the wallpaper gets a material capsule (padding h 7 v 2) — only when one is set.
pub fn legible(content: impl IntoElement, active: bool, theme: &Theme) -> AnyElement {
    if active {
        div().px(px(7.0)).py(px(2.0)).rounded_full().bg(with_alpha(theme.content_bg(), 0.82)).child(content).into_any_element()
    } else {
        content.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn images_and_opacity() {
        for (id, _) in super::OPTIONS.iter().skip(1) {
            assert!(super::image(id).is_some(), "{id}");
        }
        assert!(super::image("none").is_none());
        assert!(super::image("../secret").is_none());
        assert_eq!(super::opacity("silk", false), 1.0);
        assert_eq!(super::opacity("silk", true), 0.12);
        assert_eq!(super::opacity("night", true), 1.0);
        assert_eq!(super::opacity("night", false), 0.16);
    }
}
