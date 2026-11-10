//! Identity and header constants of the official Zalo PC build Olaz pins. Port of `ClientFingerprint.swift`.
//!
//! Every value was captured passively from the official client 26.10.20 (Electron 22.3.9 / Chromium 108.0.5359.215) —
//! see `reanalysis/wire-fingerprint.md` upstream. Zalo sees these on the wire, so they must match the official client
//! exactly; re-capture when the pinned runtime is bumped.

use std::sync::RwLock;

/// `getClientVersion()` of the official main process, and the `?client_version=` the page query carries. Overridden at
/// boot with the value found in the installed runtime (`set_client_version`).
pub const DEFAULT_CLIENT_VERSION: &str = crate::CLIENT_VERSION;

/// The client-hints brand list of Chromium 108 as Electron builds it (no "Google Chrome" brand).
pub const SEC_CH_UA: &str = r#""Not?A_Brand";v="8", "Chromium";v="108""#;
pub const SEC_CH_UA_MOBILE: &str = "?0";
/// The identity is the macOS client's (`ZaloPC-darwin-…`): the platform hint must agree.
pub const SEC_CH_UA_PLATFORM: &str = r#""macOS""#;
/// Chromium's Accept-Encoding.
pub const ACCEPT_ENCODING: &str = "gzip, deflate, br";
/// Chromium's Accept for `<img>` loads.
pub const IMAGE_ACCEPT: &str = "image/webp,image/apng,image/*,*/*;q=0.8";

static CLIENT_VERSION: RwLock<String> = RwLock::new(String::new());

/// The client version of the runtime in use (`DEFAULT_CLIENT_VERSION` until boot sets it).
pub fn client_version() -> String {
    let v = CLIENT_VERSION.read().map(|v| v.clone()).unwrap_or_default();
    if v.is_empty() { DEFAULT_CLIENT_VERSION.to_string() } else { v }
}

pub fn set_client_version(version: &str) {
    if let Ok(mut v) = CLIENT_VERSION.write() {
        *v = version.to_string();
    }
}

/// The whole User-Agent official main sets: `ZaloPC-darwin-<type>v<clientVersion>`.
pub fn user_agent() -> String {
    crate::user_agent(crate::CLIENT_TYPE, &client_version())
}

/// `getClientVersion:function(){return 694}` in the runtime's `main-dist/main.js`.
pub fn client_version_from_main_js(source: &[u8]) -> Option<String> {
    const NEEDLE: &[u8] = b"getClientVersion:function(){return ";
    let start = source.windows(NEEDLE.len()).position(|w| w == NEEDLE)? + NEEDLE.len();
    let digits: Vec<u8> = source[start..].iter().take_while(|b| b.is_ascii_digit()).copied().collect();
    (!digits.is_empty() && source.get(start + digits.len()) == Some(&b'}')).then(|| String::from_utf8(digits).unwrap())
}

/// Chromium's Accept-Language from the user's languages (`LANGUAGE`, then `LC_ALL` / `LC_MESSAGES` / `LANG`). The official
/// client on a single-language system sends exactly that language (`en-US`, no q-values); further entries get descending
/// q-values.
pub fn accept_language() -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let raw = env("LANGUAGE")
        .or_else(|| env("LC_ALL"))
        .or_else(|| env("LC_MESSAGES"))
        .or_else(|| env("LANG"))
        .unwrap_or_default();
    accept_language_from(&raw)
}

pub fn accept_language_from(raw: &str) -> String {
    let mut langs: Vec<String> = Vec::new();
    for part in raw.split(':') {
        // `vi_VN.UTF-8@euro` → `vi-VN`
        let tag = part.split(['.', '@']).next().unwrap_or("").replace('_', "-");
        if tag.is_empty() || tag == "C" || tag == "POSIX" || langs.contains(&tag) {
            continue;
        }
        langs.push(tag);
    }
    if langs.is_empty() {
        return "en-US".into();
    }
    langs
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, l)| if i == 0 { l.clone() } else { format!("{l};q={:.1}", 1.0 - 0.1 * i as f64) })
        .collect::<Vec<_>>()
        .join(",")
}

/// Which Chromium load a request corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// An API XHR of the renderer (no Accept of ours: the page sets its own).
    Api,
    /// An `<img>` load: no-cors, image Accept.
    Image,
    /// A `fetch()` by the renderer: cors mode.
    Cors,
}

/// Headers Chromium 108 adds to the equivalent load. The official client sent exactly these on its API XHRs — notably
/// **no** `Origin` and no `Referer`. Callers only fill in names they did not set themselves.
pub fn client_headers(flavour: Flavour) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("sec-ch-ua", SEC_CH_UA.to_string()),
        ("sec-ch-ua-mobile", SEC_CH_UA_MOBILE.to_string()),
        ("sec-ch-ua-platform", SEC_CH_UA_PLATFORM.to_string()),
    ];
    match flavour {
        Flavour::Api => {
            h.push(("sec-fetch-dest", "empty".into()));
            h.push(("sec-fetch-mode", "cors".into()));
        }
        Flavour::Image => {
            h.push(("Accept", IMAGE_ACCEPT.into()));
            h.push(("sec-fetch-dest", "image".into()));
            h.push(("sec-fetch-mode", "no-cors".into()));
        }
        Flavour::Cors => {
            h.push(("Accept", "*/*".into()));
            h.push(("sec-fetch-dest", "empty".into()));
            h.push(("sec-fetch-mode", "cors".into()));
        }
    }
    h.push(("sec-fetch-site", "cross-site".into()));
    h.push(("accept-language", accept_language()));
    h.push(("accept-encoding", ACCEPT_ENCODING.into()));
    h
}

/// Chromium's own multipart boundary: `----WebKitFormBoundary` + 16 chars of `[0-9A-Za-z]` (Blink's alphabet). The label
/// is server-visible, so it must be the official client's.
pub fn chromium_form_boundary() -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("random");
    let tail: String = bytes.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect();
    format!("----WebKitFormBoundary{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_matches_chromium() {
        let b = chromium_form_boundary();
        assert_eq!(b.len(), 22 + 16);
        assert!(b.starts_with("----WebKitFormBoundary"));
        assert!(b[22..].bytes().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(b, chromium_form_boundary());
    }

    #[test]
    fn languages() {
        assert_eq!(accept_language_from("vi_VN.UTF-8"), "vi-VN");
        assert_eq!(accept_language_from("vi_VN:en_US:en"), "vi-VN,en-US;q=0.9,en;q=0.8");
        assert_eq!(accept_language_from("C.UTF-8"), "en-US");
        assert_eq!(accept_language_from(""), "en-US");
    }

    #[test]
    fn version_from_main_js() {
        assert_eq!(client_version_from_main_js(b"x,getClientVersion:function(){return 701},y").as_deref(), Some("701"));
        assert_eq!(client_version_from_main_js(b"getClientVersion:function(){return a}"), None);
        assert_eq!(client_version_from_main_js(b"nothing"), None);
    }

    #[test]
    fn headers_have_no_origin() {
        for f in [Flavour::Api, Flavour::Image, Flavour::Cors] {
            assert!(client_headers(f).iter().all(|(n, _)| !n.eq_ignore_ascii_case("origin") && !n.eq_ignore_ascii_case("referer")));
        }
    }
}
