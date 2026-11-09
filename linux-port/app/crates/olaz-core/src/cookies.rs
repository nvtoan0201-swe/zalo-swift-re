//! The shared, persistent cookie jar: one instance for both NetworkProxies and the
//! main process; persisted to `$XDG_DATA_HOME/olaz/cookies.json` (debounced ≤ 1 s, atomic, 0600).

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cookie_store::CookieStore;
use url::Url;

pub const SESSION_COOKIE: &str = "zpw_sek";
pub const AUTH_DOMAIN: &str = "https://wpa.zaloapp.com";
pub const SESSION_COOKIE_DOMAINS: [&str; 3] = ["zaloapp.com", "zalo.me", "chat.zalo.me"];
pub const ZALO_DOMAINS: [&str; 5] = ["zalo.me", "zaloapp.com", "zadn.vn", "zdn.vn", "zing.vn"];

pub struct CookieJar {
    store: Mutex<CookieStore>,
    file: Option<PathBuf>,
    flush_scheduled: AtomicBool,
}

impl CookieJar {
    /// In-memory jar (tests, MediaLoader-free sessions).
    pub fn memory() -> Arc<Self> {
        Arc::new(Self { store: Mutex::new(CookieStore::default()), file: None, flush_scheduled: AtomicBool::new(false) })
    }

    /// Loads (or creates) the persistent jar at `file`.
    pub fn persistent(file: &Path) -> Arc<Self> {
        let store = std::fs::File::open(file)
            .ok()
            .and_then(|f| cookie_store::serde::json::load(BufReader::new(f)).ok())
            .unwrap_or_default();
        Arc::new(Self { store: Mutex::new(store), file: Some(file.to_path_buf()), flush_scheduled: AtomicBool::new(false) })
    }

    /// Stores every `Set-Cookie` of a response to `url`.
    pub fn store_response(&self, url: &Url, set_cookie_headers: impl Iterator<Item = String>) {
        let mut changed = false;
        {
            let mut store = self.store.lock().unwrap();
            for header in set_cookie_headers {
                if let Ok(raw) = cookie::Cookie::parse(header) {
                    if store.insert_raw(&raw.into_owned(), url).is_ok() {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.changed();
        }
    }

    /// `Cookie` header value for a request to `url` (`a=b; c=d`), `None` when no cookie matches.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let store = self.store.lock().unwrap();
        let pairs: Vec<String> = store.get_request_values(url).map(|(n, v)| format!("{n}={v}")).collect();
        if pairs.is_empty() { None } else { Some(pairs.join("; ")) }
    }

    /// Names of the cookies sent to `url` (diagnostics: names only, never values).
    pub fn names_for(&self, url: &Url) -> Vec<String> {
        let store = self.store.lock().unwrap();
        let mut names: Vec<String> = store.get_request_values(url).map(|(n, _)| n.to_string()).collect();
        names.sort();
        names
    }

    pub fn value_for(&self, url: &Url, name: &str) -> Option<String> {
        let store = self.store.lock().unwrap();
        store.get_request_values(url).find(|(n, _)| *n == name).map(|(_, v)| v.to_string())
    }

    /// `ZaloMainProcess.setCookie`: always a *domain* cookie (leading `.`), path `/`, Secure,
    /// expires now + `days`, SameSite=None, HttpOnly per argument.
    pub fn set_cookie(&self, name: &str, value: &str, domain: &str, days: f64, http_only: bool) {
        let bare = domain.trim_start_matches('.');
        let dotted = format!(".{bare}");
        let expires = time::OffsetDateTime::now_utc() + time::Duration::seconds_f64(days * 86_400.0);
        let raw = cookie::Cookie::build((name.to_string(), value.to_string()))
            .domain(dotted)
            .path("/")
            .secure(true)
            .http_only(http_only)
            .same_site(cookie::SameSite::None)
            .expires(expires)
            .build();
        let Ok(url) = Url::parse(&format!("https://{bare}/")) else { return };
        let ok = self.store.lock().unwrap().insert_raw(&raw, &url).is_ok();
        if ok {
            self.changed();
        }
    }

    fn remove_where(&self, pred: impl Fn(&str, &str) -> bool) {
        let mut store = self.store.lock().unwrap();
        let doomed: Vec<(String, String, String)> = store
            .iter_any()
            .filter(|c| pred(c.name(), &domain_of(c)))
            .map(|c| (domain_of(c), String::from(&c.path), c.name().to_string()))
            .collect();
        let changed = !doomed.is_empty();
        for (d, p, n) in doomed {
            store.remove(&d, &p, &n);
        }
        drop(store);
        if changed {
            self.changed();
        }
    }

    /// Deletes every cookie named `name` (any domain).
    pub fn delete_named(&self, name: &str) {
        self.remove_where(|n, _| n == name);
    }

    /// `clearCookies`: cookies whose domain equals or is a subdomain of a Zalo domain.
    pub fn clear_zalo(&self) {
        self.remove_where(|_, d| is_zalo_domain(d));
    }

    /// `hasSession`: the cookies for `https://wpa.zaloapp.com` include a non-empty `zpw_sek`.
    pub fn has_session(&self) -> bool {
        let url = Url::parse(AUTH_DOMAIN).unwrap();
        self.value_for(&url, SESSION_COOKIE).is_some_and(|v| !v.is_empty())
    }

    /// `_electron_set-app-cookie`: exactly one `zpw_sek` per session domain.
    pub fn set_app_cookie(&self, value: &str) {
        self.delete_named(SESSION_COOKIE);
        for domain in SESSION_COOKIE_DOMAINS {
            self.set_cookie(SESSION_COOKIE, value, domain, 7.0, true);
        }
    }

    pub fn count_named(&self, name: &str) -> usize {
        self.store.lock().unwrap().iter_unexpired().filter(|c| c.name() == name).count()
    }

    /// Persists after a change. Writes are tiny and changes rare (login, cookie refresh), so the file is
    /// written right away; concurrent changes from both proxies collapse into one write.
    fn changed(&self) {
        if self.file.is_none() || self.flush_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.flush_now();
        self.flush_scheduled.store(false, Ordering::SeqCst);
    }

    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let store = self.store.lock().unwrap();
        let _ = cookie_store::serde::json::save(&store, &mut out);
        out
    }

    /// Writes the jar now (atomic, mode 0600).
    pub fn flush_now(&self) {
        let Some(file) = &self.file else { return };
        let data = self.serialize();
        if crate::paths::write_atomic(file, &data).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
        }
    }
}

fn domain_of(c: &cookie_store::Cookie<'_>) -> String {
    match &c.domain {
        cookie_store::CookieDomain::HostOnly(d) | cookie_store::CookieDomain::Suffix(d) => d.clone(),
        _ => String::new(),
    }
}

pub fn is_zalo_domain(domain: &str) -> bool {
    let d = domain.strip_prefix('.').unwrap_or(domain);
    ZALO_DOMAINS.iter().any(|z| d == *z || d.ends_with(&format!(".{z}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_cookie_reaches_realtime_and_api_hosts() {
        let jar = CookieJar::memory();
        jar.set_app_cookie("test");
        for host in ["https://ws5-msg.chat.zalo.me/", "https://chat.zalo.me/", "https://wpa.zaloapp.com/", "https://tt-profile-wpa.zaloapp.com/"] {
            assert!(jar.names_for(&Url::parse(host).unwrap()).contains(&"zpw_sek".to_string()), "{host}");
        }
        assert_eq!(jar.count_named("zpw_sek"), 3);
        assert!(jar.has_session());
        jar.set_app_cookie("again");
        assert_eq!(jar.count_named("zpw_sek"), 3);
        jar.clear_zalo();
        assert!(!jar.has_session());
        assert_eq!(jar.count_named("zpw_sek"), 0);
    }

    #[test]
    fn secure_cookie_not_sent_over_ws_scheme_but_header_for_https() {
        let jar = CookieJar::memory();
        jar.set_cookie("a", "1", "zalo.me", 1.0, false);
        let header = jar.header_for(&Url::parse("https://ws1-msg.chat.zalo.me/").unwrap());
        assert_eq!(header.as_deref(), Some("a=1"));
        assert!(jar.header_for(&Url::parse("https://example.com/").unwrap()).is_none());
    }

    #[test]
    fn response_cookies_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cookies.json");
        let jar = CookieJar::persistent(&file);
        let url = Url::parse("https://id.zalo.me/account").unwrap();
        jar.store_response(&url, vec!["zlogin_session=abc; Domain=zalo.me; Path=/; Max-Age=3600; Secure".to_string(), "tmp=1".to_string()].into_iter());
        assert_eq!(jar.value_for(&Url::parse("https://chat.zalo.me/").unwrap(), "zlogin_session").as_deref(), Some("abc"));
        jar.set_app_cookie("sek");
        jar.flush_now();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let again = CookieJar::persistent(&file);
        assert!(again.has_session());
        assert_eq!(again.value_for(&Url::parse("https://chat.zalo.me/").unwrap(), "zlogin_session").as_deref(), Some("abc"));
        // session cookies (no expiry) are not persisted, like HTTPCookieStorage
        assert!(again.value_for(&url, "tmp").is_none());
    }

    #[test]
    fn zalo_domains() {
        assert!(is_zalo_domain(".zalo.me"));
        assert!(is_zalo_domain("s120-ava-talk.zadn.vn"));
        assert!(!is_zalo_domain("notzalo.me"));
        assert!(!is_zalo_domain("example.com"));
    }
}
