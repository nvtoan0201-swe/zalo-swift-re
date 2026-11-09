//! The engine's executor seam implemented on GPUI: tasks on the foreground (main) thread, timers on the
//! background executor — the `@MainActor` of the port.

use std::time::Duration;

use gpui::{BackgroundExecutor, ForegroundExecutor};
use olaz_core::exec::{LocalBoxFuture, LocalExecutor};

pub struct GpuiExecutor {
    pub fg: ForegroundExecutor,
    pub bg: BackgroundExecutor,
}

impl LocalExecutor for GpuiExecutor {
    fn spawn(&self, fut: LocalBoxFuture<()>) {
        self.fg.spawn(fut).detach();
    }
    fn timer(&self, after: Duration) -> LocalBoxFuture<()> {
        let t = self.bg.timer(after);
        Box::pin(async move {
            t.await;
        })
    }
}
