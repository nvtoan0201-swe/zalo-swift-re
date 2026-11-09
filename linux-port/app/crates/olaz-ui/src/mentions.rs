//! @mentions in the composer (ChatComposer.swift): the query being typed after "@", and the mentions of the final text.
//! The Linux textarea has no attributed tokens, so a picked mention is remembered as its "@Name" label and located again
//! (UTF-16 offsets, as the official API counts) when the message is sent; a label edited away drops its mention.

use olaz_engine::models::Mention;

pub const MAX_QUERY_CHARS: usize = 30;

/// `(byte offset of "@", query)` when the cursor sits right after "@query" that starts a word.
pub fn mention_query(text: &str, cursor: usize) -> Option<(usize, String)> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    if query.contains('\n') || query.contains("  ") || query.chars().count() > MAX_QUERY_CHARS || query.starts_with(' ') {
        return None;
    }
    let starts_word = before[..at].chars().next_back().is_none_or(|c| c.is_whitespace());
    starts_word.then(|| (at, query.to_string()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    pub uid: String,
    pub label: String,
    pub is_all: bool,
}

/// The mentions of `text`: each picked label at its first unused occurrence (UTF-16 offsets).
pub fn resolve(text: &str, picked: &[Picked]) -> Vec<Mention> {
    let mut used: Vec<(usize, usize)> = vec![];
    let mut out = vec![];
    for p in picked {
        let mut from = 0;
        while let Some(i) = text[from..].find(&p.label) {
            let start = from + i;
            let end = start + p.label.len();
            from = start + 1;
            if used.iter().any(|(a, b)| start < *b && *a < end) {
                continue;
            }
            // The label must end a word ("@An" must not match "@Anh").
            if text[end..].chars().next().is_some_and(|c| c.is_alphanumeric()) {
                continue;
            }
            used.push((start, end));
            out.push(Mention {
                uid: if p.is_all { "-1".into() } else { p.uid.clone() },
                pos: text[..start].encode_utf16().count() as i64,
                len: p.label.encode_utf16().count() as i64,
                is_all: p.is_all,
            });
            break;
        }
    }
    out.sort_by_key(|m| m.pos);
    out
}

/// Picks whose label is no longer in the text.
pub fn prune(text: &str, picked: &mut Vec<Picked>) {
    picked.retain(|p| text.contains(&p.label));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries() {
        assert_eq!(mention_query("hi @an", 6), Some((3, "an".into())));
        assert_eq!(mention_query("@", 1), Some((0, String::new())));
        assert_eq!(mention_query("mail@an", 7), None, "not at a word start");
        assert_eq!(mention_query("hi @an\nx", 8), None);
        assert_eq!(mention_query("hi @Nguyễn Văn", "hi @Nguyễn Văn".len()), Some((3, "Nguyễn Văn".into())));
        assert_eq!(mention_query("@a  b", 5), None);
    }

    #[test]
    fn resolves_utf16_positions() {
        let picked = vec![Picked { uid: "1".into(), label: "@Ánh".into(), is_all: false }, Picked { uid: "x".into(), label: "@All".into(), is_all: true }];
        let text = "😀 @Ánh và @All ơi";
        let m = resolve(text, &picked);
        assert_eq!(m, vec![Mention { uid: "1".into(), pos: 3, len: 4, is_all: false }, Mention { uid: "-1".into(), pos: 11, len: 4, is_all: true }]);
        assert!(resolve("@Ánhx", &picked[..1]).is_empty());
        let mut p = picked.clone();
        prune("chỉ @All", &mut p);
        assert_eq!(p.len(), 1);
    }
}
