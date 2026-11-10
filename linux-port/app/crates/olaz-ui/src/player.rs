//! Video / voice player window: GStreamer frames drawn as GPUI images, play/pause, scrubbing, time.
//! When GStreamer or a decoder is missing, the window says so and offers to open the file with another application.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement, Render,
    RenderImage, StatefulInteractiveElement, Styled, StyledImage, Task, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions,
    div, img, prelude::FluentBuilder, px, relative, size,
};
use olaz_media::player::{Player, PlayerEvent};

use crate::icon::icon;
use crate::widgets::spinner;

pub struct PlayerView {
    path: PathBuf,
    title: String,
    audio_only: bool,
    player: Option<Player>,
    error: Option<String>,
    image: Option<Arc<RenderImage>>,
    shown_seq: u64,
    _pump: Option<Task<()>>,
}

fn clock(ns: i64) -> String {
    let s = (ns / 1_000_000_000).max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

impl PlayerView {
    fn new(path: PathBuf, title: String, audio_only: bool, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let (player, error) = match Player::open(&path, move || {
            let _ = tx.unbounded_send(());
        }) {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(if olaz_media::player::available() { format!("Không phát được nội dung này ({e}).") } else { "Máy chưa có GStreamer để phát nội dung này.".into() })),
        };
        let pump = cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while rx.next().await.is_some() {
                // Coalesce bursts: one render per batch of player updates.
                while let Ok(Some(_)) = rx.try_next() {}
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        PlayerView { path, title, audio_only, player, error, image: None, shown_seq: 0, _pump: Some(pump) }
    }

    fn toggle(&mut self) {
        if let Some(p) = &self.player {
            if p.state.lock().unwrap().playing {
                p.pause()
            } else {
                p.play()
            }
        }
    }

    fn open_elsewhere(&self) {
        let _ = std::process::Command::new("xdg-open").arg(&self.path).spawn();
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (frame, seq, pos, dur, playing, event) = match &self.player {
            Some(p) => {
                let s = p.state.lock().unwrap();
                (s.frame.clone(), s.frame_seq, s.position_ns, s.duration_ns, s.playing, s.event.clone())
            }
            None => (None, 0, 0, 0, false, None),
        };
        if seq != self.shown_seq {
            if let Some(f) = frame {
                if let Some(buf) = image::RgbaImage::from_raw(f.width, f.height, (*f.bgra).clone()) {
                    let next = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buf)]));
                    if let Some(old) = self.image.replace(next) {
                        // Each frame is a new texture: release the previous one from the atlas.
                        let _ = window.drop_image(old);
                    }
                }
            }
            self.shown_seq = seq;
        }
        let error = self.error.clone().or(match &event {
            Some(PlayerEvent::Error(e)) => Some(e.clone()),
            _ => None,
        });
        let content: AnyElement = if let Some(e) = &error {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(14.0))
                .p(px(30.0))
                .child(icon("exclamationmark.triangle.fill", 34.0, gpui::hsla(0.08, 0.9, 0.55, 1.0)))
                .child(div().max_w(px(440.0)).text_center().text_size(px(13.5)).text_color(gpui::white()).child(e.clone()))
                .child(
                    div()
                        .id("player-open-elsewhere")
                        .px(px(14.0))
                        .py(px(6.0))
                        .rounded_full()
                        .bg(gpui::white().opacity(0.15))
                        .text_size(px(13.0))
                        .text_color(gpui::white())
                        .child("Mở bằng ứng dụng khác")
                        .on_click(cx.listener(|this, _, _, _| this.open_elsewhere())),
                )
                .into_any_element()
        } else if self.audio_only || (self.image.is_none() && dur > 0 && seq == 0 && pos > 500_000_000) {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child(icon("waveform", 48.0, gpui::white()))
                .into_any_element()
        } else if let Some(image) = self.image.clone() {
            img(image).size_full().object_fit(ObjectFit::Contain).into_any_element()
        } else {
            div().size_full().flex().items_center().justify_center().child(spinner(22.0, gpui::white())).into_any_element()
        };
        let fraction = if dur > 0 { (pos as f32 / dur as f32).clamp(0.0, 1.0) } else { 0.0 };
        let ended = event == Some(PlayerEvent::Ended);
        let controls = (error.is_none()).then(|| {
            div()
                .absolute()
                .left(px(16.0))
                .right(px(16.0))
                .bottom(px(16.0))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(14.0))
                .bg(gpui::black().opacity(0.6))
                .flex()
                .items_center()
                .gap(px(12.0))
                .text_color(gpui::white())
                .child(
                    div()
                        .id("player-toggle")
                        .size(px(30.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(gpui::white().opacity(0.15)))
                        .child(icon(if playing && !ended { "pause.fill" } else { "play.fill" }, 15.0, gpui::white()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle();
                            cx.notify();
                        })),
                )
                .child(div().text_size(px(12.0)).child(clock(pos)))
                .child(
                    div()
                        .id("player-scrub")
                        .flex_1()
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .child(div().w_full().h(px(4.0)).rounded_full().bg(gpui::white().opacity(0.25)).child(div().h_full().rounded_full().bg(gpui::white()).w(relative(fraction))))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &gpui::MouseDownEvent, window, _| {
                            // Scrub to where the bar was clicked (the bar spans the window minus the side controls).
                            let w = window.viewport_size().width;
                            let left = px(16.0 + 14.0 + 30.0 + 12.0 + 40.0 + 12.0);
                            let right = px(16.0 + 14.0 + 40.0 + 12.0);
                            let span = (w - left - right).max(px(1.0));
                            let f = ((ev.position.x - left) / span).clamp(0.0, 1.0);
                            if let Some(p) = &this.player {
                                let dur = p.state.lock().unwrap().duration_ns;
                                p.seek((dur as f64 * f as f64) as i64);
                            }
                        })),
                )
                .child(div().text_size(px(12.0)).child(clock(dur)))
        });
        crate::chrome::frame(
            div()
                .size_full()
                .relative()
                .bg(gpui::black())
                .rounded(crate::chrome::inner_radius(window))
                .font_family(crate::theme::FONT)
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "space" {
                        this.toggle();
                        cx.notify();
                    }
                }))
                .child(div().absolute().inset_0().pt(px(36.0)).when(self.audio_only && error.is_none(), |el| el.pb(px(70.0))).child(content))
                .children(controls)
                .child(div().absolute().top(px(10.0)).left(px(16.0)).text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(gpui::white().opacity(0.8)).child(self.title.clone()))
                .child(crate::chrome::titlebar_overlay_on_dark(36.0, window, cx)),
            window,
            cx,
        )
    }
}

/// Opens the player window for a downloaded file.
pub fn open(path: PathBuf, title: String, audio_only: bool, cx: &mut App) {
    let s = if audio_only { size(px(420.0), px(260.0)) } else { size(px(960.0), px(600.0)) };
    let bounds = Bounds::centered(None, s, cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some(title.clone().into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some(olaz_core::APP_ID.into()),
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| PlayerView::new(path, title, audio_only, cx));
            let _ = window;
            cx.new(|cx| crate::chrome::root(view, window, cx))
        },
    );
    if let Ok(h) = opened {
        crate::dev_input::note_window(h.into());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clock_text() {
        assert_eq!(super::clock(0), "0:00");
        assert_eq!(super::clock(125_400_000_000), "2:05");
    }
}
