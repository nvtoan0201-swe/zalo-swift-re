//! Vietnamese-aware string ordering (stand-in for `localizedCompare` with the `vi` locale):
//! base letters first (đ sorts after d, diacritics ignored at the primary level), then diacritics, then case.

use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;

fn primary(s: &str) -> Vec<(u32, u32)> {
    // (base letter, tie-break for đ) per character
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'đ' => ('d' as u32, 1),
            other => (other as u32, 0),
        })
        .collect()
}

fn secondary(s: &str) -> Vec<char> {
    s.nfd().flat_map(|c| c.to_lowercase()).collect()
}

pub fn compare(a: &str, b: &str) -> Ordering {
    primary(a).cmp(&primary(b)).then_with(|| secondary(a).cmp(&secondary(b))).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vietnamese_order() {
        let mut names = vec!["Đức", "Dũng", "an", "Ánh", "Bình", "anh"];
        names.sort_by(|a, b| compare(a, b));
        assert_eq!(names, vec!["an", "anh", "Ánh", "Bình", "Dũng", "Đức"]);
        assert_eq!(compare("a", "A"), "a".cmp("A").then(Ordering::Equal).then("a".cmp("A")));
    }
}
