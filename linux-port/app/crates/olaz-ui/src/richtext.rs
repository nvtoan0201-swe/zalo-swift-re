//! Message text with links and @mentions: links underlined (white on my bubble), mentions semibold
//! in the accent (white on mine), mentions of everyone highlighted. Emoji are drawn with the color emoji font.

use gpui::{AnyElement, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled, StyledText, TextRun, UnderlineStyle, div, px};
use olaz_engine::models::Mention;

use crate::theme::{Theme, with_alpha};

/// UTF-16 offset → byte offset in `s` (clamped).
pub fn byte_index(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// Highlight ranges (byte ranges) for links and mentions.
pub fn spans(text: &str, mentions: &[Mention]) -> (Vec<std::ops::Range<usize>>, Vec<(std::ops::Range<usize>, bool)>) {
    let finder = linkify::LinkFinder::new();
    let links: Vec<_> = finder.links(text).filter(|l| *l.kind() == linkify::LinkKind::Url).map(|l| l.start()..l.end()).collect();
    let mut ms = Vec::new();
    for m in mentions {
        if m.pos < 0 || m.len <= 0 {
            continue;
        }
        let start = byte_index(text, m.pos as usize);
        let end = byte_index(text, (m.pos + m.len) as usize);
        if start < end && text[start..].starts_with('@') {
            ms.push((start..end, m.is_all));
        }
    }
    (links, ms)
}

pub fn message_text(text: &str, mentions: &[Mention], mine: bool, size: f32, theme: &Theme) -> AnyElement {
    let (links, ms) = spans(text, mentions);
    let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let link_color = if mine { theme.white() } else { theme.accent };
    for r in links {
        highlights.push((r, HighlightStyle { color: Some(link_color), underline: Some(UnderlineStyle { thickness: px(1.0), color: Some(link_color), wavy: false }), ..Default::default() }));
    }
    for (r, all) in ms {
        let mut style = HighlightStyle { color: Some(if mine { theme.white() } else { theme.accent }), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        if all {
            style.background_color = Some(if mine { with_alpha(theme.white(), 0.22) } else { with_alpha(theme.accent, 0.16) });
        }
        highlights.push((r, style));
    }
    highlights.sort_by_key(|(r, _)| r.start);
    // Overlapping ranges are not allowed: keep the first.
    let mut clean: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for (r, s) in highlights {
        if clean.last().is_none_or(|(last, _)| r.start >= last.end) {
            clean.push((r, s));
        }
    }
    let base = if mine { theme.white() } else { theme.label() };
    div()
        .text_size(px(size))
        .line_height(px((size * 1.25).round()))
        .child(StyledText::new(SharedString::from(text.to_string())).with_runs(text_runs(text, &clean, base)))
        .into_any_element()
}

/// Whether `c` belongs to an emoji sequence (pictographs, symbols with emoji presentation, joiners, variation
/// selectors, skin tones, regional indicators, keycap and tag characters).
pub fn is_emoji_char(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
        | 0x200D | 0xFE0F | 0x20E3 | 0xE0020..=0xE007F | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

/// Byte ranges of emoji sequences in `text` (a digit/#/* followed by U+FE0F or U+20E3 counts as a keycap).
pub fn emoji_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        // Text symbols (digits, arrows, ©, ™…) become emoji only with U+FE0F / U+20E3 after them.
        let keycap = !is_emoji_char(c) && chars.get(i + 1).is_some_and(|(_, n)| *n == '\u{FE0F}' || *n == '\u{20E3}');
        if is_emoji_char(c) && c != '\u{200D}' && c != '\u{FE0F}' || keycap {
            let mut j = i + 1;
            while j < chars.len() && is_emoji_char(chars[j].1) {
                j += 1;
            }
            let end = chars.get(j).map(|(b, _)| *b).unwrap_or(text.len());
            out.push(start..end);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Text runs for `text`: highlights (links, mentions) applied over the base style; emoji in the color emoji font.
fn text_runs(text: &str, highlights: &[(std::ops::Range<usize>, HighlightStyle)], base: gpui::Hsla) -> Vec<TextRun> {
    // Cut points: every highlight and emoji boundary.
    let emoji = emoji_ranges(text);
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (r, _) in highlights {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for r in &emoji {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.retain(|c| *c <= text.len() && text.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut runs = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a == b {
            continue;
        }
        let style = highlights.iter().find(|(r, _)| r.start <= a && b <= r.end).map(|(_, s)| s.clone()).unwrap_or_default();
        // Emoji-only fonts cannot be a primary font in GPUI: every run is Inter with the color emoji font first in
        // its fallback chain (the system fallback would pick DejaVu Sans' monochrome emoticons).
        let mut font = gpui::font(crate::theme::FONT);
        font.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![crate::theme::EMOJI_FONT.to_string()]));
        if let Some(w) = style.font_weight {
            font.weight = w;
        }
        runs.push(TextRun {
            len: b - a,
            font,
            color: style.color.unwrap_or(base),
            background_color: style.background_color,
            underline: style.underline,
            strikethrough: None,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_sequences() {
        let t = "Hi 😀 👍🏽 ❤️ 👨‍👩‍👧 🇻🇳 1️⃣ ↔️ ok → ©";
        let found: Vec<&str> = emoji_ranges(t).into_iter().map(|r| &t[r]).collect();
        assert_eq!(found, ["😀", "👍🏽", "❤️", "👨‍👩‍👧", "🇻🇳", "1️⃣", "↔️"]);
        assert!(emoji_ranges("Xin chào, 12 giờ").is_empty());
        let runs = text_runs("a😀b", &[], gpui::black());
        assert_eq!(runs.iter().map(|r| r.len).collect::<Vec<_>>(), [1, 4, 1]);
        assert!(runs[1].font.fallbacks.as_ref().is_some_and(|f| f.fallback_list().iter().any(|n| n == crate::theme::EMOJI_FONT)));
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), "a😀b".len());
    }

    #[test]
    fn utf16_to_bytes_and_spans() {
        assert_eq!(byte_index("😀 @An", 3), 5);
        let m = Mention { uid: "1".into(), pos: 3, len: 3, is_all: false };
        let (links, ms) = spans("😀 @An xem https://zalo.me/x", &[m]);
        assert_eq!(ms, vec![(5..8, false)]);
        assert_eq!(links.len(), 1);
        let bad = Mention { uid: "1".into(), pos: 0, len: 2, is_all: false };
        assert!(spans("hi there", &[bad]).1.is_empty());
    }
}
