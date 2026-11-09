//! Serving `pc-dist/` out of app.asar for `zalo-app://local/<path>`. Port of `AsarSchemeHandler`.
//! Pure: the webhost turns a [`Served`] into a WebKit scheme response.

use crate::asar::AsarArchive;
use crate::scripts;

pub const SCHEME: &str = "zalo-app";
pub const BRIDGE_SYNC_PATH: &str = "/__olaz/bridge-sync";

#[derive(Debug, Clone, PartialEq)]
pub struct SchemeResponse {
    pub status: u16,
    /// Exactly the 4 headers the macOS handler sends for 200; none for 404.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum Served {
    /// `/__olaz/bridge-sync`: the body goes to `ZaloBridge::handle_sync_request` (never looked up in the asar).
    BridgeSync,
    Response(SchemeResponse),
}

/// Result of a patch: the bytes to serve and, when the original was served, the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub bytes: Vec<u8>,
    pub unpatched_reason: Option<&'static str>,
}

impl Patch {
    fn unpatched(data: &[u8], why: &'static str) -> Self {
        Patch { bytes: data.to_vec(), unpatched_reason: Some(why) }
    }
}

/// Serves a request for the URL path `path` (as `URL.path()`: starts with `/`).
/// `debug` selects debug-build behaviour (worker eval hook + the insertToDb-failed forwarder).
/// `log` receives host log lines (404s, unpatched files) — the caller forwards them to EngineLog/diagnostics.
pub fn serve(asar: &AsarArchive, path: &str, debug: bool, log: &mut dyn FnMut(String)) -> Served {
    if path == BRIDGE_SYNC_PATH {
        return Served::BridgeSync;
    }
    let candidates = [format!("pc-dist{path}"), path.strip_prefix('/').unwrap_or(path).to_string()];
    let Some(data) = candidates.iter().find_map(|c| asar.read(c)) else {
        log(format!("[404] {path}"));
        return Served::Response(SchemeResponse { status: 404, headers: vec![], body: vec![] });
    };
    let mut body = data.into_owned();
    if path.starts_with("/dal-worker.") && path.ends_with(".js") {
        let mut out = worker_prelude(debug).into_bytes();
        out.extend_from_slice(&body);
        body = out;
    }
    if path.starts_with("/sync-v2-sub-worker.") && path.ends_with(".js") {
        let patch = expose_webpack_require(&body);
        if let Some(why) = patch.unpatched_reason {
            log(format!("host: sync-v2-sub-worker served unpatched ({why}) — DAL imports may fail"));
        }
        body = patch.bytes;
    }
    if path.starts_with("/lazy/main-startup.") && path.ends_with(".js") {
        let patch = patch_sync_global_id_resolver(&body);
        if let Some(why) = patch.unpatched_reason {
            log(format!(
                "host: main-startup served without global-id fallback ({why}) — unmappable 1-1 conversations will fail sync"
            ));
        }
        body = patch.bytes;
    }
    if debug && path.starts_with("/sync-v2-sub-worker.") && path.ends_with(".js") {
        // Debug builds only: surface the sub-worker's swallowed import error.
        let needle = b"return this.logger.zsymb(18,\"CytfSU\",";
        if let Some(at) = find(&body, needle, 0) {
            let insert = b"self.__olazFwd&&self.__olazFwd('insertToDb-failed',e&&((e.name||'')+': '+(e.message||e)+' | '+String(e.stack||'').split('\\n').slice(0,6).join(' < ')));";
            body.splice(at..at, insert.iter().copied());
        }
    }
    Served::Response(ok_response(path, body))
}

/// 200 with exactly the 4 headers of the macOS handler.
pub fn ok_response(path: &str, body: Vec<u8>) -> SchemeResponse {
    SchemeResponse { status: 200, headers: headers_for(mime_for(path), body.len()), body }
}

pub fn json_response(body: Vec<u8>) -> SchemeResponse {
    SchemeResponse { status: 200, headers: headers_for("application/json".into(), body.len()), body }
}

fn headers_for(mime: String, len: usize) -> Vec<(String, String)> {
    vec![
        ("Content-Type".into(), mime),
        ("Content-Length".into(), len.to_string()),
        ("Access-Control-Allow-Origin".into(), "*".into()),
        ("Cache-Control".into(), "no-cache".into()),
    ]
}

pub fn mime_for(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => String::new(),
    };
    match ext.as_str() {
        "js" | "jsapi" => "text/javascript; charset=utf-8".into(),
        "wasm" => "application/wasm".into(),
        "json" => "application/json".into(),
        "html" => "text/html; charset=utf-8".into(),
        "css" => "text/css; charset=utf-8".into(),
        "" => "application/octet-stream".into(),
        other => mime_guess::from_ext(other).first().map(|m| m.essence_str().to_string()).unwrap_or_else(|| "application/octet-stream".into()),
    }
}

/// Prepended to `dal-worker*.js`.
pub fn worker_prelude(debug: bool) -> String {
    let base = scripts::shared_script(scripts::DAL_WORKER_PRELUDE);
    base.replace("/*DEBUG_EVAL*/", if debug { scripts::shared_script(scripts::DAL_WORKER_DEBUG_EVAL) } else { "" })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// Inserts `self.__swReq=<ident>;` before `<ident>.e=function(e){var t=[],n=a[e]` (byte-level).
pub fn expose_webpack_require(data: &[u8]) -> Patch {
    let tail = b"=function(e){var t=[],n=a[e]";
    let Some(at) = find(data, tail, 0) else { return Patch::unpatched(data, "runtime shape not found") };
    if find(data, tail, at + tail.len()).is_some() {
        return Patch::unpatched(data, "runtime shape ambiguous");
    }
    if at < 2 || data[at - 1] != b'e' || data[at - 2] != b'.' {
        return Patch::unpatched(data, "no .e accessor");
    }
    let ident_end = at - 2;
    let mut ident_start = ident_end;
    while ident_start > 0 && is_ident(data[ident_start - 1]) {
        ident_start -= 1;
    }
    if !(ident_start < ident_end)
        || data[ident_start].is_ascii_digit()
        || ident_start == 0
        || !(data[ident_start - 1] == b'}' || data[ident_start - 1] == b';')
    {
        return Patch::unpatched(data, "not at a statement boundary");
    }
    let ident = &data[ident_start..ident_end];
    let mut out = Vec::with_capacity(data.len() + ident.len() + 16);
    out.extend_from_slice(&data[..ident_start]);
    out.extend_from_slice(b"self.__swReq=");
    out.extend_from_slice(ident);
    out.push(b';');
    out.extend_from_slice(&data[ident_start..]);
    Patch { bytes: out, unpatched_reason: None }
}

/// DEVIATION FROM OFFICIAL: the sync-v2 global-id resolver also asks
/// `window.__swzGlobalIdFallback` for ids `getminiprofiles` left unmapped.
pub fn patch_sync_global_id_resolver(data: &[u8]) -> Patch {
    let Ok(source) = std::str::from_utf8(data) else { return Patch::unpatched(data, "not utf8") };
    // Hand-written matcher for the global-id resolver pattern (no regex dependency).
    //   (ident)\.resolveWithRetry\((ident),\((ident)=>(ident)\.default\.getGlobalIdsFromUserIds\(\3\)\)\)
    // (a backtracking regex hits its limit on the 3 MB chunk). Leftmost-match semantics as the regex.
    let mut matches = Vec::new();
    let needle = ".resolveWithRetry(";
    let mut from = 0;
    while let Some(rel) = source[from..].find(needle) {
        let at = from + rel;
        if let Some(m) = match_resolver(source, at) {
            from = m.end;
            matches.push(m);
            if matches.len() > 1 {
                break;
            }
        } else {
            from = at + needle.len();
        }
    }
    if matches.len() != 1 {
        return Patch::unpatched(data, if matches.is_empty() { "resolver shape not found" } else { "resolver shape ambiguous" });
    }
    let m = &matches[0];
    let (svc, ids, arg, mgr) = (&source[m.svc.clone()], &source[m.ids.clone()], &source[m.arg.clone()], &source[m.mgr.clone()]);
    let replacement = format!(
        "{svc}.resolveWithRetry({ids},(async {arg}=>{{let __r=await {mgr}.default.getGlobalIdsFromUserIds({arg})||{{}};\
const __m={arg}.filter(__i=>!__r[__i]),__f=window.__swzGlobalIdFallback;\
if(__m.length&&typeof __f===\"function\")try{{__r=Object.assign({{}},__r,await __f(__m))}}catch(__e){{}}return __r}}))"
    );
    let mut out = String::with_capacity(source.len() + replacement.len());
    out.push_str(&source[..m.start]);
    out.push_str(&replacement);
    out.push_str(&source[m.end..]);
    Patch { bytes: out.into_bytes(), unpatched_reason: None }
}

struct ResolverMatch {
    start: usize,
    end: usize,
    svc: std::ops::Range<usize>,
    ids: std::ops::Range<usize>,
    arg: std::ops::Range<usize>,
    mgr: std::ops::Range<usize>,
}

fn ident_end(s: &[u8], at: usize) -> Option<usize> {
    if at >= s.len() || !(s[at].is_ascii_alphabetic() || s[at] == b'_' || s[at] == b'$') {
        return None;
    }
    let mut i = at + 1;
    while i < s.len() && is_ident(s[i]) {
        i += 1;
    }
    Some(i)
}

fn expect(s: &[u8], at: usize, lit: &str) -> Option<usize> {
    s.get(at..at + lit.len()).filter(|w| *w == lit.as_bytes()).map(|_| at + lit.len())
}

/// Tries the resolver shape around the `.resolveWithRetry(` at byte `dot`.
fn match_resolver(source: &str, dot: usize) -> Option<ResolverMatch> {
    let s = source.as_bytes();
    // group 1: the leftmost start in the identifier run before `dot` that begins with [A-Za-z_$].
    let mut run = dot;
    while run > 0 && is_ident(s[run - 1]) {
        run -= 1;
    }
    let svc_start = (run..dot).find(|&i| !s[i].is_ascii_digit())?;
    let mut i = expect(s, dot, ".resolveWithRetry(")?;
    let ids_end = ident_end(s, i)?;
    let ids = i..ids_end;
    i = expect(s, ids_end, ",(")?;
    let arg_end = ident_end(s, i)?;
    let arg = i..arg_end;
    i = expect(s, arg_end, "=>")?;
    let mgr_end = ident_end(s, i)?;
    let mgr = i..mgr_end;
    i = expect(s, mgr_end, ".default.getGlobalIdsFromUserIds(")?;
    i = expect(s, i, &source[arg.clone()])?;
    let end = expect(s, i, ")))")?;
    Some(ResolverMatch { start: svc_start, end, svc: svc_start..dot, ids, arg, mgr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asar::tests::make_archive;

    const RESOLVER: &str = "var ZS={};ZS.resolveWithRetry=async(e,t)=>{let s=await t(e)||{},n=e.filter((e=>!s[e]));for(let i=1;i<=2&&n.length>0;i++){const a=await t(n)||{};s=Object.assign({},s,a);n=e.filter((e=>!s[e]))}return s};\nZS.oneToOnePCNoised2gNoised=async e=>{try{return e&&Array.isArray(e)&&0!==e.length?await ZS.resolveWithRetry(e,(e=>Xr.default.getGlobalIdsFromUserIds(e))):{}}catch(t){throw t}};\n";

    fn node() -> Option<std::path::PathBuf> {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("node")).find(|n| n.is_file()))
    }

    /// Runs the (patched) resolver in Node with a profile service that only knows `known`.
    fn run(patched: &str, known: &str, fallback: Option<&str>) -> Option<serde_json::Value> {
        let node = node()?;
        let script = format!(
            "var window=globalThis;var Xr={{default:{{getGlobalIdsFromUserIds:async(ids)=>{{const k={known};const o={{}};for(const i of ids)if(k[i])o[i]=k[i];return o}}}}}};\
             {}\n{patched}\nZS.oneToOnePCNoised2gNoised(['1111111111','2222222222']).then(r=>console.log(JSON.stringify(r)));",
            fallback.map(|f| format!("window.__swzGlobalIdFallback={f};")).unwrap_or_default()
        );
        let out = std::process::Command::new(node).arg("-e").arg(script).output().ok()?;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).ok()
    }

    #[test]
    fn patched_resolver_merges_fallback_for_unmapped_ids() {
        let patched = patch_sync_global_id_resolver(RESOLVER.as_bytes());
        assert_eq!(patched.unpatched_reason, None);
        let text = String::from_utf8(patched.bytes).unwrap();
        assert!(text.contains("__swzGlobalIdFallback"));
        assert!(!text.contains("(e=>Xr.default.getGlobalIdsFromUserIds(e))"));
        if let Some(r) = run(&text, "{'1111111111':'gA'}", Some("async (ids)=>({[ids[0]]:'gB'})")) {
            assert_eq!(r, serde_json::json!({"1111111111": "gA", "2222222222": "gB"}));
        }
    }

    #[test]
    fn patched_resolver_is_harmless_without_hook_or_when_hook_fails() {
        let text = String::from_utf8(patch_sync_global_id_resolver(RESOLVER.as_bytes()).bytes).unwrap();
        if let Some(r) = run(&text, "{'1111111111':'gA'}", None) {
            assert_eq!(r, serde_json::json!({"1111111111": "gA"}));
        }
        if let Some(r) = run(&text, "{'1111111111':'gA'}", Some("async()=>{throw new Error('x')}")) {
            assert_eq!(r, serde_json::json!({"1111111111": "gA"}));
        }
    }

    #[test]
    fn global_id_patch_replacement_is_exact() {
        let src = "x;A.resolveWithRetry(b,(c=>D.default.getGlobalIdsFromUserIds(c)));y";
        let out = String::from_utf8(patch_sync_global_id_resolver(src.as_bytes()).bytes).unwrap();
        assert_eq!(
            out,
            "x;A.resolveWithRetry(b,(async c=>{let __r=await D.default.getGlobalIdsFromUserIds(c)||{};const __m=c.filter(__i=>!__r[__i]),__f=window.__swzGlobalIdFallback;if(__m.length&&typeof __f===\"function\")try{__r=Object.assign({},__r,await __f(__m))}catch(__e){}return __r}));y"
        );
        // \3 backreference: a different argument name does not match.
        let other = "A.resolveWithRetry(b,(c=>D.default.getGlobalIdsFromUserIds(d)))";
        assert_eq!(patch_sync_global_id_resolver(other.as_bytes()).unpatched_reason, Some("resolver shape not found"));
    }

    #[test]
    fn ambiguous_or_missing_shape_is_served_unchanged() {
        let none = patch_sync_global_id_resolver(b"var a=1;");
        assert_eq!(none.bytes, b"var a=1;");
        assert_eq!(none.unpatched_reason, Some("resolver shape not found"));
        let twice = format!("{RESOLVER}{RESOLVER}");
        let p = patch_sync_global_id_resolver(twice.as_bytes());
        assert_eq!(p.bytes, twice.as_bytes());
        assert_eq!(p.unpatched_reason, Some("resolver shape ambiguous"));
        let bad = patch_sync_global_id_resolver(&[0xff, 0xfe]);
        assert_eq!(bad.unpatched_reason, Some("not utf8"));
    }

    #[test]
    fn expose_webpack_require_success() {
        let src = b"foo();}n.e=function(e){var t=[],n=a[e];bar";
        let p = expose_webpack_require(src);
        assert_eq!(p.unpatched_reason, None);
        assert_eq!(p.bytes, b"foo();}self.__swReq=n;n.e=function(e){var t=[],n=a[e];bar");
        let src2 = b"x;$r_1.e=function(e){var t=[],n=a[e]";
        assert_eq!(expose_webpack_require(src2).bytes, b"x;self.__swReq=$r_1;$r_1.e=function(e){var t=[],n=a[e]");
    }

    #[test]
    fn expose_webpack_require_unpatched_reasons() {
        let cases: [(&[u8], &str); 5] = [
            (b"nothing here", "runtime shape not found"),
            (b";n.e=function(e){var t=[],n=a[e];n.e=function(e){var t=[],n=a[e]", "runtime shape ambiguous"),
            (b";n.x=function(e){var t=[],n=a[e]", "no .e accessor"),
            (b";1n.e=function(e){var t=[],n=a[e]", "not at a statement boundary"),
            (b"(n.e=function(e){var t=[],n=a[e]", "not at a statement boundary"),
        ];
        for (src, why) in cases {
            let p = expose_webpack_require(src);
            assert_eq!(p.unpatched_reason, Some(why), "{}", String::from_utf8_lossy(src));
            assert_eq!(p.bytes, src);
        }
        // empty identifier, and identifier at the very start of the file
        assert_eq!(expose_webpack_require(b";.e=function(e){var t=[],n=a[e]").unpatched_reason, Some("not at a statement boundary"));
        assert_eq!(expose_webpack_require(b"n.e=function(e){var t=[],n=a[e]").unpatched_reason, Some("not at a statement boundary"));
    }

    #[test]
    fn mime_table() {
        assert_eq!(mime_for("/a/b.JS"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("/x.jsapi"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("/x.wasm"), "application/wasm");
        assert_eq!(mime_for("/x.json"), "application/json");
        assert_eq!(mime_for("/index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("/s.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("/i.png"), "image/png");
        assert_eq!(mime_for("/noext"), "application/octet-stream");
        assert_eq!(mime_for("/x.zzzunknown"), "application/octet-stream");
    }

    #[test]
    fn serve_paths_headers_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_archive(
            dir.path(),
            &[
                ("pc-dist/login.html", b"<html>"),
                ("pc-dist/dal-worker.abc.js", b"WORKER"),
                ("pc-dist/sync-v2-sub-worker.1.js", b";n.e=function(e){var t=[],n=a[e]"),
                ("pc-dist/lazy/main-startup.2.js", b"var a=1;"),
                ("other/root.txt", b"r"),
            ],
        );
        let asar = AsarArchive::open(&path).unwrap();
        let mut logs = Vec::new();
        let mut log = |s: String| logs.push(s);
        assert_eq!(serve(&asar, BRIDGE_SYNC_PATH, false, &mut log), Served::BridgeSync);
        let Served::Response(r) = serve(&asar, "/login.html", false, &mut log) else { panic!() };
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"<html>");
        assert_eq!(
            r.headers,
            vec![
                ("Content-Type".to_string(), "text/html; charset=utf-8".to_string()),
                ("Content-Length".to_string(), "6".to_string()),
                ("Access-Control-Allow-Origin".to_string(), "*".to_string()),
                ("Cache-Control".to_string(), "no-cache".to_string()),
            ]
        );
        // Second candidate: path inside the asar root.
        let Served::Response(r) = serve(&asar, "/other/root.txt", false, &mut log) else { panic!() };
        assert_eq!(r.body, b"r");
        let Served::Response(r) = serve(&asar, "/missing.js", false, &mut log) else { panic!() };
        assert_eq!((r.status, r.headers.len(), r.body.len()), (404, 0, 0));
        let Served::Response(r) = serve(&asar, "/dal-worker.abc.js", false, &mut log) else { panic!() };
        let text = String::from_utf8(r.body).unwrap();
        assert!(text.starts_with("(() => {"), "{}", &text[..40]);
        assert!(text.ends_with("})();\n\nWORKER"));
        assert!(!text.contains("DEBUG_EVAL") && !text.contains("__olazEval"));
        let Served::Response(r) = serve(&asar, "/dal-worker.abc.js", true, &mut log) else { panic!() };
        assert!(String::from_utf8(r.body).unwrap().contains("__olazEval"));
        let Served::Response(r) = serve(&asar, "/sync-v2-sub-worker.1.js", false, &mut log) else { panic!() };
        assert_eq!(r.body, b";self.__swReq=n;n.e=function(e){var t=[],n=a[e]");
        let Served::Response(r) = serve(&asar, "/lazy/main-startup.2.js", false, &mut log) else { panic!() };
        assert_eq!(r.body, b"var a=1;");
        assert_eq!(logs[0], "[404] /missing.js");
        assert!(logs[1].starts_with("host: main-startup served without global-id fallback (resolver shape not found)"));
    }

    /// With the real pinned asar (OLAZ_ZALO_ASAR or the dev cache), all 3 patches apply.
    #[test]
    fn real_asar_patches_apply() {
        let Some(asar) = crate::scripts::tests::dev_asar() else { return };
        for (prefix, which) in [("pc-dist/sync-v2-sub-worker.", 1), ("pc-dist/lazy/main-startup.", 2)] {
            let paths: Vec<_> = asar.paths_with_prefix(prefix).into_iter().filter(|p| p.ends_with(".js")).collect();
            assert!(!paths.is_empty(), "{prefix} missing");
            for p in paths {
                let data = asar.read(&p).unwrap();
                let patch = if which == 1 { expose_webpack_require(&data) } else { patch_sync_global_id_resolver(&data) };
                assert_eq!(patch.unpatched_reason, None, "{p}");
                if which == 2 {
                    assert_eq!(String::from_utf8(patch.bytes).unwrap().matches("__swzGlobalIdFallback").count(), 1);
                }
            }
        }
        assert!(!asar.paths_with_prefix("pc-dist/dal-worker.").is_empty());
    }
}
