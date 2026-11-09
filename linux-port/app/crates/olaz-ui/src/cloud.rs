//! zCloud (CloudColumns.swift, CloudView.swift, CloudMediaView.swift): storage card and routes in the list column; media
//! grid (all / by conversation / Cloud của tôi) with kind filter, sort, preview and delete; plan details; offload setting.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, Hsla, InteractiveElement, IntoElement, ObjectFit, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px, relative,
};
use gpui_component::input::{Input, InputEvent, InputState};
use olaz_engine::cloud::{CloudFetch, CloudItem, CloudItemKind, CloudPlan, CloudSort, CloudState, PlanKind};
use olaz_engine::contacts::fold;
use olaz_engine::media::MediaResolution;

use crate::app::engine;
use crate::chrome::{self, TITLEBAR_HEIGHT};
use crate::icon::icon;
use crate::theme::{Theme, with_alpha};
use crate::widgets::{ButtonStyle, alert, avatar, button, content_unavailable, spinner};

pub const PAGE: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Media,
    Conversations,
    MyCloud,
    Plan,
    Settings,
}

/// Binary sizes shown the way Zalo does (100 GiB as "100 GB").
pub fn size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    const K: f64 = 1024.0;
    if b < K {
        return format!("{} B", b as i64);
    }
    let (v, unit) = if b < K * K {
        (b / K, "KB")
    } else if b < K * K * K {
        (b / (K * K), "MB")
    } else if b < K * K * K * K {
        (b / (K * K * K), "GB")
    } else {
        (b / (K * K * K * K), "TB")
    };
    if v >= 100.0 || (v - v.round()).abs() < 0.05 { format!("{} {unit}", v.round() as i64) } else { format!("{:.1} {unit}", v).replace('.', ",") }
}

fn day(ms: Option<f64>) -> String {
    ms.map(|m| crate::format::local(m).format("%d/%m/%Y").to_string()).unwrap_or_else(|| "—".into())
}

fn has_media_screens(c: &CloudState) -> bool {
    c.plan.as_ref().is_none_or(|p| !matches!(p.kind, PlanKind::Free | PlanKind::AfterGrace))
}

fn plan_title(p: &CloudPlan) -> String {
    if p.is_paid() { p.name.clone() } else { "Gói miễn phí".into() }
}

fn colors(theme: &Theme) -> [Hsla; 4] {
    [theme.orange(), theme.green(), gpui::yellow(), gpui::hsla(0.6, 0.38, 0.55, 1.0)]
}

fn usage_bar(p: &CloudPlan, h: f32, theme: &Theme) -> AnyElement {
    let quota = p.quota.max(1) as f32;
    let c = colors(theme);
    let mut row = div().w_full().h(px(h)).rounded_full().overflow_hidden().bg(theme.quaternary()).flex().gap(px(1.5));
    for (i, v) in [p.cloud_media, p.message_backup, p.my_cloud, p.family].into_iter().enumerate() {
        if v > 0 {
            row = row.child(div().h_full().w(relative((v as f32 / quota).clamp(0.005, 1.0))).bg(c[i]));
        }
    }
    row.into_any_element()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    All,
    Conversation(String, String),
    MyCloud(String),
}

pub struct CloudView {
    pub route: Route,
    media: Option<Entity<CloudMedia>>,
    conv_search: Entity<InputState>,
    conv_query: String,
    conv_by_size: bool,
    confirm_offload: Option<bool>,
    offload_busy: bool,
    offload_message: Option<String>,
}

impl CloudView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        engine(cx).refresh_cloud();
        let conv_search = cx.new(|cx| InputState::new(window, cx).placeholder("Nhập tên trò chuyện"));
        cx.subscribe(&conv_search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.conv_query = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        CloudView { route: Route::Media, media: None, conv_search, conv_query: String::new(), conv_by_size: true, confirm_offload: None, offload_busy: false, offload_message: None }
    }

    fn open(&mut self, route: Route, scope: Option<Scope>, cx: &mut Context<Self>) {
        self.route = route;
        self.media = scope.map(|s| cx.new(|cx| CloudMedia::new(s, cx)));
        cx.notify();
    }

    fn effective_scope(&self, c: &CloudState, cx: &App) -> Option<Scope> {
        let self_id = c.self_id.clone().or_else(|| engine(cx).state.borrow().self_conversation_id.clone());
        match self.route {
            Route::Media if has_media_screens(c) => Some(Scope::All),
            Route::Media | Route::MyCloud => self_id.map(Scope::MyCloud),
            _ => None,
        }
    }

    fn render_list_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let c = engine(cx).state.borrow().cloud.clone();
        let subtitle = c.plan.as_ref().map(|p| format!("{} / {}", size(p.usage + p.family), size(p.quota))).unwrap_or_default();
        let colors = colors(&theme);
        let card = div().m(px(12.0)).p(px(12.0)).rounded(px(16.0)).bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().flex_col().gap(px(9.0)).child(
            div()
                .flex()
                .child(div().flex_1().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("Dung lượng lưu trữ"))
                .when(!subtitle.is_empty(), |el| el.child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(subtitle.clone()))),
        );
        let card = match &c.plan {
            Some(p) => {
                let legend = |color: Hsla, t: &str| div().flex().items_center().gap(px(4.0)).child(div().size(px(6.0)).rounded_full().bg(color)).child(div().text_size(px(10.5)).text_color(theme.secondary()).child(t.to_string()));
                card.child(usage_bar(p, 8.0, &theme))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.0))
                            .child(legend(colors[0], "Media"))
                            .child(legend(colors[1], "Sao lưu"))
                            .child(legend(colors[2], "Cloud của tôi"))
                            .when(p.family > 0, |el| el.child(legend(colors[3], "Gia đình"))),
                    )
                    .child(div().text_size(px(11.5)).text_color(theme.secondary()).child(format!("Còn trống {}", size(p.remaining()))))
            }
            None => card.child(div().flex().items_center().gap(px(8.0)).child(spinner(11.0, theme.secondary())).child(div().text_size(px(12.0)).text_color(theme.secondary()).child("Đang tính toán…"))),
        };
        let row = |id: &'static str, title: &'static str, sym: &'static str, route: Route, cx: &mut Context<Self>| {
            let sel = self.route == route;
            div()
                .id(id)
                .mx(px(10.0))
                .px(px(10.0))
                .h(px(34.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                .when(!sel, |el| el.hover(|s| s.bg(theme.hover())))
                .child(icon(sym, 14.0, if sel { theme.white() } else { theme.accent }))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(title))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let c = engine(cx).state.borrow().cloud.clone();
                    this.route = route;
                    let scope = this.effective_scope(&c, cx);
                    this.open(route, scope, cx);
                }))
        };
        let mut col = div()
            .w(px(340.0))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(theme.separator())
            .flex()
            .flex_col()
            .child(
                chrome::drag_area("cloud-toolbar")
                    .h(px(TITLEBAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px(px(16.0))
                    .child(div().text_size(px(15.0)).font_weight(FontWeight::BOLD).child("zCloud"))
                    .when(!subtitle.is_empty(), |el| el.child(div().text_size(px(11.0)).text_color(theme.secondary()).child(subtitle.clone()))),
            )
            .child(card);
        if has_media_screens(&c) {
            col = col.child(row("cr-media", "Tin nhắn media", "photo.on.rectangle.angled", Route::Media, cx)).child(row("cr-conv", "Theo trò chuyện", "bubble.left.and.bubble.right.fill", Route::Conversations, cx));
        }
        col.child(row("cr-my", "Cloud của tôi", "folder.fill", Route::MyCloud, cx))
            .child(div().h(px(12.0)))
            .child(row("cr-plan", "Thông tin gói", "externaldrive.fill", Route::Plan, cx))
            .child(row("cr-settings", "Cài đặt zCloud", "gearshape.fill", Route::Settings, cx))
            .into_any_element()
    }

    fn render_conversations(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let e = engine(cx);
        let c = e.state.borrow().cloud.clone();
        let known: HashMap<String, (String, Option<String>)> = e.state.borrow().conversations.iter().map(|x| (x.id.clone(), (x.display_name(), x.avatar.clone()))).collect();
        let q = fold(self.conv_query.trim());
        let mut rows: Vec<_> = c
            .conversations
            .iter()
            .map(|x| {
                let k = known.get(&x.id);
                let name = if !x.name.is_empty() { x.name.clone() } else { k.map(|k| k.0.clone()).unwrap_or_else(|| if x.is_group { "Nhóm".into() } else { "Trò chuyện".into() }) };
                (x.clone(), name, x.avatar.clone().or_else(|| k.and_then(|k| k.1.clone())))
            })
            .filter(|(_, n, _)| q.is_empty() || fold(n).contains(&q))
            .collect();
        if self.conv_by_size {
            rows.sort_by(|a, b| b.0.total.cmp(&a.0.total));
        } else {
            rows.sort_by(|a, b| b.0.last_activity.unwrap_or(0.0).total_cmp(&a.0.last_activity.unwrap_or(0.0)));
        }
        let seg = |id: &'static str, label: &'static str, on: bool, v: bool, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(12.0))
                .py(px(4.0))
                .rounded_full()
                .text_size(px(12.0))
                .when(on, |el| el.bg(theme.accent).text_color(theme.white()))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.conv_by_size = v;
                    cx.notify();
                }))
        };
        let header = div()
            .px(px(20.0))
            .pt(px(14.0))
            .pb(px(8.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(div().flex_1().h(px(30.0)).px(px(10.0)).rounded_full().bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().items_center().gap(px(6.0)).child(icon("magnifyingglass", 12.0, theme.secondary())).child(div().flex_1().child(Input::new(&self.conv_search).appearance(false).text_size(px(13.0)))))
            .child(div().flex().p(px(3.0)).rounded_full().bg(theme.glass()).child(seg("cs-size", "Dung lượng", self.conv_by_size, true, cx)).child(seg("cs-date", "Hoạt động gần nhất", !self.conv_by_size, false, cx)));
        let body: AnyElement = if !c.loaded {
            div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element()
        } else if !c.hidden_lookup {
            content_unavailable("lock.shield", "Chưa thể hiển thị", "Chưa xác định được các trò chuyện ẩn nên dữ liệu theo trò chuyện tạm thời bị ẩn.", &theme).into_any_element()
        } else if rows.is_empty() {
            content_unavailable("bubble.left.and.bubble.right", if self.conv_query.is_empty() { "Chưa có dữ liệu" } else { "Không tìm thấy" }, "", &theme).into_any_element()
        } else {
            let mut list = div().flex().flex_col();
            for (conv, name, av) in rows {
                let (id, n) = (conv.id.clone(), name.clone());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("cc-{}", conv.id)))
                        .px(px(14.0))
                        .py(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .hover(|s| s.bg(theme.hover()))
                        .child(avatar(av.as_deref(), &name, 40.0, cx))
                        .child(div().flex_1().min_w(px(0.0)).flex().flex_col().gap(px(2.0)).child(div().truncate().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(name.clone())).child(div().text_size(px(11.5)).text_color(theme.secondary()).child(if conv.is_group { "Nhóm" } else { "Cá nhân" })))
                        .child(div().text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(size(conv.total)))
                        .child(div().w(px(82.0)).text_right().text_size(px(12.0)).text_color(theme.secondary()).child(day(conv.last_activity)))
                        .child(icon("chevron.right", 11.0, theme.tertiary()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let scope = Scope::Conversation(id.clone(), n.clone());
                            this.media = Some(cx.new(|cx| CloudMedia::new(scope, cx)));
                            cx.notify();
                        })),
                );
            }
            div().id("cloud-convs").flex_1().min_h(px(0.0)).overflow_y_scroll().child(list).into_any_element()
        };
        div().size_full().flex().flex_col().child(header).child(body).into_any_element()
    }

    fn card(theme: &Theme) -> gpui::Div {
        div().p(px(16.0)).rounded(px(22.0)).bg(theme.glass()).border_1().border_color(theme.glass_border()).flex().flex_col().gap(px(12.0))
    }

    fn render_plan(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let c = engine(cx).state.borrow().cloud.clone();
        let Some(p) = c.plan.clone() else {
            return div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element();
        };
        let status = match p.kind {
            PlanKind::Paid => p.expire_at.map(|d| format!("Đang hoạt động • hết hạn {}", day(Some(d)))).unwrap_or_else(|| "Đang hoạt động".into()),
            PlanKind::Grace => "Đã hết hạn — dữ liệu còn được giữ trong thời gian gia hạn".into(),
            PlanKind::Free => "Chỉ lưu trữ Cloud của tôi".into(),
            PlanKind::AfterGrace => "Đã hết thời gian gia hạn zCloud".into(),
        };
        let colors = colors(&theme);
        let legend = |color: Hsla, t: &str, b: i64| {
            div().flex().items_center().gap(px(8.0)).child(div().size(px(9.0)).rounded_full().bg(color)).child(div().flex_1().text_size(px(12.5)).child(t.to_string())).child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(size(b)))
        };
        let info = |t: &str, v: String| div().py(px(9.0)).flex().child(div().flex_1().text_size(px(13.0)).text_color(theme.secondary()).child(t.to_string())).child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).child(v));
        let mut details = Self::card(&theme).gap(px(0.0)).py(px(6.0)).child(info("Gói", plan_title(&p)));
        if p.is_paid() {
            details = details.child(info("Hết hạn vào", day(p.expire_at)));
            if let Some(a) = p.auto_renew {
                details = details.child(info("Tự động gia hạn", if a { "Bật".into() } else { "Tắt".into() }));
            }
            if p.months > 0 {
                details = details.child(info("Chu kỳ", format!("{} tháng", p.months)));
            }
        }
        if p.file_limit > 0 {
            // The server config (`cloud_media_file_size_limit`) is in MB; the macOS app printed it as bytes.
            let bytes = if p.file_limit < 1024 * 1024 { p.file_limit * 1024 * 1024 } else { p.file_limit };
            details = details.child(info("Dung lượng tối đa mỗi file", size(bytes)));
        }
        details = details.child(info("Khoá cloud", if p.has_key { "Đã có".into() } else { "Chưa có — mở Zalo trên điện thoại để đồng bộ".into() }));
        let media = c.media;
        let mine = c.my_cloud_types;
        let kinds = [CloudItemKind::Photo, CloudItemKind::Video, CloudItemKind::File, CloudItemKind::Voice];
        let max = kinds.iter().map(|k| media.bytes(*k) + mine.bytes(*k)).max().unwrap_or(1).max(1) as f32;
        let mut types = Self::card(&theme).child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Dung lượng của tôi"));
        for (i, k) in kinds.into_iter().enumerate() {
            let b = media.bytes(k) + mine.bytes(k);
            types = types.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(div().flex().items_center().gap(px(6.0)).child(icon(k.symbol(), 12.0, theme.secondary())).child(div().flex_1().text_size(px(13.0)).child(k.title())).child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(theme.secondary()).child(size(b))))
                    .child(div().h(px(6.0)).rounded_full().bg(theme.quaternary()).child(div().h_full().rounded_full().w(relative(if b > 0 { (b as f32 / max).max(0.01) } else { 0.0 })).bg(colors[i.min(3)]))),
            );
        }
        let toggle = |t: &str, on: bool| {
            div().flex().items_center().child(div().flex_1().text_size(px(13.0)).child(t.to_string())).child(div().flex().items_center().gap(px(4.0)).text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(if on { theme.green() } else { theme.secondary() }).child(icon(if on { "checkmark.circle.fill" } else { "minus.circle" }, 12.0, if on { theme.green() } else { theme.secondary() })).child(if on { "Bật" } else { "Tắt" }))
        };
        div()
            .id("cloud-plan")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .p(px(20.0))
                    .max_w(px(760.0))
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        Self::card(&theme).flex_row().items_center().gap(px(14.0)).child(div().size(px(56.0)).rounded(px(16.0)).bg(theme.accent).flex().items_center().justify_center().child(icon("externaldrive.fill", 26.0, theme.white()))).child(
                            div().flex_1().flex().flex_col().gap(px(3.0)).child(div().text_size(px(20.0)).font_weight(FontWeight::BOLD).child(plan_title(&p))).child(div().text_size(px(13.0)).text_color(theme.secondary()).child(status)),
                        ).child(div().px(px(12.0)).py(px(6.0)).rounded_full().bg(theme.glass()).text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD).child(size(p.quota))),
                    )
                    .child(
                        Self::card(&theme)
                            .child(div().flex().items_center().gap(px(6.0)).child(div().flex_1().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Dung lượng lưu trữ")).child(div().text_size(px(13.0)).font_weight(FontWeight::MEDIUM).child(format!("Đã dùng {}", size(p.usage + p.family)))).child(div().text_size(px(13.0)).text_color(theme.secondary()).child(format!("• Còn trống {}", size(p.remaining())))))
                            .child(usage_bar(&p, 12.0, &theme))
                            .child(legend(colors[0], "Tin nhắn media", p.cloud_media))
                            .child(legend(colors[1], "Sao lưu tin nhắn", p.message_backup))
                            .child(legend(colors[2], "Cloud của tôi", p.my_cloud))
                            .when(p.family > 0, |el| el.child(legend(colors[3], "Gia đình", p.family))),
                    )
                    .child(types)
                    .child(details)
                    .child(
                        Self::card(&theme)
                            .gap(px(10.0))
                            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Tự động lưu trữ lên zCloud"))
                            .child(toggle("Tin nhắn media", p.backup_media))
                            .child(toggle("Cloud của tôi", p.backup_my_cloud))
                            .child(toggle("Sao lưu tin nhắn", p.backup_messages))
                            .child(div().text_size(px(11.5)).text_color(theme.secondary()).child("Cài đặt lưu trữ được điều khiển từ Zalo trên điện thoại.")),
                    )
                    .child(div().px(px(4.0)).flex().items_center().gap(px(5.0)).text_size(px(12.5)).text_color(theme.secondary()).child(icon("info.circle", 12.0, theme.secondary())).child("Bạn có thể quản lý gói này trên điện thoại.")),
            )
            .into_any_element()
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::get(cx).clone();
        let c = engine(cx).state.borrow().cloud.clone();
        let s = c.settings.clone();
        let ttl = s.ttl_days.max(1);
        let hint = (!s.offload_available).then(|| match &c.plan {
            None => "Đang tải thông tin gói…",
            Some(p) if !p.is_paid() => "Tính năng này chỉ hoạt động khi bạn nâng cấp zCloud.",
            Some(p) if !p.has_key => "Vui lòng mở Zalo trên điện thoại để hoàn tất đồng bộ.",
            _ => "Tính năng tạm thời chưa khả dụng, vui lòng thử lại sau.",
        });
        let last = match (s.last_offload_at, s.last_offload_size) {
            (Some(at), n) if n > 0 => format!("Tối ưu dung lượng gần nhất lúc {}, giải phóng {} trên máy tính.", crate::format::local(at).format("%H:%M %d/%m/%Y"), size(n)),
            _ => "Quá trình dọn dẹp diễn ra tự động, bạn sẽ nhận được thông báo khi hoàn tất.".into(),
        };
        let enabled = s.offload_available && !self.offload_busy;
        let on = s.offload_enabled;
        let confirm = self.confirm_offload.map(|en| {
            let me = cx.entity().downgrade();
            alert(
                if en { "Bật tự động giải phóng dung lượng?" } else { "Tắt tự động giải phóng dung lượng?" },
                &if en { format!("Nội dung đã lưu trên zCloud sẽ được gỡ khỏi máy sau {ttl} ngày. Bạn có thể xem lại những nội dung này bất kỳ lúc nào.") } else { "Zalo sẽ không tự động gỡ nội dung đã lưu trên zCloud khỏi máy nữa.".into() },
                if en { "Bật" } else { "Tắt" },
                "Huỷ",
                false,
                &theme,
                move |ok, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.confirm_offload = None;
                        if ok {
                            this.offload_busy = true;
                            this.offload_message = None;
                            let e = engine(cx);
                            cx.spawn(async move |this, cx| {
                                let r = e.cloud_set_offload(en).await;
                                let _ = this.update(cx, |this, cx| {
                                    this.offload_busy = false;
                                    if !r.ok {
                                        this.offload_message = Some(r.error.unwrap_or_else(|| "Không thay đổi được cài đặt.".into()));
                                    }
                                    cx.notify();
                                });
                            })
                            .detach();
                        }
                        cx.notify();
                    });
                },
            )
        });
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .p(px(20.0))
                    .max_w(px(680.0))
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(
                        Self::card(&theme)
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(14.0))
                                    .child(div().flex_1().flex().flex_col().gap(px(4.0)).child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Tự động giải phóng dung lượng")).child(div().text_size(px(12.5)).text_color(theme.secondary()).child(format!("Nội dung đã lưu trên zCloud sẽ được gỡ khỏi máy sau {ttl} ngày. Bạn có thể xem lại những nội dung này bất kỳ lúc nào."))))
                                    .child(
                                        div()
                                            .id("offload-toggle")
                                            .w(px(38.0))
                                            .h(px(22.0))
                                            .rounded_full()
                                            .p(px(2.0))
                                            .flex()
                                            .when(!enabled, |el| el.opacity(0.5))
                                            .when(on, |el| el.justify_end().bg(theme.accent))
                                            .when(!on, |el| el.bg(theme.quaternary()))
                                            .child(div().size(px(18.0)).rounded_full().bg(gpui::white()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if enabled {
                                                    this.confirm_offload = Some(!on);
                                                    cx.notify();
                                                }
                                            })),
                                    ),
                            )
                            .when_some(hint, |el, h| el.child(div().flex().items_center().gap(px(5.0)).text_size(px(12.0)).text_color(theme.secondary()).child(icon("info.circle", 11.0, theme.secondary())).child(h)))
                            .when(hint.is_none() && on, |el| el.child(div().text_size(px(12.0)).text_color(theme.secondary()).child(last)))
                            .when_some(self.offload_message.clone(), |el, m| el.child(div().text_size(px(12.0)).text_color(theme.orange()).child(m))),
                    )
                    .child(
                        Self::card(&theme)
                            .gap(px(6.0))
                            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child("Hỗ trợ"))
                            .child(div().text_size(px(12.5)).text_color(theme.secondary()).child("Quản lý gói, gia hạn và cài đặt lưu trữ (Tin nhắn media, Cloud của tôi, Sao lưu tin nhắn) được thực hiện trên Zalo điện thoại.")),
                    ),
            )
            .children(confirm)
            .into_any_element()
    }
}

impl Render for CloudView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let c = engine(cx).state.borrow().cloud.clone();
        if !has_media_screens(&c) && matches!(self.route, Route::Media | Route::Conversations) && c.plan.is_some() {
            self.route = Route::MyCloud;
            self.media = None;
        }
        if self.media.is_none() && matches!(self.route, Route::Media | Route::MyCloud) {
            if let Some(s) = self.effective_scope(&c, cx) {
                self.media = Some(cx.new(|cx| CloudMedia::new(s, cx)));
            }
        }
        let list = self.render_list_column(cx);
        let detail: AnyElement = match (self.route, &self.media) {
            (Route::Media | Route::MyCloud | Route::Conversations, Some(m)) => m.clone().into_any_element(),
            (Route::Conversations, None) => self.render_conversations(cx),
            (Route::Plan, _) => self.render_plan(cx),
            (Route::Settings, _) => self.render_settings(cx),
            _ => div().flex_1().flex().items_center().justify_center().child(spinner(13.0, theme.secondary())).into_any_element(),
        };
        let r = crate::chrome::inner_radius(window);
        div()
            .size_full()
            .flex()
            .rounded_tr(r)
            .rounded_br(r)
            .bg(theme.content_bg())
            .child(list)
            .child(div().flex_1().min_w(px(0.0)).h_full().flex().flex_col().child(chrome::drag_area("cloud-detail-toolbar").h(px(TITLEBAR_HEIGHT)).flex_none()).child(div().flex_1().min_h(px(0.0)).child(detail)))
    }
}

// MARK: Media grid

pub struct CloudMedia {
    scope: Scope,
    kind: Option<CloudItemKind>,
    by_size: bool,
    items: Vec<CloudItem>,
    has_more: bool,
    loading: bool,
    error: Option<String>,
    thumbs: HashMap<String, Option<String>>,
    selecting: bool,
    selected: HashSet<String>,
    confirm_delete: bool,
    opening: Option<String>,
    message: Option<String>,
    _load: Option<Task<()>>,
}

impl CloudMedia {
    pub fn new(scope: Scope, cx: &mut Context<Self>) -> Self {
        let mut me = CloudMedia {
            scope,
            kind: None,
            by_size: false,
            items: vec![],
            has_more: false,
            loading: false,
            error: None,
            thumbs: HashMap::new(),
            selecting: false,
            selected: HashSet::new(),
            confirm_delete: false,
            opening: None,
            message: None,
            _load: None,
        };
        me.reload(cx);
        me
    }

    fn conv(&self) -> Option<String> {
        match &self.scope {
            Scope::All => None,
            Scope::Conversation(id, _) | Scope::MyCloud(id) => Some(id.clone()),
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.items.clear();
        self.has_more = false;
        self.load_more(cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        let (e, conv, kind, by_size, offset) = (engine(cx), self.conv(), self.kind, self.by_size, self.items.len());
        self._load = Some(cx.spawn(async move |this, cx| {
            let page = e.cloud_list(conv.as_deref(), kind, if by_size { CloudSort::Size } else { CloudSort::Date }, false, offset, PAGE).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.error = page.error.clone();
                let seen: HashSet<String> = this.items.iter().map(|i| i.id.clone()).collect();
                this.items.extend(page.items.into_iter().filter(|i| !seen.contains(&i.id)));
                this.has_more = page.has_more;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn thumb(&mut self, key: &str, cx: &mut Context<Self>) -> Option<String> {
        if let Some(t) = self.thumbs.get(key) {
            return t.clone();
        }
        self.thumbs.insert(key.to_string(), None);
        let (e, k) = (engine(cx), key.to_string());
        cx.spawn(async move |this, cx| {
            let r = e.cloud_thumbnail(&k).await;
            let _ = this.update(cx, |this, cx| {
                if let MediaResolution::File(p) = r {
                    this.thumbs.insert(k, Some(p));
                    cx.notify();
                }
            });
        })
        .detach();
        None
    }

    fn open(&mut self, item: CloudItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening.is_some() {
            return;
        }
        self.opening = Some(item.id.clone());
        self.message = None;
        let e = engine(cx);
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let r = e.cloud_fetch(&item.id).await;
            let _ = this.update(cx, |this, cx| {
                this.opening = None;
                match r {
                    CloudFetch::Ready(f) => {
                        if f.kind == CloudItemKind::Photo {
                            let url = format!("file://{}", f.path);
                            let _ = window_handle.update(cx, |_, window, cx| crate::viewer::open_photo(&url, window, cx));
                        } else if crate::viewer::may_execute(std::path::Path::new(&f.path)) {
                            let dir = std::path::Path::new(&f.path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
                            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                        } else {
                            let _ = std::process::Command::new("xdg-open").arg(&f.path).spawn();
                        }
                    }
                    CloudFetch::Expired => this.message = Some("Nội dung này đã hết hạn lưu trữ.".into()),
                    CloudFetch::Failed(t) => this.message = Some(t),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.selected.iter().cloned().collect();
        if keys.is_empty() {
            return;
        }
        let e = engine(cx);
        cx.spawn(async move |this, cx| {
            let r = e.cloud_delete(keys).await;
            let _ = this.update(cx, |this, cx| {
                let gone: HashSet<String> = r.deleted.iter().cloned().collect();
                this.items.retain(|i| !gone.contains(&i.id));
                this.selected.retain(|k| !gone.contains(k));
                if !r.ok || !r.failed.is_empty() {
                    this.message = Some(r.error.unwrap_or_else(|| format!("Không xoá được {} mục.", r.failed.len())));
                } else {
                    this.selecting = false;
                }
                e.refresh_cloud();
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for CloudMedia {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let title = match &self.scope {
            Scope::All => "Tin nhắn media".to_string(),
            Scope::Conversation(_, n) => n.clone(),
            Scope::MyCloud(_) => "Cloud của tôi".into(),
        };
        let mut chips = div().flex().gap(px(6.0));
        for (i, k) in [None, Some(CloudItemKind::Photo), Some(CloudItemKind::Video), Some(CloudItemKind::File), Some(CloudItemKind::Voice)].into_iter().enumerate() {
            let sel = self.kind == k;
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("ck-{i}")))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded_full()
                    .text_size(px(12.0))
                    .when(sel, |el| el.bg(theme.accent).text_color(theme.white()))
                    .when(!sel, |el| el.bg(theme.glass()))
                    .child(k.map(|k| k.title()).unwrap_or("Tất cả"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.kind = k;
                        this.reload(cx);
                    })),
            );
        }
        let by_size = self.by_size;
        let header = div()
            .px(px(20.0))
            .pb(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().text_size(px(17.0)).font_weight(FontWeight::BOLD).truncate().child(title))
                    .child(
                        div()
                            .id("cloud-sort")
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(theme.glass())
                            .text_size(px(12.0))
                            .child(if by_size { "Sắp xếp: Dung lượng" } else { "Sắp xếp: Mới nhất" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.by_size = !this.by_size;
                                this.reload(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("cloud-select")
                            .px(px(10.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(theme.glass())
                            .text_size(px(12.0))
                            .child(if self.selecting { "Xong" } else { "Chọn" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selecting = !this.selecting;
                                this.selected.clear();
                                cx.notify();
                            })),
                    ),
            )
            .child(chips);
        let mut grid = div().px(px(20.0)).pb(px(20.0)).flex().flex_wrap().gap(px(8.0));
        let items = self.items.clone();
        for it in items {
            let thumb = matches!(it.kind, CloudItemKind::Photo | CloudItemKind::Video).then(|| self.thumb(&it.id, cx)).flatten();
            let sel = self.selected.contains(&it.id);
            let busy = self.opening.as_deref() == Some(it.id.as_str());
            let name = if it.title.is_empty() { it.kind.title().to_string() } else { it.title.clone() };
            let it2 = it.clone();
            let face: AnyElement = match thumb {
                Some(p) => crate::widgets::remote_picture(Some(&format!("file://{p}")), 120.0, 120.0, 0.0, ObjectFit::Cover, cx),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .child(icon(it.kind.symbol(), 26.0, theme.accent))
                    .when(it.kind == CloudItemKind::File, |el| el.child(div().px(px(6.0)).w_full().text_center().truncate().text_size(px(11.0)).child(name.clone())))
                    .into_any_element(),
            };
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("ci-{}", it.id)))
                    .relative()
                    .size(px(120.0))
                    .rounded(px(10.0))
                    .overflow_hidden()
                    .bg(theme.quaternary())
                    .when(sel, |el| el.border_2().border_color(theme.accent))
                    .child(face)
                    .when(it.kind == CloudItemKind::Video || it.kind == CloudItemKind::Voice, |el| {
                        el.child(div().absolute().bottom(px(4.0)).right(px(6.0)).px(px(4.0)).rounded(px(4.0)).bg(gpui::black().opacity(0.5)).text_size(px(10.5)).text_color(gpui::white()).child(crate::format::clock(it.duration.round() as i64)))
                    })
                    .child(div().absolute().bottom(px(4.0)).left(px(6.0)).px(px(4.0)).rounded(px(4.0)).bg(gpui::black().opacity(0.5)).text_size(px(10.0)).text_color(gpui::white()).child(size(it.size)))
                    .when(self.selecting, |el| el.child(div().absolute().top(px(6.0)).right(px(6.0)).child(icon(if sel { "checkmark.circle.fill" } else { "circle" }, 18.0, if sel { theme.accent } else { gpui::white() }))))
                    .when(busy, |el| el.child(div().absolute().inset_0().bg(gpui::black().opacity(0.3)).flex().items_center().justify_center().child(spinner(14.0, gpui::white()))))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.selecting {
                            if !this.selected.remove(&it2.id) {
                                this.selected.insert(it2.id.clone());
                            }
                            cx.notify();
                        } else {
                            this.open(it2.clone(), window, cx);
                        }
                    })),
            );
        }
        let footer = if self.loading {
            Some(div().py(px(12.0)).flex().justify_center().child(spinner(13.0, theme.secondary())).into_any_element())
        } else if self.has_more {
            Some(div().py(px(12.0)).flex().justify_center().child(button("cloud-more", "Tải thêm", ButtonStyle::Glass, false, &theme, {
                let me = cx.entity().downgrade();
                move |_, _, cx| {
                    let _ = me.update(cx, |this, cx| this.load_more(cx));
                }
            })).into_any_element())
        } else {
            None
        };
        let empty = !self.loading && self.items.is_empty();
        let body: AnyElement = if empty {
            content_unavailable("externaldrive", if self.error.is_some() { "Không tải được" } else { "Chưa có nội dung" }, self.error.as_deref().unwrap_or(""), &theme).into_any_element()
        } else {
            div().id("cloud-grid").flex_1().min_h(px(0.0)).overflow_y_scroll().child(grid).children(footer).into_any_element()
        };
        let select_bar = (self.selecting && !self.selected.is_empty()).then(|| {
            let n = self.selected.len();
            let me = cx.entity().downgrade();
            div()
                .absolute()
                .bottom(px(20.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div().px(px(14.0)).py(px(8.0)).rounded_full().bg(theme.content_bg()).border_1().border_color(theme.separator()).shadow_lg().flex().items_center().gap(px(10.0)).child(div().text_size(px(12.5)).child(format!("Đã chọn {n}"))).child(button("cloud-del", "Xoá", ButtonStyle::Destructive, false, &theme, move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| {
                            this.confirm_delete = true;
                            cx.notify();
                        });
                    })),
                )
        });
        let confirm = self.confirm_delete.then(|| {
            let me = cx.entity().downgrade();
            alert(
                &format!("Xoá {} mục khỏi zCloud?", self.selected.len()),
                "Nội dung sẽ bị xoá vĩnh viễn khỏi zCloud và không thể khôi phục.",
                "Xoá",
                "Huỷ",
                true,
                &theme,
                move |ok, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.confirm_delete = false;
                        if ok {
                            this.delete(cx);
                        }
                        cx.notify();
                    });
                },
            )
        });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(header)
            .when_some(self.message.clone(), |el, m| el.child(div().px(px(20.0)).pb(px(6.0)).text_size(px(12.0)).text_color(theme.orange()).child(m)))
            .child(body)
            .children(select_bar)
            .children(confirm)
    }
}

#[allow(dead_code)]
fn unused(_: f32) -> Hsla {
    with_alpha(gpui::black(), 0.0)
}

#[cfg(test)]
mod tests {
    use super::size;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1536), "1,5 KB");
        assert_eq!(size(100 * 1024 * 1024 * 1024), "100 GB");
        assert_eq!(size(5 * 1024 * 1024), "5 MB");
    }
}
