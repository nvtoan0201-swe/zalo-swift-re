//! Layout dump for the visual tests: components register `(role, text, bounds)` while they prepaint, using
//! AppKit accessibility role names; `--ui-render` writes the result as `<id>.layout.json` with the schema
//! `{role, label, frame:{x,y,w,h}, children}` (frames in points from the content origin). Off (no elements added)
//! outside `--ui-render`.

use std::cell::{Cell, RefCell};

use gpui::{AnyElement, Bounds, IntoElement, Pixels, Styled, canvas};
use serde_json::{Value, json};

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static FRAME: RefCell<Vec<Element>> = const { RefCell::new(Vec::new()) };
    static LAST: RefCell<Vec<Element>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub role: &'static str,
    pub text: String,
    pub bounds: (f32, f32, f32, f32),
}

pub fn enable() {
    ENABLED.with(|e| e.set(true));
}

pub fn enabled() -> bool {
    ENABLED.with(|e| e.get())
}

/// `--ui-render` windows pop up over the desktop: a transparent layer on top keeps real pointer events (someone working
/// at the machine) from changing the scene. Left out when the dev input channel drives the scene (hover checks).
pub fn input_shield() -> Option<AnyElement> {
    use gpui::InteractiveElement as _;
    (enabled() && std::env::var_os("OLAZ_DEV_EVAL").is_none()).then(|| gpui::div().absolute().inset_0().occlude().into_any_element())
}

/// Called at the start of each frame of the render root: the previous frame's elements become the dump.
pub fn begin_frame() {
    if !enabled() {
        return;
    }
    let done = FRAME.with(|f| std::mem::take(&mut *f.borrow_mut()));
    if !done.is_empty() {
        LAST.with(|l| *l.borrow_mut() = done);
    }
}

pub fn record(role: &'static str, text: impl Into<String>, bounds: Bounds<Pixels>) {
    let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    FRAME.with(|f| f.borrow_mut().push(Element { role, text: text.into(), bounds: (x, y, w, h) }));
}

/// An invisible child filling its parent that records the parent's box (add to a `relative()` container).
pub fn probe(role: &'static str, text: impl Into<String>) -> Option<AnyElement> {
    if !enabled() {
        return None;
    }
    let text = text.into();
    Some(canvas(move |bounds, _, _| record(role, text.clone(), bounds), |_, _, _, _| {}).absolute().inset_0().into_any_element())
}

/// The last complete frame in that schema, frames relative to the content origin (`offset` = window paddings).
pub fn dump(offset: (f32, f32)) -> Value {
    // Between frames the current buffer holds the last complete frame; mid-frame, fall back to the previous one.
    let current = FRAME.with(|f| f.borrow().clone());
    let elements = if current.is_empty() { LAST.with(|l| l.borrow().clone()) } else { current };
    to_json(&elements, offset)
}

pub fn to_json(elements: &[Element], offset: (f32, f32)) -> Value {
    let children: Vec<Value> = elements
        .iter()
        .map(|e| {
            json!({
                "role": e.role,
                "label": e.text,
                "frame": {"x": e.bounds.0 - offset.0, "y": e.bounds.1 - offset.1, "w": e.bounds.2, "h": e.bounds.3},
            })
        })
        .collect();
    json!({"role": "AXWindow", "children": children})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_and_offset() {
        let v = to_json(&[Element { role: "AXButton", text: "Xong".into(), bounds: (30.0, 40.0, 60.0, 28.0) }], (20.0, 20.0));
        assert_eq!(v["role"], "AXWindow");
        assert_eq!(v["children"][0]["role"], "AXButton");
        assert_eq!(v["children"][0]["label"], "Xong");
        assert_eq!(v["children"][0]["frame"]["x"], 10.0);
        assert_eq!(v["children"][0]["frame"]["h"], 28.0);
        assert!(probe("AXButton", "x").is_none(), "off outside --ui-render");
    }
}
