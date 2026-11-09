//! Engine against the real official renderer: boot → Login phase with a QR PNG (needs a display, the webhost
//! binary and the Zalo app dir in `OLAZ_ZALO_APP` = the extracted `Zalo.app`). Skipped otherwise.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use olaz_core::exec::RealExecutor;
use olaz_core::paths::XdgPaths;
use olaz_engine::{LoginStep, Phase, Services, ZaloEngine};

#[test]
fn boots_to_login_with_qr() {
    let Some(app) = std::env::var_os("OLAZ_ZALO_APP").map(PathBuf::from) else { return };
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/olaz-webhost");
    if !bin.is_file() || (std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()) {
        return;
    }
    let dir = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let paths = XdgPaths::rooted(dir.path());
    let current = paths.runtime_root().join("current/Contents");
    std::fs::create_dir_all(&current).unwrap();
    std::os::unix::fs::symlink(app.join("Contents/Info.plist"), current.join("Info.plist")).unwrap();
    std::os::unix::fs::symlink(app.join("Contents/Resources"), current.join("Resources")).unwrap();

    let exec = RealExecutor::new();
    let engine = ZaloEngine::new(
        exec.exec(),
        paths,
        Services {
            media: None,
            secrets: Arc::new(olaz_main::secrets::MemoryStore::default()),
            open_url: Rc::new(|_| {}),
            badge: None,
            webhost_binary: bin,
            debug: false,
        },
    );
    engine.start();
    assert!(matches!(engine.phase(), Phase::Login(_)), "{:?}", engine.phase());
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        exec.run_for(Duration::from_millis(100));
        if let Phase::Login(l) = engine.phase() {
            if l.step == LoginStep::WaitingScan && l.qr_png.as_ref().is_some_and(|p| p.starts_with(b"\x89PNG")) {
                engine.shutdown();
                return;
            }
        }
    }
    panic!("no QR: {:?}", engine.phase());
}
