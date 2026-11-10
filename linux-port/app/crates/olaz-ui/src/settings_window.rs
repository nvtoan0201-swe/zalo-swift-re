//! Settings window (port of SettingsWindow.swift): Chung, Giao diện, Thông báo, Quyền riêng tư (hidden-chat PIN), Sticker, Tài khoản.

use gpui::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, prelude::FluentBuilder, px,
    size,
};
use olaz_engine::conversation_actions::{FlowKind, HiddenChatFlow};

use crate::app::engine;
use crate::conversation_actions::{HiddenChatSheet, SheetClosed};
use crate::icon::icon;
use crate::settings::AppPrefs;
use crate::theme::{ACCENTS, Theme, hex, with_alpha};
use crate::widgets::{ButtonStyle, avatar, button};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Appearance,
    Notifications,
    Privacy,
    Stickers,
    Account,
}

const TABS: [(Tab, &str, &str); 6] = [
    (Tab::General, "gearshape", "Chung"),
    (Tab::Appearance, "paintpalette", "Giao diện"),
    (Tab::Notifications, "bell.badge", "Thông báo"),
    (Tab::Privacy, "lock", "Quyền riêng tư"),
    (Tab::Stickers, "face.smiling", "Sticker"),
    (Tab::Account, "person.crop.circle", "Tài khoản"),
];

pub struct SettingsView {
    tab: Tab,
    pin_sheet: Option<Entity<HiddenChatSheet>>,
    sticker_busy: bool,
    sticker_failed: bool,
    /// Engine ▸ "Chuyển sang SQLite…" asks before logging out.
    confirm_sqlite: bool,
    /// `--ui-render` panel scenes: only the tab's form, without tabs and window chrome.
    form_only: bool,
}

impl SettingsView {
    pub fn new(tab: Tab, form_only: bool) -> Self {
        SettingsView { tab, pin_sheet: None, sticker_busy: false, sticker_failed: false, confirm_sqlite: false, form_only }
    }
}

fn set(cx: &mut App, f: impl FnOnce(&mut crate::settings::Prefs)) {
    let prefs = cx.global_mut::<AppPrefs>();
    f(&mut prefs.prefs);
    prefs.save();
    crate::app::apply_theme(cx);
    cx.refresh_windows();
}

fn section(title: &str, theme: &Theme) -> gpui::Div {
    crate::widgets::static_text(div().pt(px(18.0)).pb(px(6.0)).text_size(px(12.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()), title)
}

fn group(theme: &Theme) -> gpui::Div {
    div().rounded(px(12.0)).bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().flex_col()
}

fn row(title: &str, subtitle: Option<&str>, trailing: AnyElement, theme: &Theme) -> gpui::Div {
    div()
        .px(px(14.0))
        .py(px(9.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(px(13.0)).child(title.to_string()))
                .when_some(subtitle.map(String::from), |el, s| el.child(div().text_size(px(11.5)).text_color(theme.secondary()).child(s))),
        )
        .child(trailing)
}

fn switch(id: String, on: bool, enabled: bool, theme: &Theme, f: impl Fn(bool, &mut App) + 'static) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .w(px(38.0))
        .h(px(22.0))
        .flex_none()
        .rounded_full()
        .p(px(2.0))
        .flex()
        .when(!enabled, |el| el.opacity(0.45))
        .when(on, |el| el.justify_end().bg(theme.accent))
        .when(!on, |el| el.bg(theme.quaternary()))
        .child(div().size(px(18.0)).rounded_full().bg(gpui::white()))
        .on_click(move |_, _, cx| {
            if enabled {
                f(!on, cx)
            }
        })
        .into_any_element()
}

impl SettingsView {
    fn general(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let prefs = cx.global::<AppPrefs>().prefs.clone();
        let mut modes = div().flex().gap(px(6.0)).p(px(10.0));
        for (key, label) in [("system", "Tự động"), ("light", "Sáng"), ("dark", "Tối")] {
            let sel = prefs.appearance == key;
            modes = modes.child(
                div()
                    .id(SharedString::from(format!("ap-{key}")))
                    .flex_1()
                    .h(px(28.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                    .when(!sel, |el| el.bg(theme.quaternary()))
                    .child(label)
                    .on_click(move |_, _, cx| set(cx, |p| p.appearance = key.into())),
            );
        }
        let client = engine(cx).state.borrow().client_version.clone();
        div()
            .flex()
            .flex_col()
            .child(section("Chế độ giao diện", theme))
            .child(group(theme).child(modes))
            .child(section("Về Olaz", theme))
            .child(
                group(theme)
                    .child(row("Phiên bản", None, div().text_size(px(13.0)).text_color(theme.secondary()).child(env!("CARGO_PKG_VERSION")).into_any_element(), theme))
                    .when_some(client, |el, v| el.child(row("Zalo PC", None, div().text_size(px(13.0)).text_color(theme.secondary()).child(v).into_any_element(), theme))),
            )
            .into_any_element()
    }

    fn appearance(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let prefs = cx.global::<AppPrefs>().prefs.clone();
        let mut accents = div().flex().flex_wrap().gap(px(12.0)).p(px(14.0));
        for (key, title, color) in ACCENTS {
            let selected = prefs.accent == key;
            accents = accents.child(
                div()
                    .id(SharedString::from(format!("ac-{key}")))
                    .size(px(26.0))
                    .rounded_full()
                    .bg(hex(color))
                    .cursor_pointer()
                    .when(selected, |el| el.border_2().border_color(theme.label()))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(title).build(window, cx))
                    .on_click(move |_, _, cx| set(cx, |p| p.accent = key.into())),
            );
        }
        let mut sizes = div().flex().gap(px(6.0)).p(px(10.0));
        for (key, label) in [("small", "Nhỏ"), ("standard", "Vừa"), ("large", "Lớn"), ("extraLarge", "Rất lớn")] {
            let sel = prefs.text_size == key;
            sizes = sizes.child(
                div()
                    .id(SharedString::from(format!("ts-{key}")))
                    .flex_1()
                    .h(px(28.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                    .when(!sel, |el| el.bg(theme.quaternary()))
                    .child(label)
                    .on_click(move |_, _, cx| set(cx, |p| p.text_size = key.into())),
            );
        }
        // Preview: two bubbles at 14 × scale over the chosen wallpaper (or primary@0.07).
        let bubble = |text: &'static str, mine: bool| {
            div()
                .px(px(12.0))
                .py(px(7.0))
                .rounded(px(16.0))
                .text_size(px(14.0 * theme.text_scale))
                .when(mine, |el| el.bg(theme.accent).text_color(theme.white()))
                .when(!mine, |el| el.bg(theme.content_bg()).text_color(theme.label()))
                .child(text)
        };
        let preview = div()
            .relative()
            .mt(px(10.0))
            .rounded(px(12.0))
            .overflow_hidden()
            .border_1()
            .border_color(theme.separator())
            .bg(with_alpha(theme.label(), 0.07))
            .children(crate::wallpaper::fill(&prefs.wallpaper, theme.dark))
            .child(
                div()
                    .relative()
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().flex().pr(px(40.0)).child(bubble("Chiều nay đi cà phê không?", false)))
                    .child(div().flex().justify_end().pl(px(40.0)).child(bubble("Được chứ, 3 giờ nhé!", true))),
            );
        // Wallpaper grid: 5 tiles, aspect 1.3, mini bubbles, 2.5 tint ring on the selected one.
        let mut tiles = div().flex().gap(px(10.0)).py(px(4.0)).px(px(12.0));
        for (key, title) in crate::wallpaper::OPTIONS {
            let selected = prefs.wallpaper == key;
            let incoming_alpha = if key == "night" { 0.35 } else { 0.9 };
            tiles = tiles.child(
                div()
                    .id(SharedString::from(format!("wp-{key}")))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_pointer()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(52.0))
                            .rounded(px(10.0))
                            .overflow_hidden()
                            .bg(with_alpha(theme.label(), 0.07))
                            .border_1()
                            .border_color(theme.separator())
                            .when(selected, |el| el.border_2().border_color(theme.accent))
                            .children(crate::wallpaper::image(key).map(|img| {
                                { use gpui::StyledImage as _; gpui::img(img).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Cover).into_any_element() }
                            }))
                            .child(
                                div()
                                    .relative()
                                    .size_full()
                                    .p(px(8.0))
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .gap(px(4.0))
                                    .child(div().w(px(26.0)).h(px(7.0)).rounded_full().bg(with_alpha(theme.content_bg(), incoming_alpha)))
                                    .child(div().flex().justify_end().child(div().w(px(22.0)).h(px(7.0)).rounded_full().bg(theme.accent))),
                            ),
                    )
                    .child(div().text_size(px(11.0)).text_color(if selected { theme.label() } else { theme.secondary() }).child(title))
                    .on_click(move |_, _, cx| set(cx, |p| p.wallpaper = key.into())),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(section("Màu nhấn", theme))
            .child(group(theme).child(accents))
            .child(section("Cỡ chữ tin nhắn", theme))
            .child(group(theme).child(sizes))
            .child(preview)
            .child(section("Hình nền trò chuyện", theme))
            .child(group(theme).child(tiles))
            .into_any_element()
    }

    fn notifications(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let e = engine(cx);
        let s = e.notification_settings();
        let now = olaz_engine::notifications::now_s();
        let mutes = {
            let st = e.state.borrow();
            if st.conversation_actions.hidden_known { s.active_mutes(now).into_iter().filter(|(id, _)| !st.conversation_actions.is_hidden(id)).collect::<Vec<_>>() } else { vec![] }
        };
        let mut col = div()
            .flex()
            .flex_col()
            .child(section("Thông báo", theme))
            .child(
                group(theme)
                    .child(row("Thông báo tin nhắn mới", Some("Nhận được thông báo mỗi khi có tin nhắn mới"), switch("n-enabled".into(), s.enabled, s.is_loaded, theme, |on, cx| engine(cx).set_notifications_enabled(on)), theme))
                    .child(row("Phát âm thanh khi có tin nhắn & thông báo mới", None, switch("n-sound".into(), s.sound, s.is_loaded && s.enabled, theme, |on, cx| engine(cx).set_notification_sound(on)), theme))
                    .child(row("Hiện nội dung tin nhắn", Some("Tắt để chỉ hiện “Bạn có tin nhắn mới”"), switch("n-preview".into(), s.shows_preview, true, theme, |on, cx| engine(cx).set_message_preview(on)), theme)),
            )
            .child(div().pt(px(6.0)).text_size(px(11.5)).text_color(theme.secondary()).child("Zalo quyết định khi nào báo tin mới (tắt thông báo hội thoại, nhóm hay cá nhân, đang mở ứng dụng…). Chế độ Không làm phiền của máy vẫn được áp dụng."));
        if !mutes.is_empty() {
            let mut g = group(theme);
            for (id, info) in mutes {
                let name = crate::conversation_actions::conversation_name(&id, cx);
                let id2 = id.clone();
                g = g.child(row(
                    &name,
                    Some(&info.description(crate::format::now())),
                    button(SharedString::from(format!("unmute-{id}")), "Bật lại", ButtonStyle::Glass, false, theme, move |_, _, cx| engine(cx).unmute_conversation(&id2)).into_any_element(),
                    theme,
                ));
            }
            col = col.child(section("Hội thoại đã tắt thông báo", theme)).child(g);
        }
        col.into_any_element()
    }

    fn open_pin(&mut self, kind: FlowKind, has_pin: bool, window: &mut Window, cx: &mut Context<Self>) {
        let sheet = cx.new(|cx| HiddenChatSheet::new(HiddenChatFlow::new(kind, has_pin), true, None, window, cx));
        cx.subscribe(&sheet, |this, _, _: &SheetClosed, cx| {
            this.pin_sheet = None;
            cx.notify();
        })
        .detach();
        self.pin_sheet = Some(sheet);
        cx.notify();
    }

    fn privacy(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let a = engine(cx).state.borrow().conversation_actions.clone();
        let body: AnyElement = if a.loaded && a.hidden_enabled {
            let me = cx.entity().downgrade();
            let open = move |kind: FlowKind, has: bool| {
                let me = me.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    let kind = kind.clone();
                    let _ = me.update(cx, |this, cx| this.open_pin(kind, has, window, cx));
                }
            };
            if a.has_hidden_pin {
                group(theme)
                    .child(row("Đổi mã PIN", Some("Đổi mã PIN cho trò chuyện ẩn"), button("pin-change", "Đổi mã PIN", ButtonStyle::Glass, false, theme, open(FlowKind::ChangePin, true)).into_any_element(), theme))
                    .child(row("Đặt lại mã PIN", Some("Tất cả trò chuyện đang ẩn sẽ bị xóa và không thể khôi phục"), button("pin-reset", "Đặt lại mã PIN", ButtonStyle::Destructive, false, theme, open(FlowKind::ResetPin, true)).into_any_element(), theme))
                    .into_any_element()
            } else {
                group(theme)
                    .child(row("Ẩn trò chuyện", Some("Đặt mã PIN cho các cuộc trò chuyện riêng tư để tránh người khác nhìn thấy trên máy của bạn"), button("pin-set", "Cài đặt mã PIN", ButtonStyle::Glass, false, theme, open(FlowKind::SetPin, false)).into_any_element(), theme))
                    .into_any_element()
            }
        } else {
            group(theme)
                .child(div().p(px(14.0)).text_size(px(13.0)).text_color(theme.secondary()).child(if a.loaded { "Tính năng ẩn trò chuyện chưa được bật cho tài khoản này." } else { "Đang tải thiết lập từ Zalo…" }))
                .into_any_element()
        };
        div().flex().flex_col().child(section("Thiết lập ẩn trò chuyện", theme)).child(body).into_any_element()
    }

    fn stickers(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let e = engine(cx);
        let setting = e.state.borrow().sticker_suggest;
        if setting.is_none() {
            let e2 = e.clone();
            cx.spawn(async move |this, cx| {
                for _ in 0..4 {
                    e2.load_sticker_suggest_if_needed().await;
                    if e2.state.borrow().sticker_suggest.is_some() {
                        break;
                    }
                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let me = cx.entity().downgrade();
        let on = setting.map(|s| s.enabled).unwrap_or(true);
        let enabled = setting.is_some() && !self.sticker_busy;
        div()
            .flex()
            .flex_col()
            .child(section("Gợi ý Sticker", theme))
            .child(group(theme).child(row(
                "Hiện gợi ý Sticker phù hợp với nội dung tin nhắn đang soạn",
                None,
                switch("st-suggest".into(), on, enabled, theme, move |v, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.sticker_busy = true;
                        this.sticker_failed = false;
                        let e = engine(cx);
                        cx.spawn(async move |this, cx| {
                            let now = e.set_sticker_suggest(v).await;
                            let _ = this.update(cx, |this, cx| {
                                this.sticker_busy = false;
                                this.sticker_failed = now.map(|n| n.enabled) != Some(v);
                                cx.notify();
                            });
                        })
                        .detach();
                        cx.notify();
                    });
                }),
                theme,
            )))
            .when(self.sticker_failed, |el| el.child(div().pt(px(6.0)).text_size(px(11.5)).text_color(theme.orange()).child("Không thể đổi cài đặt này lúc này. Hãy thử lại sau.")))
            .into_any_element()
    }

    fn account(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let e = engine(cx);
        let (profile, can, reason, client, convs) = {
            let s = e.state.borrow();
            (s.profile.clone(), s.can_start_transfer, s.transfer_blocked_reason.clone(), s.client_version.clone(), s.conversation_actions.hidden_known)
        };
        let count = if convs { e.visible_conversations().len().to_string() } else { "—".into() };
        let name = profile.as_ref().map(|p| p.display_name.clone()).unwrap_or_else(|| "Chưa đăng nhập".into());
        let sub = profile.as_ref().and_then(|p| p.phone.clone()).filter(|p| !p.is_empty()).unwrap_or_else(|| "Tài khoản Zalo".into());
        let av = avatar(profile.as_ref().and_then(|p| p.avatar.as_deref()), &name, 52.0, cx);
        let signed_in = profile.is_some();
        use olaz_engine::engine::StorageBackend;
        let storage = e.storage_backend();
        let storage_text = match storage {
            Some(StorageBackend::Sqlite) => "SQLite (gốc)",
            Some(StorageBackend::IndexedDb) => "IndexedDB (WebKit)",
            None => "—",
        };
        let me = cx.entity().downgrade();
        let ask = {
            let me = me.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    this.confirm_sqlite = true;
                    cx.notify();
                });
            }
        };
        let cancel = {
            let me = me.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |this, cx| {
                    this.confirm_sqlite = false;
                    cx.notify();
                });
            }
        };
        let confirm = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = me.update(cx, |this, cx| {
                this.confirm_sqlite = false;
                cx.notify();
            });
            engine(cx).switch_to_sqlite();
        };
        div()
            .flex()
            .flex_col()
            .child(div().h(px(12.0)))
            .child(group(theme).child(div().p(px(14.0)).flex().items_center().gap(px(14.0)).child(av).child(div().flex().flex_col().gap(px(2.0)).child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(name)).child(div().text_size(px(12.5)).text_color(theme.secondary()).child(sub)))))
            .child(section("Sao lưu & đồng bộ", theme))
            .child(
                group(theme)
                    .child(row("Đồng bộ tin nhắn từ điện thoại", None, button("acc-transfer", "Đồng bộ ngay", ButtonStyle::Glass, !can, theme, |_, _, cx| engine(cx).start_transfer()).into_any_element(), theme))
                    .when(!can, |el| {
                        let r = reason.clone().unwrap_or_default();
                        el.child(div().px(px(14.0)).pb(px(9.0)).text_size(px(11.5)).text_color(theme.secondary()).child(olaz_engine::transfer::transfer_blocked_text(&r)))
                    }),
            )
            .child(section("Engine", theme))
            .child(
                group(theme)
                    .child(row("Giao thức", None, div().text_size(px(13.0)).text_color(theme.secondary()).child(format!("Zalo PC {} (mã chính thức)", client.unwrap_or_else(|| "—".into()))).into_any_element(), theme))
                    .child(row("Hội thoại", None, div().text_size(px(13.0)).text_color(theme.secondary()).child(count).into_any_element(), theme))
                    .child(row("Lưu trữ", None, div().text_size(px(13.0)).text_color(theme.secondary()).child(storage_text).into_any_element(), theme))
                    .when(storage == Some(StorageBackend::IndexedDb) && !self.confirm_sqlite, |el| {
                        el.child(row("", None, button("acc-sqlite", "Chuyển sang SQLite…", ButtonStyle::Glass, false, theme, ask).into_any_element(), theme))
                    })
                    .when(storage == Some(StorageBackend::IndexedDb) && self.confirm_sqlite, |el| {
                        el.child(
                            div()
                                .px(px(14.0))
                                .py(px(9.0))
                                .flex()
                                .flex_col()
                                .gap(px(8.0))
                                .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child("Chuyển sang SQLite?"))
                                .child(div().text_size(px(11.5)).text_color(theme.secondary()).child(
                                    "Tin nhắn đã lưu trên máy sẽ không được chuyển: sau khi đăng nhập lại, hãy đồng bộ tin nhắn từ điện thoại. SQLite nhanh hơn nhiều với tài khoản có nhiều tin nhắn.",
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap(px(8.0))
                                        .child(button("acc-sqlite-cancel", "Hủy", ButtonStyle::Glass, false, theme, cancel))
                                        .child(button("acc-sqlite-go", "Đăng xuất và chuyển", ButtonStyle::Destructive, false, theme, confirm)),
                                ),
                        )
                    }),
            )
            .child(div().pt(px(16.0)).child(button("acc-logout", "Đăng xuất", ButtonStyle::Destructive, !signed_in, theme, |_, _, cx| engine(cx).log_out())))
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let mut tabs = div().flex().justify_center().gap(px(4.0)).pb(px(6.0)).border_b_1().border_color(theme.separator());
        for (t, sym, title) in TABS {
            let sel = self.tab == t;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("tab-{title}")))
                    .w(px(84.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(3.0))
                    .when(sel, |el| el.bg(with_alpha(theme.accent, 0.14)))
                    .hover(|s| s.bg(theme.hover()))
                    .child(icon(sym, 18.0, if sel { theme.accent } else { theme.secondary() }))
                    .child(div().text_size(px(11.0)).text_color(if sel { theme.accent } else { theme.secondary() }).child(title))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = t;
                        cx.notify();
                    })),
            );
        }
        let body = match self.tab {
            Tab::General => self.general(&theme, cx),
            Tab::Appearance => self.appearance(&theme, cx),
            Tab::Notifications => self.notifications(&theme, cx),
            Tab::Privacy => self.privacy(&theme, cx),
            Tab::Stickers => self.stickers(&theme, cx),
            Tab::Account => self.account(&theme, cx),
        };
        let sheet = self.pin_sheet.clone().map(|s| div().absolute().inset_0().bg(gpui::black().opacity(0.2)).flex().items_center().justify_center().child(s));
        if self.form_only {
            return div()
                .id("settings-form")
                .size_full()
                .bg(theme.window_bg())
                .text_color(theme.label())
                .font_family(crate::theme::FONT)
                .overflow_y_scroll()
                .px(px(24.0))
                .pb(px(20.0))
                .child(body)
                .into_any_element();
        }
        crate::chrome::frame(
            div()
                .size_full()
                .relative()
                .text_color(theme.label())
                .font_family(crate::theme::FONT)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .pt(px(40.0))
                        .flex()
                        .flex_col()
                        .child(tabs)
                        .child(div().id("settings-body").flex_1().min_h(px(0.0)).overflow_y_scroll().px(px(24.0)).pb(px(20.0)).child(body)),
                )
                .children(sheet)
                .child(crate::chrome::titlebar_overlay(36.0, window, cx)),
            window,
            cx,
        )
        .into_any_element()
    }
}

pub fn open(_window: &mut Window, cx: &mut App) {
    open_window(cx);
}

/// One settings window: an open one is brought to front.
pub fn open_window(cx: &mut App) {
    for w in cx.windows() {
        if let Some(h) = w.downcast::<gpui_component::Root>() {
            let is_settings = h.update(cx, |root, _, cx| root.view().clone().downcast::<SettingsView>().is_ok() && { let _ = cx; true }).unwrap_or(false);
            if is_settings {
                let _ = h.update(cx, |_, window, _| window.activate_window());
                return;
            }
        }
    }
    let bounds = Bounds::centered(None, size(px(580.0), px(560.0)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Cài đặt".into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some(olaz_core::APP_ID.into()),
            window_background: gpui::WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|_| SettingsView::new(Tab::General, false));
            cx.new(|cx| crate::chrome::root(view, window, cx))
        },
    );
    if let Ok(h) = opened {
        crate::dev_input::note_window(h.into());
    }
}
