//! Custom window chrome: client-side decorations, GNOME-style window buttons, drag/zoom on empty
//! titlebar space. GNOME/Wayland has no server decorations, so every window draws its own.

use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, Window, div,
    px,
};

use gpui::prelude::FluentBuilder;

use crate::theme::Theme;

pub const TITLEBAR_HEIGHT: f32 = 52.0;
pub const WINDOW_RADIUS: f32 = 12.0;
pub const CONTROLS_INSET: f32 = 14.0;

/// GNOME-style window buttons (minimize, maximize/restore, close) at the trailing edge — Linux replaces the
/// GNOME-style window buttons (minimize, maximize, close) on the right.
pub fn window_controls(window: &Window, cx: &App) -> impl IntoElement {
    let theme = Theme::get(cx).clone();
    let button = |id: &'static str, path: &'static str| {
        div()
            .id(id)
            .size(px(24.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.quaternary())
            .hover(|s| s.bg(theme.separator().opacity(0.9)))
            .cursor_pointer()
            .child(gpui::svg().path(path).size(px(12.0)).text_color(theme.label()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    };
    let maximized = window.is_maximized();
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(button("wc-min", "icons/window-minimize.svg").on_click(|_, window, _| window.minimize_window()))
        .child(
            button("wc-max", if maximized { "icons/window-restore.svg" } else { "icons/window-maximize.svg" })
                .on_click(|_, window, _| window.zoom_window()),
        )
        .child(button("wc-close", "icons/window-close.svg").on_click(|_, window, cx| {
            window.remove_window();
            cx.quit();
        }))
}

/// A transparent bar of `height` over full-bleed content: drags the window, double click zooms, lights at leading.
pub fn titlebar_overlay(height: f32, window: &Window, cx: &App) -> impl IntoElement {
    div()
        .id("titlebar")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(height))
        .flex()
        .items_center()
        .justify_end()
        .pr(px(CONTROLS_INSET))
        .on_mouse_down(MouseButton::Left, |ev, window, _| {
            if ev.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
        .child(window_controls(window, cx))
}

/// Drag region for toolbar areas of the shell (no lights).
pub fn drag_area(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div().id(id).on_mouse_down(MouseButton::Left, |ev, window, _| {
        if ev.click_count >= 2 {
            window.zoom_window();
        } else {
            window.start_window_move();
        }
    })
}

/// Rounded outer corners while the window floats; square when maximized or tiled. The shadow, resize edges and the
/// platform client inset come from `gpui_component::Root`'s window border (a second border here would reset the
/// inset to 0 and misplace the decorations).
/// Corner radius for content that paints its own background edge to edge (GPUI clips children to rectangles, so a
/// square background would show outside the rounded frame).
pub fn inner_radius(window: &Window) -> gpui::Pixels {
    if window.is_maximized() || window.is_fullscreen() { px(0.0) } else { px(WINDOW_RADIUS - 1.0) }
}

pub fn frame(content: impl IntoElement, window: &Window, cx: &App) -> impl IntoElement {
    let theme = Theme::get(cx);
    let tiled = window.is_maximized() || window.is_fullscreen();
    div()
        .size_full()
        .bg(theme.window_bg())
        // Inside the window border's 1-px line (vendor/gpui-component window_border.rs, same radius).
        .when(!tiled, |el| el.rounded(px(WINDOW_RADIUS - 1.0)))
        .child(content)
}

/// The window root: gpui-component's `Root` with a transparent surface, so only the rounded frame paints the corners
/// (its default theme background is a square that would show outside them).
pub fn root(view: impl Into<gpui::AnyView>, window: &mut Window, cx: &mut gpui::Context<gpui_component::Root>) -> gpui_component::Root {
    gpui_component::Root::new(view, window, cx).bg(gpui::transparent_black())
}
