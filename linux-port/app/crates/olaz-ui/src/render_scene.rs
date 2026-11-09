//! `olaz --ui-render <scene-id>` (debug builds): one window showing a catalog scene built from
//! fixtures with a fixed clock, no webhost. When the scene has settled the window geometry is written to the
//! ready file and the window stays open until the capture script closes the process.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// What the caller asked for.
#[derive(Debug, Clone)]
pub struct RenderScene {
    pub id: String,
    /// "light" | "dark"
    pub appearance: String,
    pub fixtures_dir: PathBuf,
    /// The scene's "now" (epoch ms), so relative times and day separators are reproducible.
    pub now_ms: i64,
    pub ready_file: Option<PathBuf>,
}

/// One catalog entry: data, window size and the UI state to reach.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneSpec {
    pub fixture: Option<&'static str>,
    pub size: (f32, f32),
    /// `login.json` phase id.
    pub login_capture: Option<String>,
    pub inspector: bool,
    pub switcher: bool,
    pub wallpaper: Option<&'static str>,
    pub text_size: Option<&'static str>,
    /// Panel scenes: one view filling the window instead of the shell.
    pub panel: Option<Panel>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Panel {
    /// ReactionDetailView of a message.
    Reactions { conversation: &'static str, message: &'static str },
    /// PollDetailSheet, on the vote list or the voters.
    Poll { id: &'static str, conversation: &'static str, question: &'static str, voters: bool },
    /// The settings window (`form_only` = just the tab's form).
    Settings { tab: crate::settings_window::Tab, form_only: bool },
    CreateGroup,
    /// The composer's sticker picker for a conversation (Linux-only hover scene).
    Stickers { conversation: &'static str },
}

const GROUP: &str = "g3000000000000000003";
const POLL_QUESTION: &str = "Họp tổng kết vào ngày nào?";

const MAIN: (f32, f32) = (1180.0, 760.0);
const SETUP: (f32, f32) = (900.0, 620.0);

/// Scene ids: S01–S18 follow the macOS screens, L-* are Linux-only checks.
pub const CATALOG: &[&str] = &[
    "S01-splash",
    "S02-login-generating",
    "S02-login-qr",
    "S02-login-qr2",
    "S02-login-confirm",
    "S02-login-expired",
    "S02-login-failed",
    "S03-shell-empty",
    "S06-chat-basic",
    "S06-chat-group-cards",
    "S06-chat-basic-wallpaper-silk",
    "S06-chat-basic-text-xl",
    "S07-inspector-1to1",
    "S15-switcher-empty",
    "S07-inspector-group",
    "S12-reactions-all",
    "S17-poll-vote",
    "S17-poll-voters",
    "S14-settings-first-tab",
    "S14-appearance",
    "S14-notifications",
    "S18-create-group-loading",
    // Linux-only: sticker picker with a sprite sticker, for the hover animation check (tools/sticker-hover-check.py).
    "L-sticker-hover",
];

pub fn spec(id: &str) -> Option<SceneSpec> {
    use crate::settings_window::Tab;
    let base = SceneSpec { fixture: None, size: MAIN, login_capture: None, inspector: false, switcher: false, wallpaper: None, text_size: None, panel: None };
    let panel = |fixture: &'static str, w: f32, h: f32, p: Panel| SceneSpec { fixture: Some(fixture), size: (w, h), panel: Some(p), ..base.clone() };
    Some(match id {
        "S01-splash" => SceneSpec { size: SETUP, ..base },
        _ if id.starts_with("S02-login-") && CATALOG.contains(&id) => SceneSpec { fixture: Some("login"), size: SETUP, login_capture: Some(id.to_string()), ..base },
        "S03-shell-empty" => SceneSpec { fixture: Some("base-account"), ..base },
        "S06-chat-basic" => SceneSpec { fixture: Some("chat-basic"), ..base },
        "S06-chat-group-cards" => SceneSpec { fixture: Some("chat-group-cards"), ..base },
        "S06-chat-basic-wallpaper-silk" => SceneSpec { fixture: Some("chat-basic"), wallpaper: Some("silk"), ..base },
        "S06-chat-basic-text-xl" => SceneSpec { fixture: Some("chat-basic"), text_size: Some("extraLarge"), ..base },
        "S07-inspector-1to1" => SceneSpec { fixture: Some("chat-basic"), size: (1460.0, 760.0), inspector: true, ..base },
        "S15-switcher-empty" => SceneSpec { fixture: Some("base-account"), switcher: true, ..base },
        "S07-inspector-group" => SceneSpec { fixture: Some("dialogs"), size: (1460.0, 760.0), inspector: true, ..base },
        "S12-reactions-all" => panel("dialogs", 320.0, 260.0, Panel::Reactions { conversation: GROUP, message: "g08" }),
        "S17-poll-vote" => panel("dialogs", 440.0, 560.0, Panel::Poll { id: "77", conversation: GROUP, question: POLL_QUESTION, voters: false }),
        "S17-poll-voters" => panel("dialogs", 440.0, 560.0, Panel::Poll { id: "77", conversation: GROUP, question: POLL_QUESTION, voters: true }),
        "S14-settings-first-tab" => panel("base-account", 520.0, 210.0, Panel::Settings { tab: Tab::General, form_only: false }),
        "S14-appearance" => panel("base-account", 520.0, 700.0, Panel::Settings { tab: Tab::Appearance, form_only: true }),
        "S14-notifications" => panel("base-account", 520.0, 440.0, Panel::Settings { tab: Tab::Notifications, form_only: true }),
        "S18-create-group-loading" => panel("base-account", 440.0, 640.0, Panel::CreateGroup),
        "L-sticker-hover" => panel("stickers", 360.0, 420.0, Panel::Stickers { conversation: "2000000000000000002" }),
        _ => return None,
    })
}

/// Window root of a panel scene: builds the view on first render (after the fixtures were applied) and fills the window.
pub struct PanelHost {
    panel: Panel,
    view: Option<gpui::AnyView>,
}

impl PanelHost {
    pub fn new(panel: Panel) -> Self {
        PanelHost { panel, view: None }
    }
}

impl gpui::Render for PanelHost {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{AppContext as _, ParentElement as _, Styled as _};
        crate::layout_dump::begin_frame();
        if self.view.is_none() {
            let view: gpui::AnyView = match self.panel.clone() {
                Panel::Reactions { conversation, message } => {
                    let reactions = crate::app::engine(cx)
                        .state
                        .borrow()
                        .messages
                        .get(conversation)
                        .and_then(|l| l.iter().find(|m| m.id == message).map(|m| m.reactions.clone()))
                        .unwrap_or_default();
                    cx.new(|cx| crate::reactions::ReactionDetailView::new(conversation.into(), message.into(), reactions, cx)).into()
                }
                Panel::Poll { id, conversation, question, voters } => {
                    cx.new(|cx| crate::polls::PollDetailSheet::new(id.into(), conversation.into(), question.into(), voters, cx)).into()
                }
                Panel::Settings { tab, form_only } => cx.new(|_| crate::settings_window::SettingsView::new(tab, form_only)).into(),
                Panel::CreateGroup => cx.new(|cx| crate::group_create::CreateGroupSheet::new(vec![], window, cx)).into(),
                Panel::Stickers { conversation } => {
                    crate::app::engine(cx).state.borrow_mut().open_conversation_id = Some(conversation.into());
                    cx.new(|cx| crate::stickers::StickerPicker::new(conversation.into(), window, cx)).into()
                }
            };
            self.view = Some(view);
        }
        let theme = crate::theme::Theme::get(cx).clone();
        gpui::div()
            .size_full()
            .bg(theme.window_bg())
            .text_color(theme.label())
            .font_family(crate::theme::FONT)
            .flex()
            .items_start()
            .justify_start()
            .relative()
            .children(self.view.clone())
            .children(crate::layout_dump::input_shield())
    }
}

/// The fixtures' merged `shell` block (later fixtures override earlier ones).
pub fn shell_state(fixtures: &[olaz_engine::fixtures::Fixture]) -> Map<String, Value> {
    let mut out = Map::new();
    for f in fixtures {
        for (k, v) in &f.shell {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Parses `olaz --ui-render <id> [--appearance light|dark] [--fixtures DIR] [--now MS] [--ready FILE]`.
pub fn parse_args(args: &[String]) -> Result<RenderScene, String> {
    let id = args.first().filter(|a| !a.starts_with("--")).ok_or("usage: olaz --ui-render <scene-id> [--appearance light|dark] [--fixtures DIR] [--now MS] [--ready FILE]")?;
    if spec(id).is_none() {
        return Err(format!("unknown scene {id}; known: {}", CATALOG.join(", ")));
    }
    let mut scene = RenderScene {
        id: id.clone(),
        appearance: "light".into(),
        fixtures_dir: olaz_engine::fixtures::default_dir(),
        now_ms: olaz_core::clock::now_ms(),
        ready_file: None,
    };
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--appearance" if value == "light" || value == "dark" => scene.appearance = value.clone(),
            "--fixtures" => scene.fixtures_dir = PathBuf::from(value),
            "--now" => scene.now_ms = value.parse().map_err(|_| format!("bad --now {value}"))?,
            "--ready" => scene.ready_file = Some(PathBuf::from(value)),
            _ => return Err(format!("bad option {flag} {value}")),
        }
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_resolve() {
        for id in CATALOG {
            let s = spec(id).unwrap_or_else(|| panic!("{id}"));
            if let Some(f) = s.fixture {
                assert!(olaz_engine::fixtures::load(&olaz_engine::fixtures::default_dir(), f, 0).is_ok(), "{id}");
            }
        }
        assert!(spec("S02-login-nope").is_none());
        assert_eq!(spec("S07-inspector-1to1").unwrap().size, (1460.0, 760.0));
    }

    #[test]
    fn args() {
        let a: Vec<String> = ["S06-chat-basic", "--appearance", "dark", "--now", "1790000000000", "--ready", "/x/r"].iter().map(|s| s.to_string()).collect();
        let s = parse_args(&a).unwrap();
        assert_eq!((s.appearance.as_str(), s.now_ms), ("dark", 1_790_000_000_000));
        assert_eq!(s.ready_file, Some(PathBuf::from("/x/r")));
        assert!(parse_args(&["S99".to_string()]).is_err());
        assert!(parse_args(&["S06-chat-basic".to_string(), "--appearance".to_string(), "sepia".to_string()]).is_err());
    }
}
