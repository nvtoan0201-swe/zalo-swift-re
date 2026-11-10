//! Application bootstrap and root routing.

use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled, TitlebarOptions, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use olaz_core::paths::XdgPaths;
use olaz_engine::{Phase, Services, ZaloEngine};

use crate::chrome;
use crate::settings::AppPrefs;
use crate::setup::{self, SetupStage};
use crate::shell::ShellView;
use crate::theme::{Theme, accent_color, hex, system_prefers_dark};
use crate::widgets::Images;

/// Everything the views need from the outside world.
pub struct UiConfig {
    pub paths: XdgPaths,
    pub services: Services,
    /// `--ui-render`: show one fixture scene instead of starting the client.
    pub render: Option<crate::render_scene::RenderScene>,
}

/// The engine as a GPUI global (views read it in `render`).
pub struct EngineGlobal(pub Rc<ZaloEngine>);
impl gpui::Global for EngineGlobal {}

pub fn engine(cx: &App) -> Rc<ZaloEngine> {
    cx.global::<EngineGlobal>().0.clone()
}

pub fn apply_theme(cx: &mut App) {
    let prefs = cx.global::<AppPrefs>();
    let dark = match prefs.prefs.appearance.as_str() {
        "dark" => true,
        "light" => false,
        _ => system_prefers_dark(),
    };
    let accent = hex(accent_color(&prefs.prefs.accent));
    let text_scale = prefs.text_scale();
    cx.set_global(Theme { dark, accent, text_scale });
    let mode = if dark { gpui_component::theme::ThemeMode::Dark } else { gpui_component::theme::ThemeMode::Light };
    gpui_component::theme::Theme::change(mode, None, cx);
}

pub fn run(config: UiConfig) {
    let app = gpui_platform::application().with_assets(crate::assets::OlazAssets);
    app.run(move |cx: &mut App| {
        crate::assets::register_fonts(cx);
        gpui_component::init(cx);
        crate::keymap::init(cx);
        let mut prefs = AppPrefs::load(&config.paths);
        let render = config.render.clone().map(|scene| {
            let spec = crate::render_scene::spec(&scene.id).expect("known scene");
            olaz_core::clock::set_fixed(Some(scene.now_ms));
            olaz_engine::models::allow_fixture_links(&scene.fixtures_dir);
            crate::layout_dump::enable();
            // In memory only: a render never writes the profile's settings.
            prefs.prefs.appearance = scene.appearance.clone();
            prefs.prefs.did_finish_setup = true;
            prefs.prefs.setup_pending = false;
            prefs.prefs.wallpaper = spec.wallpaper.unwrap_or("none").into();
            prefs.prefs.text_size = spec.text_size.unwrap_or("standard").into();
            (scene, spec)
        });
        cx.set_global(prefs);
        apply_theme(cx);

        let exec: olaz_core::exec::Exec =
            Rc::new(crate::exec::GpuiExecutor { fg: cx.foreground_executor().clone(), bg: cx.background_executor().clone() });
        let ua = olaz_core::fingerprint::user_agent();
        let mut roots = vec![config.paths.user_data(), config.paths.cache()];
        if let Some((scene, _)) = &render {
            // Fixture media are `file://` URLs under the fixtures directory.
            roots.push(scene.fixtures_dir.canonicalize().unwrap_or(scene.fixtures_dir.clone()));
        }
        let loader = olaz_media::MediaLoader::new(&config.paths.cache(), roots, &ua);
        cx.set_global(Images::new(loader.clone()));
        let mut services = config.services;
        services.media = Some(loader);
        let engine = ZaloEngine::new(exec, config.paths.clone(), services);
        cx.set_global(EngineGlobal(engine.clone()));

        // Engine changes → re-render (coalesced per frame).
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<olaz_engine::Change>();
        engine.subscribe(move |c| {
            let _ = tx.unbounded_send(c.clone());
        });
        cx.spawn(async move |cx| {
            while let Some(first) = rx.next().await {
                let mut changes = vec![first];
                while let Ok(Some(c)) = rx.try_next() {
                    changes.push(c);
                }
                let _ = cx.update(|cx| {
                    if ACTIVATE.with(|a| a.replace(false)) {
                        if let Some(w) = cx.windows().first().cloned() {
                            let _ = w.update(cx, |_, window, _| window.activate_window());
                        }
                    }
                    crate::shell::engine_changed(&changes, cx);
                    cx.refresh_windows();
                });
            }
        })
        .detach();

        let (w, h) = render.as_ref().map(|(_, spec)| spec.size).unwrap_or((1180.0, 760.0));
        let title = if render.is_some() { "olaz-ui-render" } else { "Olaz" };
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(if render.is_some() { size(px(w.min(960.0)), px(h.min(620.0))) } else { size(px(960.0), px(620.0)) }),
            app_id: Some(olaz_core::APP_ID.into()),
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        let render_panel = render.as_ref().and_then(|(_, spec)| spec.panel.clone());
        let window = cx
            .open_window(options, |window, cx| match render_panel {
                Some(panel) => {
                    let host = cx.new(|_| crate::render_scene::PanelHost::new(panel));
                    cx.new(|cx| crate::chrome::root(host, window, cx))
                }
                None => {
                    let root = cx.new(|cx| RootView::new(window, cx));
                    cx.new(|cx| crate::chrome::root(root, window, cx))
                }
            })
            .expect("main window");
        cx.set_global(crate::chrome::MainWindow(window.into()));
        crate::dev_input::start_if_enabled(window.into(), cx);
        cx.on_app_quit(|cx| {
            let engine = crate::app::engine(cx);
            async move {
                engine.shutdown();
            }
        })
        .detach();
        match render {
            Some((scene, spec)) => start_render(scene, spec, engine.clone(), cx),
            None => engine.start(),
        }
        cx.activate(true);
    });
}

/// Feeds the scene's fixtures, reaches its UI state, waits until it settles and writes the ready file.
fn start_render(scene: crate::render_scene::RenderScene, spec: crate::render_scene::SceneSpec, engine: Rc<ZaloEngine>, cx: &mut App) {
    let fixtures = match spec.fixture {
        Some(name) => olaz_engine::fixtures::load(&scene.fixtures_dir, name, scene.now_ms).unwrap_or_else(|e| panic!("fixture {name}: {e}")),
        None => Vec::new(),
    };
    if let Some(capture) = &spec.login_capture {
        let phase = fixtures.iter().flat_map(|f| f.phases.iter()).find(|p| p.get("capture").and_then(|c| c.as_str()) == Some(capture.as_str()));
        let login = phase.and_then(olaz_engine::fixtures::login_state).unwrap_or_else(|| panic!("login phase {capture}"));
        engine.show_login_state(login);
    } else if spec.fixture.is_some() {
        engine.apply_fixtures(&fixtures);
    }
    let shell = crate::render_scene::shell_state(&fixtures);
    cx.spawn(async move |cx| {
        // The shell exists after the first frame; then give images and layout time to settle.
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        let _ = cx.update(|cx| {
            crate::shell::apply_render_state(&shell, spec.inspector, spec.switcher, cx);
            cx.refresh_windows();
        });
        cx.background_executor().timer(std::time::Duration::from_millis(2500)).await;
        // The content area must equal the capture size: grow the window by its client-decoration paddings.
        let pads = cx
            .update(|cx| {
                let w = cx.windows().first().cloned()?;
                w.update(cx, |_, window, _| {
                    let p = gpui_component::window_paddings(window);
                    let (l, t, r, b) = (f32::from(p.left), f32::from(p.top), f32::from(p.right), f32::from(p.bottom));
                    window.resize(size(px(spec.size.0 + l + r), px(spec.size.1 + t + b)));
                    (l, t, r, b)
                })
                .ok()
            })
            .unwrap_or_default();
        cx.background_executor().timer(std::time::Duration::from_millis(800)).await;
        let _ = cx.update(|cx| {
            // Captures read the screen: be on top.
            if let Some(w) = cx.windows().first().cloned() {
                let _ = w.update(cx, |_, window, _| window.activate_window());
            }
            cx.refresh_windows();
        });
        cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
        if let Some(path) = &scene.ready_file {
            let info = serde_json::json!({"id": scene.id, "appearance": scene.appearance, "title": "olaz-ui-render", "width": spec.size.0, "height": spec.size.1, "now": scene.now_ms,
                "padding": {"left": pads.0, "top": pads.1, "right": pads.2, "bottom": pads.3}});
            // The element geometry of the settled frame (layout dump), next to the ready file.
            let layout = crate::layout_dump::dump((pads.0, pads.1));
            let _ = std::fs::write(path.with_extension("layout.json"), serde_json::to_string_pretty(&layout).unwrap_or_default());
            let _ = std::fs::write(path, info.to_string());
        }
    })
    .detach();
}

thread_local! {
    /// Set by the engine (notification click); the next engine-change pass brings the window to front.
    pub static ACTIVATE: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));
}

pub struct RootView {
    shell: Option<Entity<ShellView>>,
    sync_skipped: bool,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Follow the system appearance live while "system" is chosen.
        cx.observe_window_appearance(window, |_, _, cx| {
            apply_theme(cx);
            cx.refresh_windows();
        })
        .detach();
        // Focus bridge: window activation → the official FocusDetector.
        cx.observe_window_activation(window, |_, window, cx| {
            engine(cx).set_window_state(window.is_window_active(), true);
        })
        .detach();
        let flag = ACTIVATE.with(|a| a.clone());
        *engine(cx).on_activate.borrow_mut() = Some(Rc::new(move || flag.set(true)));
        Self { shell: None, sync_skipped: false }
    }

    fn shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ShellView> {
        if let Some(s) = &self.shell {
            return s.clone();
        }
        let s = cx.new(|cx| ShellView::new(window, cx));
        self.shell = Some(s.clone());
        s
    }

    fn finish_setup(&mut self, start_tour: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prefs = cx.global_mut::<AppPrefs>();
        prefs.prefs.did_finish_setup = true;
        prefs.prefs.setup_pending = false;
        prefs.save();
        if start_tour {
            let shell = self.shell(window, cx);
            shell.update(cx, |s, cx| s.start_tour_later(cx));
        }
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::layout_dump::begin_frame();
        let engine = engine(cx);
        let phase = engine.phase();
        // Phase side effects.
        let setup_pending = {
            let prefs = cx.global_mut::<AppPrefs>();
            if matches!(phase, Phase::Login(_)) && !prefs.prefs.setup_pending {
                prefs.prefs.setup_pending = true;
                prefs.save();
            }
            if phase == Phase::Ready && !prefs.prefs.setup_pending && !prefs.prefs.did_finish_setup {
                prefs.prefs.did_finish_setup = true;
                prefs.save();
            }
            prefs.prefs.setup_pending
        };
        if matches!(phase, Phase::Login(_)) {
            if let Some(s) = &self.shell {
                s.update(cx, |s, _| s.reset());
            }
        }
        let theme = Theme::get(cx).clone();
        let notice = engine.state.borrow().sign_out_notice.clone();
        let content: gpui::AnyElement = match phase {
            Phase::Starting => setup::splash("Đang khởi động Olaz…", cx),
            Phase::Preparing(s) => {
                let e = engine.clone();
                setup::runtime_setup(&s, move |_| e.retry_runtime_setup(), cx)
            }
            Phase::Login(login) => {
                let e = engine.clone();
                let view = setup::login_view(&login, notice.as_deref(), setup::LoginActions { refresh: Box::new(move |_| e.refresh_qr()) }, cx);
                setup::setup_card(SetupStage::SignIn, false, view, window, cx)
            }
            Phase::SigningIn if setup_pending => {
                let view = setup::signing_in_view(cx);
                setup::setup_card(SetupStage::SignIn, false, view, window, cx)
            }
            Phase::SigningIn => setup::splash("Đang đăng nhập và đồng bộ dữ liệu…", cx),
            Phase::Ready if setup_pending => {
                let profile = engine.state.borrow().profile.clone();
                let me = cx.entity().downgrade();
                let me2 = me.clone();
                let view = setup::welcome_view(
                    profile.as_ref().map(|p| (p.display_name.as_str(), p.avatar.as_deref())),
                    self.sync_skipped,
                    move |window, cx| {
                        let _ = me.update(cx, |this, cx| this.finish_setup(true, window, cx));
                    },
                    move |window, cx| {
                        let _ = me2.update(cx, |this, cx| this.finish_setup(false, window, cx));
                    },
                    cx,
                );
                setup::setup_card(SetupStage::Welcome, self.sync_skipped, view, window, cx)
            }
            Phase::Ready => self.shell(window, cx).into_any_element(),
        };
        let full_bleed = !(engine.phase() == Phase::Ready && !setup_pending);
        let message_confirm = engine.state.borrow().message_confirm.clone();
        let confirm = engine
            .composer_confirm()
            .map(|c| crate::widgets::confirm_alert(&c, &theme))
            .or_else(|| message_confirm.map(|c| crate::widgets::message_confirm_alert(&c, &theme)));
        chrome::frame(
            div()
                .size_full()
                .relative()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| crate::app::engine(cx).note_user_activity())
                .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| crate::app::engine(cx).note_user_activity())
                .on_key_down(|_, _, cx| crate::app::engine(cx).note_user_activity())
                .font_family(crate::theme::FONT)
                .text_color(theme.label())
                .child(content)
                .when(full_bleed, |el| el.child(chrome::titlebar_overlay(36.0, window, cx)))
                .children(confirm)
                .children(crate::layout_dump::input_shield()),
            window,
            cx,
        )
    }
}

#[allow(dead_code)]
fn unused(_: Arc<()>) {}
