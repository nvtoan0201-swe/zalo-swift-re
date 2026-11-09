//! Dev eval channel: debug builds with `OLAZ_DEV_EVAL=1`. Directory `<data root>/dev/`, polled every 1 s:
//! `eval.js` (mtime changed) → run in the main window → `eval.out.json`; `worker-eval.js` → shared-worker window →
//! `worker-eval.out.json`; `open-conv.txt` → navigate the UI there (file deleted).

use std::rc::Rc;
use std::time::{Duration, SystemTime};

use olaz_core::exec::spawn;

use crate::engine::{Change, ZaloEngine};

pub fn wrap(source: &str) -> String {
    format!(
        "try {{ const __r = await (async () => {{ {source} }})(); return JSON.stringify({{ok: true, value: __r}}, (k, v) => typeof v === 'bigint' ? String(v) : v, 1); }} catch (e) {{ return JSON.stringify({{ok: false, error: String(e && e.stack || e)}}); }}"
    )
}

pub fn start_if_enabled(engine: &Rc<ZaloEngine>) {
    if !cfg!(debug_assertions) || std::env::var_os("OLAZ_DEV_EVAL").is_none() {
        return;
    }
    let dir = engine.paths.app_data().join("dev");
    let _ = std::fs::create_dir_all(&dir);
    let weak = Rc::downgrade(engine);
    let exec = engine.exec.clone();
    spawn(&engine.exec, async move {
        let mut seen: [Option<SystemTime>; 2] = [None, None];
        loop {
            exec.timer(Duration::from_secs(1)).await;
            let Some(engine) = weak.upgrade() else { return };
            let open = dir.join("open-conv.txt");
            if let Ok(id) = std::fs::read_to_string(&open) {
                let _ = std::fs::remove_file(&open);
                let id = id.trim().to_string();
                if !id.is_empty() {
                    engine.state.borrow_mut().navigation_request = Some(id);
                    engine.changed(Change::Other);
                }
            }
            // `compose.json {method, args, files}` → one composer action with those files as the pick.
            let compose = dir.join("compose.json");
            if let Ok(text) = std::fs::read_to_string(&compose) {
                let _ = std::fs::remove_file(&compose);
                if let Ok(job) = serde_json::from_str::<serde_json::Value>(&text) {
                    let method = job["method"].as_str().unwrap_or("").to_string();
                    let args: Vec<serde_json::Value> = job["args"].as_array().cloned().unwrap_or_default();
                    let files: Vec<std::path::PathBuf> =
                        job["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(Into::into)).collect()).unwrap_or_default();
                    let conv = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
                    let outcome = match method.as_str() {
                        "sendMedia" => engine.send_media(files, &conv).await,
                        "sendFiles" => engine.send_files(files, &conv).await,
                        _ => crate::composer::ComposerOutcome::Failed(format!("unknown method {method}")),
                    };
                    let _ = std::fs::write(dir.join("compose.out.txt"), format!("{outcome:?}"));
                }
            }
            // `snapshot.json {key: value}` → handled as if the page had posted it (local UI checks; nothing is sent).
            let snap = dir.join("snapshot.json");
            if let Ok(text) = std::fs::read_to_string(&snap) {
                let _ = std::fs::remove_file(&snap);
                if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(key) = map.keys().next().cloned() {
                        crate::features::handle(&engine, &key, &map);
                    }
                }
            }
            // `conv-plan.txt <id>` → the conversation menu plan and hidden-chat state (read only).
            let plan = dir.join("conv-plan.txt");
            if let Ok(id) = std::fs::read_to_string(&plan) {
                let _ = std::fs::remove_file(&plan);
                let first = {
                    let s = engine.state.borrow();
                    let self_id = s.self_conversation_id.clone();
                    s.conversations.iter().find(|c| Some(&c.id) != self_id.as_ref()).map(|c| c.id.clone()).unwrap_or_default()
                };
                // `*` = the first conversation that is not My Cloud; `g` = the first read group (opening it changes nothing).
                let group = {
                    let s = engine.state.borrow();
                    s.conversations.iter().find(|c| c.is_group && c.unread == 0).map(|c| c.id.clone()).unwrap_or_default()
                };
                let id = match id.trim() {
                    "*" => first.as_str(),
                    "g" => group.as_str(),
                    other => other,
                };
                let _ = std::fs::write(dir.join("conv-plan.id.txt"), id);
                let plans = (
                    engine.conversation_menu_plan(id, crate::conversation_actions::MenuPreset::Thread),
                    engine.conversation_menu_plan(id, crate::conversation_actions::MenuPreset::ChatBox),
                );
                let s = engine.state.borrow();
                let a = &s.conversation_actions;
                let out = format!(
                    "loaded={} hidden_known={} hidden_enabled={} has_pin={} hidden={} flags={} visible={}/{}\nthread={:?}\nchatbox={:?}",
                    a.loaded,
                    a.hidden_known,
                    a.hidden_enabled,
                    a.has_hidden_pin,
                    a.hidden_ids.len(),
                    a.flags_for(id),
                    s.conversations.iter().filter(|c| !a.is_hidden(&c.id)).count(),
                    s.conversations.len(),
                    plans.0,
                    plans.1,
                );
                drop(s);
                let c = &engine.state.borrow().contacts;
                let out = format!(
                    "{out}\ncontacts friends={} (loaded {}) online={} groups={} (loaded {}) received={} sent={} pending={} loading={}",
                    c.friends.len(),
                    c.friends_loaded,
                    c.friends.iter().filter(|f| f.online).count(),
                    c.groups.len(),
                    c.groups_loaded,
                    c.received.len(),
                    c.sent.len(),
                    c.pending_request_count,
                    c.loading_requests
                );
                let _ = std::fs::write(dir.join("conv-plan.out.txt"), out);
            }
            // `msg-action.json {action, conv, index}` → a non-destructive message action on the index-th newest message.
            let action = dir.join("msg-action.json");
            if let Ok(text) = std::fs::read_to_string(&action) {
                let _ = std::fs::remove_file(&action);
                let job: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                let conv = job["conv"].as_str().unwrap_or("").to_string();
                let index = job["index"].as_u64().unwrap_or(0) as usize;
                let m = engine.state.borrow().messages.get(&conv).and_then(|l| l.iter().rev().nth(index).cloned());
                let out = match (m, job["action"].as_str().unwrap_or("")) {
                    (None, _) => "no message".to_string(),
                    (Some(m), "copyText") => {
                        engine.copy_text(&m.copyable_text().unwrap_or_default(), &conv);
                        format!("copy {:?} menu {:?}", m.copyable_text(), m.menu)
                    }
                    (Some(m), "copyPhoto") => {
                        engine.copy_photo(&m);
                        format!("copyPhoto {:?}", m.kind)
                    }
                    (Some(m), "forward") => {
                        engine.request_forward(&m);
                        format!("forward request {}", engine.state.borrow().forward_request.is_some())
                    }
                    (Some(m), "dump") => format!("{m:#?}"),
                    (Some(m), "describe") => format!("{:?} {:?} {:?}", m.kind, m.text, m.menu),
                    (Some(_), other) => format!("unsupported {other}"),
                };
                let _ = std::fs::write(dir.join("msg-action.out.txt"), out);
            }
            for (i, (input, output, worker)) in [("eval.js", "eval.out.json", false), ("worker-eval.js", "worker-eval.out.json", true)].iter().enumerate() {
                let path = dir.join(input);
                let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else { continue };
                if seen[i] == Some(modified) {
                    continue;
                }
                seen[i] = Some(modified);
                let Ok(source) = std::fs::read_to_string(&path) else { continue };
                let host = if *worker { engine.main_process().and_then(|m| m.worker_host()) } else { engine.host() };
                let Some(host) = host else { continue };
                let result = host.evaluate(&wrap(&source)).await;
                let text = result.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "{\"ok\":false,\"error\":\"no result\"}".into());
                let _ = olaz_core::paths::write_atomic(&dir.join(output), text.as_bytes());
            }
        }
    });
}
