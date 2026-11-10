//! Emoji picker of the composer: recently used, the Unicode categories, and search by name or `:shortcode:`.
//! Picking inserts the emoji at the cursor and keeps the picker open (like the system character viewer).

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::icon::icon;
use crate::settings::AppPrefs;
use crate::theme::{Theme, with_alpha};

/// Emoji newer than this may be missing from the bundled font (they would draw as boxes).
const MAX_UNICODE: (u32, u32) = (15, 1);
const RECENT_MAX: usize = 24;

/// Tabs: (title, the emoji shown on the tab, group; `None` = recently used).
pub const TABS: [(&str, &str, Option<emojis::Group>); 10] = [
    ("Gần đây", "🕘", None),
    ("Mặt cười & cảm xúc", "😀", Some(emojis::Group::SmileysAndEmotion)),
    ("Con người", "👋", Some(emojis::Group::PeopleAndBody)),
    ("Động vật & thiên nhiên", "🐻", Some(emojis::Group::AnimalsAndNature)),
    ("Đồ ăn & thức uống", "🍔", Some(emojis::Group::FoodAndDrink)),
    ("Du lịch & địa điểm", "✈️", Some(emojis::Group::TravelAndPlaces)),
    ("Hoạt động", "⚽", Some(emojis::Group::Activities)),
    ("Đồ vật", "💡", Some(emojis::Group::Objects)),
    ("Ký hiệu", "🔣", Some(emojis::Group::Symbols)),
    ("Cờ", "🏁", Some(emojis::Group::Flags)),
];

fn supported(e: &emojis::Emoji) -> bool {
    let v = e.unicode_version();
    (v.major(), v.minor()) <= MAX_UNICODE && e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default)
}

/// The emoji of one category, in Unicode order.
pub fn group_emojis(group: emojis::Group) -> Vec<&'static str> {
    group.emojis().filter(|e| supported(e)).map(|e| e.as_str()).collect()
}

/// Emoji whose name or shortcode contains every word of `query` (case-insensitive), names starting with it first.
pub fn search(query: &str) -> Vec<&'static str> {
    let q = query.trim().trim_matches(':').to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(bool, &'static str)> = emojis::iter()
        .filter(|e| supported(e))
        .filter_map(|e| {
            let name = e.name().to_lowercase();
            let codes: Vec<&str> = e.shortcodes().collect();
            let matches = words.iter().all(|w| name.contains(w) || codes.iter().any(|c| c.contains(w)));
            matches.then(|| (name.starts_with(&q) || codes.iter().any(|c| c.starts_with(&q)), e.as_str()))
        })
        .collect();
    hits.sort_by_key(|(prefix, _)| !*prefix);
    hits.into_iter().map(|(_, e)| e).take(240).collect()
}

/// `recent` with `emoji` moved to the front (most recent first, at most 24).
pub fn push_recent(recent: &[String], emoji: &str) -> Vec<String> {
    let mut out = vec![emoji.to_string()];
    out.extend(recent.iter().filter(|e| e.as_str() != emoji).cloned());
    out.truncate(RECENT_MAX);
    out
}

pub fn recent(cx: &App) -> Vec<String> {
    cx.try_global::<AppPrefs>().map(|p| p.prefs.emoji_recent.split_whitespace().map(str::to_string).collect()).unwrap_or_default()
}

/// Emitted with the picked emoji.
pub struct EmojiPicked(pub String);

pub struct EmojiPicker {
    search: Entity<InputState>,
    query: String,
    tab: usize,
}

impl EventEmitter<EmojiPicked> for EmojiPicker {}

impl EmojiPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Tìm biểu tượng cảm xúc"));
        cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.query = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        // Recently used first when there are some, else the smileys.
        let tab = if recent(cx).is_empty() { 1 } else { 0 };
        EmojiPicker { search, query: String::new(), tab }
    }

    fn pick(&mut self, emoji: &str, cx: &mut Context<Self>) {
        let prefs = cx.global_mut::<AppPrefs>();
        let current: Vec<String> = prefs.prefs.emoji_recent.split_whitespace().map(str::to_string).collect();
        prefs.prefs.emoji_recent = push_recent(&current, emoji).join(" ");
        prefs.save();
        cx.emit(EmojiPicked(emoji.to_string()));
        cx.notify();
    }

    fn grid(&self, items: Vec<String>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut grid = div().flex().flex_wrap().px(px(6.0));
        for (i, e) in items.into_iter().enumerate() {
            let pick = e.clone();
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("emoji-{i}")))
                    .size(px(38.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(24.0))
                    
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover()))
                    .child(e)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(&pick, cx))),
            );
        }
        grid.into_any_element()
    }
}

impl Render for EmojiPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::get(cx).clone();
        let searching = !self.query.trim().is_empty();
        let (title, items): (String, Vec<String>) = if searching {
            ("Kết quả".into(), search(&self.query).into_iter().map(str::to_string).collect())
        } else {
            let (title, _, group) = TABS[self.tab];
            let items = match group {
                Some(g) => group_emojis(g).into_iter().map(str::to_string).collect(),
                None => recent(cx),
            };
            (title.into(), items)
        };
        let empty = items.is_empty();
        let body = if empty {
            div()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.5))
                .text_color(theme.secondary())
                .child(if searching { "Không tìm thấy biểu tượng nào" } else { "Chưa có biểu tượng dùng gần đây" })
                .into_any_element()
        } else {
            self.grid(items, &theme, cx)
        };
        let mut tabs = div().flex().justify_between().px(px(6.0)).py(px(4.0)).border_t_1().border_color(theme.separator());
        for (i, (name, glyph, _)) in TABS.iter().enumerate() {
            let sel = !searching && i == self.tab;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("emoji-tab-{i}")))
                    .size(px(30.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(17.0))
                    
                    .cursor_pointer()
                    .when(sel, |el| el.bg(with_alpha(theme.accent, 0.16)))
                    .when(!sel, |el| el.hover(|s| s.bg(theme.hover())))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(*name).build(window, cx))
                    .child(*glyph)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.tab = i;
                        this.query.clear();
                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    })),
            );
        }
        div()
            .id("emoji-picker")
            .relative()
            .child(crate::widgets::occluder())
            .w(px(372.0))
            .h(px(420.0))
            .rounded(px(14.0))
            .bg(theme.content_bg())
            .border_1()
            .border_color(theme.separator())
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div().p(px(10.0)).child(
                    div()
                        .h(px(32.0))
                        .px(px(10.0))
                        .rounded_full()
                        .bg(theme.glass())
                        .border_1()
                        .border_color(theme.glass_border())
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(icon("magnifyingglass", 12.0, theme.secondary()))
                        .child(div().flex_1().child(Input::new(&self.search).appearance(false).text_size(px(13.0)))),
                ),
            )
            .child(div().px(px(14.0)).pb(px(4.0)).text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme.secondary()).child(title))
            .child(div().id("emoji-grid").flex_1().min_h(px(0.0)).overflow_y_scroll().child(body))
            .child(tabs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_search_and_recent() {
        let smileys = group_emojis(emojis::Group::SmileysAndEmotion);
        assert!(smileys.len() > 100, "full smileys category: {}", smileys.len());
        assert_eq!(smileys[0], "😀");
        let total: usize = TABS.iter().filter_map(|t| t.2).map(|g| group_emojis(g).len()).sum();
        assert!(total > 1500, "full set: {total}");
        assert!(search("heart").contains(&"❤️"));
        assert_eq!(search(":thumbsup:").first(), Some(&"👍"));
        assert!(search("zzzz-no-such").is_empty());
        let r = push_recent(&["😀".into(), "👍".into()], "👍");
        assert_eq!(r, ["👍", "😀"]);
        let many: Vec<String> = (0..30).map(|i| i.to_string()).collect();
        assert_eq!(push_recent(&many, "x").len(), 24);
    }
}
