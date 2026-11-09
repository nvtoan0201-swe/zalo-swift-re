//! `$zelectronNative` clipboard  over `arboard` (Wayland data-control, else X11).

use std::rc::Rc;

use olaz_core::value::{Args, BV};
use olaz_host::bridge::ZaloBridge;

fn with_clipboard<T>(f: impl FnOnce(&mut arboard::Clipboard) -> T) -> Option<T> {
    arboard::Clipboard::new().ok().map(|mut c| f(&mut c))
}

pub fn install(bridge: &Rc<ZaloBridge>) {
    bridge.sync("$zelectronNative.clipboardText", |_| Ok(BV::String(with_clipboard(|c| c.get_text().ok()).flatten().unwrap_or_default())));
    bridge.sync("$zelectronNative.clipboardWriteText", |a| {
        let text = a.string(0).unwrap_or_default();
        with_clipboard(|c| c.set_text(text).ok());
        Ok(BV::Undefined)
    });
    bridge.sync("$zelectronNative.clipboardFormats", |_| {
        let mut formats = Vec::new();
        with_clipboard(|c| {
            if c.get_text().is_ok() {
                formats.push(BV::str("text/plain"));
            }
            if c.get().html().is_ok() {
                formats.push(BV::str("text/html"));
            }
            if c.get_image().is_ok() {
                formats.push(BV::str("image/png"));
            }
            if c.get().file_list().is_ok_and(|l| !l.is_empty()) {
                formats.push(BV::str("text/uri-list"));
            }
        });
        Ok(BV::Array(formats))
    });
}
