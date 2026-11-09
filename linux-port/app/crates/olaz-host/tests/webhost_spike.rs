//! M0 spike: the real `olaz-webhost` (WebKitGTK) hosting probe pages served from a test asar.
//! Needs a display (Wayland or X11) and the webhost binary (`cargo build -p olaz-webhost`); skipped otherwise.
//!
//! Covers: script injection order + globals in iframes, R2 sync XHR with a 64 KB POST body answered over IPC,
//! ordering of 1000 posted messages, reply rejection, `e.code` from "CODE: detail" errors, R3 IndexedDB +
//! Worker `importScripts` on the custom scheme with persistence across a webhost restart, R1 timer cadence
//! in a hidden view, asar scheme 404 + headers, navigation policy (http link → external).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use olaz_core::cookies::CookieJar;
use olaz_core::exec::RealExecutor;
use olaz_core::value::{Args, BV};
use olaz_host::{ClientHost, Page, WebHostConfig, WebHostProcess};
use serde_json::{Map, Value};

fn make_asar(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    fn insert(node: &mut Value, parts: &[&str], leaf: Value) {
        let files = node.as_object_mut().unwrap().entry("files").or_insert_with(|| serde_json::json!({}));
        if parts.len() == 1 {
            files[parts[0]] = leaf;
        } else {
            if files.get(parts[0]).is_none() {
                files[parts[0]] = serde_json::json!({});
            }
            insert(files.get_mut(parts[0]).unwrap(), &parts[1..], leaf);
        }
    }
    let mut tree = serde_json::json!({});
    let mut payload = Vec::new();
    for (path, data) in files {
        let parts: Vec<&str> = path.split('/').collect();
        insert(&mut tree, &parts, serde_json::json!({"size": data.len(), "offset": payload.len().to_string()}));
        payload.extend_from_slice(data.as_bytes());
    }
    let json = serde_json::to_vec(&tree).unwrap();
    let padded = (json.len() + 3) & !3;
    let mut out = Vec::new();
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&((8 + padded) as u32).to_le_bytes());
    out.extend_from_slice(&((padded + 4) as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out.resize(16 + padded, 0);
    out.extend_from_slice(&payload);
    let path = dir.join("app.asar");
    std::fs::write(&path, out).unwrap();
    path
}

const PROBE: &str = r#"<!doctype html><html><head><meta charset="utf-8"></head><body>
<iframe id="f" src="frame.html"></iframe>
<a id="ext" href="https://example.com/x">x</a>
<input id="pick1" type="file"><input id="pickN" type="file" multiple>
<script>
const post = (o) => webkit.messageHandlers.zstate.postMessage(Object.assign({page: 'probe'}, o));
const globals = () => ['__zaloBridgeSpec','__zaloNet','__zaloLocal','__zaloBridge','__zaloStateAdapter'].filter(k => !window[k]);
window.addEventListener('DOMContentLoaded', () => post({event: 'globals', missing: globals()}));
window.addEventListener('message', (e) => { if (e.data && e.data.frameMissing) post({event: 'frame', missing: e.data.frameMissing}); });
(async () => {
  // R2: sync XHR with a 64 KB body, answered after an IPC round trip.
  const big = 'x'.repeat(65536);
  let ok = 0;
  for (let i = 0; i < 300; i++) { if ($ztest.echoLen(big + i) === 65536 + String(i).length) ok++; }
  post({event: 'sync', ok});
  // Ordering of 1000 posts on one channel.
  const ps = [];
  for (let n = 0; n < 1000; n++) ps.push(webkit.messageHandlers.zbridge.postMessage({kind: 'call', path: '$ztest.order', args: [n]}));
  await Promise.all(ps);
  // Reject with the handler's message; "CODE: detail" → e.code.
  let rejected = null; try { await webkit.messageHandlers.zbridge.postMessage({kind: 'call', path: '$ztest.fail', args: []}); } catch (e) { rejected = String(e && e.message || e); }
  let code = null; try { $ztest.enoent(); } catch (e) { code = e.code; }
  // bytes both ways through bridge.js
  const bytes = await $ztest.bytes(new Uint8Array([1, 2, 3]));
  // async member via bridge.js + Node callback style
  const cb = await new Promise(r => $ztest.later(7, (err, v) => r([err, v])));
  // R3: IndexedDB in the page
  const idb = await new Promise((res) => {
    const r = indexedDB.open('olaz-probe', 1);
    r.onupgradeneeded = () => r.result.createObjectStore('kv');
    r.onerror = () => res('open-error ' + r.error);
    r.onsuccess = () => {
      const db = r.result; const tx = db.transaction('kv', 'readwrite');
      const st = tx.objectStore('kv'); const g = st.get('k');
      g.onsuccess = () => { const prev = g.result || null; st.put('v' + Date.now(), 'k'); tx.oncomplete = () => res({prev}); };
      tx.onerror = () => res('tx-error');
    };
  });
  // R3: worker from the custom scheme, importScripts + IndexedDB inside it
  const worker = await new Promise((res) => {
    try {
      const w = new Worker('zalo-app://local/worker.js');
      w.onmessage = (e) => res(e.data);
      w.onerror = (e) => res('worker-error ' + (e.message || ''));
    } catch (e) { res('worker-throw ' + e); }
    setTimeout(() => res('worker-timeout'), 8000);
  });
  // 404 and headers
  const r404 = await fetch('zalo-app://local/missing.js').then(r => r.status, e => 'err ' + e);
  const hdr = await fetch('zalo-app://local/lib.js').then(r => [r.status, r.headers.get('content-type'), r.headers.get('cache-control')], e => 'err ' + e);
  post({event: 'done', rejected, code, bytes: Array.from(bytes), cb, idb, worker, r404, hdr, ua: navigator.userAgent});
  // R1: timer cadence while hidden
  let ticks = 0; const t0 = Date.now();
  const iv = setInterval(() => { ticks++; if (Date.now() - t0 >= 3000) { clearInterval(iv); post({event: 'ticks', ticks}); } }, 100);
})();
</script></body></html>"#;

const FRAME: &str = r#"<!doctype html><html><body><script>
const missing = ['__zaloBridgeSpec','__zaloNet','__zaloLocal','__zaloBridge'].filter(k => !window[k]);
parent.postMessage({frameMissing: missing}, '*');
</script></body></html>"#;

const WORKER: &str = r#"importScripts('zalo-app://local/lib.js');
const r = indexedDB.open('olaz-worker', 1);
r.onupgradeneeded = () => r.result.createObjectStore('kv');
r.onerror = () => postMessage('idb-error');
r.onsuccess = () => { const tx = r.result.transaction('kv', 'readwrite'); tx.objectStore('kv').put(1, 'a'); tx.oncomplete = () => postMessage('ok ' + self.libValue); };
"#;

fn display_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

fn webhost_bin() -> Option<PathBuf> {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/olaz-webhost");
    bin.is_file().then_some(bin)
}

struct Probe {
    exec: Rc<RealExecutor>,
    process: Rc<WebHostProcess>,
    host: Rc<ClientHost>,
    states: Rc<RefCell<Vec<Map<String, Value>>>>,
    order: Rc<RefCell<Vec<i64>>>,
    external: Rc<RefCell<Vec<String>>>,
}

fn start(dir: &Path, asar: &Path) -> Probe {
    let exec = RealExecutor::new();
    let config = WebHostConfig {
        binary: webhost_bin().unwrap(),
        asar: asar.to_path_buf(),
        data_dir: dir.join("webkit").to_path_buf(),
        cache_dir: dir.join("cache").to_path_buf(),
        debug: false,
    };
    let process = WebHostProcess::spawn(config, exec.exec()).unwrap();
    let external = Rc::new(RefCell::new(Vec::new()));
    let ext = external.clone();
    *process.on_open_external.borrow_mut() = Some(Rc::new(move |u| ext.borrow_mut().push(u)));
    let order = Rc::new(RefCell::new(Vec::new()));
    let o2 = order.clone();
    let host = ClientHost::new(&process, "ZaloPC-darwin-23v694", CookieJar::memory(), false, move |b| {
        b.sync("$ztest.echoLen", |args| Ok(BV::int(args.string(0).map(|s| s.len()).unwrap_or(0) as i64)));
        b.async_now("$ztest.order", move |args| {
            o2.borrow_mut().push(args.int(0).unwrap_or(-1));
            Ok(BV::Undefined)
        });
        b.async_now("$ztest.fail", |_| Err("x".into()));
        b.sync("$ztest.enoent", |_| Err("ENOENT: no such file".into()));
        b.sync("$ztest.bytes", |args| Ok(BV::Bytes(args.data(0).unwrap_or_default().iter().map(|b| b * 2).collect())));
        b.async_now("$ztest.later", |args| Ok(BV::int(args.int(0).unwrap_or(0) + 1)));
        b.declare_global("$zcommon");
    });
    let states = Rc::new(RefCell::new(Vec::new()));
    let s2 = states.clone();
    *host.on_state.borrow_mut() = Some(Rc::new(move |m| {
        if m.get("page").and_then(Value::as_str) == Some("probe") {
            s2.borrow_mut().push(m);
        }
    }));
    Probe { exec, process, host, states, order, external }
}

impl Probe {
    fn wait_for(&self, event: &str, timeout: Duration) -> Option<Map<String, Value>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(s) = self.states.borrow().iter().find(|s| s.get("event").and_then(Value::as_str) == Some(event)) {
                return Some(s.clone());
            }
            self.exec.run_for(Duration::from_millis(50));
        }
        None
    }
}

#[test]
fn m0_spike_webkit_gtk() {
    if !display_available() || webhost_bin().is_none() {
        eprintln!("skipped: needs a display and target/debug/olaz-webhost");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let asar = make_asar(
        dir.path(),
        &[
            ("pc-dist/probe.html", PROBE),
            ("pc-dist/frame.html", FRAME),
            ("pc-dist/worker.js", WORKER),
            ("pc-dist/lib.js", "self.libValue = 42;"),
        ],
    );

    let p = start(dir.path(), &asar);
    p.host.load(Page::Main, &BTreeMap::new());
    // `load` always targets index.html; navigate the view to the probe page directly.
    p.process.send(olaz_ipc::AppToHost::Load { view: p.host.view.get(), url: "zalo-app://local/probe.html".into() });

    let globals = p.wait_for("globals", Duration::from_secs(20)).expect("globals snapshot");
    assert_eq!(globals["missing"], serde_json::json!([]), "injected globals at DOMContentLoaded");
    let frame = p.wait_for("frame", Duration::from_secs(10)).expect("iframe snapshot");
    assert_eq!(frame["missing"], serde_json::json!([]), "injected globals in child iframes");

    let sync = p.wait_for("sync", Duration::from_secs(60)).expect("R2 sync XHR");
    assert_eq!(sync["ok"], 300, "R2: every 64 KB sync XHR answered");

    let done = p.wait_for("done", Duration::from_secs(30)).expect("probe done");
    assert_eq!(*p.order.borrow(), (0..1000).collect::<Vec<i64>>(), "1000 posts arrive in order");
    assert_eq!(done["rejected"], "x", "reply error rejects with the handler message");
    assert_eq!(done["code"], "ENOENT");
    assert_eq!(done["bytes"], serde_json::json!([2, 4, 6]));
    assert_eq!(done["cb"], serde_json::json!([null, 8]));
    assert_eq!(done["idb"], serde_json::json!({"prev": null}), "R3: IndexedDB in page");
    assert_eq!(done["worker"], "ok 42", "R3: worker importScripts + IndexedDB on zalo-app://");
    assert_eq!(done["r404"], 404);
    assert_eq!(done["hdr"], serde_json::json!([200, "text/javascript; charset=utf-8", "no-cache"]));
    assert_eq!(done["ua"], "ZaloPC-darwin-23v694", "R8: user agent");

    let ticks = p.wait_for("ticks", Duration::from_secs(15)).expect("R1 ticks");
    let n = ticks["ticks"].as_i64().unwrap();
    assert!(n >= 27, "R1: hidden view timers throttled ({n} ticks of 100 ms in 3 s)");

    // Navigation policy: an http(s) link click in the main frame opens externally.
    let fut = p.host.evaluate("document.getElementById('ext').click(); return 1;");
    let _ = p.exec.block_on(fut);
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.external.borrow().is_empty() && Instant::now() < deadline {
        p.exec.run_for(Duration::from_millis(50));
    }
    assert_eq!(*p.external.borrow(), vec!["https://example.com/x".to_string()]);
    let still = p.exec.block_on(p.host.evaluate("return location.href"));
    assert_eq!(still, Some(Value::String("zalo-app://local/probe.html".into())));

    // evaluate returns JSON results; errors become None.
    assert_eq!(p.exec.block_on(p.host.evaluate("return {a: [1, 'b']}")), Some(serde_json::json!({"a": [1, "b"]})));
    assert_eq!(p.exec.block_on(p.host.evaluate("throw new Error('boom')")), None);
    assert_eq!(p.exec.block_on(p.host.evaluate("await new Promise(r => setTimeout(r, 50)); return 5")), Some(serde_json::json!(5)));

    // R5: a programmatic click inside the API evaluation opens the chooser, answered with the pending files.
    let f1 = dir.path().join("a.txt");
    let f2 = dir.path().join("b.png");
    std::fs::write(&f1, b"hello").unwrap();
    std::fs::write(&f2, b"\x89PNG").unwrap();
    let pick = |id: &str| {
        format!(
            "const i = document.getElementById('{id}'); const done = new Promise(r => i.addEventListener('change', () => r(Array.from(i.files).map(f => f.name + ':' + f.size)), {{once: true}})); i.click(); return await Promise.race([done, new Promise(r => setTimeout(() => r('timeout'), 4000))]);"
        )
    };
    let multi = p.exec.block_on(p.host.evaluate_providing_files(&pick("pickN"), vec![f1.clone(), f2.clone()]));
    assert_eq!(multi, Some(serde_json::json!(["a.txt:5", "b.png:4"])), "R5 multiple");
    let single = p.exec.block_on(p.host.evaluate_providing_files(&pick("pick1"), vec![f1.clone(), f2.clone()]));
    assert_eq!(single, Some(serde_json::json!(["a.txt:5"])), "R5 single input takes the first file");

    // R3 persistence: a new webhost process sees the IndexedDB value written by the first.
    p.process.quit();
    drop(p);
    let p2 = start(dir.path(), &asar);
    p2.process.send(olaz_ipc::AppToHost::Load { view: p2.host.view.get(), url: "zalo-app://local/probe.html".into() });
    let done2 = p2.wait_for("done", Duration::from_secs(60)).expect("second run");
    assert!(done2["idb"]["prev"].as_str().is_some_and(|v| v.starts_with('v')), "R3: IndexedDB persisted: {:?}", done2["idb"]);
    p2.process.quit();
    let _ = Arc::new(());
}
