// Olaz patch: a lenient COMPOUND_TEXT → UTF-8 decoder. ibus (and others) commit Vietnamese letters as raw
use alloc::string::String;
use alloc::vec::Vec;
// Latin-1 GR bytes (e.g. 0xEA = "ê") that `xim_ctext` rejects, which panicked the app on the first "ê".
// Default GL = ASCII, GR = ISO-8859-1 right half; `ESC % G … ESC % @` switches to UTF-8; other designations
// are skipped.

pub fn compound_text_to_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = xim_ctext::compound_text_to_utf8(bytes) {
        if !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return s;
        }
    }
    let mut out = String::new();
    let mut i = 0;
    let mut utf8 = false;
    let mut buf: Vec<u8> = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1B {
            // flush a UTF-8 run
            if utf8 {
                out.push_str(&String::from_utf8_lossy(&buf));
                buf.clear();
            }
            if bytes.get(i + 1) == Some(&b'%') && bytes.get(i + 2) == Some(&b'G') {
                utf8 = true;
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'%') && bytes.get(i + 2) == Some(&b'@') {
                utf8 = false;
                i += 3;
                continue;
            }
            // skip an escape sequence: ESC, intermediates 0x20–0x2F, final 0x30–0x7E
            i += 1;
            while i < bytes.len() && (0x20..=0x2F).contains(&bytes[i]) {
                i += 1;
            }
            i += 1;
            continue;
        }
        if utf8 {
            buf.push(b);
        } else if b >= 0xA0 || b < 0x80 {
            out.push(b as char); // ASCII or Latin-1 right half
        }
        i += 1;
    }
    if utf8 {
        out.push_str(&String::from_utf8_lossy(&buf));
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn latin1_and_utf8_runs() {
        assert_eq!(super::compound_text_to_utf8(&[0xEA]), "ê");
        assert_eq!(super::compound_text_to_utf8(b"ab"), "ab");
        let mut v = b"\x1b%G".to_vec();
        v.extend_from_slice("Việt".as_bytes());
        v.extend_from_slice(b"\x1b%@x");
        assert_eq!(super::compound_text_to_utf8(&v), "Việtx");
    }
}
