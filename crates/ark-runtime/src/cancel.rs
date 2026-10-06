//! Cooperative cancellation. A front-end holds a [`CancelToken`] and trips it (Ctrl+C, a Stop
//! button, a request timeout); the harness notices at its next await point, stops calling the
//! model and tools, saves the working state, and returns an outcome marked `cancelled`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    flag: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst)
    }

    /// Resolves once [`cancel`](CancelToken::cancel) has been called.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// The process is stopping (a deploy, SIGTERM, the app quitting): runs stop at their next safe
/// point, and the process waits for them before it exits. One per engine and the engines
/// scoped from it ([`crate::Engine::stop_soon`]).
#[derive(Default)]
pub(crate) struct Stopping {
    pub(crate) token: CancelToken,
    runs: AtomicUsize,
    idle: Notify,
}

impl Stopping {
    pub(crate) fn run(self: &Arc<Self>) -> RunGuard {
        self.runs.fetch_add(1, Ordering::SeqCst);
        RunGuard(self.clone())
    }

    pub(crate) async fn idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.runs.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// A run in progress (a turn, a task's attempt), counted until it is dropped.
pub struct RunGuard(Arc<Stopping>);

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.0.runs.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// How long a process told to stop lets running work reach a safe point
/// ([`crate::Engine::stop_soon`]): `ARK_STOP_GRACE_SECS`, default 25. A host that kills the
/// process after a timeout (a container platform's stop timeout) should allow a little more.
pub fn stop_grace() -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var("ARK_STOP_GRACE_SECS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(25),
    )
}

/// Wait on an optional token; pends forever when there is none.
pub(crate) async fn wait(token: &Option<CancelToken>) {
    match token {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Wait until either token is cancelled; pends forever when neither is.
pub(crate) async fn wait_either(a: &Option<CancelToken>, b: &Option<CancelToken>) {
    tokio::select! {
        _ = wait(a) => {}
        _ = wait(b) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_resolves_after_cancel() {
        let t = CancelToken::new();
        let t2 = t.clone();
        let waiter = tokio::spawn(async move { t2.cancelled().await });
        tokio::task::yield_now().await;
        t.cancel();
        waiter.await.unwrap();
        assert!(t.is_cancelled());
        // Already-cancelled tokens resolve immediately.
        t.cancelled().await;
    }
}
