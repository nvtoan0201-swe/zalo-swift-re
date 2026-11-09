//! Host scripts (verbatim copies of the macOS app's, in `linux-port/shared/host-scripts`) and their injection order.

macro_rules! shared {
    ($name:literal) => {
        include_str!(concat!("../../../../shared/host-scripts/", $name))
    };
}

pub const CONSOLE_HOOK: &str = shared!("console-hook.js");
pub const DAL_WORKER_PRELUDE: &str = shared!("dal-worker-prelude.js");
pub const DAL_WORKER_DEBUG_EVAL: &str = shared!("dal-worker-debug-eval.js");
pub const BRIDGE_TABLE_JSON: &str = include_str!("../../../../shared/resources/bridge-table.json");

/// The adapter scripts injected after the spec script, by name, in order.
pub const AFTER_SPEC: &[(&str, &str)] = &[
    ("local-modules", shared!("local-modules.js")),
    ("bridge", shared!("bridge.js")),
    ("state-adapter", shared!("state-adapter.js")),
    ("chat-adapter", shared!("chat-adapter.js")),
    ("media-store-adapter", shared!("media-store-adapter.js")),
    ("card-actions-adapter", shared!("card-actions-adapter.js")),
    ("sticker-adapter", shared!("sticker-adapter.js")),
    ("composer-adapter", shared!("composer-adapter.js")),
    ("message-actions-adapter", shared!("message-actions-adapter.js")),
    ("transfer-adapter", shared!("transfer-adapter.js")),
    ("conversations-adapter", shared!("conversations-adapter.js")),
    ("conversation-actions-adapter", shared!("conversation-actions-adapter.js")),
    ("contacts-adapter", shared!("contacts-adapter.js")),
    ("group-create-adapter", shared!("group-create-adapter.js")),
    ("notification-adapter", shared!("notification-adapter.js")),
    ("zcloud-adapter", shared!("zcloud-adapter.js")),
    ("poll-adapter", shared!("poll-adapter.js")),
    ("call-adapter", shared!("call-adapter.js")),
];

pub const NETWORK_SHIM: &str = shared!("network-shim.js");

/// Files extracted from Swift literals carry a one-line `// EXTRACTED VERBATIM …` header that the Swift
/// literal does not have; drop it so the served bytes equal macOS.
pub fn shared_script(text: &str) -> &str {
    if text.starts_with("// EXTRACTED VERBATIM") {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => "",
        }
    } else {
        text
    }
}

fn host_script(name: &str, source: &str) -> String {
    format!("{source}\n//# sourceURL=olaz-{name}.js")
}

/// All user scripts for one window, in the exact order the macOS host injects them.
/// `spec_script` is `ZaloBridge::spec_script()` (generated after every member is registered).
pub fn injection_order(spec_script: &str, trace_workers: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(AFTER_SPEC.len() + 4);
    out.push(if trace_workers { "window.__zaloTraceWorkers = true;".to_string() } else { String::new() });
    out.push(shared_script(CONSOLE_HOOK).to_string());
    out.push(host_script("network-shim", NETWORK_SHIM));
    out.push(spec_script.to_string());
    for (name, source) in AFTER_SPEC {
        out.push(host_script(name, source));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::asar::AsarArchive;

    /// The real pinned asar for tests: `OLAZ_ZALO_ASAR`, else the installed runtime, else the dev cache.
    pub fn dev_asar() -> Option<AsarArchive> {
        let mut candidates = Vec::new();
        if let Some(p) = std::env::var_os("OLAZ_ZALO_ASAR") {
            candidates.push(std::path::PathBuf::from(p));
        }
        let paths = crate::paths::XdgPaths::from_env();
        candidates.push(paths.runtime_root().join("current/Contents/Resources/app.asar"));
        AsarArchive::open(candidates.iter().find(|p| p.is_file())?).ok()
    }

    #[test]
    fn order_matches_h03() {
        let scripts = injection_order("window.__zaloBridgeSpec = {};", false);
        assert_eq!(scripts.len(), 22);
        assert_eq!(scripts[0], "");
        assert!(scripts[1].starts_with("(() => {\n  const post = (lvl, a)"));
        assert!(scripts[2].contains("window.__zaloNet") && scripts[2].ends_with("//# sourceURL=olaz-network-shim.js"));
        assert_eq!(scripts[3], "window.__zaloBridgeSpec = {};");
        assert!(scripts[4].ends_with("//# sourceURL=olaz-local-modules.js"));
        assert!(scripts[5].ends_with("//# sourceURL=olaz-bridge.js"));
        assert!(scripts[21].ends_with("//# sourceURL=olaz-call-adapter.js"));
        assert_eq!(injection_order("", true)[0], "window.__zaloTraceWorkers = true;");
    }

    #[test]
    fn extracted_header_is_stripped() {
        assert!(!shared_script(DAL_WORKER_PRELUDE).contains("EXTRACTED"));
        assert_eq!(shared_script("abc"), "abc");
    }
}
