//! Splash + runtime setup, first-run setup card (QR sign-in → welcome).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{
    AnyElement, App, FontWeight, Image, ImageFormat, IntoElement, ObjectFit, ParentElement, SharedString,
    Styled, StyledImage, Window, div, img, prelude::FluentBuilder, px,
};
use olaz_engine::{LoginState, LoginStep, RuntimeSetup};
use olaz_runtime::Step;

use crate::format;
use crate::icon::icon;
use crate::theme::{FONT, ROUNDED, Theme, with_alpha};
use crate::widgets::{ButtonStyle, avatar, button_sized, spinner};

// MARK: brand background

/// Three blurred accent circles, pre-rendered at quarter scale (GPUI has no element blur).
pub fn brand_background(cx: &App) -> AnyElement {
    let theme = Theme::get(cx);
    let key = (format!("{:?}", theme.accent.to_rgb()), theme.dark);
    static CACHE: OnceLock<Mutex<HashMap<(String, bool), Arc<Image>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let image = cache.lock().unwrap().get(&key).cloned();
    let image = image.unwrap_or_else(|| {
        let img = render_brand(theme.accent.to_rgb(), theme.window_bg().to_rgb());
        cache.lock().unwrap().insert(key, img.clone());
        img
    });
    // Rounded like the window frame: GPUI clips children to rectangles, so square corners would show outside it.
    let r = px(crate::chrome::WINDOW_RADIUS - 1.0);
    div()
        .absolute()
        .inset_0()
        .rounded(r)
        .bg(theme.window_bg())
        .child(img(image).size_full().object_fit(ObjectFit::Cover).rounded(r))
        .into_any_element()
}

fn render_brand(accent: gpui::Rgba, bg: gpui::Rgba) -> Arc<Image> {
    const SCALE: f32 = 4.0;
    let (w, h) = (1920.0 / SCALE, 1200.0 / SCALE);
    let mut buf = image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba([(bg.r * 255.0) as u8, (bg.g * 255.0) as u8, (bg.b * 255.0) as u8, 255]));
    // (alpha, blur, size, dx, dy) from the window center
    let circles = [(0.18f32, 100.0f32, 620.0f32, -300.0f32, -200.0f32), (0.10, 120.0, 520.0, 320.0, 240.0), (0.08, 110.0, 440.0, 140.0, -280.0)];
    let mut acc = vec![0f32; (w * h) as usize];
    let mut layers = Vec::new();
    for (alpha, blur, size, dx, dy) in circles {
        let mut layer = image::ImageBuffer::<image::Luma<f32>, Vec<f32>>::new(w as u32, h as u32);
        let (cx, cy, r) = ((w / 2.0) + dx / SCALE, (h / 2.0) + dy / SCALE, size / 2.0 / SCALE);
        for (x, y, p) in layer.enumerate_pixels_mut() {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            p.0[0] = if d <= r { alpha } else { 0.0 };
        }
        let blurred = image::imageops::blur(&layer, blur / 2.0 / SCALE);
        layers.push(blurred);
    }
    for layer in &layers {
        for (i, p) in layer.pixels().enumerate() {
            acc[i] = 1.0 - (1.0 - acc[i]) * (1.0 - p.0[0]);
        }
    }
    for (i, p) in buf.pixels_mut().enumerate() {
        let a = acc[i];
        p.0[0] = ((bg.r * (1.0 - a) + accent.r * a) * 255.0) as u8;
        p.0[1] = ((bg.g * (1.0 - a) + accent.g * a) * 255.0) as u8;
        p.0[2] = ((bg.b * (1.0 - a) + accent.b * a) * 255.0) as u8;
    }
    let mut png = Vec::new();
    let _ = image::DynamicImage::ImageRgba8(buf).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    Arc::new(Image::from_bytes(ImageFormat::Png, png))
}

pub fn app_icon(size: f32) -> impl IntoElement {
    img("AppIcon-c.png").size(px(size)).flex_none().object_fit(ObjectFit::Contain)
}

// MARK: splash

pub fn splash(message: &str, cx: &App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    div()
        .size_full()
        .relative()
        .child(brand_background(cx))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(16.0))
                .child(app_icon(84.0))
                .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child("Olaz"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(spinner(14.0, theme.secondary()))
                        .child(div().text_size(px(13.0)).text_color(theme.secondary()).child(message.to_string())),
                ),
        )
        .into_any_element()
}

// MARK: runtime setup

pub const RUNTIME_EXPLANATION: &str = "Lần đầu mở, Olaz tải phần lõi chính thức của Zalo PC từ máy chủ của VNG (khoảng 340 MB) và kiểm tra chữ ký số của VNG. Bạn không cần cài Zalo PC.";

pub fn runtime_status(step: &Step) -> String {
    match step {
        Step::Looking => "Đang chuẩn bị…".into(),
        Step::Downloading { received, total: Some(t) } => format!("Đang tải Zalo PC… {} / {}", format::bytes(*received), format::bytes(*t)),
        Step::Downloading { received, total: None } => format!("Đang tải Zalo PC… {}", format::bytes(*received)),
        Step::Verifying => "Đang kiểm tra chữ ký của VNG…".into(),
        Step::Installing => "Đang cài đặt…".into(),
    }
}

pub fn runtime_setup(setup: &RuntimeSetup, on_retry: impl Fn(&mut App) + 'static, cx: &App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let progress: AnyElement = match &setup.failure {
        Some(failure) => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .items_center()
                    .text_size(px(13.0))
                    .text_color(theme.red())
                    .child(icon("exclamationmark.triangle.fill", 13.0, theme.red()))
                    .child(div().text_center().child(failure.clone())),
            )
            .child(button_sized("retry-runtime", "Thử lại", ButtonStyle::Prominent, false, &theme, move |_, _, cx| on_retry(cx)))
            .into_any_element(),
        None => {
            let status = runtime_status(&setup.step);
            let fraction = match setup.step {
                Step::Downloading { received, total: Some(t) } if t > 0 => Some(received as f32 / t as f32),
                _ => None,
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.0))
                .text_color(theme.secondary())
                .map(|el| match fraction {
                    Some(f) => el.child(
                        div().w(px(300.0)).h(px(4.0)).rounded_full().bg(theme.quaternary()).child(div().h_full().rounded_full().bg(theme.accent).w(px(300.0 * f.clamp(0.0, 1.0)))),
                    ),
                    None => el.child(spinner(14.0, theme.secondary())),
                })
                .child(status)
                .into_any_element()
        }
    };
    div()
        .size_full()
        .relative()
        .child(brand_background(cx))
        .child(
            div().absolute().inset_0().flex().items_center().justify_center().child(
                div()
                    .max_w(px(420.0))
                    .p(px(32.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(16.0))
                    .child(app_icon(84.0))
                    .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child("Chuẩn bị Olaz"))
                    .child(div().text_center().text_size(px(13.0)).text_color(theme.secondary()).child(RUNTIME_EXPLANATION))
                    .child(div().pt(px(6.0)).child(progress)),
            ),
        )
        .into_any_element()
}

// MARK: setup card

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SetupStage {
    SignIn,
    Sync,
    Welcome,
}

#[derive(Clone, Copy, PartialEq)]
enum DotState {
    Done,
    Current,
    Upcoming,
    Skipped,
}

fn stepper(stage: SetupStage, sync_skipped: bool, theme: &Theme) -> impl IntoElement {
    let items = [
        ("Đăng nhập", "Quét mã QR bằng Zalo".to_string()),
        ("Đồng bộ tin nhắn", if sync_skipped { "Đã để sau".into() } else { "Từ điện thoại sang máy tính".to_string() }),
        ("Làm quen với Olaz", "Hướng dẫn nhanh, 1 phút".to_string()),
    ];
    let index = match stage {
        SetupStage::SignIn => 0,
        SetupStage::Sync => 1,
        SetupStage::Welcome => 2,
    };
    let mut col = div().relative().flex().flex_col();
    // connector bar behind rows 1–2
    col = col.child(
        div()
            .absolute()
            .left(px(11.0))
            .top(px(28.0))
            .bottom(px(28.0))
            .w(px(2.0))
            .flex()
            .flex_col()
            .child(div().flex_1().bg(if index > 0 { theme.accent } else { theme.quaternary() }))
            .child(div().flex_1().bg(if index > 1 { theme.accent } else { theme.quaternary() })),
    );
    for (i, (title, subtitle)) in items.iter().enumerate() {
        let state = if i < index {
            if i == 1 && sync_skipped { DotState::Skipped } else { DotState::Done }
        } else if i == index {
            DotState::Current
        } else {
            DotState::Upcoming
        };
        let dot = match state {
            DotState::Done => div().size(px(24.0)).rounded_full().bg(theme.accent).flex().items_center().justify_center().child(icon("checkmark", 10.5, theme.white())),
            DotState::Skipped => div().size(px(24.0)).rounded_full().bg(theme.quaternary()).flex().items_center().justify_center().child(icon("minus", 10.5, theme.secondary())),
            DotState::Current => div().size(px(24.0)).rounded_full().bg(theme.accent).border_2().border_color(with_alpha(theme.accent, 0.22)).flex().items_center().justify_center().child(
                div().font_family(ROUNDED).text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(theme.white()).child((i + 1).to_string()),
            ),
            DotState::Upcoming => div().size(px(24.0)).rounded_full().bg(theme.quaternary()).flex().items_center().justify_center().child(
                div().font_family(ROUNDED).text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(theme.secondary()).child((i + 1).to_string()),
            ),
        };
        let dim = matches!(state, DotState::Upcoming | DotState::Skipped);
        col = col.child(
            div()
                .flex()
                .items_start()
                .gap(px(12.0))
                .min_h(px(24.0))
                .when(i < 2, |el| el.pb(px(22.0)))
                .child(dot.flex_none())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(if dim { FontWeight::MEDIUM } else { FontWeight::SEMIBOLD })
                                .text_color(if dim { theme.secondary() } else { theme.label() })
                                .child(*title),
                        )
                        .child(div().text_size(px(11.5)).text_color(theme.secondary()).child(subtitle.clone())),
                ),
        );
    }
    col
}

pub fn setup_card(stage: SetupStage, sync_skipped: bool, content: AnyElement, window: &Window, cx: &App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let size = window.viewport_size();
    let w = (f32::from(size.width) - 96.0).clamp(760.0, 880.0);
    let h = (f32::from(size.height) - 110.0).clamp(500.0, 600.0);
    let sidebar = div()
        .w(px(276.0))
        .flex_none()
        .h_full()
        .px(px(26.0))
        .pt(px(30.0))
        .pb(px(22.0))
        .flex()
        .flex_col()
        .child(app_icon(64.0))
        .child(div().pt(px(12.0)).text_size(px(21.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child("Chào mừng đến Olaz"))
        .child(div().pt(px(26.0)).child(stepper(stage, sync_skipped, &theme)))
        .child(div().flex_1().min_h(px(12.0)))
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(7.0))
                .child(div().pt(px(1.0)).child(icon("lock.shield.fill", 12.0, theme.green())))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(11.0))
                        .text_color(theme.secondary())
                        .child("Olaz là ứng dụng độc lập, không liên kết với VNG hay Zalo. Ứng dụng chạy phần lõi chính thức của Zalo PC, tải trực tiếp từ VNG; tin nhắn đi thẳng tới máy chủ Zalo."),
                ),
        );
    let content = div()
        .flex_1()
        .h_full()
        .mt(px(8.0))
        .mr(px(8.0))
        .mb(px(8.0))
        .px(px(36.0))
        .py(px(22.0))
        .rounded(px(22.0))
        .bg(theme.content_bg())
        .border_1()
        .border_color(theme.separator())
        .flex()
        .items_center()
        .justify_center()
        .child(content);
    div()
        .size_full()
        .relative()
        .child(brand_background(cx))
        .child(
            div().absolute().inset_0().flex().items_center().justify_center().child(
                div()
                    .w(px(w))
                    .h(px(h))
                    .flex()
                    .rounded(px(30.0))
                    .bg(theme.glass())
                    .border_1()
                    .border_color(theme.glass_border())
                    .shadow_lg()
                    .child(sidebar)
                    .child(content),
            ),
        )
        .into_any_element()
}

// MARK: login

pub fn login_heading(step: &LoginStep) -> &'static str {
    match step {
        LoginStep::WaitingConfirm { .. } => "Xác nhận trên điện thoại",
        _ => "Đăng nhập bằng Zalo",
    }
}

pub fn login_subtitle(login: &LoginState) -> String {
    match &login.step {
        LoginStep::Generating => "Đang tạo mã QR…".into(),
        LoginStep::WaitingScan if login.is_second_qr => {
            "Đã quét lần 1. Tài khoản của bạn cần quét thêm mã mới dưới đây để xác nhận thiết bị.".into()
        }
        LoginStep::WaitingScan => "Mở Zalo trên điện thoại, chạm biểu tượng quét mã QR rồi quét mã dưới đây.".into(),
        LoginStep::WaitingConfirm { .. } => "Chọn Đăng nhập trên điện thoại của bạn để tiếp tục.".into(),
        LoginStep::Expired => "Mã QR đã hết hạn.".into(),
        LoginStep::Failed(m) => m.clone(),
    }
}

/// QR PNG bytes → GPUI image, cached by content.
pub fn qr_image(png: &[u8]) -> Arc<Image> {
    static LAST: OnceLock<Mutex<Option<(u64, Arc<Image>)>>> = OnceLock::new();
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        png.hash(&mut h);
        h.finish()
    };
    let cell = LAST.get_or_init(|| Mutex::new(None));
    let mut last = cell.lock().unwrap();
    if let Some((k, img)) = last.as_ref() {
        if *k == hash {
            return img.clone();
        }
    }
    let img = Arc::new(Image::from_bytes(ImageFormat::Png, png.to_vec()));
    *last = Some((hash, img.clone()));
    img
}

pub struct LoginActions {
    pub refresh: Box<dyn Fn(&mut App)>,
}

pub fn login_view(login: &LoginState, notice: Option<&str>, actions: LoginActions, cx: &mut App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let qr_side = if notice.is_some() { 168.0 } else { 216.0 };
    let refresh = Arc::new(actions.refresh);
    let qr_area: AnyElement = match &login.step {
        LoginStep::WaitingConfirm { name, avatar: url } => div()
            .size(px(qr_side))
            .p(px(16.0))
            .rounded(px(24.0))
            .bg(crate::theme::fade(theme.quaternary(), 0.5))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .child(avatar(url.as_deref(), name, 84.0, cx))
            .child(div().text_center().text_size(px(17.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.label()).line_clamp(2).child(name.clone()))
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .items_center()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.accent)
                    .child(icon("checkmark.circle.fill", 12.5, theme.accent))
                    .child("Đã quét mã"),
            )
            .into_any_element(),
        LoginStep::Failed(_) => div()
            .size(px(qr_side))
            .rounded(px(24.0))
            .bg(crate::theme::fade(theme.quaternary(), 0.5))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .child(icon("exclamationmark.triangle.fill", 34.0, theme.orange()))
            .child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child("Chưa lấy được mã QR"))
            .into_any_element(),
        step => {
            let expired = *step == LoginStep::Expired;
            let inner: AnyElement = match &login.qr_png {
                Some(png) => img(qr_image(png)).size_full().object_fit(ObjectFit::Contain).into_any_element(),
                None => div().size_full().flex().items_center().justify_center().child(spinner(22.0, crate::theme::hex(0x8E8E93))).into_any_element(),
            };
            let r = refresh.clone();
            div()
                .size(px(qr_side))
                .relative()
                .rounded(px(24.0))
                .bg(crate::theme::hex(0xFFFFFF))
                .shadow_md()
                .p(px(16.0))
                .child(div().size_full().when(expired, |el| el.opacity(0.4)).child(inner))
                .when(expired, |el| {
                    el.child(
                        div().absolute().inset_0().flex().items_center().justify_center().child(
                            button_sized("qr-refresh", "Lấy mã mới", ButtonStyle::Prominent, true, &theme, move |_, _, cx| (r)(cx))
                                .child(icon("arrow.clockwise", 13.0, theme.white())),
                        ),
                    )
                })
                .into_any_element()
        }
    };
    let r = refresh.clone();
    let footer: AnyElement = match &login.step {
        LoginStep::WaitingConfirm { .. } => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(spinner(12.0, theme.secondary()))
                    .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Đang chờ xác nhận trên điện thoại…")),
            )
            .child(button_sized("other-qr", "Dùng mã QR khác", ButtonStyle::Link, false, &theme, move |_, _, cx| (r)(cx)).text_size(px(12.5)))
            .into_any_element(),
        LoginStep::Failed(_) => button_sized("retry-qr", "Thử lại", ButtonStyle::Prominent, true, &theme, move |_, _, cx| (r)(cx)).into_any_element(),
        _ => {
            let row = |n: u32, text: &str| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .size(px(20.0))
                            .rounded_full()
                            .bg(theme.accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family(ROUNDED)
                            .text_size(px(11.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.white())
                            .child(n.to_string()),
                    )
                    .child(div().text_size(px(12.5)).text_color(theme.secondary()).child(text.to_string()))
            };
            div()
                .flex()
                .flex_col()
                .gap(px(7.0))
                .child(row(1, "Mở Zalo trên điện thoại"))
                .child(row(2, "Nhấn biểu tượng QR ở thanh tìm kiếm"))
                .child(row(3, "Quét mã và xác nhận đăng nhập"))
                .into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.0))
        .font_family(FONT)
        .when_some(notice, |el, notice| {
            el.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .p(px(12.0))
                    .rounded(px(12.0))
                    .bg(with_alpha(theme.orange(), 0.12))
                    .text_size(px(12.0))
                    .text_color(theme.orange())
                    .child(icon("exclamationmark.triangle.fill", 12.0, theme.orange()))
                    .child(div().flex_1().child(notice.to_string())),
            )
        })
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.0))
                .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child(login_heading(&login.step)))
                .child(div().max_w(px(380.0)).text_center().text_size(px(13.0)).text_color(theme.secondary()).child(login_subtitle(login))),
        )
        .child(qr_area)
        .child(footer)
        .into_any_element()
}

pub fn signing_in_view(cx: &App) -> AnyElement {
    let theme = Theme::get(cx).clone();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.0))
        .child(spinner(28.0, theme.secondary()))
        .child(div().text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child("Đang đăng nhập…"))
        .child(div().text_size(px(13.0)).text_color(theme.secondary()).child("Đang tải dữ liệu từ Zalo. Việc này có thể mất một lúc."))
        .into_any_element()
}

/// Welcome step (the tour starts from the shell).
pub fn welcome_view(
    profile: Option<(&str, Option<&str>)>,
    sync_skipped: bool,
    on_tour: impl Fn(&mut Window, &mut App) + 'static,
    on_skip: impl Fn(&mut Window, &mut App) + 'static,
    cx: &mut App,
) -> AnyElement {
    let theme = Theme::get(cx).clone();
    let lead = format!(
        "{}Xem nhanh 5 điều giúp bạn dùng Olaz thuận tay nhất, chưa tới một phút.",
        if sync_skipped { "Bạn có thể đồng bộ tin nhắn cũ bất cứ lúc nào trong Cài đặt. " } else { "" }
    );
    let picture: AnyElement = match profile {
        Some((name, url)) => avatar(url, name, 64.0, cx),
        None => app_icon(64.0).into_any_element(),
    };
    let tour = crate::tour::STEPS.map(|s| (s.symbol, s.title));
    let mut list = div().flex().flex_col().px(px(14.0)).py(px(6.0)).max_w(px(340.0)).w_full().rounded(px(14.0)).bg(crate::theme::fade(theme.label(), 0.05));
    for (sym, title) in tour {
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .min_h(px(28.0))
                .child(div().w(px(26.0)).h(px(22.0)).rounded(px(7.0)).bg(with_alpha(theme.accent, 0.13)).flex().items_center().justify_center().child(icon(sym, 12.0, theme.accent)))
                .child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(theme.label()).child(title)),
        );
    }
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div().relative().child(picture).child(
                div()
                    .absolute()
                    .right(px(-4.0))
                    .bottom(px(-4.0))
                    .size(px(22.0))
                    .rounded_full()
                    .bg(theme.white())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("checkmark.circle.fill", 18.0, theme.green())),
            ),
        )
        .child(div().pt(px(12.0)).text_size(px(22.0)).font_weight(FontWeight::BOLD).text_color(theme.label()).child("Olaz đã sẵn sàng"))
        .child(div().pt(px(6.0)).max_w(px(400.0)).text_center().text_size(px(13.0)).text_color(theme.secondary()).child(lead))
        .child(div().pt(px(14.0)).child(list))
        .child(
            div()
                .pt(px(16.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .child(button_sized("start-tour", "Bắt đầu hướng dẫn", ButtonStyle::Prominent, true, &theme, move |_, window, cx| on_tour(window, cx)))
                .child(button_sized("skip-tour", "Bỏ qua, vào Olaz", ButtonStyle::Link, false, &theme, move |_, window, cx| on_skip(window, cx)).text_size(px(12.5))),
        )
        .into_any_element()
}

pub fn element_id(s: &str) -> SharedString {
    SharedString::from(s.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_texts() {
        assert_eq!(runtime_status(&Step::Downloading { received: 12_300_000, total: Some(337_900_000) }), "Đang tải Zalo PC… 12,3 MB / 337,9 MB");
        assert_eq!(runtime_status(&Step::Verifying), "Đang kiểm tra chữ ký của VNG…");
        let l = LoginState { step: LoginStep::WaitingScan, qr_png: None, is_second_qr: true };
        assert!(login_subtitle(&l).starts_with("Đã quét lần 1."));
        assert_eq!(login_heading(&LoginStep::WaitingConfirm { name: "A".into(), avatar: None }), "Xác nhận trên điện thoại");
    }
}
