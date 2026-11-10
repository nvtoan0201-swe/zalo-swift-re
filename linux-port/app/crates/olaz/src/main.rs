//! `olaz` — Olaz for Linux. Wires the engine, the webhost and the GPUI interface.
//!
//! CLI: `olaz` (app) · `olaz --engine-probe [login|main] [seconds]` · `olaz --dump-bridge-registry`.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use olaz_core::paths::XdgPaths;
use olaz_engine::Services;

fn services(debug: bool) -> Services {
    Services {
        media: None,
            secrets: Arc::new(olaz_main::secrets::SecretServiceStore),
        open_url: Rc::new(|url: &str| {
            if url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
            }
        }),
        badge: None,
        webhost_binary: olaz_host::host::webhost_binary(),
        debug,
    }
}

fn paths() -> XdgPaths {
    match std::env::var_os("OLAZ_HOME") {
        Some(root) => XdgPaths::rooted(&PathBuf::from(root)),
        None => XdgPaths::from_env(),
    }
}

/// GPUI / wgpu diagnostics (adapter choice, surface alpha mode, warnings) → engine.log.
struct RenderLog;

impl log::Log for RenderLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let t = m.target();
        (t.starts_with("gpui") || t.starts_with("wgpu")) && (m.level() <= log::Level::Warn || (m.level() == log::Level::Info && t.starts_with("gpui")))
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            olaz_core::log::write(format!("render {} {}: {}", r.level(), r.target(), r.args()));
        }
    }
    fn flush(&self) {}
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = paths();
    olaz_core::log::EngineLog::install(olaz_core::log::EngineLog::open(&paths.logs()));
    if log::set_logger(&RenderLog).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let debug = cfg!(debug_assertions);
    match args.first().map(String::as_str) {
        Some("--engine-probe") => probe::run(paths, services(debug), &args[1..]),
        Some("--dump-bridge-registry") => probe::dump_registry(paths),
        Some("--version") => println!("olaz {}", env!("CARGO_PKG_VERSION")),
        // Scene renders: debug builds only, always with a throwaway OLAZ_HOME (see tools/visual-run.py).
        Some("--ui-render") if debug && args.get(1).map(String::as_str) == Some("--list") => {
            for id in olaz_ui::render_scene::CATALOG {
                println!("{id}");
            }
        }
        Some("--ui-render") if debug => match olaz_ui::render_scene::parse_args(&args[1..]) {
            Ok(scene) => olaz_ui::run(olaz_ui::UiConfig { paths, services: services(debug), render: Some(scene) }),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        _ => olaz_ui::run(olaz_ui::UiConfig { paths, services: services(debug), render: None }),
    }
}

mod probe;
