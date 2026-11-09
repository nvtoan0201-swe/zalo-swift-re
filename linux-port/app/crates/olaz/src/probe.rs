//! `--engine-probe`: boots the renderer with the real bridge and prints console, state (QR replaced by
//! its size; `OLAZ_QR_OUT=<file>` writes the PNG), the first 3 calls of each bridge path and network lines.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use olaz_core::exec::RealExecutor;
use olaz_core::paths::XdgPaths;
use olaz_engine::{Services, ZaloEngine};

pub fn run(paths: XdgPaths, services: Services, args: &[String]) {
    let seconds: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(30);
    let exec = RealExecutor::new();
    let engine = ZaloEngine::new(exec.exec(), paths, services);
    engine.subscribe({
        let e = Rc::downgrade(&engine);
        move |c| {
            if let (olaz_engine::Change::Phase, Some(e)) = (c, e.upgrade()) {
                println!("phase {:?}", short_phase(&e.phase()));
            }
        }
    });
    engine.start();
    if let Some(host) = engine.host() {
        let counts: Rc<std::cell::RefCell<HashMap<String, u32>>> = Rc::default();
        let previous_state = host.on_state.borrow().clone();
        *host.on_state.borrow_mut() = Some(Rc::new(move |s: serde_json::Map<String, serde_json::Value>| {
            let mut shown = s.clone();
            if let Some(q) = s.get("qr").and_then(|v| v.as_str()) {
                if let (Some(out), Some(png)) = (std::env::var_os("OLAZ_QR_OUT"), olaz_engine::engine::decode_data_url(q)) {
                    if png.starts_with(b"\x89PNG") {
                        let _ = std::fs::write(out, png);
                    }
                }
                shown.insert("qr".into(), serde_json::Value::String(format!("<{} chars …>", q.len())));
            }
            println!("state {}", serde_json::Value::Object(shown).to_string().chars().take(500).collect::<String>());
            if let Some(p) = &previous_state {
                p(s);
            }
        }));
        let previous_console = host.on_console.borrow().clone();
        *host.on_console.borrow_mut() = Some(Rc::new(move |l: String| {
            println!("console {}", l.chars().take(300).collect::<String>());
            if let Some(p) = &previous_console {
                p(l);
            }
        }));
        let previous_trace = host.bridge.trace.borrow().clone();
        *host.bridge.trace.borrow_mut() = Some(Rc::new(move |k: &str, a: &[olaz_core::BV]| {
            let mut c = counts.borrow_mut();
            let n = c.entry(k.to_string()).or_default();
            *n += 1;
            if *n <= 3 {
                println!("bridge {k}{}", olaz_core::log::shape(a));
            }
            if let Some(p) = &previous_trace {
                p(k, a);
            }
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        exec.run_for(Duration::from_millis(100));
    }
    println!("final phase {:?}", short_phase(&engine.phase()));
    engine.shutdown();
}

fn short_phase(p: &olaz_engine::Phase) -> String {
    match p {
        olaz_engine::Phase::Login(l) => format!("Login({:?}, qr={})", l.step, l.qr_png.as_ref().map(|q| q.len()).unwrap_or(0)),
        other => format!("{other:?}"),
    }
}

/// `--dump-bridge-registry`: every answered member/channel with its kind (parity with tools/gen-bridge-registry.py).
pub fn dump_registry(paths: XdgPaths) {
    let exec = RealExecutor::new();
    let bridge = olaz_host::ZaloBridge::new(exec.exec());
    let platform = Rc::new(olaz_main::ZaloPlatform::new("0", paths.clone()));
    let _ = platform;
    // A main process needs a webhost; the registry is static, so build one against a throwaway process path.
    let main_registry = olaz_main::main_process::registry_without_host(&bridge, &paths);
    for (name, kind) in main_registry {
        println!("{kind}\t{name}");
    }
}
