//! `SharedWorkerRouter` — port of main's `sharedWorkerManager`. Routes `$zsharedWorker` tasks between
//! the main window and the hidden `shared-worker.html` window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::Duration;

use olaz_core::exec::spawn;
use olaz_core::value::{Args, BV, interpolate};
use olaz_host::bridge::ZaloBridge;

struct Pending {
    requester: Weak<ZaloBridge>,
    request_id: BV,
    kind: String,
    handler_id: u64,
}

#[derive(Default)]
struct State {
    sender_ids: Vec<(Weak<ZaloBridge>, u64)>,
    next_sender_id: u64,
    handlers: HashMap<String, Weak<ZaloBridge>>,
    pending: HashMap<String, Pending>,
    last_remote_config: Option<BV>,
    worker: Option<Weak<ZaloBridge>>,
}

#[derive(Default)]
pub struct SharedWorkerRouter {
    state: RefCell<State>,
    pub trace: RefCell<Option<Rc<dyn Fn(String)>>>,
}

fn interrupted() -> BV {
    BV::obj([("name", BV::str("WorkerInterrupted")), ("message", BV::str("Worker is interrupted"))])
}

impl SharedWorkerRouter {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    fn trace(&self, line: String) {
        let t = self.trace.borrow().clone();
        if let Some(t) = t {
            t(line);
        }
    }

    /// Ids increase from 1 and are never reused; dead entries are dropped on every lookup.
    pub fn sender_id(&self, bridge: &Rc<ZaloBridge>) -> u64 {
        let mut s = self.state.borrow_mut();
        s.sender_ids.retain(|(w, _)| w.strong_count() > 0);
        if let Some((_, id)) = s.sender_ids.iter().find(|(w, _)| w.upgrade().is_some_and(|b| Rc::ptr_eq(&b, bridge))) {
            return *id;
        }
        s.next_sender_id += 1;
        let id = s.next_sender_id;
        s.sender_ids.push((Rc::downgrade(bridge), id));
        id
    }

    pub fn worker(&self) -> Option<Rc<ZaloBridge>> {
        self.state.borrow().worker.as_ref().and_then(Weak::upgrade)
    }

    pub fn register_worker(&self, bridge: &Rc<ZaloBridge>) {
        let mut s = self.state.borrow_mut();
        s.worker = Some(Rc::downgrade(bridge));
        s.handlers.retain(|_, w| !w.upgrade().is_some_and(|b| Rc::ptr_eq(&b, bridge)));
    }

    pub fn reset_worker(&self, bridge: &Rc<ZaloBridge>) {
        let id = self.sender_id(bridge);
        let interrupted_entries: Vec<Pending> = {
            let mut s = self.state.borrow_mut();
            if s.worker.as_ref().and_then(Weak::upgrade).is_some_and(|w| Rc::ptr_eq(&w, bridge)) {
                s.worker = None;
            }
            s.handlers.retain(|_, w| w.upgrade().is_some_and(|b| !Rc::ptr_eq(&b, bridge)));
            s.pending.retain(|_, p| p.requester.strong_count() > 0);
            let keys: Vec<String> = s.pending.iter().filter(|(_, p)| p.handler_id == id).map(|(k, _)| k.clone()).collect();
            keys.into_iter().filter_map(|k| s.pending.remove(&k)).collect()
        };
        for entry in interrupted_entries {
            respond(&entry, None, Some(interrupted()));
        }
    }

    pub fn reset_session(&self) {
        let mut s = self.state.borrow_mut();
        s.last_remote_config = None;
        s.pending.clear();
    }

    pub fn worker_did_finish_load(&self) {
        let config = self.state.borrow().last_remote_config.clone();
        if let (Some(config), Some(worker)) = (config, self.worker()) {
            worker.send("shared-worker-remote-config-change", vec![config]);
        }
    }

    pub fn pending_keys(&self) -> HashSet<String> {
        self.state.borrow().pending.keys().cloned().collect()
    }

    pub fn install(self: &Rc<Self>, bridge: &Rc<ZaloBridge>) {
        for c in ["show-shared-worker", "hide-shared-worker", "shared-worker-ping", "shared-worker-dispose"] {
            bridge.handle_now(c, |_| Ok(BV::Undefined));
        }
        let weak_bridge = Rc::downgrade(bridge);

        let (this, wb) = (Rc::downgrade(self), weak_bridge.clone());
        bridge.handle_now("shared-worker-handle", move |args| {
            let (Some(this), Some(bridge), Some(kind)) = (this.upgrade(), wb.upgrade(), args.string(0)) else { return Ok(BV::Undefined) };
            this.state.borrow_mut().handlers.insert(kind.clone(), Rc::downgrade(&bridge));
            this.trace(format!("register {kind} from #{}", this.sender_id(&bridge)));
            Ok(BV::Undefined)
        });

        let (this, wb) = (Rc::downgrade(self), weak_bridge.clone());
        bridge.handle_now("shared-worker-start", move |_| {
            let (Some(this), Some(bridge)) = (this.upgrade(), wb.upgrade()) else { return Ok(BV::Undefined) };
            let sender = this.sender_id(&bridge);
            this.trace(format!("start from #{sender}"));
            let before = this.pending_keys();
            let exec = bridge.exec().clone();
            let weak = Rc::downgrade(&this);
            spawn(&exec, {
                let exec = exec.clone();
                async move {
                    exec.timer(Duration::from_millis(100)).await;
                    let Some(this) = weak.upgrade() else { return };
                    let doomed: Vec<Pending> = {
                        let mut s = this.state.borrow_mut();
                        let keys: Vec<String> = s
                            .pending
                            .iter()
                            .filter(|(k, p)| before.contains(*k) && p.handler_id != sender)
                            .map(|(k, _)| k.clone())
                            .collect();
                        keys.into_iter().filter_map(|k| s.pending.remove(&k)).collect()
                    };
                    for entry in doomed {
                        respond(&entry, None, Some(interrupted()));
                    }
                }
            });
            Ok(BV::Undefined)
        });

        let (this, wb) = (Rc::downgrade(self), weak_bridge.clone());
        bridge.handle_now("shared-worker-run", move |args| {
            let (Some(this), Some(bridge)) = (this.upgrade(), wb.upgrade()) else { return Ok(BV::Undefined) };
            let Some(task) = args.dict(0).cloned() else { return Ok(BV::Undefined) };
            let request_id = task.get("id").cloned().unwrap_or(BV::Null);
            let kind = task.get("type").and_then(BV::as_str).unwrap_or("").to_string();
            if this.worker().is_none() {
                reply(&bridge, request_id, "Worker is not ready");
                return Ok(BV::Undefined);
            }
            let handler = this.state.borrow().handlers.get(&kind).and_then(Weak::upgrade);
            let Some(handler) = handler else {
                this.trace(format!("run {kind} -> unsupported (no handler)"));
                reply(&bridge, request_id, "Task type is not supported");
                return Ok(BV::Undefined);
            };
            let (sid, hid) = (this.sender_id(&bridge), this.sender_id(&handler));
            let key = format!("{sid}_{}", interpolate(&request_id));
            this.state.borrow_mut().pending.insert(
                key.clone(),
                Pending { requester: Rc::downgrade(&bridge), request_id, kind: kind.clone(), handler_id: hid },
            );
            this.trace(format!("run {kind} id={key} #{sid} -> #{hid}"));
            let mut routed = task;
            routed.insert("id".into(), BV::String(key));
            handler.send("shared-worker-run", vec![BV::Object(routed)]);
            Ok(BV::Undefined)
        });

        let this = Rc::downgrade(self);
        bridge.handle_now("shared-worker-resp", move |args| {
            let Some(this) = this.upgrade() else { return Ok(BV::Undefined) };
            let Some(message) = args.dict(0) else { return Ok(BV::Undefined) };
            let Some(key) = message.get("id").map(interpolate) else { return Ok(BV::Undefined) };
            let Some(entry) = this.state.borrow_mut().pending.remove(&key) else { return Ok(BV::Undefined) };
            let status = match message.get("error") {
                None => "ok".to_string(),
                Some(e) => {
                    let text = serde_json::json!({"e": e.to_plain_json()}).to_string();
                    format!("error {}", text.chars().take(200).collect::<String>())
                }
            };
            this.trace(format!("resp {} id={key} {status}", entry.kind));
            respond(&entry, message.get("result").cloned(), message.get("error").cloned());
            Ok(BV::Undefined)
        });

        let this = Rc::downgrade(self);
        bridge.handle_now("shared-worker-progress", move |args| {
            let Some(this) = this.upgrade() else { return Ok(BV::Undefined) };
            let Some(message) = args.dict(0) else { return Ok(BV::Undefined) };
            let Some(key) = message.get("id").map(interpolate) else { return Ok(BV::Undefined) };
            let s = this.state.borrow();
            let Some(entry) = s.pending.get(&key) else { return Ok(BV::Undefined) };
            let mut forwarded = vec![("id", entry.request_id.clone()), ("type", BV::String(entry.kind.clone()))];
            if let Some(p) = message.get("progress") {
                forwarded.push(("progress", p.clone()));
            }
            if let Some(c) = message.get("checkpoint") {
                forwarded.push(("checkpoint", c.clone()));
            }
            let requester = entry.requester.upgrade();
            drop(s);
            if let Some(r) = requester {
                r.send("shared-worker-progress", vec![BV::obj(forwarded)]);
            }
            Ok(BV::Undefined)
        });

        let (this, wb) = (Rc::downgrade(self), weak_bridge.clone());
        bridge.handle_now("shared-worker-abort", move |args| {
            let (Some(this), Some(bridge)) = (this.upgrade(), wb.upgrade()) else { return Ok(BV::Undefined) };
            let Some(message) = args.dict(0) else { return Ok(BV::Undefined) };
            let key = format!("{}_{}", this.sender_id(&bridge), interpolate(message.get("id").unwrap_or(&BV::Null)));
            let removed = this.state.borrow_mut().pending.remove(&key);
            if let Some(entry) = removed {
                respond(&entry, None, Some(BV::str("Aborted")));
            }
            if let Some(kind) = message.get("type").and_then(BV::as_str) {
                let handler = this.state.borrow().handlers.get(kind).and_then(Weak::upgrade);
                if let Some(h) = handler {
                    h.send("shared-worker-abort", vec![BV::obj([("id", BV::String(key))])]);
                }
            }
            Ok(BV::Undefined)
        });

        let (this, wb) = (Rc::downgrade(self), weak_bridge.clone());
        bridge.handle_now("shared-worker-update-params", move |args| {
            let (Some(this), Some(bridge)) = (this.upgrade(), wb.upgrade()) else { return Ok(BV::Undefined) };
            let Some(message) = args.dict(0) else { return Ok(BV::Undefined) };
            let Some(kind) = message.get("type").and_then(BV::as_str) else { return Ok(BV::Undefined) };
            let handler = this.state.borrow().handlers.get(kind).and_then(Weak::upgrade);
            let Some(handler) = handler else { return Ok(BV::Undefined) };
            let key = format!("{}_{}", this.sender_id(&bridge), interpolate(message.get("id").unwrap_or(&BV::Null)));
            handler.send(
                "shared-worker-update-params",
                vec![BV::obj([
                    ("id", BV::String(key)),
                    ("type", BV::str(kind)),
                    ("params", message.get("params").cloned().unwrap_or(BV::Null)),
                ])],
            );
            Ok(BV::Undefined)
        });

        let this = Rc::downgrade(self);
        bridge.handle_now("shared-worker-remote-config-change", move |args| {
            let Some(this) = this.upgrade() else { return Ok(BV::Undefined) };
            let config = args.dict(0).and_then(|d| d.get("config").cloned()).or_else(|| args.first().cloned());
            let Some(config) = config else { return Ok(BV::Undefined) };
            this.state.borrow_mut().last_remote_config = Some(config.clone());
            if let Some(w) = this.worker() {
                w.send("shared-worker-remote-config-change", vec![config]);
            }
            Ok(BV::Undefined)
        });
    }
}

fn respond(entry: &Pending, result: Option<BV>, error: Option<BV>) {
    let mut message = vec![("id", entry.request_id.clone())];
    if let Some(r) = result {
        message.push(("result", r));
    }
    if let Some(e) = error {
        message.push(("error", e));
    }
    if let Some(r) = entry.requester.upgrade() {
        r.send("shared-worker-resp", vec![BV::obj(message)]);
    }
}

fn reply(bridge: &ZaloBridge, id: BV, error: &str) {
    bridge.send("shared-worker-resp", vec![BV::obj([("id", id), ("error", BV::str(error))])]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use olaz_core::exec::{LocalBoxFuture, TestExecutor};
    use olaz_host::bridge::PageSink;
    use serde_json::{Value, json};

    struct Page(RefCell<Vec<(String, Value)>>);
    impl PageSink for Page {
        fn call(&self, _body: &str, args: Value) {
            self.0.borrow_mut().push((args["channel"].as_str().unwrap_or("").to_string(), args["args"].clone()));
        }
        fn eval(&self, _: &str, _: Value) -> LocalBoxFuture<Result<Value, String>> {
            Box::pin(async { Ok(Value::Null) })
        }
    }

    fn window(t: &Rc<TestExecutor>, router: &Rc<SharedWorkerRouter>) -> (Rc<ZaloBridge>, Rc<Page>) {
        let b = ZaloBridge::new(t.exec());
        let page = Rc::new(Page(RefCell::new(vec![])));
        let dyn_page: Rc<dyn PageSink> = page.clone();
        b.set_page(Rc::downgrade(&dyn_page));
        std::mem::forget(dyn_page);
        router.install(&b);
        (b, page)
    }

    fn invoke(t: &TestExecutor, b: &ZaloBridge, channel: &str, args: Value) {
        b.handle_message(json!({"kind": "invoke", "channel": channel, "args": args}), Box::new(|_, _| {}));
        t.run_until_stalled();
    }

    #[test]
    fn run_resp_round_trip() {
        let t = TestExecutor::new();
        let r = SharedWorkerRouter::new();
        let (main, main_page) = window(&t, &r);
        let (worker, worker_page) = window(&t, &r);
        // before a worker exists
        invoke(&t, &main, "shared-worker-run", json!([{"id": 6, "type": "X"}]));
        assert_eq!(main_page.0.borrow().last().unwrap().1, json!([{"id": 6, "error": "Worker is not ready"}]));
        r.register_worker(&worker);
        invoke(&t, &main, "shared-worker-run", json!([{"id": 6, "type": "X"}]));
        assert_eq!(main_page.0.borrow().last().unwrap().1, json!([{"id": 6, "error": "Task type is not supported"}]));
        assert_eq!(r.sender_id(&main), 1);
        invoke(&t, &worker, "shared-worker-handle", json!(["X"]));
        invoke(&t, &main, "shared-worker-run", json!([{"id": 7, "type": "X", "p": 1}]));
        assert_eq!(worker_page.0.borrow().last().unwrap(), &("shared-worker-run".to_string(), json!([{"id": "1_7", "type": "X", "p": 1}])));
        invoke(&t, &worker, "shared-worker-progress", json!([{"id": "1_7", "progress": 50}]));
        assert_eq!(main_page.0.borrow().last().unwrap().1, json!([{"id": 7, "type": "X", "progress": 50}]));
        invoke(&t, &worker, "shared-worker-resp", json!([{"id": "1_7", "result": {"ok": true}}]));
        assert_eq!(main_page.0.borrow().last().unwrap(), &("shared-worker-resp".to_string(), json!([{"id": 7, "result": {"ok": true}}])));
        assert!(r.pending_keys().is_empty());
    }

    #[test]
    fn worker_restart_interrupts_only_old_pending_after_100ms() {
        let t = TestExecutor::new();
        let r = SharedWorkerRouter::new();
        let (main, main_page) = window(&t, &r);
        let (worker, _) = window(&t, &r);
        r.register_worker(&worker);
        invoke(&t, &worker, "shared-worker-handle", json!(["X"]));
        // a task handled by the *main* window itself, pending when the worker restarts
        invoke(&t, &main, "shared-worker-handle", json!(["M"]));
        invoke(&t, &main, "shared-worker-run", json!([{"id": 1, "type": "X"}]));
        invoke(&t, &worker, "shared-worker-start", json!([]));
        // a task created within the 100 ms is not cancelled
        invoke(&t, &main, "shared-worker-run", json!([{"id": 2, "type": "M"}]));
        t.advance(Duration::from_millis(50));
        assert_eq!(r.pending_keys().len(), 2);
        t.advance(Duration::from_millis(60));
        // pending #1 was handled by the worker (sender of start) → kept; nothing else was pending before
        assert_eq!(r.pending_keys().len(), 2);
        // now restart from the main window: pending handled by the worker (≠ main) and existing before → interrupted
        invoke(&t, &main, "shared-worker-start", json!([]));
        t.advance(Duration::from_millis(100));
        assert_eq!(r.pending_keys(), ["2_2".to_string()].into_iter().collect());
        assert_eq!(
            main_page.0.borrow().last().unwrap().1,
            json!([{"id": 1, "error": {"name": "WorkerInterrupted", "message": "Worker is interrupted"}}])
        );
    }

    #[test]
    fn abort_reset_and_remote_config() {
        let t = TestExecutor::new();
        let r = SharedWorkerRouter::new();
        let (main, main_page) = window(&t, &r);
        let (worker, worker_page) = window(&t, &r);
        r.register_worker(&worker);
        invoke(&t, &worker, "shared-worker-handle", json!(["X"]));
        invoke(&t, &main, "shared-worker-run", json!([{"id": "a", "type": "X"}]));
        invoke(&t, &main, "shared-worker-abort", json!([{"id": "a", "type": "X"}]));
        assert_eq!(main_page.0.borrow().last().unwrap().1, json!([{"id": "a", "error": "Aborted"}]));
        assert_eq!(worker_page.0.borrow().last().unwrap(), &("shared-worker-abort".to_string(), json!([{"id": "2_a"}])));
        invoke(&t, &main, "shared-worker-remote-config-change", json!([{"config": {"k": 1}}]));
        assert_eq!(worker_page.0.borrow().last().unwrap(), &("shared-worker-remote-config-change".to_string(), json!([{"k": 1}])));
        worker_page.0.borrow_mut().clear();
        r.worker_did_finish_load();
        assert_eq!(worker_page.0.borrow().len(), 1);
        invoke(&t, &main, "shared-worker-run", json!([{"id": 9, "type": "X"}]));
        r.reset_worker(&worker);
        assert_eq!(main_page.0.borrow().last().unwrap().1["0"], Value::Null);
        assert_eq!(main_page.0.borrow().last().unwrap().1[0]["error"]["name"], "WorkerInterrupted");
        assert!(r.worker().is_none());
        r.reset_session();
        assert!(r.pending_keys().is_empty());
    }
}
