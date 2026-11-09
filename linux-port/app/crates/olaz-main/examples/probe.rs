//! M0 probe: boots the official renderer (login page) with the real bridge and waits for the QR snapshot.
//! usage: OLAZ_ZALO_ASAR=<app.asar> cargo run -p olaz-main --example probe [seconds]
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use olaz_core::exec::RealExecutor;
use olaz_core::paths::XdgPaths;
use olaz_host::{WebHostConfig, WebHostProcess};
use olaz_main::secrets::MemoryStore;
use olaz_main::{MainProcess, ZaloPlatform};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(40);
    let asar = std::env::var("OLAZ_ZALO_ASAR").expect("OLAZ_ZALO_ASAR");
    let root = std::env::temp_dir().join("olaz-probe");
    let paths = XdgPaths::rooted(&root);
    let exec = RealExecutor::new();
    let webhost = WebHostProcess::spawn(
        WebHostConfig {
            binary: olaz_host::host::webhost_binary(),
            asar: asar.into(),
            data_dir: paths.webkit_data(),
            cache_dir: paths.cache().join("webkit"),
            debug: false,
        },
        exec.exec(),
    )
    .unwrap();
    *webhost.diagnostics.borrow_mut() = Some(Rc::new(|l| println!("diag {l}")));
    let platform = Rc::new(ZaloPlatform::new("26.10.20", paths.clone()));
    let jar = olaz_core::cookies::CookieJar::persistent(&paths.cookies_file());
    let main = MainProcess::new(platform, jar, Arc::new(MemoryStore::default()), webhost.clone(), exec.exec());
    let host = main.create_window();
    main.set_host(&host);
    *host.on_console.borrow_mut() = Some(Rc::new(|l| println!("console {}", l.chars().take(300).collect::<String>())));
    let got_qr = Rc::new(std::cell::Cell::new(false));
    let g = got_qr.clone();
    *host.on_state.borrow_mut() = Some(Rc::new(move |s| {
        let mut shown = s.clone();
        if let Some(q) = shown.get("qr").and_then(|v| v.as_str()).map(str::to_string) {
            if let (Some(out), true) = (std::env::var_os("OLAZ_QR_OUT"), q.starts_with("data:image/png")) {
                use base64::Engine;
                let b64 = q.split_once(',').map(|x| x.1).unwrap_or("");
                let _ = std::fs::write(out, base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default());
            }
            shown.insert("qr".into(), serde_json::Value::String(format!("<{} chars …>", q.len())));
            if s.get("event").and_then(|v| v.as_str()) == Some("qr") || q.starts_with("data:image/png") { g.set(true); }
        }
        println!("state {}", serde_json::Value::Object(shown).to_string().chars().take(400).collect::<String>());
    }));
    let m = main.clone();
    *host.on_page_event.borrow_mut() = Some(Rc::new(move |e| {
        println!("page {e:?}");
        match e {
            olaz_host::PageEvent::StartedLoading => m.page_did_start_loading(),
            olaz_host::PageEvent::FinishedLoading(p) => m.page_did_finish_loading(p),
            _ => {}
        }
    }));
    *host.bridge.trace.borrow_mut() = Some(Rc::new(|k: &str, a: &[olaz_core::BV]| {
        if !k.contains("ZLog") && !k.starts_with("$zelectron.store") {
            println!("bridge {k}{}", olaz_core::log::shape(a));
        }
    }));
    *host.network.on_request_completed.borrow_mut() = Some(Rc::new(|m: &str, u: &url::Url, s: i64| println!("net {s} {m} {}{}", u.host_str().unwrap_or(""), u.path())));
    *host.network.socket_trace.borrow_mut() = Some(Rc::new(|l| println!("{l}")));
    main.start();
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        exec.run_for(Duration::from_millis(100));
        if got_qr.get() && std::env::var_os("OLAZ_PROBE_UNTIL_QR").is_some() {
            break;
        }
    }
    println!("RESULT qr={}", got_qr.get());
    webhost.quit();
}
