//! Message media resolution through the official loader. Port of `ZaloEngine+Media.swift`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::{LocalBoxFuture, Shared};
use serde_json::{Value, json};

use crate::engine::ZaloEngine;
use crate::models::J;

#[derive(Debug, Clone, PartialEq)]
pub enum MediaResolution {
    File(String),
    Expired,
    Unavailable,
    Unsupported,
}

impl MediaResolution {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(v) = v.filter(|v| v.is_object()) else { return Self::Unavailable };
        match v.s("state").as_str() {
            "ok" => {
                let p = v.s("path");
                if p.starts_with('/') { Self::File(p) } else { Self::Unavailable }
            }
            "expired" => Self::Expired,
            "unsupported" => Self::Unsupported,
            _ => Self::Unavailable,
        }
    }
    pub fn log_name(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Expired => "expired",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaKind {
    Photo,
    VideoThumb,
    Video,
}

impl MediaKind {
    pub fn raw(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::VideoThumb => "videoThumb",
            Self::Video => "video",
        }
    }
}

pub const UNAVAILABLE_TTL: Duration = Duration::from_secs(30);

type Flight = Shared<LocalBoxFuture<'static, Option<MediaResolution>>>;

/// De-duplicates and throttles resolutions: one flight per key, ≤ `max` running, newest waiter first (LIFO).
pub struct MediaResolver {
    settled: RefCell<HashMap<String, (MediaResolution, Option<Instant>)>>,
    flights: RefCell<HashMap<String, Flight>>,
    running: std::cell::Cell<usize>,
    queue: RefCell<Vec<oneshot::Sender<()>>>,
    pub max: usize,
}

impl MediaResolver {
    pub fn new(max: usize) -> Rc<Self> {
        Rc::new(Self { settled: RefCell::default(), flights: RefCell::default(), running: Default::default(), queue: RefCell::default(), max })
    }

    pub fn known(&self, key: &str) -> Option<MediaResolution> {
        self.settled.borrow().get(key).map(|(r, _)| r.clone())
    }

    async fn acquire(self: &Rc<Self>) {
        if self.running.get() < self.max {
            self.running.set(self.running.get() + 1);
            return;
        }
        let (tx, rx) = oneshot::channel();
        self.queue.borrow_mut().push(tx);
        let _ = rx.await;
    }

    fn release(&self) {
        // Newest first: on-screen photos are not stuck behind off-screen ones. Dropped waiters are skipped.
        loop {
            let next = self.queue.borrow_mut().pop();
            match next {
                Some(tx) => {
                    if tx.send(()).is_ok() {
                        return;
                    }
                }
                None => {
                    self.running.set(self.running.get().saturating_sub(1));
                    return;
                }
            }
        }
    }

    pub async fn resolve(
        self: &Rc<Self>,
        key: &str,
        force: bool,
        run: impl FnOnce() -> LocalBoxFuture<'static, MediaResolution> + 'static,
    ) -> MediaResolution {
        let known = self.settled.borrow().get(key).cloned();
        if let Some((res, until)) = known {
            let live = until.is_none_or(|u| u > Instant::now());
            let reuse = live
                && match &res {
                    MediaResolution::Unavailable => !force,
                    MediaResolution::File(p) => !force && std::path::Path::new(p).exists(),
                    _ => true,
                };
            if reuse {
                return res;
            }
        }
        let existing = self.flights.borrow().get(key).cloned();
        let flight = match existing {
            Some(f) => f,
            None => {
                let this = self.clone();
                let f: Flight = async move {
                    this.acquire().await;
                    let r = run().await;
                    this.release();
                    Some(r)
                }
                .boxed_local()
                .shared();
                self.flights.borrow_mut().insert(key.to_string(), f.clone());
                f
            }
        };
        let outcome = flight.await;
        self.flights.borrow_mut().remove(key);
        let Some(res) = outcome else { return MediaResolution::Unavailable };
        let until = match res {
            MediaResolution::Unavailable => Some(Instant::now() + UNAVAILABLE_TTL),
            _ => None,
        };
        let mut s = self.settled.borrow_mut();
        s.insert(key.to_string(), (res.clone(), until));
        if s.len() > 2000 {
            let now = Instant::now();
            s.retain(|_, (_, u)| u.is_none_or(|u| u > now));
        }
        res
    }
}

impl ZaloEngine {
    /// `window.__zaloChat.resolveMedia(conv, msgId, kind)`: local copy → official download path → "expired".
    pub async fn resolve_media(&self, conversation: &str, message: &str, kind: MediaKind, force: bool) -> MediaResolution {
        if self.host().is_none() && self.fake_renderer.borrow().is_none() {
            return MediaResolution::Unavailable;
        }
        let key = format!("{conversation}|{message}|{}", kind.raw());
        if force {
            if let Some(MediaResolution::File(bad)) = self.media_resolver.known(&key) {
                if bad.starts_with(&*self.paths.cache().to_string_lossy()) {
                    let _ = std::fs::remove_file(bad);
                }
            }
        }
        let fut = self.await_call(
            "__zaloChat",
            "resolveMedia",
            vec![json!(conversation), json!(message), json!(kind.raw())],
            "{state: 'unavailable', reason: 'missing'}",
        );
        let resolver = self.media_resolver.clone();
        resolver
            .resolve(&key, force, move || {
                async move {
                    let r = MediaResolution::from_json(fut.await.as_ref());
                    olaz_core::log::write(format!("resolveMedia kind={} → {}", kind.raw(), r.log_name()));
                    r
                }
                .boxed_local()
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use olaz_core::exec::{TestExecutor, spawn};
    use std::cell::Cell;

    #[test]
    fn parse() {
        assert_eq!(MediaResolution::from_json(Some(&json!({"state": "ok", "path": "/a"}))), MediaResolution::File("/a".into()));
        assert_eq!(MediaResolution::from_json(Some(&json!({"state": "ok", "path": "a"}))), MediaResolution::Unavailable);
        assert_eq!(MediaResolution::from_json(Some(&json!({"state": "expired"}))), MediaResolution::Expired);
        assert_eq!(MediaResolution::from_json(None), MediaResolution::Unavailable);
    }

    #[test]
    fn dedupe_ttl_force() {
        let t = TestExecutor::new();
        let r = MediaResolver::new(3);
        let runs = Rc::new(Cell::new(0));
        let results = Rc::new(RefCell::new(Vec::new()));
        for _ in 0..3 {
            let (r, runs, results) = (r.clone(), runs.clone(), results.clone());
            spawn(&t.exec(), async move {
                let res = r
                    .resolve("k", false, move || {
                        runs.set(runs.get() + 1);
                        async { MediaResolution::Unavailable }.boxed_local()
                    })
                    .await;
                results.borrow_mut().push(res);
            });
        }
        t.run_until_stalled();
        assert_eq!(runs.get(), 1, "one flight per key");
        assert_eq!(results.borrow().len(), 3);
        // remembered unavailable is reused unless forced
        let (r2, runs2) = (r.clone(), runs.clone());
        spawn(&t.exec(), async move {
            r2.resolve("k", false, move || {
                runs2.set(runs2.get() + 1);
                async { MediaResolution::Expired }.boxed_local()
            })
            .await;
        });
        t.run_until_stalled();
        assert_eq!(runs.get(), 1);
        let (r3, runs3) = (r.clone(), runs.clone());
        spawn(&t.exec(), async move {
            r3.resolve("k", true, move || {
                runs3.set(runs3.get() + 1);
                async { MediaResolution::Expired }.boxed_local()
            })
            .await;
        });
        t.run_until_stalled();
        assert_eq!(runs.get(), 2);
        assert_eq!(r.known("k"), Some(MediaResolution::Expired));
    }

    #[test]
    fn concurrency_cap_and_lifo() {
        let t = TestExecutor::new();
        let r = MediaResolver::new(1);
        let order = Rc::new(RefCell::new(Vec::new()));
        let gates: Vec<oneshot::Sender<()>> = Vec::new();
        let gates = Rc::new(RefCell::new(gates));
        for i in 0..3 {
            let (r, order, gates) = (r.clone(), order.clone(), gates.clone());
            spawn(&t.exec(), async move {
                r.resolve(&format!("k{i}"), false, move || {
                    let (tx, rx) = oneshot::channel::<()>();
                    gates.borrow_mut().push(tx);
                    order.borrow_mut().push(i);
                    async move {
                        let _ = rx.await;
                        MediaResolution::Expired
                    }
                    .boxed_local()
                })
                .await;
            });
        }
        t.run_until_stalled();
        assert_eq!(*order.borrow(), vec![0], "only one runs");
        let g = gates.borrow_mut().remove(0);
        let _ = g.send(());
        t.run_until_stalled();
        assert_eq!(*order.borrow(), vec![0, 2], "newest waiter served first");
    }
}
