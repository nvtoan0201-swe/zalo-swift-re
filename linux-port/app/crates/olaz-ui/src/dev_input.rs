//! Dev input channel (debug builds with `OLAZ_DEV_EVAL=1`): `<data root>/dev/input.json` holds a list of synthetic
//! events dispatched to the main window through GPUI's own input path (the X11 backend reads only XInput2, so tools
//! like xdotool cannot drive it headless). Events: `{"t":"click"|"rclick"|"move","x":..,"y":..}`, `{"t":"key","k":"ctrl-c"}`,
//! `{"t":"type","s":"text"}`, `{"t":"wait","ms":..}`, `{"t":"play","path":..,"audio":bool}` (player window), `{"t":"photo","url":..}` (photo viewer). Add `"win":"last"` to send an event to the newest window. Writes `input.out.txt` when done.

use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AsyncApp, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, Point, px,
};
use serde_json::Value;

thread_local! {
    static LAST_OPENED: std::cell::Cell<Option<AnyWindowHandle>> = const { std::cell::Cell::new(None) };
}

/// Secondary windows register here so dev events with `"win":"last"` reach them.
pub fn note_window(window: AnyWindowHandle) {
    LAST_OPENED.with(|l| l.set(Some(window)));
}

pub fn start_if_enabled(window: AnyWindowHandle, cx: &mut App) {
    if !cfg!(debug_assertions) || std::env::var_os("OLAZ_DEV_EVAL").is_none() {
        return;
    }
    let dir = crate::app::engine(cx).paths.app_data().join("dev");
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let path = dir.join("input.json");
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let _ = std::fs::remove_file(&path);
            let events: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
            let mut log = Vec::new();
            for ev in events {
                if ev["t"] == "type" {
                    // Text goes through the platform input handler, which synthetic key events never reach: paste it instead
                    // (the user's clipboard is put back afterwards).
                    let text = ev["s"].as_str().unwrap_or("").to_string();
                    let saved = cx.update(|cx| cx.read_from_clipboard());
                    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)));
                    for input in to_inputs(&serde_json::json!({"t": "key", "k": "ctrl-v"})) {
                        let r = window.update(cx, |_, window, cx| !window.dispatch_event(input, cx).propagate);
                        log.push(format!("{ev} → {r:?}"));
                    }
                    // The paste reads the clipboard asynchronously: restore only after it landed.
                    cx.background_executor().timer(Duration::from_millis(700)).await;
                    if let Some(saved) = saved {
                        cx.update(|cx| cx.write_to_clipboard(saved));
                    }
                    continue;
                }
                if ev["t"] == "photo" {
                    let url = ev["url"].as_str().unwrap_or("").to_string();
                    cx.update(|cx| crate::viewer::open_photo_of(&url, None, cx));
                    log.push(format!("{ev} → opened"));
                    continue;
                }
                if ev["t"] == "play" {
                    let path = std::path::PathBuf::from(ev["path"].as_str().unwrap_or(""));
                    let audio = ev["audio"].as_bool().unwrap_or(false);
                    cx.update(|cx| crate::player::open(path, "Dev".into(), audio, cx));
                    log.push(format!("{ev} → opened"));
                    continue;
                }
                if ev["t"] == "raise" {
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                    continue;
                }
                if ev["t"] == "wait" {
                    cx.background_executor().timer(Duration::from_millis(ev["ms"].as_u64().unwrap_or(300))).await;
                    continue;
                }
                let inputs = to_inputs(&ev);
                // `"win":"last"` targets the newest window (player, settings…) instead of the main one.
                let target = if ev["win"] == "last" { LAST_OPENED.with(|l| l.get()).unwrap_or(window) } else { window };
                for input in inputs {
                    let r = target.update(cx, |_, window, cx| {
                        let r = window.dispatch_event(input, cx);
                        window.refresh();
                        !r.propagate
                    });
                    log.push(format!("{} → {:?}", ev, r));
                    cx.background_executor().timer(Duration::from_millis(150)).await;
                }
            }
            if !log.is_empty() {
                let _ = std::fs::write(dir.join("input.out.txt"), log.join("\n"));
            }
        }
    })
    .detach();
}

fn to_inputs(ev: &Value) -> Vec<PlatformInput> {
    let pos = Point::new(px(ev["x"].as_f64().unwrap_or(0.0) as f32), px(ev["y"].as_f64().unwrap_or(0.0) as f32));
    let mv = PlatformInput::MouseMove(MouseMoveEvent { position: pos, pressed_button: None, modifiers: Modifiers::default() });
    let click = |button| {
        vec![
            mv.clone(),
            PlatformInput::MouseDown(MouseDownEvent { button, position: pos, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }),
            PlatformInput::MouseUp(MouseUpEvent { button, position: pos, modifiers: Modifiers::default(), click_count: 1 }),
        ]
    };
    let key = |k: &str| -> Vec<PlatformInput> {
        let Some(keystroke) = Keystroke::parse(Box::leak(k.to_string().into_boxed_str())).ok() else { return vec![] };
        vec![
            PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false }),
            PlatformInput::KeyUp(KeyUpEvent { keystroke }),
        ]
    };
    match ev["t"].as_str().unwrap_or("") {
        "move" => vec![mv],
        "click" => click(MouseButton::Left),
        "rclick" => click(MouseButton::Right),
        "key" => key(ev["k"].as_str().unwrap_or("")),
        "type" => ev["s"]
            .as_str()
            .unwrap_or("")
            .chars()
            .flat_map(|c| {
                let mut ks = Keystroke::parse(if c == ' ' { "space" } else { "a" }).unwrap();
                ks.key = if c == ' ' { "space".into() } else { c.to_string() };
                ks.key_char = Some(c.to_string());
                vec![
                    PlatformInput::KeyDown(KeyDownEvent { keystroke: ks.clone(), is_held: false, prefer_character_input: true }),
                    PlatformInput::KeyUp(KeyUpEvent { keystroke: ks }),
                ]
            })
            .collect(),
        _ => vec![],
    }
}
