//! Actions and key bindings (the macOS shortcuts with ⌘ → Ctrl).

use gpui::{App, KeyBinding, actions};

actions!(olaz, [QuickSwitch, FocusSearch, ToggleSidebar, ToggleInspector, Reply, Quit, OpenSettings]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-k", QuickSwitch, None),
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("ctrl-alt-s", ToggleSidebar, None),
        KeyBinding::new("ctrl-alt-i", ToggleInspector, None),
        KeyBinding::new("ctrl-r", Reply, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &QuickSwitch, cx| crate::shell::request_switcher(cx));
    cx.on_action(|_: &OpenSettings, cx| crate::settings_window::open_window(cx));
    cx.on_action(|_: &ToggleInspector, cx| {
        crate::inspector::toggle_shown();
        cx.refresh_windows();
    });
}
