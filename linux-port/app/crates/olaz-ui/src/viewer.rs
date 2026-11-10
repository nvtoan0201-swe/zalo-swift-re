//! Media viewer and media playback: photos open in a viewer window; videos, voice and files are fetched
//! through MediaLoader and handed to the desktop (portal OpenURI / xdg-open) until the GStreamer player lands.

use std::sync::Arc;

use gpui::{
    App, AppContext, Bounds, Context, Image, IntoElement, ObjectFit, ParentElement, Render, Styled, StyledImage, TitlebarOptions, Window,
    WindowBounds, WindowDecorations, WindowOptions, div, img, px, size,
};

use crate::widgets::{ImageState, Images, remote_image, spinner};

pub struct PhotoViewer {
    url: String,
}

impl Render for PhotoViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = remote_image(&self.url, cx);
        let content = match state {
            ImageState::Ready(image) => img(image).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            ImageState::Loading => div().size_full().flex().items_center().justify_center().child(spinner(22.0, gpui::white())).into_any_element(),
            ImageState::Failed => div().size_full().flex().items_center().justify_center().text_color(gpui::white()).child("Không tải được ảnh").into_any_element(),
        };
        crate::chrome::frame(
            div()
                .size_full()
                .relative()
                .bg(gpui::black())
                .child(div().absolute().inset_0().pt(px(36.0)).child(content))
                .child(crate::chrome::titlebar_overlay(36.0, window, cx)),
            window,
            cx,
        )
    }
}

pub fn open_photo(url: &str, _window: &mut Window, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(900.0), px(700.0)), cx);
    let url = url.to_string();
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Ảnh".into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some(olaz_core::APP_ID.into()),
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|_| PhotoViewer { url });
            cx.new(|cx| crate::chrome::root(view, window, cx))
        },
    );
}

fn fetch_then_open(url: &str, name: Option<String>, cx: &mut App) {
    if !cx.has_global::<Images>() {
        return;
    }
    let loader = cx.global::<Images>().loader.clone();
    let fut = loader.local_file(url, |_| {});
    // Named copies live in Olaz's cache, never next to the source (a `file://` source may be anywhere).
    let named_dir = crate::app::engine(cx).paths.cache().join("named");
    cx.spawn(async move |_| {
        if let Ok(path) = fut.await {
            let target = match name {
                // Keep the official file name for the desktop handler (copy next to the cached file).
                Some(n) if !n.is_empty() => {
                    let safe: String = n.chars().map(|c| if c == '/' || c == '\0' { '_' } else { c }).collect();
                    let dir = named_dir.clone();
                    let _ = std::fs::create_dir_all(&dir);
                    let named = dir.join(safe);
                    if std::fs::hard_link(&path, &named).is_err() {
                        let _ = std::fs::copy(&path, &named);
                    }
                    named
                }
                _ => path,
            };
            if may_execute(&target) {
                let dir = target.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
            } else {
                let _ = std::process::Command::new("xdg-open").arg(target).spawn();
            }
        }
    })
    .detach();
}

/// The user's Downloads folder (`xdg-user-dir DOWNLOAD`, else ~/Downloads, else home) — the save dialog's start.
pub fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| "/".into());
    let from_tool = std::process::Command::new("xdg-user-dir")
        .arg("DOWNLOAD")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| std::path::PathBuf::from(s.trim()))
        .filter(|p| p.is_dir() && p != &home);
    from_tool.or_else(|| Some(home.join("Downloads")).filter(|p| p.is_dir())).unwrap_or(home)
}

/// A name safe as a file name (no path separators or NULs), never empty.
pub fn safe_file_name(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c }).collect();
    if s.is_empty() || s == "." || s == ".." { "tep-zalo".into() } else { s }
}

/// "Lưu về máy…": downloads the file (into the media cache) while the system save dialog (xdg-desktop-portal) asks
/// where to put it, then copies it there. The outcome is shown as a toast.
pub fn save_file_as(url: &str, name: &str, cx: &mut App) {
    use olaz_engine::conversation_actions::ToastKind;
    if !cx.has_global::<Images>() {
        return;
    }
    let download = cx.global::<Images>().loader.clone().local_file(url, |_| {});
    let suggested = safe_file_name(name);
    let chosen = cx.prompt_for_new_path(&downloads_dir(), Some(&suggested));
    olaz_core::log::write("save-as: asking for a destination");
    cx.spawn(async move |cx| {
        let answer = chosen.await;
        olaz_core::log::write(format!("save-as: dialog answered {:?}", answer.as_ref().map(|r| r.as_ref().map(|p| p.is_some()).map_err(|e| e.to_string()))));
        let target = match answer {
            Ok(Ok(Some(path))) => path,
            Ok(Ok(None)) => return, // cancelled
            _ => {
                let _ = cx.update(|cx| crate::app::engine(cx).show_conv_toast(ToastKind::Failure, "Không mở được hộp thoại lưu tệp."));
                return;
            }
        };
        let result = match download.await {
            Ok(cached) => std::fs::copy(&cached, &target).map(|_| ()).map_err(|e| e.to_string()),
            Err(e) => Err(format!("{e:?}")),
        };
        let _ = cx.update(|cx| {
            let e = crate::app::engine(cx);
            match result {
                Ok(()) => {
                    let shown = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    e.show_conv_toast(ToastKind::Success, &format!("Đã lưu {shown}"));
                }
                Err(err) => {
                    olaz_core::log::write(format!("save file failed: {err}"));
                    e.show_conv_toast(ToastKind::Failure, "Không lưu được tệp. Hãy thử lại.");
                }
            }
        });
    })
    .detach();
}

/// Files that may run code (scripts, installers, executables) are revealed in the file manager instead of opened.
pub fn may_execute(path: &std::path::Path) -> bool {
    const EXT: [&str; 19] = ["sh", "bash", "zsh", "csh", "ksh", "run", "bin", "appimage", "desktop", "jar", "py", "pl", "rb", "exe", "msi", "deb", "rpm", "flatpakref", "snap"];
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if EXT.contains(&ext.as_str()) {
        return true;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Downloads the media, then plays it in Olaz's own player window (GStreamer); without GStreamer it goes to the desktop handler.
fn fetch_then_play(url: &str, title: &str, audio_only: bool, cx: &mut App) {
    if !olaz_media::player::available() {
        return fetch_then_open(url, None, cx);
    }
    if !cx.has_global::<Images>() {
        return;
    }
    let fut = cx.global::<Images>().loader.clone().local_file(url, |_| {});
    let title = title.to_string();
    cx.spawn(async move |cx| {
        if let Ok(path) = fut.await {
            let _ = cx.update(|cx| crate::player::open(path, title, audio_only, cx));
        }
    })
    .detach();
}

pub fn open_video(url: &str, cx: &mut App) {
    fetch_then_play(url, "Video", false, cx);
}

pub fn play_audio(url: &str, cx: &mut App) {
    fetch_then_play(url, "Tin nhắn thoại", true, cx);
}

pub fn open_file(url: &str, name: &str, cx: &mut App) {
    fetch_then_open(url, Some(name.to_string()), cx);
}

#[allow(dead_code)]
fn unused(_: Arc<Image>) {}

#[cfg(test)]
mod tests {
    /// The whole "Lưu về máy…" flow on GPUI's test platform: the save dialog is answered with a path, the file is
    /// fetched through the media loader and copied there, and a success toast is shown.
    #[test]
    fn save_file_as_copies_to_the_chosen_path() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("bao-cao.pdf"), b"%PDF-1.4 test").unwrap();
        let target = dir.path().join("chosen").join("Báo cáo.pdf");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let mut app = gpui::TestApp::new();
        let engine = crate::switcher::tests::test_engine(&dir.path().join("home"));
        app.update(|cx| {
            let loader = olaz_media::MediaLoader::new(&dir.path().join("cache"), vec![src.clone()], "test");
            cx.set_global(crate::widgets::Images::new(loader));
            cx.set_global(crate::app::EngineGlobal(engine.clone()));
            super::save_file_as(&format!("file://{}", src.join("bao-cao.pdf").display()), "Báo cáo.pdf", cx);
        });
        app.run_until_parked();
        assert!(app.did_prompt_for_new_path(), "the save dialog was asked for");
        let chosen = target.clone();
        app.simulate_new_path_selection(move |start| {
            assert!(start.is_dir(), "dialog starts in a real folder");
            Some(chosen)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !target.exists() && std::time::Instant::now() < deadline {
            app.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.run_until_parked();
        assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-1.4 test");
        let toast = engine.state.borrow().conversation_ui.toast.clone().map(|t| t.text);
        assert_eq!(toast.as_deref(), Some("Đã lưu Báo cáo.pdf"));
    }

    #[test]
    fn file_names() {
        assert_eq!(super::safe_file_name("../a/b.pdf"), ".._a_b.pdf");
        assert_eq!(super::safe_file_name("  "), "tep-zalo");
        assert_eq!(super::safe_file_name("Báo cáo.xlsx"), "Báo cáo.xlsx");
        assert!(super::downloads_dir().is_dir());
    }

    #[test]
    fn executables() {
        assert!(super::may_execute(std::path::Path::new("/x/setup.AppImage")));
        assert!(super::may_execute(std::path::Path::new("/x/a.desktop")));
        assert!(!super::may_execute(std::path::Path::new("/nonexistent/a.pdf")));
    }
}
