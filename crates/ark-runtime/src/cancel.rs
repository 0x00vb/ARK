//! Cooperative cancellation. A front-end holds a [`CancelToken`] and trips it (Ctrl+C, a Stop
//! button, a request timeout); the harness notices at its next await point, stops calling the
//! model and tools, saves the working state, and returns an outcome marked `cancelled`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

#[derive(Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

struct Inner {
    id: String,
    flag: AtomicBool,
    commit: Mutex<CommitState>,
    children: Mutex<Vec<CancelToken>>,
    notify: Notify,
}

#[derive(Default)]
struct CommitState {
    closed: bool,
    hooks: std::collections::BTreeMap<String, Lifecycle>,
}

struct Lifecycle {
    initialized: bool,
    stopped: bool,
    finished: bool,
    binding: Arc<ark_tools::TurnLifecycle>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner {
                id: uuid::Uuid::new_v4().to_string(),
                flag: AtomicBool::new(false),
                commit: Mutex::new(CommitState::default()),
                children: Mutex::new(Vec::new()),
                notify: Notify::new(),
            }),
        }
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        request_cancel(&self.inner);
    }

    /// Creates a separately fenced attempt whose cancellation is linked to this token.
    pub fn child(&self) -> Option<Self> {
        let mut children = self
            .inner
            .children
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() {
            return None;
        }
        let child = Self::new();
        children.push(child.clone());
        Some(child)
    }

    /// Releases completed attempt fences after their owning task has reached a final state.
    pub fn release_drained_children(&self) {
        let mut children = self
            .inner
            .children
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        children.retain(|child| {
            !child
                .inner
                .commit
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .closed
        });
    }

    /// Request cancellation and wait for an already-started commit to finish.
    pub async fn cancel_and_wait(&self) -> std::io::Result<()> {
        self.cancel();
        self.wait_for_commit().await
    }

    /// Drain any in-flight commit and complete registered lifecycle transitions.
    pub async fn wait_for_commit(&self) -> std::io::Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || drain(&inner))
            .await
            .map_err(std::io::Error::other)?
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

fn request_cancel(inner: &Inner) {
    inner.flag.store(true, Ordering::SeqCst);
    inner.notify.notify_waiters();
    let children = inner
        .children
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    for child in children {
        child.cancel();
    }
}

fn drain(inner: &Inner) -> std::io::Result<()> {
    {
        let mut state = inner
            .commit
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for hook in state.hooks.values_mut() {
            if inner.flag.load(Ordering::SeqCst) {
                if !hook.stopped {
                    run_hook(inner, &hook.binding.on_stop)?;
                    hook.stopped = true;
                }
            } else if !hook.finished {
                run_hook(inner, &hook.binding.on_finish)?;
                hook.finished = true;
            }
        }
        if inner.flag.load(Ordering::SeqCst) {
            for hook in state.hooks.values_mut() {
                if !hook.stopped {
                    run_hook(inner, &hook.binding.on_stop)?;
                    hook.stopped = true;
                }
            }
        }
        state.closed = true;
    }
    if inner.flag.load(Ordering::SeqCst) {
        let children = inner
            .children
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        for child in children {
            child.cancel();
            drain(&child.inner)?;
        }
    }
    Ok(())
}

fn run_hook(inner: &Inner, callback: &ark_tools::TurnLifecycleCallback) -> std::io::Result<()> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback())) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            request_cancel(inner);
            Err(std::io::Error::other(error))
        }
        Err(_) => {
            request_cancel(inner);
            Err(std::io::Error::other("Turn lifecycle panicked"))
        }
    }
}

impl ark_tools::CommitFence for CancelToken {
    fn turn_id(&self) -> &str {
        &self.inner.id
    }

    fn register_lifecycle(&self, lifecycle: Arc<ark_tools::TurnLifecycle>) -> Result<bool, String> {
        if lifecycle.key.is_empty() || lifecycle.key.len() > 256 {
            return Err("Invalid lifecycle identity".into());
        }
        let mut state = self
            .inner
            .commit
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() || state.closed {
            return Ok(false);
        }
        let hook = state
            .hooks
            .entry(lifecycle.key.clone())
            .or_insert_with(|| Lifecycle {
                initialized: false,
                stopped: false,
                finished: false,
                binding: lifecycle.clone(),
            });
        if !Arc::ptr_eq(&hook.binding, &lifecycle) {
            return Err("Lifecycle identity is already bound".into());
        }
        if hook.finished || hook.stopped {
            return Err("Turn lifecycle has ended".into());
        }
        if !hook.initialized {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (hook.binding.initialize)()
            })) {
                Ok(Ok(())) => hook.initialized = true,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    self.cancel();
                    return Err("Lifecycle initialization panicked".into());
                }
            }
        }
        Ok(true)
    }

    fn if_active(&self, commit: &mut dyn FnMut()) -> bool {
        let fence = self
            .inner
            .commit
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() || fence.closed {
            return false;
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(commit)) {
            Ok(()) => true,
            Err(error) => {
                self.cancel();
                std::panic::resume_unwind(error)
            }
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
    use ark_tools::CommitFence;

    #[tokio::test]
    async fn lifecycle_is_drained_and_late_registration_is_refused() {
        use std::sync::atomic::AtomicUsize;
        let token = CancelToken::new();
        let stopped = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let binding = Arc::new(ark_tools::TurnLifecycle {
            key: "project".into(),
            initialize: Arc::new(|| Ok(())),
            on_stop: {
                let count = stopped.clone();
                Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
            on_finish: {
                let count = finished.clone();
                Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        });
        assert!(token.register_lifecycle(binding.clone()).unwrap());
        assert!(token.register_lifecycle(binding.clone()).unwrap());
        let different = Arc::new(ark_tools::TurnLifecycle {
            key: "project".into(),
            initialize: Arc::new(|| Ok(())),
            on_stop: Arc::new(|| Ok(())),
            on_finish: Arc::new(|| Ok(())),
        });
        assert!(token.register_lifecycle(different).is_err());
        token.wait_for_commit().await.unwrap();
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert!(!token.register_lifecycle(binding).unwrap());
        assert!(!token.if_active(&mut || panic!("late commit")));
        token.cancel_and_wait().await.unwrap();
        token.cancel_and_wait().await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn completed_attempts_release_callback_captures_after_task_finalizes() {
        let parent = CancelToken::new();
        let mut weak = Vec::new();
        for _ in 0..100 {
            let child = parent.child().unwrap();
            weak.push(Arc::downgrade(&child.inner));
            child.wait_for_commit().await.unwrap();
        }
        assert!(weak.iter().all(|entry| entry.upgrade().is_some()));
        parent.release_drained_children();
        assert!(weak.iter().all(|entry| entry.upgrade().is_none()));
    }

    #[tokio::test]
    async fn finish_failure_cancels_and_reconciles_with_stop() {
        let token = CancelToken::new();
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let binding = Arc::new(ark_tools::TurnLifecycle {
            key: "project".into(),
            initialize: Arc::new(|| Ok(())),
            on_stop: {
                let stopped = stopped.clone();
                Arc::new(move || {
                    stopped.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
            on_finish: Arc::new(|| Err("injected finish failure".into())),
        });
        token.register_lifecycle(binding).unwrap();
        assert!(token.wait_for_commit().await.is_err());
        assert!(token.is_cancelled());
        token.wait_for_commit().await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn drain_propagates_parent_flag_before_running_child_hooks() {
        let parent = CancelToken::new();
        let child = parent.child().unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let binding = Arc::new(ark_tools::TurnLifecycle {
            key: "child".into(),
            initialize: Arc::new(|| Ok(())),
            on_stop: {
                let n = stopped.clone();
                Arc::new(move || {
                    n.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
            on_finish: {
                let n = finished.clone();
                Arc::new(move || {
                    n.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        });
        child.register_lifecycle(binding).unwrap();
        parent.inner.flag.store(true, Ordering::SeqCst);
        parent.wait_for_commit().await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parent_cancellation_drains_each_attempt_and_refuses_new_attempts() {
        let parent = CancelToken::new();
        let first = parent.child().unwrap();
        let second = parent.child().unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for (index, child) in [first, second].iter().enumerate() {
            let count = stopped.clone();
            let binding = Arc::new(ark_tools::TurnLifecycle {
                key: format!("attempt-{index}"),
                initialize: Arc::new(|| Ok(())),
                on_stop: Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                on_finish: Arc::new(|| Ok(())),
            });
            assert!(child.register_lifecycle(binding).unwrap());
        }
        parent.cancel_and_wait().await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 2);
        assert!(parent.child().is_none());
    }

    #[tokio::test]
    async fn failed_initialization_remains_registered_and_failed_stop_retries() {
        use std::sync::atomic::AtomicUsize;
        let token = CancelToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let binding = Arc::new(ark_tools::TurnLifecycle {
            key: "project".into(),
            initialize: Arc::new(|| Err("injected initialization error".into())),
            on_stop: {
                let calls = calls.clone();
                Arc::new(move || {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("injected stop error".into())
                    } else {
                        Ok(())
                    }
                })
            },
            on_finish: Arc::new(|| Ok(())),
        });
        assert!(token.register_lifecycle(binding).is_err());
        assert!(token.cancel_and_wait().await.is_err());
        token.cancel_and_wait().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

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

    #[test]
    fn cancel_before_commit_refuses_it() {
        let token = CancelToken::new();
        token.cancel();
        let mut committed = false;
        assert!(!token.if_active(&mut || committed = true));
        assert!(!committed);
    }

    #[tokio::test]
    async fn panicking_commit_cancels_and_further_commits_are_refused() {
        let token = CancelToken::new();
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            token.if_active(&mut || panic!("fixture"));
        }));
        assert!(attempt.is_err());
        token.cancel_and_wait().await.unwrap();
        assert!(token.is_cancelled());
        assert!(!token.if_active(&mut || panic!("must not run")));
    }

    #[tokio::test]
    async fn cancel_waits_for_a_commit_that_started_first() {
        let token = CancelToken::new();
        let commit_token = token.clone();
        let (started, wait_started) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            commit_token.if_active(&mut || {
                started.send(()).unwrap();
                wait_release.recv().unwrap();
            })
        });
        wait_started.recv().unwrap();
        let cancel_token = token.clone();
        let (ack, wait_ack) = std::sync::mpsc::channel();
        let stopper = tokio::spawn(async move {
            cancel_token.cancel_and_wait().await.unwrap();
            ack.send(()).unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(wait_ack.try_recv().is_err());
        release.send(()).unwrap();
        assert!(worker.join().unwrap());
        stopper.await.unwrap();
        wait_ack.recv().unwrap();
        assert!(token.is_cancelled());
    }
}
