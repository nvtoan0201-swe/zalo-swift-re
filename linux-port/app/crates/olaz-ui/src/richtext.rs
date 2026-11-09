//! Message text with links and @mentions: links underlined (white on my bubble), mentions semibold
//! in the accent (white on mine), mentions of everyone highlighted.

use gpui::{AnyElement, FontWeight, HighlightStyle, IntoElement, ParentElement, SharedString, Styled, StyledText, UnderlineStyle, div, px};
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
    div()
        .text_size(px(size))
        .line_height(px((size * 1.25).round()))
        .child(StyledText::new(SharedString::from(text.to_string())).with_highlights(clean))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

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
