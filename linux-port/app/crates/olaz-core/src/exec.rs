//! Single-threaded executor seam (the `@MainActor` of the port). The app implements it on GPUI's
//! foreground executor; tests use [`TestExecutor`]. Everything host/main/engine-side that awaits runs here,
//! in order, on one thread.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

pub trait LocalExecutor {
    fn spawn(&self, fut: LocalBoxFuture<()>);
    fn timer(&self, after: Duration) -> LocalBoxFuture<()>;
}

pub type Exec = Rc<dyn LocalExecutor>;

pub fn spawn(exec: &Exec, fut: impl Future<Output = ()> + 'static) {
    exec.spawn(Box::pin(fut));
}

/// Runs a future on the executor and returns a receiver for its output.
pub fn spawn_with_result<T: 'static>(exec: &Exec, fut: impl Future<Output = T> + 'static) -> futures::channel::oneshot::Receiver<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    exec.spawn(Box::pin(async move {
        let _ = tx.send(fut.await);
    }));
    rx
}

/// Deterministic executor for tests: a `LocalPool`; timers complete when virtual time passes them
/// (`advance`) — no real sleeping.
pub struct TestExecutor {
    pool: RefCell<futures::executor::LocalPool>,
    spawner: futures::executor::LocalSpawner,
    now: RefCell<Duration>,
    timers: RefCell<Vec<(Duration, futures::channel::oneshot::Sender<()>)>>,
}

impl TestExecutor {
    pub fn new() -> Rc<Self> {
        let pool = futures::executor::LocalPool::new();
        let spawner = pool.spawner();
        Rc::new(Self { pool: RefCell::new(pool), spawner, now: RefCell::new(Duration::ZERO), timers: RefCell::new(Vec::new()) })
    }

    /// Runs every ready task until none can make progress.
    pub fn run_until_stalled(&self) {
        self.pool.borrow_mut().run_until_stalled();
    }

    /// Advances virtual time, firing due timers in deadline order.
    pub fn advance(&self, by: Duration) {
        let target = *self.now.borrow() + by;
        loop {
            self.run_until_stalled();
            let next = {
                let mut timers = self.timers.borrow_mut();
                timers.sort_by_key(|t| t.0);
                if timers.first().is_some_and(|t| t.0 <= target) { Some(timers.remove(0)) } else { None }
            };
            match next {
                Some((at, tx)) => {
                    *self.now.borrow_mut() = at;
                    let _ = tx.send(());
                }
                None => break,
            }
        }
        *self.now.borrow_mut() = target;
        self.run_until_stalled();
    }

    pub fn exec(self: &Rc<Self>) -> Exec {
        self.clone()
    }
}

impl LocalExecutor for TestExecutor {
    fn spawn(&self, fut: LocalBoxFuture<()>) {
        use futures::task::LocalSpawnExt;
        self.spawner.spawn_local(fut).expect("spawn");
    }
    fn timer(&self, after: Duration) -> LocalBoxFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        let at = *self.now.borrow() + after;
        self.timers.borrow_mut().push((at, tx));
        Box::pin(async move {
            let _ = rx.await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn timers_fire_in_virtual_time() {
        let t = TestExecutor::new();
        let exec = t.exec();
        let hits = Rc::new(Cell::new(0));
        for d in [100u64, 5000] {
            let (h, e) = (hits.clone(), exec.clone());
            spawn(&exec, async move {
                e.timer(Duration::from_millis(d)).await;
                h.set(h.get() + 1);
            });
        }
        t.run_until_stalled();
        assert_eq!(hits.get(), 0);
        t.advance(Duration::from_millis(150));
        assert_eq!(hits.get(), 1);
        t.advance(Duration::from_secs(5));
        assert_eq!(hits.get(), 2);
    }
}

/// Real-time executor for CLIs and integration tests: a `LocalPool` driven on the current thread,
/// timers served by one background thread.
pub struct RealExecutor {
    pool: RefCell<futures::executor::LocalPool>,
    spawner: futures::executor::LocalSpawner,
    timers: std::sync::Arc<TimerThread>,
}

struct TimerThread {
    queue: std::sync::Mutex<Vec<(std::time::Instant, futures::channel::oneshot::Sender<()>)>>,
    cv: std::sync::Condvar,
}

impl RealExecutor {
    pub fn new() -> Rc<Self> {
        let pool = futures::executor::LocalPool::new();
        let spawner = pool.spawner();
        let timers = std::sync::Arc::new(TimerThread { queue: std::sync::Mutex::new(Vec::new()), cv: std::sync::Condvar::new() });
        let t = timers.clone();
        std::thread::Builder::new()
            .name("olaz-timers".into())
            .spawn(move || {
                let mut q = t.queue.lock().unwrap();
                loop {
                    let now = std::time::Instant::now();
                    let (due, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|(at, _)| *at <= now);
                    *q = rest;
                    for (_, tx) in due {
                        let _ = tx.send(());
                    }
                    let next = q.iter().map(|(at, _)| *at).min();
                    q = match next {
                        Some(at) => t.cv.wait_timeout(q, at.saturating_duration_since(std::time::Instant::now())).unwrap().0,
                        None => t.cv.wait(q).unwrap(),
                    };
                }
            })
            .expect("timer thread");
        Rc::new(Self { pool: RefCell::new(pool), spawner, timers })
    }

    pub fn exec(self: &Rc<Self>) -> Exec {
        self.clone()
    }

    /// Drives every spawned task until `fut` completes.
    pub fn block_on<T>(&self, fut: impl Future<Output = T>) -> T {
        self.pool.borrow_mut().run_until(fut)
    }

    /// Drives tasks for `d` of real time.
    pub fn run_for(&self, d: Duration) {
        let timer = self.timer(d);
        self.block_on(timer);
    }
}

impl LocalExecutor for RealExecutor {
    fn spawn(&self, fut: LocalBoxFuture<()>) {
        use futures::task::LocalSpawnExt;
        self.spawner.spawn_local(fut).expect("spawn");
    }
    fn timer(&self, after: Duration) -> LocalBoxFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.timers.queue.lock().unwrap().push((std::time::Instant::now() + after, tx));
        self.timers.cv.notify_one();
        Box::pin(async move {
            let _ = rx.await;
        })
    }
}
