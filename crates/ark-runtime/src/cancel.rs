//! Cooperative cancellation. A front-end holds a [`CancelToken`] and trips it (Ctrl+C, a Stop
//! button, a request timeout); the harness notices at its next await point, stops calling the
//! model and tools, saves the working state, and returns an outcome marked `cancelled`.

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Wait on an optional token; pends forever when there is none.
pub(crate) async fn wait(token: &Option<CancelToken>) {
    match token {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
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
