//! Provider capacity. A subscription allows a few requests at a time and limits how fast they
//! come; an API key has rate limits too. A [`Limiter`] holds one account's limits and makes
//! callers wait their turn instead of sending requests the provider will refuse. [`Limited`]
//! puts any [`Provider`] behind one.
//!
//! Waiting is first come, first served: a call that cannot start yet is queued, and the
//! queue length is visible in [`CapacitySnapshot`] so a front-end can show backpressure.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{Completion, Message, Provider, Result, TextSink, ToolDef};

/// What one account allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    /// Requests in flight at once.
    pub max_concurrent: usize,
    /// Minimum time between the starts of two requests, in milliseconds (0: none).
    #[serde(default)]
    pub min_interval_ms: u64,
    /// Vendor sessions kept open at once (subscriptions only). The least recently used idle
    /// one is released when a new one would go over.
    #[serde(default = "default_sessions")]
    pub max_sessions: usize,
}

fn default_sessions() -> usize {
    8
}

impl Capacity {
    pub fn concurrent(n: usize) -> Self {
        Capacity {
            max_concurrent: n.max(1),
            min_interval_ms: 0,
            max_sessions: default_sessions(),
        }
    }
    pub fn with_min_interval(mut self, ms: u64) -> Self {
        self.min_interval_ms = ms;
        self
    }
    pub fn with_max_sessions(mut self, n: usize) -> Self {
        self.max_sessions = n.max(1);
        self
    }

    /// The default for a provider name, overridable with `ARK_<NAME>_CONCURRENCY` (for
    /// example `ARK_CLAUDE_CONCURRENCY=3`). Subscriptions get 2 requests at a time; local and
    /// API providers 4.
    pub fn for_provider(name: &str) -> Self {
        let base = match name {
            "claude" | "codex" => 2,
            _ => 4,
        };
        let var = format!("ARK_{}_CONCURRENCY", name.to_ascii_uppercase());
        let n = std::env::var(var)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(base);
        Capacity::concurrent(n)
    }
}

/// A limiter's state right now, for display.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacitySnapshot {
    /// The account this limiter guards (`claude`, `ollama`, ...).
    pub account: String,
    pub max_concurrent: usize,
    pub in_flight: usize,
    /// Calls waiting for a slot.
    pub waiting: usize,
    /// Requests started since the limiter was made.
    pub started: u64,
    /// Milliseconds left in a cool-down the provider asked for (0 when none).
    #[serde(default)]
    pub cooling_ms: u64,
}

/// One account's limits, shared by every caller of that account.
pub struct Limiter {
    account: String,
    capacity: Capacity,
    slots: Arc<Semaphore>,
    waiting: AtomicUsize,
    started: AtomicU64,
    /// When the last request started, for `min_interval_ms`.
    last_start: tokio::sync::Mutex<Option<Instant>>,
    /// Requests may not start before this instant (a provider's rate limit).
    cool_until: Mutex<Option<Instant>>,
}

/// A slot in a [`Limiter`]. The slot frees when this is dropped.
pub struct Permit {
    _permit: OwnedSemaphorePermit,
}

impl Limiter {
    pub fn new(account: impl Into<String>, capacity: Capacity) -> Self {
        Limiter {
            account: account.into(),
            slots: Arc::new(Semaphore::new(capacity.max_concurrent.max(1))),
            capacity,
            waiting: AtomicUsize::new(0),
            started: AtomicU64::new(0),
            last_start: tokio::sync::Mutex::new(None),
            cool_until: Mutex::new(None),
        }
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn capacity(&self) -> Capacity {
        self.capacity
    }

    /// Wait for a slot, then for the minimum interval and any cool-down.
    pub async fn acquire(&self) -> Permit {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the limiter's semaphore is never closed");
        {
            let mut last = self.last_start.lock().await;
            let cool = *self.cool_until.lock().unwrap();
            let now = Instant::now();
            let mut start = now;
            if let Some(prev) = *last {
                start = start.max(prev + Duration::from_millis(self.capacity.min_interval_ms));
            }
            if let Some(c) = cool {
                start = start.max(c);
            }
            if start > now {
                tokio::time::sleep(start - now).await;
            }
            *last = Some(Instant::now());
        }
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        self.started.fetch_add(1, Ordering::SeqCst);
        Permit { _permit: permit }
    }

    /// Hold every request back for `wait` (the provider said it is rate limited).
    pub fn cool_down(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut c = self.cool_until.lock().unwrap();
        if c.is_none_or(|t| t < until) {
            *c = Some(until);
        }
    }

    pub fn snapshot(&self) -> CapacitySnapshot {
        let max = self.capacity.max_concurrent.max(1);
        let cooling_ms = self
            .cool_until
            .lock()
            .unwrap()
            .and_then(|t| t.checked_duration_since(Instant::now()))
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        CapacitySnapshot {
            account: self.account.clone(),
            max_concurrent: max,
            in_flight: max - self.slots.available_permits().min(max),
            waiting: self.waiting.load(Ordering::SeqCst),
            started: self.started.load(Ordering::SeqCst),
            cooling_ms,
        }
    }
}

/// When a failed call says the account is out of requests for now, how long to hold back.
pub(crate) fn rate_limit_wait(e: &crate::ProviderError) -> Option<Duration> {
    match e {
        crate::ProviderError::Status {
            status: 429,
            retry_after,
            ..
        } => Some(retry_after.unwrap_or(Duration::from_secs(10))),
        crate::ProviderError::Overloaded { .. } => Some(Duration::from_secs(5)),
        _ => None,
    }
}

/// A provider behind a [`Limiter`]: every call waits for a slot first.
pub struct Limited {
    inner: Arc<dyn Provider>,
    limiter: Arc<Limiter>,
}

impl Limited {
    pub fn new(inner: Arc<dyn Provider>, limiter: Arc<Limiter>) -> Self {
        Limited { inner, limiter }
    }
    pub fn limiter(&self) -> &Arc<Limiter> {
        &self.limiter
    }
}

#[async_trait]
impl Provider for Limited {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Completion> {
        let _slot = self.limiter.acquire().await;
        let r = self.inner.complete(system, messages, tools).await;
        if let Some(wait) = r.as_ref().err().and_then(rate_limit_wait) {
            self.limiter.cool_down(wait);
        }
        r
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        on_text: TextSink<'_>,
    ) -> Result<Completion> {
        let _slot = self.limiter.acquire().await;
        let r = self
            .inner
            .complete_streaming(system, messages, tools, on_text)
            .await;
        if let Some(wait) = r.as_ref().err().and_then(rate_limit_wait) {
            self.limiter.cool_down(wait);
        }
        r
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }

    fn model_id(&self) -> String {
        self.inner.model_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;

    #[tokio::test]
    async fn never_more_in_flight_than_allowed() {
        let limiter = Arc::new(Limiter::new("acct", Capacity::concurrent(2)));
        let peak = Arc::new(AtomicUsize::new(0));
        let now = Arc::new(AtomicUsize::new(0));
        let mut jobs = Vec::new();
        for _ in 0..6 {
            let (l, peak, now) = (limiter.clone(), peak.clone(), now.clone());
            jobs.push(tokio::spawn(async move {
                let _p = l.acquire().await;
                let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                now.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        let snap = limiter.snapshot();
        assert_eq!(snap.in_flight, 2);
        assert_eq!(snap.waiting, 4, "the rest queue");
        for j in jobs {
            j.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(limiter.snapshot().started, 6);
        assert_eq!(limiter.snapshot().waiting, 0);
    }

    #[tokio::test]
    async fn min_interval_spaces_starts() {
        let limiter = Limiter::new("acct", Capacity::concurrent(4).with_min_interval(30));
        let t = Instant::now();
        for _ in 0..3 {
            let _p = limiter.acquire().await;
        }
        assert!(
            t.elapsed() >= Duration::from_millis(60),
            "{:?}",
            t.elapsed()
        );
    }

    #[tokio::test]
    async fn limited_provider_passes_calls_through() {
        let mock = Arc::new(MockProvider::new(
            "m",
            vec![Completion {
                content: "hi".into(),
                ..Default::default()
            }],
        ));
        let limiter = Arc::new(Limiter::new("m", Capacity::concurrent(1)));
        let p = Limited::new(mock.clone(), limiter.clone());
        let c = p.complete("", &[Message::user("q")], &[]).await.unwrap();
        assert_eq!(c.content, "hi");
        assert_eq!(p.model_id(), "m");
        assert_eq!(limiter.snapshot().started, 1);
    }

    #[test]
    fn rate_limits_cool_down() {
        let l = Limiter::new("a", Capacity::concurrent(1));
        l.cool_down(Duration::from_secs(5));
        assert!(l.snapshot().cooling_ms > 4000);
        let e = crate::ProviderError::Status {
            provider: "x".into(),
            status: 429,
            detail: String::new(),
            retry_after: Some(Duration::from_secs(3)),
        };
        assert_eq!(rate_limit_wait(&e), Some(Duration::from_secs(3)));
    }
}
